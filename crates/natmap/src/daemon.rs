//! Natmap daemon — HTTP API server over Unix socket.
//!
//! The daemon is the central authority for all iptables NAT rules. It:
//!
//! - Hosts an HTTP API on a Unix socket (`/run/natmap.sock`)
//! - Auto-discovers Docker container ports on start/stop events
//! - Persists state to JSON and recovers after crashes
//! - Prevents port conflicts using [`PortAllocator`]

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
use crate::models::*;
use crate::port_allocator::PortAllocator;

/// Shared application state held by all Axum route handlers.
#[derive(Clone)]
pub struct AppState {
    /// The in-memory daemon state.
    pub state: Arc<RwLock<DaemonState>>,
    /// iptables rule manager.
    pub iptables: Arc<IptablesManager>,
    /// Docker client (None if Docker is unavailable).
    pub docker: Option<Docker>,
    /// Filesystem path for persisting state to JSON.
    pub state_path: PathBuf,
    /// Auto-incrementing ID counter for mapping entries.
    pub next_id: Arc<AtomicU64>,
    /// Port reservation system for conflict prevention.
    pub ports: Arc<PortAllocator>,
}

impl AppState {
    /// Returns the next unique mapping ID and advances the counter.
    fn allocate_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Generates a port reservation key in the format `"<ip>:<port>"`.
    fn port_key(ip: &str, port: u16) -> String {
        format!("{}:{}", ip, port)
    }

    /// Reserves all ports from a comma-separated port list using the port allocator.
    async fn bind_ports(
        ports: &PortAllocator,
        ip: &str,
        ports_csv: &str,
    ) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
        for p in ports_csv.split(',') {
            let p: u16 = p.trim().parse().map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: format!("Invalid port: {}", p),
                    }),
                )
            })?;
            let addr = SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), p);
            ports
                .allocate(&Self::port_key(ip, p), addr)
                .await
                .map_err(|e| {
                    (
                        StatusCode::CONFLICT,
                        Json(ErrorResponse {
                            error: e.to_string(),
                        }),
                    )
                })?;
        }
        Ok(())
    }

    /// Releases all ports from a comma-separated port list from the port allocator.
    async fn unbind_ports(ports: &PortAllocator, ip: &str, ports_csv: &str) {
        for p in ports_csv.split(',') {
            if let Ok(p) = p.trim().parse::<u16>() {
                ports.deallocate(&Self::port_key(ip, p)).await;
            }
        }
    }
}

/// JSON error response returned by the daemon API on failures.
#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

const DEFAULT_STATE_FILE: &str = "/var/lib/natmap/state.json";
const DEFAULT_SOCKET_PATH: &str = "/run/natmap.sock";

/// Runs the daemon with default paths (`/run/natmap.sock`, `/var/lib/natmap/state.json`).
pub async fn run_daemon() -> Result<()> {
    run_daemon_with_paths(DEFAULT_SOCKET_PATH, DEFAULT_STATE_FILE, "natmap").await
}

/// Runs the natmap daemon with explicit paths for the socket, state file, and group.
///
/// Sets up iptables chains, loads persisted state, spawns Docker event listeners,
/// installs a Ctrl-C handler for clean shutdown, and starts the HTTP API server.
pub async fn run_daemon_with_paths(
    socket_path: &str,
    state_file: &str,
    socket_group: &str,
) -> Result<()> {
    info!("Starting natmap daemon...");

    let docker_client = docker::connect().ok();
    if docker_client.is_none() {
        info!("Docker not available — running without Docker support");
    }
    let iptables = Arc::new(IptablesManager::new());

    let state_path = PathBuf::from(state_file);
    let state_dir = state_path.parent().unwrap();
    if !state_dir.exists() {
        fs::create_dir_all(state_dir).map_err(|e| {
            color_eyre::eyre::eyre!(
                "Failed to create state directory {}: {e}",
                state_dir.display()
            )
        })?;
    }

    iptables
        .setup()
        .map_err(|e| color_eyre::eyre::eyre!("Failed to set up iptables chains: {e}"))?;

    let ports = Arc::new(PortAllocator::new());
    let daemon_state = Arc::new(RwLock::new(DaemonState::default()));

    let state = AppState {
        state: daemon_state.clone(),
        iptables: iptables.clone(),
        docker: docker_client,
        state_path: state_path.clone(),
        next_id: Arc::new(AtomicU64::new(1)),
        ports: ports.clone(),
    };

    reload_state(&state, &iptables, &ports).await?;

    if state.docker.is_some() {
        let state_clone = state.clone();
        tokio::spawn(async move {
            if let Err(e) = listen_docker_events(state_clone).await {
                error!("Docker listener exited with error: {}", e);
            }
        });
    }

    let shutdown_state = state.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Shutting down: flushing iptables rules...");
        let _ = shutdown_state.iptables.flush_all_natmap();
        shutdown_state.ports.deallocate_all().await;
        info!("Shutdown complete.");
        std::process::exit(0);
    });

    let app = Router::new()
        .route("/mappings", get(list_mappings))
        .route("/remap/:container_id", put(remap_port))
        .route("/mapping/:container_id", post(add_mapping))
        .route("/mapping/{container_id}/{port}", delete(remove_mapping))
        .route("/mapping/by-id/:id", delete(remove_mapping_by_id))
        .route("/dnat", post(add_dnat))
        .route("/dnat", delete(remove_dnat))
        .route("/snat", post(add_snat))
        .route("/snat", delete(remove_snat))
        .route("/hairpin", post(add_hairpin))
        .route("/hairpin", delete(remove_hairpin))
        .with_state(state);

    let socket_path_str = socket_path.to_string();
    if std::path::Path::new(&socket_path_str).exists() {
        let _ = fs::remove_file(&socket_path_str);
    }

    let listener = tokio::net::UnixListener::bind(&socket_path_str).map_err(|e| {
        color_eyre::eyre::eyre!("Failed to bind Unix socket at {}: {e}", socket_path_str)
    })?;

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

