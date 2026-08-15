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
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::eyre;
use futures_util::stream::StreamExt;
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto::Builder;
use lab_ops_lab_lib::port::PortAllocator;
use serde::Serialize;
use tokio::process::Command;
use tokio::sync::RwLock;
use tower_service::Service;
use tracing::info;

use crate::api::add_dnat;
use crate::api::add_hairpin;
use crate::api::add_mapping;
use crate::api::add_policy_route;
use crate::api::add_snat;
use crate::api::clear_all;
use crate::api::list_mappings;
use crate::api::list_rules;
use crate::api::remap_port;
use crate::api::remove_dnat;
use crate::api::remove_hairpin;
use crate::api::remove_mapping;
use crate::api::remove_mapping_by_id;
use crate::api::remove_policy_route;
use crate::api::remove_snat;
use crate::docker;
use crate::iptables::Iptables;
use crate::iptables::IptablesManager;
use crate::models::DaemonState;
use crate::models::DnatConfig;
use crate::models::DockerPortMap;
use crate::models::DockerPortMapRequest;
use crate::models::HairpinConfig;
use crate::policy_route::PolicyRouteManager;

/// Shared application state held by all Axum route handlers.
#[derive(Clone)]
pub struct AppState {
    /// The in-memory daemon state.
    pub daemon_state: Arc<RwLock<DaemonState>>,
    /// iptables rule manager.
    pub iptables: Arc<dyn Iptables>,
    /// Policy routing manager.
    pub policy_route: Arc<PolicyRouteManager>,
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
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Builds the HTTP API router for the daemon.
///
/// Exposed separately from [`Daemon::new`] so tests can serve the same router
/// over a temporary Unix socket without touching iptables or Docker.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/mappings", get(list_mappings))
        .route("/rules", get(list_rules))
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
        .route("/policy-route", post(add_policy_route))
        .route("/policy-route", delete(remove_policy_route))
        .route("/clear", delete(clear_all))
        .with_state(state)
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
        tracing::info!(daemon = "natmap", "starting natmap daemon");

