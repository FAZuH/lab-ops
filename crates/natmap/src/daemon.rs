use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::delete;
use axum::routing::get;
use axum::routing::post;
use axum::routing::put;
use bollard::Docker;
use bollard::query_parameters::EventsOptions;
use bollard::query_parameters::ListContainersOptions;
use color_eyre::Result;
use futures_util::stream::StreamExt;
use serde::Serialize;
use tokio::sync::RwLock;
use tracing::error;
use tracing::info;

use crate::docker;
use crate::iptables::IptablesManager;
use crate::models::ActivePortMapping;
use crate::models::AddMappingRequest;
use crate::models::PortMappingRequest;
use crate::models::RemapRequest;
use crate::models::TransportProtocol;

#[derive(Clone)]
pub struct AppState {
    pub mappings: Arc<RwLock<HashMap<String, Vec<ActivePortMapping>>>>,
    pub iptables: Arc<IptablesManager>,
    pub docker: Docker,
    pub state_path: PathBuf,
    pub next_id: Arc<AtomicU64>,
}

impl AppState {
    fn allocate_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

const DEFAULT_STATE_FILE: &str = "/var/lib/natmap/state.json";
const DEFAULT_SOCKET_PATH: &str = "/run/natmap.sock";

pub async fn run_daemon() -> Result<()> {
    run_daemon_with_paths(DEFAULT_SOCKET_PATH, DEFAULT_STATE_FILE, "natmap").await
}

pub async fn run_daemon_with_paths(
    socket_path: &str,
    state_file: &str,
    socket_group: &str,
) -> Result<()> {
    info!("Starting natmap daemon...");

    // Initialize dependencies
    let docker_client = docker::connect()?;
    let iptables = Arc::new(IptablesManager::new());

    // Create state dir if not exists
    let state_path = PathBuf::from(state_file);
    let state_dir = state_path.parent().unwrap();
    if !state_dir.exists() {
        fs::create_dir_all(state_dir).map_err(|e| {
            color_eyre::eyre::eyre!(
                "Failed to create state directory {}: {e}\n\
                 This daemon needs root (or CAP_NET_ADMIN + write access to /var/lib).\n\
                 Try: sudo lab-ops natmap daemon\n\
                 Or for testing: lab-ops natmap daemon --state-dir /tmp/natmap --socket /tmp/natmap.sock",
                state_dir.display()
            )
        })?;
    }

    // Setup chains
    iptables.setup().map_err(|e| {
        color_eyre::eyre::eyre!(
            "Failed to set up iptables chains: {e}\n\
             This daemon needs root (or CAP_NET_ADMIN) to manage iptables.\n\
             Try: sudo lab-ops natmap daemon"
        )
    })?;

    let mappings = Arc::new(RwLock::new(HashMap::new()));
    let state = AppState {
        mappings: mappings.clone(),
        iptables: iptables.clone(),
        docker: docker_client.clone(),
        state_path: state_path.clone(),
        next_id: Arc::new(AtomicU64::new(1)),
    };

    // Reload state on startup
    reload_state(&state).await?;

    // Spawn docker event listener
    let state_clone = state.clone();
    tokio::spawn(async move {
        if let Err(e) = listen_docker_events(state_clone).await {
            error!("Docker listener exited with error: {}", e);
        }
    });

    // Start API server on unix socket
    let app = Router::new()
        .route("/mappings", get(list_mappings))
        .route("/remap/:container_id", put(remap_port))
        .route("/mapping/:container_id", post(add_mapping))
        .route("/mapping/{container_id}/{port}", delete(remove_mapping))
        .route("/mapping/by-id/:id", delete(remove_mapping_by_id))
        .with_state(state);

    let socket_path_str = socket_path.to_string();
    if std::path::Path::new(&socket_path_str).exists() {
        let _ = fs::remove_file(&socket_path_str);
    }

    let listener = tokio::net::UnixListener::bind(&socket_path_str).map_err(|e| {
        let sp = socket_path_str.clone();
        color_eyre::eyre::eyre!(
            "Failed to bind Unix socket at {sp}: {e}\n\
             This daemon needs root to bind to /run.\n\
             Try: sudo lab-ops natmap daemon\n\
             Or for testing: lab-ops natmap daemon --state-dir /tmp/natmap --socket /tmp/natmap.sock"
        )
    })?;

    // Set socket permissions so non-root users in the group can access it
    let _ = std::process::Command::new("chown")
        .args([format!("root:{}", socket_group), socket_path_str.clone()])
        .status();
    let _ = std::process::Command::new("chmod")
        .args(["660", &socket_path_str])
        .status();

    info!("Listening on unix socket: {}", socket_path_str);

    use hyper_util::rt::TokioExecutor;
    use hyper_util::rt::TokioIo;
    use hyper_util::server::conn::auto::Builder;
    use tower_service::Service;

    loop {
        let (socket, _) = listener.accept().await?;
        let tower_service = app.clone();

        tokio::spawn(async move {
            let socket = TokioIo::new(socket);

            let hyper_service = hyper::service::service_fn(
                move |request: hyper::Request<hyper::body::Incoming>| {
                    tower_service.clone().call(request)
                },
            );

            if let Err(err) = Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(socket, hyper_service)
                .await
            {
                error!("failed to serve connection: {err:#}");
            }
        });
    }

    #[allow(unreachable_code)]
    Ok(())
}

async fn reload_state(state: &AppState) -> Result<()> {
    info!("Reloading state from {}", state.state_path.display());

    // Load JSON state
    let mut stored_mappings: HashMap<String, Vec<ActivePortMapping>> = HashMap::new();
    if state.state_path.exists()
        && let Ok(data) = fs::read_to_string(&state.state_path)
        && let Ok(parsed) = serde_json::from_str(&data)
    {
        stored_mappings = parsed;
    }

    // Reconcile with actual containers
    let mut active = HashMap::new();

    let opts = ListContainersOptions {
        all: false,
        ..Default::default()
    };

    let containers = state.docker.list_containers(Some(opts)).await?;
    let mut running_ids = HashSet::new();
    for c in containers {
        if let Some(id) = c.id {
            running_ids.insert(id);
        }
    }

    // Purge old mappings from store and from iptables
    let mut max_id: u64 = 0;
    for (container_id, container_mappings) in stored_mappings.into_iter() {
        if !running_ids.contains(&container_id) {
            info!(
                "Container {} no longer running, flushing rules",
                container_id
            );
            let _ = state.iptables.flush_container_rules(&container_id);
        } else {
            for mapping in &container_mappings {
                let _ = state.iptables.install_mapping(mapping);
                if mapping.id > max_id {
                    max_id = mapping.id;
                }
            }
            active.insert(container_id, container_mappings);
        }
    }

    // Set next_id past the highest known id
    state
        .next_id
        .store(max_id.saturating_add(1), Ordering::SeqCst);

    *state.mappings.write().await = active;
    persist_state(state).await;

    Ok(())
}

async fn persist_state(state: &AppState) {
    let data = {
        let lock = state.mappings.read().await;
        serde_json::to_string(&*lock).unwrap_or_default()
    };

    let tmp = state.state_path.with_extension("tmp");
    if fs::write(&tmp, data).is_ok() {
        let _ = fs::rename(&tmp, &state.state_path);
    }
}

async fn listen_docker_events(state: AppState) -> Result<()> {
    let opts = EventsOptions {
        since: None,
        until: None,
        filters: Some(
            [("type".to_string(), vec!["container".to_string()])]
                .into_iter()
                .collect(),
        ),
    };

    let mut stream = state.docker.events(Some(opts));

    while let Some(msg) = stream.next().await {
        let event = match msg {
            Ok(e) => e,
            Err(_) => continue,
        };

        let action = event.action.unwrap_or_default();
        let actor = match event.actor {
            Some(a) => a,
            None => continue,
        };
        let container_id = actor.id.unwrap_or_default();

        if action == "start" || action == "network connect" {
            info!(
                "Container {} started or connected to network. Parsing mappings.",
                container_id
            );
            if let Ok(discovered) = docker::get_port_mappings(&state.docker, &container_id).await {
                // Assign IDs and install rules
                let mut assigned = Vec::new();
                for mut m in discovered {
                    m.id = state.allocate_id();
                    if let Err(e) = state.iptables.install_mapping(&m) {
                        error!("Failed to install mapping {:?}: {}", m, e);
                        let _ = state.iptables.remove_mapping(&m);
                    }
                    assigned.push(m);
                }

                // Merge with existing: keep manual mappings, add/refresh auto-discovered
                let mut lock = state.mappings.write().await;
                let existing = lock.entry(container_id.clone()).or_default();
                let auto_comments: HashSet<String> =
                    assigned.iter().map(|m| m.rule_comment.clone()).collect();
                existing.retain(|m| !auto_comments.contains(&m.rule_comment));
                existing.extend(assigned);
                drop(lock);
                persist_state(&state).await;
            }
        } else if action == "die" || action == "kill" || action == "network disconnect" {
            info!(
                "Container {} died or disconnected. Flushing rules.",
                container_id
            );
            let _ = state.iptables.flush_container_rules(&container_id);
            state.mappings.write().await.remove(&container_id);
            persist_state(&state).await;
        }
    }

    Ok(())
}

// API Routes

async fn list_mappings(State(state): State<AppState>) -> Json<Vec<ActivePortMapping>> {
    let mut res = Vec::new();
    for mappings in state.mappings.read().await.values() {
        res.extend(mappings.iter().cloned());
    }
    Json(res)
}

async fn remap_port(
    State(state): State<AppState>,
    Path(container_id): Path<String>,
    Json(req): Json<RemapRequest>,
) -> Result<Json<Vec<ActivePortMapping>>, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.mappings.write().await;