/// Loads persisted state from disk and reconciles with the current system state.
///
/// Flushes stale iptables rules, releases old port reservations, and re-installs
/// rules for surviving containers and static configurations.
async fn reload_state(
    state: &AppState,
    iptables: &IptablesManager,
    ports: &PortAllocator,
) -> Result<()> {
    info!("Crash recovery: flushing stale iptables rules");
    let _ = iptables.flush_all_natmap();
    ports.deallocate_all().await;

    let mut daemon_state: DaemonState = if state.state_path.exists()
        && let Ok(data) = fs::read_to_string(&state.state_path)
    {
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        DaemonState::default()
    };

    // Reconcile Docker mappings
    if let Some(docker) = &state.docker {
        let containers = docker
            .list_containers(Some(ListContainersOptions {
                all: false,
                ..Default::default()
            }))
            .await?;
        let running_ids: HashSet<String> = containers.into_iter().filter_map(|c| c.id).collect();

        let mut max_id: u64 = 0;
        let docker_entries: Vec<(String, Vec<ActivePortMapping>)> =
            daemon_state.docker.drain().collect();
        let mut new_docker = HashMap::new();

        for (container_id, mappings) in docker_entries {
            if !running_ids.contains(&container_id) {
                info!("Container {} gone, removing mappings", container_id);
                continue;
            }
            let mut kept = Vec::new();
            for m in mappings {
                if m.id > max_id {
                    max_id = m.id;
                }
                let key = AppState::port_key(
                    &m.request.host_addr.ip().to_string(),
                    m.request.host_addr.port(),
                );
                if ports.is_allocated(&key).await {
                    info!("Port {} already held, removing stale mapping", key);
                    continue;
                }
                match ports.allocate(&key.clone(), m.request.host_addr).await {
                    Ok(()) => {
                        let _ = iptables.install_mapping(&m);
                        kept.push(m);
                    }
                    Err(e) => error!("Port {} in use, dropping mapping: {}", key, e),
                }
            }
            if !kept.is_empty() {
                new_docker.insert(container_id, kept);
            }
        }
        daemon_state.docker = new_docker;
        state
            .next_id
            .store(max_id.saturating_add(1), Ordering::SeqCst);
    }

    // Reconcile static DNATs
    let mut kept_dnats = Vec::new();
    for config in daemon_state.dnats.drain(..) {
        let mut ok = true;
        for p in config.ports.split(',') {
            if let Ok(port) = p.trim().parse::<u16>() {
                let addr = SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), port);
                let key = AppState::port_key(&config.ext_ip, port);
                if ports.is_allocated(&key).await {
                    continue;
                }
                if let Err(e) = ports.allocate(&key, addr).await {
                    error!("DNAT port {} in use, dropping: {}", port, e);
                    ok = false;
                }
            }
        }
        if ok {
            let _ = iptables.install_dnat(&config);
            kept_dnats.push(config);
        } else {
            AppState::unbind_ports(ports, &config.ext_ip, &config.ports).await;
        }
    }
    daemon_state.dnats = kept_dnats;

    // Reconcile static SNATs (no port binding)
    for config in &daemon_state.snats {
        let _ = iptables.install_snat(config);
    }

    // Reconcile static hairpins
    let mut kept_hairpins = Vec::new();
    for config in daemon_state.hairpins.drain(..) {
        let mut ok = true;
        for p in config.ports.split(',') {
            if let Ok(port) = p.trim().parse::<u16>() {
                let addr = SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), port);
                let key = AppState::port_key(&config.ext_ip, port);
                if ports.is_allocated(&key).await {
                    continue;
                }
                if let Err(e) = ports.allocate(&key, addr).await {
                    error!("Hairpin port {} in use, dropping: {}", port, e);
                    ok = false;
                }
            }
        }
        if ok {
            let _ = iptables.install_hairpin(&config);
            kept_hairpins.push(config);
        } else {
            AppState::unbind_ports(ports, &config.ext_ip, &config.ports).await;
        }
    }
    daemon_state.hairpins = kept_hairpins;

    *state.state.write().await = daemon_state;
    persist_state(state).await;
    Ok(())
}