        let docker = docker::connect().ok();
        if docker.is_none() {
            tracing::info!(
                "failed connecting to Docker daemon via Unix socket — running without Docker support"
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
        let policy_route = Arc::new(PolicyRouteManager::new());

        let state = AppState {
            daemon_state,
            iptables,
            policy_route,
            docker,
            state_path,
            next_id: Arc::new(AtomicU64::new(1)),
            ports,
            socket_group,
            socket_path,
        };

        Ok(Self {
            app: build_router(state.clone()),
            state,
        })
    }

    /// Runs the natmap daemon with explicit paths for the socket, state file, and group.
    ///
    /// Sets up iptables chains, loads persisted state, spawns Docker event listeners,
    /// installs a Ctrl-C handler for clean shutdown, and starts the HTTP API server.
    #[tracing::instrument(skip_all, fields(daemon = "natmap", socket.path = %self.state.socket_path.display()))]
    pub async fn run(&self) -> Result<()> {
        self.reload().await?;

        let state = self.state.clone();

        if state.docker.is_some() {
            let self_clone = self.clone();
            tokio::spawn(async move {
                if let Err(e) = self_clone.listen_docker_events().await {
                    tracing::error!(error = %e, "docker listener exited with error");
                }
            });
        }

        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("shutting down: flushing iptables rules");
            let _ = state.iptables.flush_all_natmap();
            let daemon_state = state.daemon_state.read().await;
            let _ = state.policy_route.flush_all(&daemon_state.policy_routes);
            drop(daemon_state);
            state.ports.deallocate_all().await;
            tracing::info!("shutdown complete");
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

        tracing::info!(socket.path = %socket_path_str, "listening on unix socket");

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
                    tracing::error!(error = %err, "failed to serve connection");
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
    #[tracing::instrument(skip_all, fields(mappings.count = tracing::field::Empty, dnats.count = tracing::field::Empty))]
    pub async fn reload(&self) -> Result<()> {
        info!("crash recovery: flushing stale iptables rules");
        let state = self.state.clone();
        let ports = self.state.ports.clone();
        let iptables = self.state.iptables.clone();
        let policy_route = self.state.policy_route.clone();

        // ignore flush fail. we still have more cleanup to do independent from flush
        let _ = iptables.flush_all_natmap();
        let _ = policy_route.flush_all(&state.daemon_state.read().await.policy_routes);
        ports.deallocate_all().await;

        let mut daemon_state = self.create_daemon_state();

        // Reconcile Docker mappings
        let _ = self
            .reconcile_docker_portmaps(&mut daemon_state)
            .await
            .map_err(|e| tracing::error!(error = %e, "error when reconciling docker portmaps"));

        // Reconcile NAT rules
        self.reconcile_hairpins(&mut daemon_state).await;
        self.reconcile_dnats(&mut daemon_state).await;
        self.reconcile_snats(&daemon_state).await;
        self.reconcile_policy_routes(&mut daemon_state).await;

        let mappings_count: usize = daemon_state.mapping.values().map(|m| m.len()).sum();
        let dnats_count = daemon_state.dnats.len();

        let span = tracing::Span::current();
        span.record("mappings.count", mappings_count);
        span.record("dnats.count", dnats_count);

        *state.daemon_state.write().await = daemon_state;
        self.state.persist().await;

        tracing::info!("reload complete");
        Ok(())
    }

    /// Handles a single Docker event, creating the required span.
    #[tracing::instrument(skip_all, fields(container.id = tracing::field::Empty, event.action = tracing::field::Empty))]
    pub async fn handle_docker_event(&self, event: bollard::models::EventMessage, docker: &Docker) {
        tracing::trace!(?event, "raw docker event");

        let Some(action) = event.action else {
            return;
        };
        let Some(actor) = event.actor else {
            return;
        };
        let Some(container_id) = actor.id else {
            return;
        };

        use bollard::plugin::EventMessageTypeEnum::*;
        let Some(typ) = event.typ else {
            return;
        };

        let span = tracing::Span::current();
        span.record("container.id", &container_id);
        span.record("event.action", &action);

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
            self.handle_docker_event(event, docker).await;
        }
        Ok(())
    }

    async fn on_container_stop(&self, container_id: String) {
        tracing::debug!("container died, flushing rules");
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
        tracing::debug!("container started, parsing mappings");
        let state = &self.state;

        let Ok(discovered) = docker::get_port_mappings(docker, &container_id).await else {
            return;
        };

        let assigned = self
            .apply_discovered_mappings(&container_id, discovered)
            .await;
        let mut lock = state.daemon_state.write().await;
        let existing = lock.mapping.entry(container_id.clone()).or_default();
        let auto_comments: HashSet<String> =
            assigned.iter().map(|m| m.rule_comment.clone()).collect();
        existing.retain(|m| !auto_comments.contains(&m.rule_comment));
        existing.extend(assigned);
        drop(lock);
        state.persist().await;
    }

    /// Allocates and installs rules for a container's freshly discovered mappings.
    ///
    /// Ports held by a stale mapping of the same container are released first;
    /// ports held by other active containers are skipped.
    async fn apply_discovered_mappings(
        &self,
        container_id: &str,
        discovered: Vec<DockerPortMap>,
    ) -> Vec<DockerPortMap> {
        let state = &self.state;
        let mut assigned = Vec::new();
        for mut m in discovered {
            m.id = state.allocate_id();
            let host_addr = m.request.host_addr;
            if state.ports.is_allocated(host_addr).await {
                if let Some(stale_id) =
                    resolve_stale_container(&state.daemon_state, host_addr, container_id).await
                {
                    tracing::info!(host.addr = %host_addr, stale.container.id = %stale_id,
                        "port held by stale container, removing old mapping");
                    self.on_container_stop(stale_id).await;
                } else {
                    tracing::warn!(host.addr = %host_addr, "address already allocated, skipping");
                    continue;
                }
            }
            if let Err(e) = ensure_docker_mapping(&state.ports, state.iptables.as_ref(), &m).await {
                tracing::warn!(mapping = ?m, error = %e, "failed to ensure mapping");
                continue;
            }
            assigned.push(m);
        }
        assigned
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

    /// Re-verifies and reinstalls a tracked mapping, dropping it on failure.
    ///
    /// Returns `Some(mapping)` when the mapping was kept, `None` when it was
    /// dropped (port held elsewhere, allocation failure, or install failure).
    async fn reconcile_tracked_mapping(
        &self,
        container_id: &str,
        mut m: DockerPortMap,
        current_addrs: &HashMap<SocketAddr, SocketAddr>,
    ) -> Option<DockerPortMap> {
        let host_addr = m.request.host_addr;

        // Update container_addr from live Docker inspect if changed
        // (silently falls back to stored IP if inspect failed above)
        if let Some(&current_ctn_addr) = current_addrs.get(&host_addr) {
            let proto = m.request.proto;
            if reconcile_container_addr(
                &mut m.request,
                &DockerPortMapRequest {
                    host_addr,
                    container_addr: current_ctn_addr,
                    proto,
                },
            ) {
                tracing::info!(
                    container.id = %container_id, host.port = %host_addr.port(),
                    "container IP changed on reload"
                );
            }
        }

        if self.state.ports.is_allocated(host_addr).await {
            tracing::warn!(host.addr = %host_addr, "address already held, removing stale mapping");
            return None;
        }
        if let Err(e) =
            ensure_docker_mapping(&self.state.ports, self.state.iptables.as_ref(), &m).await
        {
            tracing::warn!(container.id = %container_id, mapping = ?m, error = %e,
                "failed to ensure mapping, dropping");
            return None;
        }
        Some(m)
    }

    /// Allocates and installs mappings for a container not tracked in persisted state.
    async fn ensure_container_mappings(
        &self,
        container_id: &str,
        discovered: Vec<DockerPortMap>,
        max_id: &mut u64,
    ) -> Vec<DockerPortMap> {
        let state = &self.state;
        let mut installed = Vec::new();
        for mut m in discovered {
            m.id = state.allocate_id();
            if let Err(e) = ensure_docker_mapping(&state.ports, state.iptables.as_ref(), &m).await {
                tracing::warn!(container.id = %container_id, mapping = ?m, error = %e,
                    "failed to ensure mapping for untracked container");
                continue;
            }
            *max_id = (*max_id).max(m.id);
            installed.push(m);
        }
        installed
    }

    async fn reconcile_docker_portmaps(&self, daemon_state: &mut DaemonState) -> Result<()> {
        if let Some(docker) = &self.state.docker {
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
                    tracing::info!(container.id = %id, "container gone, removing mappings");
                    continue;
                }

                // Re-inspect container to get current IPs (may have changed if
                // Docker network was recreated while daemon was down)
                let current_addrs: HashMap<SocketAddr, SocketAddr> =
                    docker::get_port_mappings(docker, &id)
                        .await
                        .ok()
                        .into_iter()
                        .flat_map(|mappings| {
                            mappings
                                .into_iter()
                                .map(|m| (m.request.host_addr, m.request.container_addr))
                        })
                        .collect();

                // iter port mappings for this container
                let mut kept = Vec::new();
                for m in maps {
                    if let Some(m) = self.reconcile_tracked_mapping(&id, m, &current_addrs).await {
                        max_id = max_id.max(m.id);
                        kept.push(m);
                    }
                }
                if !kept.is_empty() {
                    new_docker.insert(id, kept);
                }
            }

            // Discover untracked containers (started while daemon was down)
            let tracked: HashSet<String> = new_docker.keys().cloned().collect();
            for id in untracked_container_ids(&running_ids, &tracked) {
                tracing::info!(container.id = %id, "discovering untracked container");
                let Ok(discovered) = docker::get_port_mappings(docker, id).await else {
                    continue;
                };
                let installed = self
                    .ensure_container_mappings(id, discovered, &mut max_id)
                    .await;
                if !installed.is_empty() {
                    new_docker.insert(id.to_string(), installed);
                }
            }

            daemon_state.mapping = new_docker;
            self.state
                .next_id
                .store(max_id.saturating_add(1), Ordering::SeqCst);
        }
        Ok(())
    }