    let container_mappings = match lock.get_mut(&container_id) {
        Some(m) => m,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "Container not found".into(),
                }),
            ));
        }
    };

    let mut to_replace = Vec::new();
    for (i, m) in container_mappings.iter().enumerate() {
        if m.request.host_addr.port() == req.host_port {
            to_replace.push(i);
        }
    }

    if to_replace.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Port mapping not found".into(),
            }),
        ));
    }

    let mut new_mappings = Vec::new();

    for i in to_replace {
        let old = &container_mappings[i];
        let mut new_req = old.request.clone();
        new_req.host_addr.set_port(req.new_host_port);
        let id = state.allocate_id();
        let new_mapping = ActivePortMapping::new(
            id,
            new_req,
            container_id.clone(),
            old.container_name.clone(),
        );

        // Remove old
        let _ = state.iptables.remove_mapping(old);
        // Install new
        if let Err(e) = state.iptables.install_mapping(&new_mapping) {
            // rollback
            let _ = state.iptables.remove_mapping(&new_mapping);
            let _ = state.iptables.install_mapping(old);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            ));
        }

        container_mappings[i] = new_mapping.clone();
        new_mappings.push(new_mapping);
    }

    drop(lock);
    persist_state(&state).await;

    Ok(Json(new_mappings))
}