/// Writes the current daemon state to disk (atomically via a temp file).
async fn persist_state(state: &AppState) {
    let data = {
        let lock = state.state.read().await;
        serde_json::to_string(&*lock).unwrap_or_default()
    };
    let tmp = state.state_path.with_extension("tmp");
    if fs::write(&tmp, data).is_ok() {
        let _ = fs::rename(&tmp, &state.state_path);
    }
}

/// Listens for Docker container events and automatically manages port mappings.
///
/// On `start` / `network connect`: discovers published ports and installs rules.
/// On `die` / `kill` / `network disconnect`: removes all rules for the container.
async fn listen_docker_events(state: AppState) -> Result<()> {
    let docker = state.docker.as_ref().expect("Docker not available");
    let opts = EventsOptions {
        since: None,
        until: None,
        filters: Some(
            [("type".to_string(), vec!["container".to_string()])]
                .into_iter()
                .collect(),
        ),
    };
    let mut stream = docker.events(Some(opts));

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
            info!("Container {} started, parsing mappings", container_id);
            if let Ok(discovered) = docker::get_port_mappings(docker, &container_id).await {
                let mut assigned = Vec::new();
                for mut m in discovered {
                    m.id = state.allocate_id();
                    let key = AppState::port_key(
                        &m.request.host_addr.ip().to_string(),
                        m.request.host_addr.port(),
                    );
                    if state.ports.is_allocated(&key).await {
                        info!("Port {} already allocated, skipping", key);
                        continue;
                    }
                    if let Err(e) = state
                        .ports
                        .allocate(&key.clone(), m.request.host_addr)
                        .await
                    {
                        error!("Port {} in use, skipping: {}", key, e);
                        continue;
                    }
                    if let Err(e) = state.iptables.install_mapping(&m) {
                        error!("Failed to install mapping {:?}: {}", m, e);
                        state.ports.deallocate(&key).await;
                        continue;
                    }
                    assigned.push(m);
                }
                let mut lock = state.state.write().await;
                let existing = lock.docker.entry(container_id.clone()).or_default();
                let auto_comments: HashSet<String> =
                    assigned.iter().map(|m| m.rule_comment.clone()).collect();
                existing.retain(|m| !auto_comments.contains(&m.rule_comment));
                existing.extend(assigned);
                drop(lock);
                persist_state(&state).await;
            }
        } else if action == "die" || action == "kill" || action == "network disconnect" {
            info!("Container {} died, flushing rules", container_id);
            let mut lock = state.state.write().await;
            if let Some(mappings) = lock.docker.remove(&container_id) {
                for m in &mappings {
                    let _ = state.iptables.remove_mapping(m);
                    state
                        .ports
                        .deallocate(&AppState::port_key(
                            &m.request.host_addr.ip().to_string(),
                            m.request.host_addr.port(),
                        ))
                        .await;
                }
            }
            drop(lock);
            persist_state(&state).await;
        }
    }
    Ok(())
}

// --- API Routes ---

/// `GET /mappings` — Returns all managed DNAT, SNAT, hairpin, and Docker mappings.
async fn list_mappings(State(state): State<AppState>) -> Json<ListResponse> {
    let lock = state.state.read().await;
    let mut docker_list = Vec::new();
    for mappings in lock.docker.values() {
        docker_list.extend(mappings.iter().cloned());
    }
    Json(ListResponse {
        docker: docker_list,
        dnats: lock.dnats.clone(),
        snats: lock.snats.clone(),
        hairpins: lock.hairpins.clone(),
    })
}