    async fn reconcile_hairpins(&self, daemon_state: &mut DaemonState) {
        let mut keep = Vec::new();
        for config in daemon_state.hairpins.drain(..) {
            if let Err(e) = ensure_static_rule(
                &self.state.ports,
                self.state.iptables.as_ref(),
                StaticRule::Hairpin(&config),
            )
            .await
            {
                tracing::warn!(hairpin = ?config, error = %e,
                    "failed to reconcile hairpin rule, dropping");
            } else {
                keep.push(config);
            }
        }
        daemon_state.hairpins = keep;
    }

    async fn reconcile_dnats(&self, daemon_state: &mut DaemonState) {
        let mut keep = Vec::new();
        for config in daemon_state.dnats.drain(..) {
            if let Err(e) = ensure_static_rule(
                &self.state.ports,
                self.state.iptables.as_ref(),
                StaticRule::Dnat(&config),
            )
            .await
            {
                tracing::warn!(dnat = ?config, error = %e,
                    "failed to reconcile dnat rule, dropping");
            } else {
                keep.push(config);
            }
        }
        daemon_state.dnats = keep;
    }

    async fn reconcile_snats(&self, daemon_state: &DaemonState) {
        for config in &daemon_state.snats {
            let _ = self.state.iptables.install_snat(config);
        }
    }

    async fn reconcile_policy_routes(&self, daemon_state: &mut DaemonState) {
        let mut keep = Vec::new();
        for config in daemon_state.policy_routes.drain(..) {
            if let Err(e) = self.state.policy_route.install(&config) {
                tracing::error!(error = %e, "failed to install policy route");
            } else {
                keep.push(config);
            }
        }
        daemon_state.policy_routes = keep;
    }
}

// --- Ensure primitives ---

/// A static NAT rule whose ports must be reserved before installation.
enum StaticRule<'a> {
    /// Static DNAT rule.
    Dnat(&'a DnatConfig),
    /// Static hairpin rule.
    Hairpin(&'a HairpinConfig),
}

/// Reserves the host port for a Docker mapping and installs its rules.
///
/// On install failure the reservation is released before the error is returned.
async fn ensure_docker_mapping(
    ports: &PortAllocator,
    iptables: &dyn Iptables,
    mapping: &DockerPortMap,
) -> Result<()> {
    let host_addr = mapping.request.host_addr;
    ports
        .allocate(host_addr, mapping.request.proto)
        .await
        .wrap_err("failed to reserve host port")?;
    if let Err(e) = iptables.install_dockermap(mapping) {
        ports.deallocate(host_addr).await;
        return Err(e.wrap_err("failed to install docker mapping"));
    }
    Ok(())
}

/// Reserves the ports for a static rule and installs its rules.
///
/// Ports already reserved elsewhere are skipped. On any failure all ports
/// reserved by this call are released before the error is returned.
async fn ensure_static_rule(
    ports: &PortAllocator,
    iptables: &dyn Iptables,
    rule: StaticRule<'_>,
) -> Result<()> {
    let (ext_ip, ports_csv, proto) = match &rule {
        StaticRule::Dnat(config) => (&config.ext_ip, &config.ports, config.proto),
        StaticRule::Hairpin(config) => (&config.ext_ip, &config.ports, config.proto),
    };
    let ip: IpAddr = ext_ip.parse().wrap_err("invalid IP")?;

    let mut reserved = Vec::new();
    for port in ports_csv
        .split(',')
        .filter_map(|p| p.trim().parse::<u16>().ok())
    {
        let addr = SocketAddr::new(ip, port);
        if ports.is_allocated(addr).await {
            continue;
        }
        if let Err(e) = ports.allocate(addr, proto).await {
            for reserved_addr in &reserved {
                ports.deallocate(*reserved_addr).await;
            }
            return Err(e.wrap_err("failed to reserve port"));
        }
        reserved.push(addr);
    }

    let install = match rule {
        StaticRule::Dnat(config) => iptables.install_dnat(config),
        StaticRule::Hairpin(config) => iptables.install_hairpin(config),
    };
    if let Err(e) = install {
        for addr in reserved {
            ports.deallocate(addr).await;
        }
        return Err(e.wrap_err("failed to install static rule"));
    }
    Ok(())
}

/// Looks up whether `host_addr` is claimed by a container other than
/// `new_container_id` in the persisted daemon state.
///
/// Returns `Some(stale_container_id)` if a different container owns the port,
/// indicating the old container was recreated and its mapping should be removed
/// before allocating for the new one.
async fn resolve_stale_container(
    daemon_state: &Arc<RwLock<DaemonState>>,
    host_addr: SocketAddr,
    new_container_id: &str,
) -> Option<String> {
    let lock = daemon_state.read().await;
    lock.mapping.iter().find_map(|(id, maps)| {
        (id.as_str() != new_container_id && maps.iter().any(|m| m.request.host_addr == host_addr))
            .then(|| id.clone())
    })
}

/// Returns container IDs that are running but have no tracked port mappings.
///
/// Used during daemon reload to discover containers that started while the
/// daemon was down.
fn untracked_container_ids<'a>(
    running_ids: &'a HashSet<String>,
    tracked_ids: &'a HashSet<String>,
) -> Vec<&'a str> {
    running_ids
        .iter()
        .filter(|id| !tracked_ids.contains(id.as_str()))
        .map(|id| id.as_str())
        .collect()
}

