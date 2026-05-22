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
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use axum::Router;
use axum::routing::delete;
use axum::routing::get;
use axum::routing::post;
use axum::routing::put;
use bollard::Docker;
use bollard::query_parameters::EventsOptions;
use bollard::query_parameters::ListContainersOptionsBuilder;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use futures_util::stream::StreamExt;
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto::Builder;
use serde::Serialize;
use tokio::process::Command;
use tokio::sync::RwLock;
use tower_service::Service;
use tracing::error;
use tracing::info;

use crate::api::add_dnat;
use crate::api::add_hairpin;
use crate::api::add_mapping;
use crate::api::add_snat;
use crate::api::clear_all;
use crate::api::list_mappings;
use crate::api::remap_port;
use crate::api::remove_dnat;
use crate::api::remove_hairpin;
use crate::api::remove_mapping;
use crate::api::remove_mapping_by_id;
use crate::api::remove_snat;
use crate::api::unbind_ports;
use crate::docker;
use crate::iptables::IptablesManager;
use crate::models::DaemonState;
use crate::models::DockerPortMap;
use crate::port::PortAllocator;

/// Shared application state held by all Axum route handlers.
#[derive(Clone)]
pub struct AppState {
    /// The in-memory daemon state.
    pub daemon_state: Arc<RwLock<DaemonState>>,
    /// iptables rule manager.
    pub iptables: Arc<IptablesManager>,
    /// Docker client (None if Docker is unavailable).
    pub docker: Option<Docker>,
    /// Filesystem path for persisting state to JSON.
    pub state_path: PathBuf,
    /// Path to natmap socket.
    pub socket_path: PathBuf,
    /// Group name owning the natmap socket.
    pub socket_group: String,
    /// Auto-incrementing ID counter for mapping entries.
    pub next_id: Arc<AtomicU64>,
    /// Port reservation system for conflict prevention.
    pub ports: Arc<PortAllocator>,
}

impl AppState {
    /// Returns the next unique mapping ID and advances the counter.
    pub fn allocate_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Writes the current daemon state to disk (atomically via a temp file).
    pub async fn persist(&self) {
        let data = {
            let lock = self.daemon_state.read().await;
            serde_json::to_string(&*lock).unwrap_or_default()
        };
        let tmp = self.state_path.with_extension("tmp");
        if fs::write(&tmp, data).is_ok() {
            let _ = fs::rename(&tmp, &self.state_path);
        }
    }
}

/// JSON error response returned by the daemon API on failures.
#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Clone)]
pub struct Daemon {
    state: AppState,
    app: Router<()>,
}

impl Daemon {
    pub async fn new(
        socket_path: PathBuf,
        state_path: PathBuf,
        socket_group: String,
    ) -> Result<Self> {
        info!("Starting natmap daemon...");

        let docker = docker::connect().ok();
        if docker.is_none() {
            info!(
                "Failed connecting to Docker daemon via Unix socket — running without Docker support"
            );
        }

        let state_dir = state_path.parent().unwrap();
        if !state_dir.exists() {
            fs::create_dir_all(state_dir).map_err(|e| {
                eyre!(
                    "Failed to create state directory {}: {e}",
                    state_dir.display()
                )
            })?;
        }

        let iptables = Arc::new(IptablesManager::new());
        iptables
            .setup()
            .map_err(|e| eyre!("Failed to set up iptables chains: {e}"))?;

        let ports = Arc::new(PortAllocator::new());
        let daemon_state = Arc::new(RwLock::new(DaemonState::default()));

        let state = AppState {
            daemon_state,
            iptables,
            docker,
            state_path,
            next_id: Arc::new(AtomicU64::new(1)),
            ports,
            socket_group,
            socket_path,
        };

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
            .route("/clear", delete(clear_all))
            .with_state(state.clone());

        Ok(Self { state, app })
    }