async fn add_mapping(
    State(state): State<AppState>,
    Path(container_id): Path<String>,
    Json(req): Json<AddMappingRequest>,
) -> Result<Json<ActivePortMapping>, (StatusCode, Json<ErrorResponse>)> {
    let inspect = state
        .docker
        .inspect_container(&container_id, None)
        .await
        .map_err(|e| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("Container not found: {e}"),
                }),
            )
        })?;

    let container_name = inspect
        .name
        .as_deref()
        .unwrap_or("unknown")
        .trim_start_matches('/')
        .to_string();

    let network_settings = inspect.network_settings.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Container has no network settings".into(),
            }),
        )
    })?;

    let container_ip = match network_settings.networks {
        Some(ref nets) => nets.values().find_map(|net| {
            net.ip_address
                .as_deref()
                .and_then(|ip| IpAddr::from_str(ip).ok())
        }),
        None => None,
    }
    .ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Container has no IP address".into(),
            }),
        )
    })?;

    let proto = match req.proto.to_lowercase().as_str() {
        "tcp" => TransportProtocol::Tcp,
        "udp" => TransportProtocol::Udp,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("Unsupported protocol: {other}"),
                }),
            ));
        }
    };

    let host_ip = IpAddr::from_str(&req.host_ip).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Invalid host IP: {e}"),
            }),
        )
    })?;

    let host_addr = SocketAddr::new(host_ip, req.host_port);
    let container_addr = SocketAddr::new(container_ip, req.container_port);

    let request = PortMappingRequest {
        host_addr,
        container_addr,
        proto,
    };
    let id = state.allocate_id();
    let mapping = ActivePortMapping::new(id, request, container_id.clone(), container_name);

    state.iptables.install_mapping(&mapping).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("iptables error: {e}"),
            }),
        )
    })?;

    state
        .mappings
        .write()
        .await
        .entry(container_id)
        .or_default()
        .push(mapping.clone());
    persist_state(&state).await;

    Ok(Json(mapping))
}

async fn remove_mapping(
    State(state): State<AppState>,
    Path((container_id, port_str)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let port = port_str.parse::<u16>().unwrap_or(0);

    let mut lock = state.mappings.write().await;
    let container_mappings = match lock.get_mut(&container_id) {
        Some(m) => m,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "Container not found".into(),
                }),
            ));
        }
    };

    let mut removed = false;
    container_mappings.retain(|m| {
        if m.request.host_addr.port() == port {
            let _ = state.iptables.remove_mapping(m);
            removed = true;
            false
        } else {
            true
        }
    });

    if !removed {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Port mapping not found".into(),
            }),
        ));
    }

    drop(lock);
    persist_state(&state).await;

    Ok(StatusCode::OK)
}

async fn remove_mapping_by_id(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.mappings.write().await;

    let mut removed = false;
    for (_, mappings) in lock.iter_mut() {
        if let Some(pos) = mappings.iter().position(|m| m.id == id) {
            let m = mappings.remove(pos);
            let _ = state.iptables.remove_mapping(&m);
            removed = true;
            break;
        }
    }

    if !removed {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("No mapping found with id {id}"),
            }),
        ));
    }

    drop(lock);
    persist_state(&state).await;

    Ok(StatusCode::OK)
}
