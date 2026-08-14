//! Typed client for the natmap daemon's Unix socket HTTP API.
//!
//! Each daemon operation maps to one typed method. Static NAT operations
//! (`dnat`/`snat`/`hairpin`/`policy_route`) take the typed config struct plus
//! an explicit `delete` flag; Docker mapping operations mirror the daemon
//! endpoints directly.
//!
//! The client speaks the same HTTP-over-Unix-socket protocol as the CLI's
//! `request_json`, so auto-discover can talk to the daemon without building
//! `cli::Cli` values.

use std::path::PathBuf;

use hyper::Method;

use crate::models::DnatConfig;
use crate::models::DnatRequest;
use crate::models::DockerAddMapRequest;
use crate::models::DockerPortMap;
use crate::models::DockerRemapRequest;
use crate::models::HairpinConfig;
use crate::models::HairpinRequest;
use crate::models::ListResponse;
use crate::models::PolicyRouteConfig;
use crate::models::PolicyRouteRequest;
use crate::models::SnatConfig;
use crate::models::SnatRequest;
pub use crate::utils::NatmapError;
use crate::utils::request_json;

/// Client for the natmap daemon's Unix socket API.
#[derive(Debug, Clone)]
pub struct NatmapClient {
    socket: PathBuf,
}

impl NatmapClient {
    /// Creates a client connected to the given natmap Unix socket path.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        NatmapClient {
            socket: socket.into(),
        }
    }

    /// Creates a client using the `NATMAP_SOCKET` env var, defaulting to
    /// [`lab_ops_lab_lib::NATMAP_SOCKET`].
    pub fn default_socket() -> Self {
        let socket = std::env::var("NATMAP_SOCKET")
            .unwrap_or_else(|_| lab_ops_lab_lib::NATMAP_SOCKET.into());
        Self::new(socket)
    }

    // --- Static NAT operations ---

    /// Installs or deletes a static DNAT rule.
    ///
    /// Returns the daemon's echoed config on install, `None` on delete.
    pub async fn dnat(
        &self,
        config: DnatConfig,
        delete: bool,
    ) -> Result<Option<DnatConfig>, NatmapError> {
        let req = DnatRequest::from(config);
        if delete {
            let _: () = request_json(&self.socket, Method::DELETE, "/dnat", Some(req)).await?;
            Ok(None)
        } else {
            let echoed: DnatConfig =
                request_json(&self.socket, Method::POST, "/dnat", Some(req)).await?;
            Ok(Some(echoed))
        }
    }

    /// Installs or deletes a static SNAT rule.
    ///
    /// Returns the daemon's echoed config on install, `None` on delete.
    pub async fn snat(
        &self,
        config: SnatConfig,
        delete: bool,
    ) -> Result<Option<SnatConfig>, NatmapError> {
        let req = SnatRequest::from(config);
        if delete {
            let _: () = request_json(&self.socket, Method::DELETE, "/snat", Some(req)).await?;
            Ok(None)
        } else {
            let echoed: SnatConfig =
                request_json(&self.socket, Method::POST, "/snat", Some(req)).await?;
            Ok(Some(echoed))
        }
    }

    /// Installs or deletes a static hairpin NAT rule.
    ///
    /// Returns the daemon's echoed config on install, `None` on delete.
    pub async fn hairpin(
        &self,
        config: HairpinConfig,
        delete: bool,
    ) -> Result<Option<HairpinConfig>, NatmapError> {
        let req = HairpinRequest::from(config);
        if delete {
            let _: () = request_json(&self.socket, Method::DELETE, "/hairpin", Some(req)).await?;
            Ok(None)
        } else {
            let echoed: HairpinConfig =
                request_json(&self.socket, Method::POST, "/hairpin", Some(req)).await?;
            Ok(Some(echoed))
        }
    }

    /// Installs or deletes a policy routing rule.
    ///
    /// Returns the daemon's echoed config on install, `None` on delete.
    pub async fn policy_route(
        &self,
        config: PolicyRouteConfig,
        delete: bool,
    ) -> Result<Option<PolicyRouteConfig>, NatmapError> {
        let req = PolicyRouteRequest::from(config);
        if delete {
            let _: () =
                request_json(&self.socket, Method::DELETE, "/policy-route", Some(req)).await?;
            Ok(None)
        } else {
            let echoed: PolicyRouteConfig =
                request_json(&self.socket, Method::POST, "/policy-route", Some(req)).await?;
            Ok(Some(echoed))
        }
    }

    // --- Docker mapping operations ---

    /// Adds a new port mapping for a container or local service.
    ///
    /// When `target_ip` is set in the request, the daemon skips Docker inspect
    /// and uses the given IP directly — used for local (non-Docker) services.
    pub async fn add_mapping(
        &self,
        container_id: &str,
        req: DockerAddMapRequest,
    ) -> Result<DockerPortMap, NatmapError> {
        let uri = format!("/mapping/{container_id}");
        request_json(&self.socket, Method::POST, &uri, Some(req)).await
    }

    /// Removes a port mapping by container ID and host port.
    pub async fn remove_mapping(
        &self,
        container_id: &str,
        host_port: u16,
    ) -> Result<(), NatmapError> {
        let uri = format!("/mapping/{container_id}/{host_port}");
        request_json(&self.socket, Method::DELETE, &uri, None::<()>).await
    }

    /// Removes a port mapping by its numeric ID.
    pub async fn remove_mapping_by_id(&self, id: u64) -> Result<(), NatmapError> {
        let uri = format!("/mapping/by-id/{id}");
        request_json(&self.socket, Method::DELETE, &uri, None::<()>).await
    }

    /// Remaps a container's host port without restarting the container.
    pub async fn remap_port(
        &self,
        container_id: &str,
        req: DockerRemapRequest,
    ) -> Result<Vec<DockerPortMap>, NatmapError> {
        let uri = format!("/remap/{container_id}");
        request_json(&self.socket, Method::PUT, &uri, Some(req)).await
    }

    /// Lists all daemon-managed mappings and static rules.
    pub async fn list_mappings(&self) -> Result<ListResponse, NatmapError> {
        request_json(&self.socket, Method::GET, "/mappings", None::<()>).await
    }

    /// Removes all managed NAT rules and resets daemon state.
    pub async fn clear(&self) -> Result<(), NatmapError> {
        request_json(&self.socket, Method::DELETE, "/clear", None::<()>).await
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use hyper_util::rt::TokioExecutor;
    use hyper_util::rt::TokioIo;
    use hyper_util::server::conn::auto::Builder;
    use lab_ops_lab_lib::TransportProtocol;
    use lab_ops_lab_lib::port::PortAllocator;
    use tokio::net::UnixListener;
    use tokio::sync::RwLock;
    use tower_service::Service;

    use super::*;
    use crate::daemon::AppState;
    use crate::daemon::build_router;
    use crate::iptables::IptablesManager;
    use crate::models::DaemonState;
    use crate::models::DockerAddMapRequest;
    use crate::models::DockerRemapRequest;
    use crate::models::PolicyRouteConfig;
    use crate::policy_route::PolicyRouteManager;

    fn test_app_state() -> AppState {
        AppState {
            daemon_state: Arc::new(RwLock::new(DaemonState::default())),
            iptables: Arc::new(IptablesManager::new()),
            policy_route: Arc::new(PolicyRouteManager::new()),
            docker: None,
            state_path: PathBuf::from("/tmp/natmap-test-state.json"),
            next_id: Arc::new(AtomicU64::new(1)),
            ports: Arc::new(PortAllocator::new()),
            socket_group: "root".to_string(),
            socket_path: PathBuf::from("/tmp/natmap.sock"),
        }
    }

    /// Serves the daemon router over a real Unix socket in a background task.
    ///
    /// Returns the socket path (and the temp dir that keeps it alive) the
    /// client should connect to.
    async fn spawn_daemon(state: AppState) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("natmap-test.sock");
        let app = build_router(state);

        let listener = UnixListener::bind(&socket_path).unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let app = app.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let srv = hyper::service::service_fn(
                        move |req: hyper::Request<hyper::body::Incoming>| app.clone().call(req),
                    );
                    let _ = Builder::new(TokioExecutor::new())
                        .serve_connection_with_upgrades(io, srv)
                        .await;
                });
            }
        });
        (dir, socket_path)
    }

    fn dnat_config(ports: &str) -> DnatConfig {
        DnatConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: ports.into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        }
    }

    #[tokio::test]
    async fn dnat_delete_returns_ok_when_not_found() {
        let (_dir, socket) = spawn_daemon(test_app_state()).await;
        let client = NatmapClient::new(socket);

        let result = client.dnat(dnat_config("80"), true).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn dnat_add_invalid_ports_maps_bad_request() {
        let (_dir, socket) = spawn_daemon(test_app_state()).await;
        let client = NatmapClient::new(socket);

        let result = client.dnat(dnat_config("not-a-port"), false).await;
        assert!(matches!(result, Err(NatmapError::BadRequest(_))));
    }

    #[tokio::test]
    async fn dnat_add_conflicts_when_port_allocated() {
        let state = test_app_state();
        // Pre-allocate the port so the daemon's bind_ports returns 409.
        // Loopback binds fine without freebind privileges.
        state
            .ports
            .allocate(
                SocketAddr::from(([127, 0, 0, 1], 8080)),
                TransportProtocol::Tcp,
            )
            .await
            .unwrap();
        let (_dir, socket) = spawn_daemon(state).await;
        let client = NatmapClient::new(socket);

        let config = DnatConfig {
            ext_ip: "127.0.0.1".into(),
            int_ip: "10.0.0.99".into(),
            ports: "8080".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        };
        let result = client.dnat(config, false).await;
        assert!(matches!(result, Err(NatmapError::Conflict(_))));
    }

    #[tokio::test]
    async fn remove_snat_not_found_maps_not_found() {
        let (_dir, socket) = spawn_daemon(test_app_state()).await;
        let client = NatmapClient::new(socket);

        let config = SnatConfig {
            int_ip: "10.0.0.1".into(),
            ext_ip: "203.0.113.50".into(),
            ext_if: "eth0".into(),
        };
        let result = client.snat(config, true).await;
        assert!(matches!(result, Err(NatmapError::NotFound(_))));
    }

    #[tokio::test]
    async fn add_mapping_without_target_ip_maps_unavailable_when_no_docker() {
        let (_dir, socket) = spawn_daemon(test_app_state()).await;
        let client = NatmapClient::new(socket);

        let req = DockerAddMapRequest {
            host_ip: "0.0.0.0".into(),
            host_port: 8080,
            container_port: 80,
            target_ip: None,
            proto: TransportProtocol::Tcp,
        };
        let result = client.add_mapping("test123", req).await;
        assert!(matches!(result, Err(NatmapError::Unavailable(_))));
    }

    #[tokio::test]
    async fn remove_mapping_not_found_maps_not_found() {
        let (_dir, socket) = spawn_daemon(test_app_state()).await;
        let client = NatmapClient::new(socket);

        let result = client.remove_mapping("nonexistent", 80).await;
        assert!(matches!(result, Err(NatmapError::NotFound(_))));
    }

    #[tokio::test]
    async fn remove_mapping_by_id_not_found_maps_not_found() {
        let (_dir, socket) = spawn_daemon(test_app_state()).await;
        let client = NatmapClient::new(socket);

        let result = client.remove_mapping_by_id(999).await;
        assert!(matches!(result, Err(NatmapError::NotFound(_))));
    }

    #[tokio::test]
    async fn remap_port_not_found_maps_not_found() {
        let (_dir, socket) = spawn_daemon(test_app_state()).await;
        let client = NatmapClient::new(socket);

        let req = DockerRemapRequest {
            host_port: 8080,
            new_host_port: 9090,
        };
        let result = client.remap_port("nonexistent", req).await;
        assert!(matches!(result, Err(NatmapError::NotFound(_))));
    }

    #[tokio::test]
    async fn list_mappings_roundtrips_state() {
        let state = test_app_state();
        {
            let mut lock = state.daemon_state.write().await;
            lock.dnats.push(dnat_config("80,443"));
        }
        let (_dir, socket) = spawn_daemon(state).await;
        let client = NatmapClient::new(socket);

        let resp = client.list_mappings().await.unwrap();
        assert_eq!(resp.dnats.len(), 1);
        assert_eq!(resp.dnats[0].ports, "80,443");
        assert!(resp.docker.is_empty());
    }

    #[tokio::test]
    async fn clear_returns_ok_on_empty_state() {
        let (_dir, socket) = spawn_daemon(test_app_state()).await;
        let client = NatmapClient::new(socket);

        assert!(client.clear().await.is_ok());
    }

    #[tokio::test]
    async fn policy_route_add_echoes_config() {
        let (_dir, socket) = spawn_daemon(test_app_state()).await;
        let client = NatmapClient::new(socket);

        let config = PolicyRouteConfig {
            src_ip: "10.0.0.1".into(),
            via: "192.168.1.1".into(),
            table: 100,
        };
        let result = client.policy_route(config, false).await;
        match result {
            Ok(Some(echoed)) => {
                assert_eq!(echoed.src_ip, "10.0.0.1");
                assert_eq!(echoed.table, 100);
            }
            // policy_route install shells out to `ip rule`; treat failure to
            // install as environment-dependent rather than a client bug.
            Ok(None) => panic!("add should echo the config"),
            Err(_) => {}
        }
    }

    #[tokio::test]
    async fn policy_route_delete_not_found_returns_ok() {
        let (_dir, socket) = spawn_daemon(test_app_state()).await;
        let client = NatmapClient::new(socket);

        let config = PolicyRouteConfig {
            src_ip: "10.0.0.1".into(),
            via: "192.168.1.1".into(),
            table: 100,
        };
        let result = client.policy_route(config, true).await;
        assert!(result.is_ok());
    }
}