/// Updates `stored` container address from `current` if it changed.
///
/// Returns `true` if an update was made.
fn reconcile_container_addr(
    stored: &mut DockerPortMapRequest,
    current: &DockerPortMapRequest,
) -> bool {
    if stored.container_addr == current.container_addr {
        return false;
    }
    stored.container_addr = current.container_addr;
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::net::IpAddr;
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;

    use axum::Router;
    use bollard::Docker;
    use bollard::models::EventActor;
    use bollard::models::EventMessage;
    use color_eyre::eyre::eyre;
    use lab_ops_lab_lib::port::PortAllocator;
    use tokio::sync::RwLock;
    use tracing_test::traced_test;

    use super::AppState;
    use super::Daemon;
    use super::StaticRule;
    use super::ensure_docker_mapping;
    use super::ensure_static_rule;
    use super::reconcile_container_addr;
    use super::resolve_stale_container;
    use super::untracked_container_ids;
    use crate::iptables::Iptables;
    use crate::iptables::IptablesManager;
    use crate::models::DaemonState;
    use crate::models::DnatConfig;
    use crate::models::DockerPortMap;
    use crate::models::DockerPortMapRequest;
    use crate::models::HairpinConfig;
    use crate::models::SnatConfig;
    use crate::models::TransportProtocol;
    use crate::policy_route::PolicyRouteManager;

    /// Records iptables operations without touching the host firewall.
    #[derive(Default)]
    pub(crate) struct FakeIptables {
        installed_mappings: Mutex<Vec<DockerPortMap>>,
        removed_mappings: Mutex<Vec<DockerPortMap>>,
        installed_dnats: Mutex<Vec<DnatConfig>>,
        installed_hairpins: Mutex<Vec<HairpinConfig>>,
        rules_lines: Mutex<Vec<String>>,
        fail_dockermap: AtomicBool,
        fail_dnat: AtomicBool,
        fail_hairpin: AtomicBool,
    }

    impl FakeIptables {
        /// Sets the iptables-save output lines the fake will report.
        pub(crate) fn set_rules_lines(&self, lines: Vec<String>) {
            *self.rules_lines.lock().unwrap() = lines;
        }

        pub(crate) fn installed_mappings(&self) -> Vec<DockerPortMap> {
            self.installed_mappings.lock().unwrap().clone()
        }

        fn removed_mappings(&self) -> Vec<DockerPortMap> {
            self.removed_mappings.lock().unwrap().clone()
        }

        fn installed_dnats(&self) -> Vec<DnatConfig> {
            self.installed_dnats.lock().unwrap().clone()
        }

        fn installed_hairpins(&self) -> Vec<HairpinConfig> {
            self.installed_hairpins.lock().unwrap().clone()
        }

        pub(crate) fn set_fail_dockermap(&self, fail: bool) {
            self.fail_dockermap.store(fail, Ordering::SeqCst);
        }

        fn set_fail_dnat(&self, fail: bool) {
            self.fail_dnat.store(fail, Ordering::SeqCst);
        }
    }

    impl Iptables for FakeIptables {
        fn setup(&self) -> color_eyre::Result<()> {
            Ok(())
        }

        fn flush_all_natmap(&self) -> color_eyre::Result<()> {
            Ok(())
        }

        fn install_dockermap(&self, map: &DockerPortMap) -> color_eyre::Result<()> {
            if self.fail_dockermap.load(Ordering::SeqCst) {
                return Err(eyre!("fake docker mapping install failure"));
            }
            self.installed_mappings.lock().unwrap().push(map.clone());
            Ok(())
        }

        fn remove_mapping(&self, map: &DockerPortMap) -> color_eyre::Result<()> {
            self.removed_mappings.lock().unwrap().push(map.clone());
            Ok(())
        }

        fn install_dnat(&self, config: &DnatConfig) -> color_eyre::Result<()> {
            if self.fail_dnat.load(Ordering::SeqCst) {
                return Err(eyre!("fake dnat install failure"));
            }
            self.installed_dnats.lock().unwrap().push(config.clone());
            Ok(())
        }

        fn remove_dnat(&self, _config: &DnatConfig) -> color_eyre::Result<()> {
            Ok(())
        }

        fn install_snat(&self, _config: &SnatConfig) -> color_eyre::Result<()> {
            Ok(())
        }

        fn remove_snat(&self, _config: &SnatConfig) -> color_eyre::Result<()> {
            Ok(())
        }

        fn install_hairpin(&self, config: &HairpinConfig) -> color_eyre::Result<()> {
            if self.fail_hairpin.load(Ordering::SeqCst) {
                return Err(eyre!("fake hairpin install failure"));
            }
            self.installed_hairpins.lock().unwrap().push(config.clone());
            Ok(())
        }

        fn remove_hairpin(&self, _config: &HairpinConfig) -> color_eyre::Result<()> {
            Ok(())
        }

        fn list_rules(&self) -> color_eyre::Result<Vec<String>> {
            Ok(self.rules_lines.lock().unwrap().clone())
        }
    }

    fn make_addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::from([127, 0, 0, 1]), port)
    }

    fn make_mapping(id: u64, host_port: u16, ctn_port: u16, container_id: &str) -> DockerPortMap {
        DockerPortMap::new(
            id,
            DockerPortMapRequest {
                host_addr: make_addr(host_port),
                container_addr: make_addr(ctn_port),
                proto: TransportProtocol::Tcp,
            },
            container_id.to_string(),
            "test-container".to_string(),
        )
    }

    fn make_dnat(ports: &str) -> DnatConfig {
        DnatConfig {
            ext_ip: "127.0.0.1".to_string(),
            int_ip: "10.0.0.99".to_string(),
            ports: ports.to_string(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        }
    }

    fn make_hairpin(ports: &str) -> HairpinConfig {
        HairpinConfig {
            ext_ip: "127.0.0.1".to_string(),
            int_ip: "10.0.0.99".to_string(),
            ports: ports.to_string(),
            proto: TransportProtocol::Tcp,
            lan_cidr: None,
        }
    }

    fn id_set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|&s| s.to_string()).collect()
    }

    fn test_daemon_with(
        state_path: PathBuf,
        iptables: Arc<dyn Iptables>,
        ports: Arc<PortAllocator>,
    ) -> Daemon {
        let daemon_state = Arc::new(RwLock::new(DaemonState::default()));
        let policy_route = Arc::new(PolicyRouteManager::new());

        let state = AppState {
            daemon_state,
            iptables,
            policy_route,
            docker: None,
            state_path,
            next_id: Arc::new(AtomicU64::new(1)),
            ports,
            socket_group: "root".to_string(),
            socket_path: PathBuf::from("/tmp/natmap.sock"),
        };

        Daemon {
            state,
            app: Router::new(),
        }
    }

    /// Builds an [`AppState`] backed by the given iptables fake.
    pub(crate) fn test_app_state_with(iptables: Arc<dyn Iptables>) -> AppState {
        let daemon_state = Arc::new(RwLock::new(DaemonState::default()));
        let policy_route = Arc::new(PolicyRouteManager::new());

        AppState {
            daemon_state,
            iptables,
            policy_route,
            docker: None,
            state_path: PathBuf::from("/tmp/natmap-test-state.json"),
            next_id: Arc::new(AtomicU64::new(1)),
            ports: Arc::new(PortAllocator::new()),
            socket_group: "root".to_string(),
            socket_path: PathBuf::from("/tmp/natmap.sock"),
        }
    }

    fn create_test_daemon(state_path: PathBuf) -> Daemon {
        test_daemon_with(
            state_path,
            Arc::new(IptablesManager::new()),
            Arc::new(PortAllocator::new()),
        )
    }

    #[tokio::test]
    #[traced_test]
    async fn reload_state_logs_mapping_count() {
        let temp_dir = tempfile::tempdir().unwrap();
        let state_path = temp_dir.path().join("state.json");

        let daemon = create_test_daemon(state_path);

        let _ = daemon.reload().await;

        assert!(logs_contain("mappings.count="));
    }

    #[tokio::test]
    #[traced_test]
    async fn handle_docker_event_span_has_container_id() {
        let temp_dir = tempfile::tempdir().unwrap();
        let state_path = temp_dir.path().join("state.json");

        let daemon = create_test_daemon(state_path);
        let docker = Docker::connect_with_local_defaults().unwrap();

        let event = EventMessage {
            action: Some("start".to_string()),
            actor: Some(EventActor {
                id: Some("1234567890".to_string()),
                ..Default::default()
            }),
            typ: Some(bollard::plugin::EventMessageTypeEnum::CONTAINER),
            ..Default::default()
        };

        daemon.handle_docker_event(event, &docker).await;

        assert!(logs_contain("container.id=\"1234567890\""));
    }

    #[tokio::test]
    async fn resolve_stale_returns_none_when_no_mapping() {
        let state = Arc::new(RwLock::new(DaemonState::default()));
        let addr: SocketAddr = "0.0.0.0:9000".parse().unwrap();
        let result = resolve_stale_container(&state, addr, "new-container").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_stale_returns_none_when_no_match() {
        let state = Arc::new(RwLock::new(DaemonState::default()));
        state.write().await.mapping.insert(
            "other".into(),
            vec![DockerPortMap::new(
                1,
                DockerPortMapRequest {
                    host_addr: "0.0.0.0:8080".parse().unwrap(),
                    container_addr: "10.0.0.2:8080".parse().unwrap(),
                    proto: TransportProtocol::Tcp,
                },
                "other".into(),
                "other-container".into(),
            )],
        );
        let addr: SocketAddr = "0.0.0.0:9000".parse().unwrap();
        let result = resolve_stale_container(&state, addr, "new-container").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_stale_returns_stale_id_when_match() {
        let state = Arc::new(RwLock::new(DaemonState::default()));
        state.write().await.mapping.insert(
            "stale".into(),
            vec![DockerPortMap::new(
                1,
                DockerPortMapRequest {
                    host_addr: "0.0.0.0:9000".parse().unwrap(),
                    container_addr: "10.0.0.2:9000".parse().unwrap(),
                    proto: TransportProtocol::Tcp,
                },
                "stale".into(),
                "old-container".into(),
            )],
        );
        let addr: SocketAddr = "0.0.0.0:9000".parse().unwrap();
        let result = resolve_stale_container(&state, addr, "new-container").await;
        assert_eq!(result, Some("stale".to_string()));
    }

    #[tokio::test]
    async fn resolve_stale_returns_none_for_same_container() {
        let state = Arc::new(RwLock::new(DaemonState::default()));
        state.write().await.mapping.insert(
            "same".into(),
            vec![DockerPortMap::new(
                1,
                DockerPortMapRequest {
                    host_addr: "0.0.0.0:9000".parse().unwrap(),
                    container_addr: "10.0.0.2:9000".parse().unwrap(),
                    proto: TransportProtocol::Tcp,
                },
                "same".into(),
                "same-container".into(),
            )],
        );
        let addr: SocketAddr = "0.0.0.0:9000".parse().unwrap();
        let result = resolve_stale_container(&state, addr, "same").await;
        assert!(result.is_none(), "same container should not be stale");
    }

    #[tokio::test]
    async fn resolve_stale_returns_correct_id_when_multiple_containers() {
        let state = Arc::new(RwLock::new(DaemonState::default()));
        state.write().await.mapping.insert(
            "alpha".into(),
            vec![DockerPortMap::new(
                1,
                DockerPortMapRequest {
                    host_addr: "0.0.0.0:8080".parse().unwrap(),
                    container_addr: "10.0.0.2:8080".parse().unwrap(),
                    proto: TransportProtocol::Tcp,
                },
                "alpha".into(),
                "alpha-container".into(),
            )],
        );
        state.write().await.mapping.insert(
            "bravo".into(),
            vec![DockerPortMap::new(
                2,
                DockerPortMapRequest {
                    host_addr: "0.0.0.0:9000".parse().unwrap(),
                    container_addr: "10.0.0.3:9000".parse().unwrap(),
                    proto: TransportProtocol::Tcp,
                },
                "bravo".into(),
                "bravo-container".into(),
            )],
        );
        let addr: SocketAddr = "0.0.0.0:9000".parse().unwrap();
        let result = resolve_stale_container(&state, addr, "new-container").await;
        assert_eq!(result, Some("bravo".to_string()));
    }

    #[test]
    fn untracked_returns_empty_when_all_tracked() {
        let running = id_set(&["a", "b"]);
        let tracked = id_set(&["a", "b"]);
        let result = untracked_container_ids(&running, &tracked);
        assert!(result.is_empty());
    }

    #[test]
    fn untracked_returns_new_ids() {
        let running = id_set(&["a", "b", "c"]);
        let tracked = id_set(&["a"]);
        let mut result = untracked_container_ids(&running, &tracked);
        result.sort();
        assert_eq!(result, vec!["b", "c"]);
    }

    #[test]
    fn untracked_returns_empty_when_no_running() {
        let running = HashSet::new();
        let tracked = id_set(&["a"]);
        let result = untracked_container_ids(&running, &tracked);
        assert!(result.is_empty());
    }

    #[test]
    fn untracked_ignores_tracked_not_running() {
        let running = id_set(&["b"]);
        let tracked = id_set(&["a", "b"]);
        let result = untracked_container_ids(&running, &tracked);
        assert!(result.is_empty());
    }

    #[test]
    fn reconcile_addr_no_change() {
        let mut stored = DockerPortMapRequest {
            host_addr: "0.0.0.0:9000".parse().unwrap(),
            container_addr: "10.0.0.2:9000".parse().unwrap(),
            proto: TransportProtocol::Tcp,
        };
        let current = DockerPortMapRequest {
            host_addr: "0.0.0.0:9000".parse().unwrap(),
            container_addr: "10.0.0.2:9000".parse().unwrap(),
            proto: TransportProtocol::Tcp,
        };
        assert!(!reconcile_container_addr(&mut stored, &current));
        assert_eq!(
            stored.container_addr,
            "10.0.0.2:9000".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn reconcile_addr_updated() {
        let mut stored = DockerPortMapRequest {
            host_addr: "0.0.0.0:9000".parse().unwrap(),
            container_addr: "10.0.0.2:9000".parse().unwrap(),
            proto: TransportProtocol::Tcp,
        };
        let current = DockerPortMapRequest {
            host_addr: "0.0.0.0:9000".parse().unwrap(),
            container_addr: "10.0.0.3:9000".parse().unwrap(),
            proto: TransportProtocol::Tcp,
        };
        assert!(reconcile_container_addr(&mut stored, &current));
        assert_eq!(
            stored.container_addr,
            "10.0.0.3:9000".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn reconcile_addr_different_host_port_same_container_ip() {
        let mut stored = DockerPortMapRequest {
            host_addr: "0.0.0.0:8080".parse().unwrap(),
            container_addr: "10.0.0.2:80".parse().unwrap(),
            proto: TransportProtocol::Tcp,
        };
        let current = DockerPortMapRequest {
            host_addr: "0.0.0.0:9090".parse().unwrap(),
            container_addr: "10.0.0.2:80".parse().unwrap(),
            proto: TransportProtocol::Tcp,
        };
        assert!(!reconcile_container_addr(&mut stored, &current));
        assert_eq!(
            stored.container_addr,
            "10.0.0.2:80".parse::<SocketAddr>().unwrap()
        );
    }

    // --- Ensure docker mapping ---

    #[tokio::test]
    async fn ensure_docker_mapping_allocates_and_installs() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let mapping = make_mapping(1, 39010, 8080, "c1");

        ensure_docker_mapping(&ports, fake.as_ref(), &mapping)
            .await
            .unwrap();

        assert_eq!(fake.installed_mappings(), vec![mapping]);
        assert!(ports.is_allocated(make_addr(39010)).await);
    }

    #[tokio::test]
    async fn ensure_docker_mapping_rolls_back_when_install_fails() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        fake.set_fail_dockermap(true);

        let result =
            ensure_docker_mapping(&ports, fake.as_ref(), &make_mapping(1, 39011, 8080, "c1")).await;

        assert!(result.is_err());
        assert!(fake.installed_mappings().is_empty());
        assert!(!ports.is_allocated(make_addr(39011)).await);
    }

    #[tokio::test]
    async fn ensure_docker_mapping_fails_when_port_held() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        ports
            .allocate(make_addr(39012), TransportProtocol::Tcp)
            .await
            .unwrap();

        let result =
            ensure_docker_mapping(&ports, fake.as_ref(), &make_mapping(1, 39012, 8080, "c1")).await;

        assert!(result.is_err());
        assert!(fake.installed_mappings().is_empty());
        assert!(ports.is_allocated(make_addr(39012)).await);
    }

    // --- Apply discovered mappings ---

    #[tokio::test]
    async fn apply_discovered_mappings_stale_deallocates_then_ensures() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with(
            temp_dir.path().join("state.json"),
            fake.clone(),
            ports.clone(),
        );
        let stale = make_mapping(5, 39001, 8080, "old");
        daemon
            .state
            .daemon_state
            .write()
            .await
            .mapping
            .insert("old".into(), vec![stale.clone()]);
        ports
            .allocate(make_addr(39001), TransportProtocol::Tcp)
            .await
            .unwrap();

        let assigned = daemon
            .apply_discovered_mappings("new", vec![make_mapping(0, 39001, 8080, "new")])
            .await;

        assert_eq!(fake.removed_mappings(), vec![stale]);
        assert_eq!(
            fake.installed_mappings(),
            vec![make_mapping(1, 39001, 8080, "new")]
        );
        assert_eq!(assigned, vec![make_mapping(1, 39001, 8080, "new")]);
        assert!(ports.is_allocated(make_addr(39001)).await);
    }

    #[tokio::test]
    async fn apply_discovered_mappings_skips_when_port_held_by_active_container() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with(
            temp_dir.path().join("state.json"),
            fake.clone(),
            ports.clone(),
        );
        ports
            .allocate(make_addr(39002), TransportProtocol::Tcp)
            .await
            .unwrap();

        let assigned = daemon
            .apply_discovered_mappings("new", vec![make_mapping(0, 39002, 8080, "new")])
            .await;

        assert!(assigned.is_empty());
        assert!(fake.installed_mappings().is_empty());
        assert!(fake.removed_mappings().is_empty());
    }

    // --- Ensure container mappings ---

    #[tokio::test]
    async fn ensure_container_mappings_installs_untracked_container() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with(
            temp_dir.path().join("state.json"),
            fake.clone(),
            ports.clone(),
        );

        let mut max_id = 0;
        let installed = daemon
            .ensure_container_mappings(
                "c1",
                vec![
                    make_mapping(0, 39003, 8080, "c1"),
                    make_mapping(0, 39004, 8081, "c1"),
                ],
                &mut max_id,
            )
            .await;

        assert_eq!(
            installed,
            vec![
                make_mapping(1, 39003, 8080, "c1"),
                make_mapping(2, 39004, 8081, "c1"),
            ]
        );
        assert_eq!(max_id, 2);
        assert!(ports.is_allocated(make_addr(39003)).await);
        assert!(ports.is_allocated(make_addr(39004)).await);
    }

    #[tokio::test]
    async fn ensure_container_mappings_drops_when_install_fails() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with(
            temp_dir.path().join("state.json"),
            fake.clone(),
            ports.clone(),
        );
        fake.set_fail_dockermap(true);

        let mut max_id = 0;
        let installed = daemon
            .ensure_container_mappings("c1", vec![make_mapping(0, 39005, 8080, "c1")], &mut max_id)
            .await;

        assert!(installed.is_empty());
        assert_eq!(max_id, 0);
        assert!(!ports.is_allocated(make_addr(39005)).await);
    }

    // --- Reconcile tracked mapping ---

    fn make_tracked_mapping(id: u64, host_port: u16, ctn_addr: &str) -> DockerPortMap {
        DockerPortMap::new(
            id,
            DockerPortMapRequest {
                host_addr: make_addr(host_port),
                container_addr: ctn_addr.parse().unwrap(),
                proto: TransportProtocol::Tcp,
            },
            "c1".into(),
            "test-container".into(),
        )
    }

    #[tokio::test]
    async fn reconcile_tracked_mapping_reinstalls_reverified_ip() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with(
            temp_dir.path().join("state.json"),
            fake.clone(),
            ports.clone(),
        );
        let stored = make_tracked_mapping(7, 39006, "10.0.0.2:8080");
        let current_addrs = HashMap::from([(make_addr(39006), make_addr(8080))]);

        let kept = daemon
            .reconcile_tracked_mapping("c1", stored.clone(), &current_addrs)
            .await
            .unwrap();

        assert_eq!(kept.request.container_addr, make_addr(8080));
        assert_eq!(fake.installed_mappings(), vec![kept.clone()]);
        assert!(ports.is_allocated(make_addr(39006)).await);
    }

    #[tokio::test]
    async fn reconcile_tracked_mapping_drops_when_port_held() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with(
            temp_dir.path().join("state.json"),
            fake.clone(),
            ports.clone(),
        );
        ports
            .allocate(make_addr(39007), TransportProtocol::Tcp)
            .await
            .unwrap();
        let stored = make_tracked_mapping(8, 39007, "10.0.0.2:8080");

        let kept = daemon
            .reconcile_tracked_mapping("c1", stored, &HashMap::new())
            .await;

        assert!(kept.is_none());
        assert!(fake.installed_mappings().is_empty());
    }

    #[tokio::test]
    async fn reconcile_tracked_mapping_keeps_stored_ip_when_inspect_missing() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with(
            temp_dir.path().join("state.json"),
            fake.clone(),
            ports.clone(),
        );
        let stored = make_tracked_mapping(9, 39008, "10.0.0.2:8080");

        let kept = daemon
            .reconcile_tracked_mapping("c1", stored.clone(), &HashMap::new())
            .await
            .unwrap();

        assert_eq!(
            kept.request.container_addr,
            "10.0.0.2:8080".parse().unwrap()
        );
        assert_eq!(fake.installed_mappings(), vec![kept]);
    }

    // --- Ensure static rule ---

    #[tokio::test]
    async fn ensure_static_rule_allocates_and_installs_dnat() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let config = make_dnat("39020");

        ensure_static_rule(&ports, fake.as_ref(), StaticRule::Dnat(&config))
            .await
            .unwrap();

        assert_eq!(fake.installed_dnats(), vec![config]);
        assert!(ports.is_allocated(make_addr(39020)).await);
    }

    #[tokio::test]
    async fn ensure_static_rule_allocates_and_installs_hairpin() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let config = make_hairpin("39021");

        ensure_static_rule(&ports, fake.as_ref(), StaticRule::Hairpin(&config))
            .await
            .unwrap();

        assert_eq!(fake.installed_hairpins(), vec![config]);
        assert!(ports.is_allocated(make_addr(39021)).await);
    }

    #[tokio::test]
    async fn ensure_static_rule_rolls_back_when_install_fails() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        fake.set_fail_dnat(true);

        let result =
            ensure_static_rule(&ports, fake.as_ref(), StaticRule::Dnat(&make_dnat("39022"))).await;

        assert!(result.is_err());
        assert!(fake.installed_dnats().is_empty());
        assert!(!ports.is_allocated(make_addr(39022)).await);
    }

    #[tokio::test]
    async fn ensure_static_rule_rolls_back_reserved_ports_when_later_port_held() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        // Hold the second port at the OS level (outside the allocator) so
        // allocation fails mid-loop instead of being skipped.
        let held = std::net::TcpListener::bind(make_addr(39034)).unwrap();
        let _ = &held;

        let result = ensure_static_rule(
            &ports,
            fake.as_ref(),
            StaticRule::Dnat(&make_dnat("39033,39034")),
        )
        .await;

        assert!(result.is_err());
        assert!(fake.installed_dnats().is_empty());
        assert!(
            !ports.is_allocated(make_addr(39033)).await,
            "reserved port must be released on mid-loop failure"
        );
        ports
            .allocate(make_addr(39033), TransportProtocol::Tcp)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn ensure_static_rule_rolls_back_partial_when_later_port_held() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        ports
            .allocate(make_addr(39024), TransportProtocol::Tcp)
            .await
            .unwrap();
        fake.set_fail_dnat(true);

        let result = ensure_static_rule(
            &ports,
            fake.as_ref(),
            StaticRule::Dnat(&make_dnat("39023,39024")),
        )
        .await;

        assert!(result.is_err());
        assert!(fake.installed_dnats().is_empty());
        assert!(
            !ports.is_allocated(make_addr(39023)).await,
            "newly reserved port must be released"
        );
        assert!(
            ports.is_allocated(make_addr(39024)).await,
            "pre-held port must be untouched"
        );
    }

    #[tokio::test]
    async fn ensure_static_rule_skips_allocated_port() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        ports
            .allocate(make_addr(39025), TransportProtocol::Tcp)
            .await
            .unwrap();
        let config = make_dnat("39025,39026");

        ensure_static_rule(&ports, fake.as_ref(), StaticRule::Dnat(&config))
            .await
            .unwrap();

        assert_eq!(fake.installed_dnats(), vec![config]);
        assert!(ports.is_allocated(make_addr(39025)).await);
        assert!(ports.is_allocated(make_addr(39026)).await);
    }

    #[tokio::test]
    async fn ensure_static_rule_rejects_invalid_ip() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let config = DnatConfig {
            ext_ip: "not-an-ip".to_string(),
            int_ip: "10.0.0.99".to_string(),
            ports: "39027".to_string(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        };

        let result = ensure_static_rule(&ports, fake.as_ref(), StaticRule::Dnat(&config)).await;

        assert!(result.is_err());
        assert!(fake.installed_dnats().is_empty());
        assert!(!ports.is_allocated(make_addr(39027)).await);
    }

    // --- Reconcile dnat / hairpin configs ---

    #[tokio::test]
    async fn reconcile_dnats_keeps_config_when_ensure_succeeds() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with(
            temp_dir.path().join("state.json"),
            fake.clone(),
            ports.clone(),
        );
        let mut daemon_state = DaemonState {
            dnats: vec![make_dnat("39030")],
            ..Default::default()
        };

        daemon.reconcile_dnats(&mut daemon_state).await;

        assert_eq!(daemon_state.dnats, vec![make_dnat("39030")]);
        assert_eq!(fake.installed_dnats(), vec![make_dnat("39030")]);
        assert!(ports.is_allocated(make_addr(39030)).await);
    }

    #[tokio::test]
    async fn reconcile_dnats_drops_config_when_ensure_fails() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with(
            temp_dir.path().join("state.json"),
            fake.clone(),
            ports.clone(),
        );
        fake.set_fail_dnat(true);
        let mut daemon_state = DaemonState {
            dnats: vec![make_dnat("39031")],
            ..Default::default()
        };

        daemon.reconcile_dnats(&mut daemon_state).await;

        assert!(daemon_state.dnats.is_empty());
        assert!(fake.installed_dnats().is_empty());
        assert!(!ports.is_allocated(make_addr(39031)).await);
    }

    #[tokio::test]
    async fn reconcile_hairpins_routes_through_ensure_static_rule() {
        let fake = Arc::new(FakeIptables::default());
        let ports = Arc::new(PortAllocator::new());
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with(
            temp_dir.path().join("state.json"),
            fake.clone(),
            ports.clone(),
        );
        let mut daemon_state = DaemonState {
            hairpins: vec![make_hairpin("39032")],
            ..Default::default()
        };

        daemon.reconcile_hairpins(&mut daemon_state).await;

        assert_eq!(daemon_state.hairpins, vec![make_hairpin("39032")]);
        assert_eq!(fake.installed_hairpins(), vec![make_hairpin("39032")]);
        assert!(ports.is_allocated(make_addr(39032)).await);
    }
}