    /// Runs the natmap daemon with explicit paths for the socket, state file, and group.
    ///
    /// Sets up iptables chains, loads persisted state, spawns Docker event listeners,
    /// installs a Ctrl-C handler for clean shutdown, and starts the HTTP API server.
    pub async fn run(&self) -> Result<()> {
        self.reload().await?;

        let state = self.state.clone();

        if state.docker.is_some() {
            let self_clone = self.clone();
            tokio::spawn(async move {
                if let Err(e) = self_clone.listen_docker_events().await {
                    error!("Docker listener exited with error: {}", e);
                }
            });
        }

        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            info!("Shutting down: flushing iptables rules...");
            let _ = state.iptables.flush_all_natmap();
            state.ports.deallocate_all().await;
            info!("Shutdown complete.");
            std::process::exit(0);
        });

        if state.socket_path.exists() {
            let _ = fs::remove_file(&state.socket_path);
        }

        let socket_path_str = state.socket_path.display().to_string();
        let listener = tokio::net::UnixListener::bind(state.socket_path)
            .map_err(|e| eyre!("Failed to bind Unix socket at {}: {e}", socket_path_str))?;

        let _ = Command::new("chown")
            .args([
                format!("root:{}", state.socket_group),
                socket_path_str.to_string(),
            ])
            .status()
            .await;
        let _ = Command::new("chmod")
            .args(["660", &socket_path_str])
            .status()
            .await;

        info!("Listening on unix socket: {}", socket_path_str);

        loop {
            let (socket, _) = listener.accept().await?;
            let app = self.app.clone();

            tokio::spawn(async move {
                let socket = TokioIo::new(socket);

                let srv = hyper::service::service_fn(
                    move |request: hyper::Request<hyper::body::Incoming>| app.clone().call(request),
                );

                if let Err(err) = Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(socket, srv)
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
    /// Flushes iptables rules, releases old port reservations, and re-installs
    /// rules for surviving containers and static configurations.
    pub async fn reload(&self) -> Result<()> {
        info!("Crash recovery: flushing stale iptables rules");
        let state = self.state.clone();
        let ports = self.state.ports.clone();
        let iptables = self.state.iptables.clone();

        // ignore flush fail. we still have more cleanup to do independent from flush
        let _ = iptables.flush_all_natmap();
        ports.deallocate_all().await;

        let mut daemon_state = self.create_daemon_state();

        // Reconcile Docker mappings
        let _ = self
            .reconcile_docker_portmaps(&mut daemon_state)
            .await
            .map_err(|e| error!("Error when reconciling docker portmaps: {e}"));

        // Reconcile NAT rules
        self.reconcile_hairpins(&mut daemon_state).await;
        self.reconcile_dnats(&mut daemon_state).await;
        self.reconcile_snats(&daemon_state).await;

        *state.daemon_state.write().await = daemon_state;
        self.state.persist().await;
        Ok(())
    }

    /// Listens for Docker container events and automatically manages port mappings.
    ///
    /// On `start` / `network connect`: discovers published ports and installs rules.
    /// On `die` / `kill` / `network disconnect`: removes all rules for the container.
    async fn listen_docker_events(&self) -> Result<()> {
        let docker = self
            .state
            .docker
            .as_ref()
            .ok_or_else(|| eyre!("Docker not available"))?;
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
            let Ok(event) = msg else { continue };
            let Some(action) = event.action else { continue };
            let Some(actor) = event.actor else { continue };
            let Some(container_id) = actor.id else {
                continue;
            };

            use bollard::plugin::EventMessageTypeEnum::*;
            let Some(typ) = event.typ else { continue };
            match (typ, action.as_str()) {
                (CONTAINER, "start") | (NETWORK, "connect") => {
                    self.on_container_start(container_id, docker).await
                }
                (CONTAINER, "die" | "kill") | (NETWORK, "disconnect") => {
                    self.on_container_stop(container_id).await
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn on_container_stop(&self, container_id: String) {
        info!("Container {container_id} died, flushing rules");
        let state = &self.state;
        let mut lock = state.daemon_state.write().await;

        let Some(mappings) = lock.mapping.remove(&container_id) else {
            return;
        };

        for m in &mappings {
            let _ = state.iptables.remove_mapping(m);
            state.ports.deallocate(m.request.host_addr).await;
        }
        drop(lock);
        state.persist().await;
    }

    async fn on_container_start(&self, container_id: String, docker: &Docker) {
        info!("Container {} started, parsing mappings", container_id);
        let state = &self.state;

        let Ok(discovered) = docker::get_port_mappings(docker, &container_id).await else {
            return;
        };

        let mut assigned = Vec::new();
        for mut m in discovered {
            m.id = state.allocate_id();
            let host_addr = m.request.host_addr;
            if state.ports.is_allocated(host_addr).await {
                info!("Address {host_addr} already allocated, skipping");
                continue;
            }
            if let Err(e) = state.ports.allocate(host_addr).await {
                error!("Failed allocating {host_addr}, skipping: {e}");
                continue;
            }
            if let Err(e) = state.iptables.install_dockermap(&m) {
                error!("Failed to install mapping {m:?}: {e}");
                state.ports.deallocate(host_addr).await;
                continue;
            }
            assigned.push(m);
        }
        let mut lock = state.daemon_state.write().await;
        let existing = lock.mapping.entry(container_id.clone()).or_default();
        let auto_comments: HashSet<String> =
            assigned.iter().map(|m| m.rule_comment.clone()).collect();
        existing.retain(|m| !auto_comments.contains(&m.rule_comment));
        existing.extend(assigned);
        drop(lock);
        state.persist().await;
    }

    /// Create daemon state from [`AppState::state_path`] if exists, otherwise create default.
    fn create_daemon_state(&self) -> DaemonState {
        if self.state.state_path.exists()
            && let Ok(data) = fs::read_to_string(&self.state.state_path)
        {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            DaemonState::default()
        }
    }

    async fn reconcile_docker_portmaps(&self, daemon_state: &mut DaemonState) -> Result<()> {
        let state = &self.state;
        let ports = &self.state.ports;
        let iptables = &self.state.iptables;

        if let Some(docker) = &state.docker {
            let opt = ListContainersOptionsBuilder::new().build();
            let running_ids: HashSet<String> = docker
                .list_containers(Some(opt))
                .await?
                .into_iter()
                .filter_map(|c| c.id)
                .collect();

            let mut max_id: u64 = 0;
            let old_maps: Vec<(String, Vec<DockerPortMap>)> =
                daemon_state.mapping.drain().collect();
            let mut new_docker = HashMap::new();

            // iter containers
            for (id, maps) in old_maps {
                if !running_ids.contains(&id) {
                    info!("Container {id} gone, removing mappings");
                    continue;
                }
                let mut kept = Vec::new();
                // iter port mappings for this container
                for m in maps {
                    let host_addr = m.request.host_addr;

                    // check this map
                    if ports.is_allocated(host_addr).await {
                        info!("Address {host_addr} already held, removing stale mapping");
                        continue;
                    }
                    if let Err(e) = ports.allocate(host_addr).await {
                        error!("Address {host_addr} in use, dropping mapping: {e}");
                        continue;
                    }

                    // keep this port mapping
                    let _ = iptables.install_dockermap(&m);
                    max_id = max_id.max(m.id);
                    kept.push(m);
                }
                if !kept.is_empty() {
                    new_docker.insert(id, kept);
                }
            }
            daemon_state.mapping = new_docker;
            state
                .next_id
                .store(max_id.saturating_add(1), Ordering::SeqCst);
        }
        Ok(())
    }

    async fn reconcile_hairpins(&self, daemon_state: &mut DaemonState) {
        let mut keep = Vec::new();
        for config in daemon_state.hairpins.drain(..) {
            if Self::should_reconcile(&config.ports, &config.ext_ip, &self.state.ports).await {
                let _ = self.state.iptables.install_hairpin(&config);
                keep.push(config);
            } else {
                unbind_ports(self.state.ports.clone(), &config.ext_ip, &config.ports).await;
            }
        }
        daemon_state.hairpins = keep;
    }

    async fn reconcile_dnats(&self, daemon_state: &mut DaemonState) {
        let mut keep = Vec::new();
        for config in daemon_state.dnats.drain(..) {
            if Self::should_reconcile(&config.ports, &config.ext_ip, &self.state.ports).await {
                let _ = self.state.iptables.install_dnat(&config);
                keep.push(config);
            } else {
                unbind_ports(self.state.ports.clone(), &config.ext_ip, &config.ports).await;
            }
        }
        daemon_state.dnats = keep;
    }

    async fn reconcile_snats(&self, daemon_state: &DaemonState) {
        for config in &daemon_state.snats {
            let _ = self.state.iptables.install_snat(config);
        }
    }

    /// Check if this port can be reconciled.
    async fn should_reconcile(configs_ports: &str, ext_ip: &str, ports: &PortAllocator) -> bool {
        let ip = match ext_ip.parse() {
            Ok(ip) => ip,
            Err(e) => {
                error!("Invalid IP {ext_ip}: {e}");
                return false;
            }
        };

        for port in configs_ports
            .split(',')
            .filter_map(|p| p.trim().parse::<u16>().ok())
        {
            let addr = SocketAddr::new(ip, port);
            if ports.is_allocated(addr).await {
                continue;
            }
            if let Err(e) = ports.allocate(addr).await {
                error!("Port {port} in use, dropping: {e}");
                return false;
            }
        }
        true
    }
}