// --- Static NAT handlers ---

/// `POST /dnat` — Adds a static DNAT rule.
async fn add_dnat(
    State(state): State<AppState>,
    Json(req): Json<DnatRequest>,
) -> Result<Json<DnatConfig>, (StatusCode, Json<ErrorResponse>)> {
    let config = DnatConfig {
        ext_ip: req.ext_ip.clone(),
        int_ip: req.int_ip.clone(),
        ports: req.ports.clone(),
        proto: req.proto.clone(),
        ext_if: req.ext_if.clone(),
    };
    AppState::bind_ports(&state.ports, &config.ext_ip, &config.ports).await?;
    if let Err(e) = state.iptables.install_dnat(&config) {
        AppState::unbind_ports(&state.ports, &config.ext_ip, &config.ports).await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        ));
    }
    state.state.write().await.dnats.push(config.clone());
    persist_state(&state).await;
    Ok(Json(config))
}

/// `DELETE /dnat` — Removes a static DNAT rule.
async fn remove_dnat(
    State(state): State<AppState>,
    Json(req): Json<DnatRequest>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.state.write().await;
    let idx = lock
        .dnats
        .iter()
        .position(|d| d.ext_ip == req.ext_ip && d.int_ip == req.int_ip && d.ports == req.ports);
    if let Some(i) = idx {
        let config = lock.dnats.remove(i);
        let _ = state.iptables.remove_dnat(&config);
        AppState::unbind_ports(&state.ports, &config.ext_ip, &config.ports).await;
        drop(lock);
        persist_state(&state).await;
        Ok(StatusCode::OK)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "DNAT rule not found".into(),
            }),
        ))
    }
}

/// `POST /snat` — Adds a static SNAT rule.
async fn add_snat(
    State(state): State<AppState>,
    Json(req): Json<SnatRequest>,
) -> Result<Json<SnatConfig>, (StatusCode, Json<ErrorResponse>)> {
    let config = SnatConfig {
        int_ip: req.int_ip.clone(),
        ext_ip: req.ext_ip.clone(),
        ext_if: req.ext_if.clone(),
    };
    state.iptables.install_snat(&config).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;
    state.state.write().await.snats.push(config.clone());
    persist_state(&state).await;
    Ok(Json(config))
}

/// `DELETE /snat` — Removes a static SNAT rule.
async fn remove_snat(
    State(state): State<AppState>,
    Json(req): Json<SnatRequest>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.state.write().await;
    let idx = lock
        .snats
        .iter()
        .position(|s| s.int_ip == req.int_ip && s.ext_ip == req.ext_ip && s.ext_if == req.ext_if);
    if let Some(i) = idx {
        let config = lock.snats.remove(i);
        let _ = state.iptables.remove_snat(&config);
        drop(lock);
        persist_state(&state).await;
        Ok(StatusCode::OK)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "SNAT rule not found".into(),
            }),
        ))
    }
}

/// `POST /hairpin` — Adds a static hairpin NAT rule.
async fn add_hairpin(
    State(state): State<AppState>,
    Json(req): Json<HairpinRequest>,
) -> Result<Json<HairpinConfig>, (StatusCode, Json<ErrorResponse>)> {
    let config = HairpinConfig {
        ext_ip: req.ext_ip.clone(),
        int_ip: req.int_ip.clone(),
        ports: req.ports.clone(),
        proto: req.proto.clone(),
    };
    AppState::bind_ports(&state.ports, &config.ext_ip, &config.ports).await?;
    if let Err(e) = state.iptables.install_hairpin(&config) {
        AppState::unbind_ports(&state.ports, &config.ext_ip, &config.ports).await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        ));
    }
    state.state.write().await.hairpins.push(config.clone());
    persist_state(&state).await;
    Ok(Json(config))
}

/// `DELETE /hairpin` — Removes a static hairpin NAT rule.
async fn remove_hairpin(
    State(state): State<AppState>,
    Json(req): Json<HairpinRequest>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.state.write().await;
    let idx = lock
        .hairpins
        .iter()
        .position(|h| h.ext_ip == req.ext_ip && h.int_ip == req.int_ip && h.ports == req.ports);
    if let Some(i) = idx {
        let config = lock.hairpins.remove(i);
        let _ = state.iptables.remove_hairpin(&config);
        AppState::unbind_ports(&state.ports, &config.ext_ip, &config.ports).await;
        drop(lock);
        persist_state(&state).await;
        Ok(StatusCode::OK)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Hairpin rule not found".into(),
            }),
        ))
    }
}

// --- Docker handlers ---

/// `PUT /remap/:container_id` — Remaps a host port for a running container.
async fn remap_port(
    State(state): State<AppState>,
    Path(container_id): Path<String>,
    Json(req): Json<RemapRequest>,
) -> Result<Json<Vec<ActivePortMapping>>, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.state.write().await;
    let container_mappings = lock.docker.get_mut(&container_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Container not found".into(),
            }),
        )
    })?;

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
        let new_key = AppState::port_key(
            &new_mapping.request.host_addr.ip().to_string(),
            new_mapping.request.host_addr.port(),
        );
        if let Err(e) = state
            .ports
            .allocate(&new_key, new_mapping.request.host_addr)
            .await
        {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            ));
        }
        let _ = state.iptables.remove_mapping(old);
        if let Err(e) = state.iptables.install_mapping(&new_mapping) {
            let _ = state.iptables.install_mapping(old);
            state.ports.deallocate(&new_key).await;
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            ));
        }
        state
            .ports
            .deallocate(&AppState::port_key(
                &old.request.host_addr.ip().to_string(),
                old.request.host_addr.port(),
            ))
            .await;
        container_mappings[i] = new_mapping.clone();
        new_mappings.push(new_mapping);
    }

    drop(lock);
    persist_state(&state).await;
    Ok(Json(new_mappings))
}

/// `POST /mapping/:container_id` — Adds a new port mapping to a running container.
async fn add_mapping(
    State(state): State<AppState>,
    Path(container_id): Path<String>,
    Json(req): Json<AddMappingRequest>,
) -> Result<Json<ActivePortMapping>, (StatusCode, Json<ErrorResponse>)> {
    let docker = state.docker.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "Docker not available".into(),
            }),
        )
    })?;
    let inspect = docker
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
    let container_ip = network_settings
        .networks
        .as_ref()
        .and_then(|nets| {
            nets.values().find_map(|net| {
                net.ip_address
                    .as_deref()
                    .and_then(|ip| IpAddr::from_str(ip).ok())
            })
        })
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

    let key = AppState::port_key(&host_ip.to_string(), req.host_port);
    state
        .ports
        .allocate(&key.clone(), host_addr)
        .await
        .map_err(|e| {
            (
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;
    if let Err(e) = state.iptables.install_mapping(&mapping) {
        state.ports.deallocate(&key).await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("iptables error: {e}"),
            }),
        ));
    }
    state
        .state
        .write()
        .await
        .docker
        .entry(container_id)
        .or_default()
        .push(mapping.clone());
    persist_state(&state).await;
    Ok(Json(mapping))
}

/// `DELETE /mapping/{container_id}/{port}` — Removes a specific port mapping by container and port.
async fn remove_mapping(
    State(state): State<AppState>,
    Path((container_id, port_str)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let port = port_str.parse::<u16>().unwrap_or(0);
    let mut lock = state.state.write().await;
    let container_mappings = lock.docker.get_mut(&container_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Container not found".into(),
            }),
        )
    })?;
    let pos = container_mappings
        .iter()
        .position(|m| m.request.host_addr.port() == port);
    if let Some(i) = pos {
        let m = container_mappings.remove(i);
        let _ = state.iptables.remove_mapping(&m);
        state
            .ports
            .deallocate(&AppState::port_key(
                &m.request.host_addr.ip().to_string(),
                m.request.host_addr.port(),
            ))
            .await;
        drop(lock);
        persist_state(&state).await;
        Ok(StatusCode::OK)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Port mapping not found".into(),
            }),
        ))
    }
}

/// `DELETE /mapping/by-id/:id` — Removes a port mapping by its numeric ID.
async fn remove_mapping_by_id(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.state.write().await;
    for (_, mappings) in lock.docker.iter_mut() {
        if let Some(pos) = mappings.iter().position(|m| m.id == id) {
            let m = mappings.remove(pos);
            let _ = state.iptables.remove_mapping(&m);
            state
                .ports
                .deallocate(&AppState::port_key(
                    &m.request.host_addr.ip().to_string(),
                    m.request.host_addr.port(),
                ))
                .await;
            drop(lock);
            persist_state(&state).await;
            return Ok(StatusCode::OK);
        }
    }
    Err((
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: format!("No mapping found with id {id}"),
        }),
    ))
}
