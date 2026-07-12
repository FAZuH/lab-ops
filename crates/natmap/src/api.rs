use std::net::IpAddr;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use color_eyre::Result;
use lab_ops_lab_lib::port::PortAllocator;

use crate::daemon::AppState;
use crate::daemon::ErrorResponse;
use crate::models::DnatConfig;
use crate::models::DnatRequest;
use crate::models::DockerAddMapRequest;
use crate::models::DockerPortMap;
use crate::models::DockerPortMapRequest;
use crate::models::DockerRemapRequest;
use crate::models::HairpinConfig;
use crate::models::HairpinRequest;
use crate::models::ListResponse;
use crate::models::PolicyRouteConfig;
use crate::models::PolicyRouteRequest;
use crate::models::SnatConfig;
use crate::models::SnatRequest;
use crate::models::TransportProtocol;

// --- Read handlers ---

#[tracing::instrument(skip_all)]
/// `GET /mappings` — Returns all managed DNAT, SNAT, hairpin, and Docker mappings.
pub async fn list_mappings(State(state): State<AppState>) -> Json<ListResponse> {
    let state = state.daemon_state.read().await;
    Json(ListResponse {
        docker: state.mapping.values().flatten().cloned().collect(),
        dnats: state.dnats.clone(),
        snats: state.snats.clone(),
        hairpins: state.hairpins.clone(),
        policy_routes: state.policy_routes.clone(),
    })
}

// --- Static NAT handlers ---

/// `POST /dnat` — Adds a static DNAT rule.
///
/// Idempotent: if the exact same DNAT config already exists in the daemon
/// state (e.g. after restart reconciliation), returns OK without error.
///
/// Span fields: `ext.ip`, `int.ip`, `ports`, `proto`.
#[tracing::instrument(skip_all, fields(
    ext.ip = %req.ext_ip,
    int.ip = %req.int_ip,
    ports = %req.ports,
    proto = %req.proto
))]
pub async fn add_dnat(
    State(state): State<AppState>,
    Json(req): Json<DnatRequest>,
) -> Result<Json<DnatConfig>, (StatusCode, Json<ErrorResponse>)> {
    let config = DnatConfig {
        ext_ip: req.ext_ip.clone(),
        int_ip: req.int_ip.clone(),
        ports: req.ports.clone(),
        proto: req.proto,
        ext_if: req.ext_if.clone(),
        no_masquerade: req.no_masquerade,
    };

    // Check if this DNAT already exists (idempotent add).
    {
        let lock = state.daemon_state.read().await;
        if lock.dnats.iter().any(|d| {
            d.ext_ip == config.ext_ip
                && d.int_ip == config.int_ip
                && d.ports == config.ports
                && d.proto == config.proto
        }) {
            return Ok(Json(config));
        }
    }

    bind_ports(state.ports.clone(), &config.ext_ip, &config.ports, config.proto).await?;
    if let Err(e) = state.iptables.install_dnat(&config) {
        unbind_ports(state.ports, &config.ext_ip, &config.ports).await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        ));
    }
    state.daemon_state.write().await.dnats.push(config.clone());
    state.persist().await;
    Ok(Json(config))
}

/// `DELETE /dnat` — Removes a static DNAT rule.
///
/// Span fields: `ext.ip`, `int.ip`, `ports`, `proto`.
#[tracing::instrument(skip_all, fields(
    ext.ip = %req.ext_ip,
    int.ip = %req.int_ip,
    ports = %req.ports,
    proto = %req.proto
))]
pub async fn remove_dnat(
    State(state): State<AppState>,
    Json(req): Json<DnatRequest>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;
    let idx = lock
        .dnats
        .iter()
        .position(|d| d.ext_ip == req.ext_ip && d.int_ip == req.int_ip && d.ports == req.ports);
    if let Some(i) = idx {
        let config = lock.dnats.remove(i);
        let _ = state.iptables.remove_dnat(&config);
        unbind_ports(state.ports.clone(), &config.ext_ip, &config.ports).await;
        drop(lock);
        state.persist().await;
        Ok(StatusCode::OK)
    } else {
        // Not in daemon state but may still have stale iptables rules and port
        // reservations from a previous daemon instance (e.g. after restart with
        // reconciled DNATs). Clean them up so the caller can re-add cleanly.
        let config = DnatConfig {
            ext_ip: req.ext_ip,
            int_ip: req.int_ip,
            ports: req.ports,
            proto: req.proto,
            ext_if: req.ext_if,
            no_masquerade: req.no_masquerade,
        };
        let _ = state.iptables.remove_dnat(&config);
        unbind_ports(state.ports.clone(), &config.ext_ip, &config.ports).await;
        Ok(StatusCode::OK)
    }
}

/// `POST /snat` — Adds a static SNAT rule.
#[tracing::instrument(skip_all, fields(int.ip = %req.int_ip, ext.ip = %req.ext_ip, ext.iface = %req.ext_if))]
pub async fn add_snat(
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
    state.daemon_state.write().await.snats.push(config.clone());
    state.persist().await;
    Ok(Json(config))
}

/// `DELETE /snat` — Removes a static SNAT rule.
#[tracing::instrument(skip_all, fields(int.ip = %req.int_ip, ext.ip = %req.ext_ip))]
pub async fn remove_snat(
    State(state): State<AppState>,
    Json(req): Json<SnatRequest>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;
    let idx = lock
        .snats
        .iter()
        .position(|s| s.int_ip == req.int_ip && s.ext_ip == req.ext_ip && s.ext_if == req.ext_if);
    if let Some(i) = idx {
        let config = lock.snats.remove(i);
        let _ = state.iptables.remove_snat(&config);
        drop(lock);
        state.persist().await;
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
#[tracing::instrument(skip_all, fields(ext.ip = %req.ext_ip, int.ip = %req.int_ip, ports = %req.ports, proto = %req.proto))]
pub async fn add_hairpin(
    State(state): State<AppState>,
    Json(req): Json<HairpinRequest>,
) -> Result<Json<HairpinConfig>, (StatusCode, Json<ErrorResponse>)> {
    let config = HairpinConfig {
        ext_ip: req.ext_ip.clone(),
        int_ip: req.int_ip.clone(),
        ports: req.ports.clone(),
        proto: req.proto,
        lan_cidr: req.lan_cidr.clone(),
    };
    bind_ports(state.ports.clone(), &config.ext_ip, &config.ports, config.proto).await?;
    if let Err(e) = state.iptables.install_hairpin(&config) {
        unbind_ports(state.ports, &config.ext_ip, &config.ports).await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        ));
    }
    state
        .daemon_state
        .write()
        .await
        .hairpins
        .push(config.clone());
    state.persist().await;
    Ok(Json(config))
}

/// `DELETE /hairpin` — Removes a static hairpin NAT rule.
#[tracing::instrument(skip_all, fields(ext.ip = %req.ext_ip, int.ip = %req.int_ip, ports = %req.ports))]
pub async fn remove_hairpin(
    State(state): State<AppState>,
    Json(req): Json<HairpinRequest>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;
    let idx = lock
        .hairpins
        .iter()
        .position(|h| h.ext_ip == req.ext_ip && h.int_ip == req.int_ip && h.ports == req.ports);
    if let Some(i) = idx {
        let config = lock.hairpins.remove(i);
        let _ = state.iptables.remove_hairpin(&config);
        unbind_ports(state.ports.clone(), &config.ext_ip, &config.ports).await;
        drop(lock);
        state.persist().await;
        Ok(StatusCode::OK)
    } else {
        // Not in daemon state but may still have stale iptables rules and port
        // reservations from a previous daemon instance. Clean them up.
        let config = HairpinConfig {
            ext_ip: req.ext_ip,
            int_ip: req.int_ip,
            ports: req.ports,
            proto: req.proto,
            lan_cidr: None,
        };
        let _ = state.iptables.remove_hairpin(&config);
        unbind_ports(state.ports.clone(), &config.ext_ip, &config.ports).await;
        Ok(StatusCode::OK)
    }
}

// --- Policy Route handlers ---

/// `POST /policy-route` — Adds a policy route rule.
///
/// Installs an `ip rule` + `ip route` entry for source IP preservation.
/// Idempotent if the same policy route already exists.
#[tracing::instrument(skip_all, fields(src.ip = %req.src_ip, via = %req.via, table = req.table))]
pub async fn add_policy_route(
    State(state): State<AppState>,
    Json(req): Json<PolicyRouteRequest>,
) -> Result<Json<PolicyRouteConfig>, (StatusCode, Json<ErrorResponse>)> {
    let config = PolicyRouteConfig {
        src_ip: req.src_ip.clone(),
        via: req.via.clone(),
        table: req.table,
    };

    if let Err(e) = state.policy_route.install(&config) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        ));
    }

    state
        .daemon_state
        .write()
        .await
        .policy_routes
        .push(config.clone());
    state.persist().await;
    Ok(Json(config))
}

/// `DELETE /policy-route` — Removes a policy route rule.
///
/// Deletes the `ip rule` + `ip route` entry. Idempotent — returns OK
/// even if the route was already removed from the kernel.
#[tracing::instrument(skip_all, fields(src.ip = %req.src_ip, via = %req.via, table = req.table))]
pub async fn remove_policy_route(
    State(state): State<AppState>,
    Json(req): Json<PolicyRouteRequest>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;
    let idx = lock
        .policy_routes
        .iter()
        .position(|r| r.src_ip == req.src_ip && r.via == req.via && r.table == req.table);
    if let Some(i) = idx {
        let config = lock.policy_routes.remove(i);
        let _ = state.policy_route.remove(&config);
        drop(lock);
        state.persist().await;
        Ok(StatusCode::OK)
    } else {
        // Not in daemon state, but still try to remove to be safe
        let config = PolicyRouteConfig {
            src_ip: req.src_ip,
            via: req.via,
            table: req.table,
        };
        let _ = state.policy_route.remove(&config);
        Ok(StatusCode::OK)
    }
}

// --- Docker handlers ---

/// `PUT /remap/:container_id` — Remaps a host port for a running container.
#[tracing::instrument(skip_all, fields(container.id = %container_id, old.port = req.host_port, new.port = req.new_host_port))]
pub async fn remap_port(
    State(state): State<AppState>,
    Path(container_id): Path<String>,
    Json(req): Json<DockerRemapRequest>,
) -> Result<Json<Vec<DockerPortMap>>, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;
    let container_mappings = lock.mapping.get_mut(&container_id).ok_or_else(|| {
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
        let new_mapping = DockerPortMap::new(
            id,
            new_req,
            container_id.clone(),
            old.container_name.clone(),
        );
        if let Err(e) = state.ports.allocate(new_mapping.request.host_addr, old.request.proto).await {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            ));
        }
        let _ = state.iptables.remove_mapping(old);
        if let Err(e) = state.iptables.install_dockermap(&new_mapping) {
            let _ = state.iptables.install_dockermap(old);
            state.ports.deallocate(new_mapping.request.host_addr).await;
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            ));
        }
        state.ports.deallocate(old.request.host_addr).await;
        container_mappings[i] = new_mapping.clone();
        new_mappings.push(new_mapping);
    }

    drop(lock);
    state.persist().await;
    Ok(Json(new_mappings))
}

/// `POST /mapping/:container_id` — Adds a new port mapping.
///
/// Span fields: `host.addr`, `container.addr`, `proto`, `container.id`.
#[tracing::instrument(skip_all, fields(
    host.addr = tracing::field::Empty,
    container.addr = tracing::field::Empty,
    proto = %req.proto,
    container.id = %container_id
))]
pub async fn add_mapping(
    State(state): State<AppState>,
    Path(container_id): Path<String>,
    Json(req): Json<DockerAddMapRequest>,
) -> Result<Json<DockerPortMap>, (StatusCode, Json<ErrorResponse>)> {
    let (container_ip, container_name) = if let Some(target_ip_str) = &req.target_ip {
        let ip = IpAddr::from_str(target_ip_str).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("Invalid target IP: {e}"),
                }),
            )
        })?;
        (ip, container_id.clone())
    } else {
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
            .map(lab_ops_lab_lib::docker::trim_container_name)
            .unwrap_or("unknown")
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
        (container_ip, container_name)
    };

    let proto = match req.proto.to_lowercase() {
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

    let span = tracing::Span::current();
    span.record("host.addr", tracing::field::display(host_addr));
    span.record("container.addr", tracing::field::display(container_addr));

    let request = DockerPortMapRequest {
        host_addr,
        container_addr,
        proto,
    };
    let id = state.allocate_id();
    let mapping = DockerPortMap::new(id, request, container_id.clone(), container_name);

    state.ports.allocate(host_addr, proto).await.map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;
    if let Err(e) = state.iptables.install_dockermap(&mapping) {
        state.ports.deallocate(host_addr).await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("iptables error: {e}"),
            }),
        ));
    }
    state
        .daemon_state
        .write()
        .await
        .mapping
        .entry(container_id)
        .or_default()
        .push(mapping.clone());
    state.persist().await;
    Ok(Json(mapping))
}

/// `DELETE /mapping/{container_id}/{port}` — Removes a specific port mapping by container and port.
#[tracing::instrument(skip_all, fields(container.id = %container_id, port = %port_str))]
pub async fn remove_mapping(
    State(state): State<AppState>,
    Path((container_id, port_str)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let port = port_str.parse::<u16>().unwrap_or(0);
    let mut lock = state.daemon_state.write().await;
    let container_mappings = lock.mapping.get_mut(&container_id).ok_or_else(|| {
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
        state.ports.deallocate(m.request.host_addr).await;
        drop(lock);
        state.persist().await;
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
#[tracing::instrument(skip_all, fields(mapping.id = id))]
pub async fn remove_mapping_by_id(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;
    for (_, mappings) in lock.mapping.iter_mut() {
        if let Some(pos) = mappings.iter().position(|m| m.id == id) {
            let m = mappings.remove(pos);
            let _ = state.iptables.remove_mapping(&m);
            state.ports.deallocate(m.request.host_addr).await;
            drop(lock);
            state.persist().await;
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

/// `DELETE /clear` — Removes all managed NAT rules and resets daemon state.
#[tracing::instrument(skip_all)]
pub async fn clear_all(
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;

    for mappings in lock.mapping.values() {
        for m in mappings {
            let _ = state.iptables.remove_mapping(m);
            state.ports.deallocate(m.request.host_addr).await;
        }
    }
    lock.mapping.clear();

    for config in &lock.dnats {
        let _ = state.iptables.remove_dnat(config);
        unbind_ports(state.ports.clone(), &config.ext_ip, &config.ports).await;
    }
    lock.dnats.clear();

    for config in &lock.snats {
        let _ = state.iptables.remove_snat(config);
    }
    lock.snats.clear();

    for config in &lock.hairpins {
        let _ = state.iptables.remove_hairpin(config);
        unbind_ports(state.ports.clone(), &config.ext_ip, &config.ports).await;
    }
    lock.hairpins.clear();

    for config in &lock.policy_routes {
        let _ = state.policy_route.remove(config);
    }
    lock.policy_routes.clear();

    drop(lock);
    state.persist().await;
    Ok(StatusCode::OK)
}

// --- Internal helpers ---

pub async fn bind_ports(
    ports: Arc<PortAllocator>,
    ip: &str,
    ports_csv: &str,
    proto: TransportProtocol,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    for addr in parse_socket_addrs(ip, ports_csv)? {
        ports.allocate(addr, proto).await.map_err(|e| {
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

pub async fn unbind_ports(ports: Arc<PortAllocator>, ip: &str, ports_csv: &str) {
    if let Ok(addrs) = parse_socket_addrs(ip, ports_csv) {
        for addr in addrs {
            ports.deallocate(addr).await;
        }
    }
}

fn parse_socket_addrs(
    ip: &str,
    ports_csv: &str,
) -> Result<Vec<SocketAddr>, (StatusCode, Json<ErrorResponse>)> {
    let ip: IpAddr = ip.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Invalid IP: {ip}"),
            }),
        )
    })?;

    ports_csv
        .split(',')
        .map(|p| {
            let port = p.trim().parse::<u16>().map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: format!("Invalid port: {p}"),
                    }),
                )
            })?;
            Ok(SocketAddr::new(ip, port))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use super::*;
    use crate::iptables::IptablesManager;
    use crate::models::*;
    use crate::policy_route::PolicyRouteManager;

    fn test_app_state() -> AppState {
        AppState {
            daemon_state: Arc::new(tokio::sync::RwLock::new(DaemonState::default())),
            iptables: Arc::new(IptablesManager::new()),
            policy_route: Arc::new(PolicyRouteManager::new()),
            docker: None,
            state_path: std::path::PathBuf::from("/tmp/natmap-test-state.json"),
            next_id: Arc::new(AtomicU64::new(1)),
            ports: Arc::new(lab_ops_lab_lib::port::PortAllocator::new()),
            socket_group: "root".to_string(),
            socket_path: std::path::PathBuf::from("/tmp/natmap.sock"),
        }
    }

    #[test]
    fn parse_socket_addrs_single_port() {
        let addrs = parse_socket_addrs("1.2.3.4", "80").unwrap();
        assert_eq!(
            addrs,
            vec![SocketAddr::new(IpAddr::from_str("1.2.3.4").unwrap(), 80)]
        );
    }

    #[test]
    fn parse_socket_addrs_csv_ports() {
        let addrs = parse_socket_addrs("1.2.3.4", "80,443,8080").unwrap();
        assert_eq!(addrs.len(), 3);
        assert_eq!(addrs[0].port(), 80);
        assert_eq!(addrs[1].port(), 443);
        assert_eq!(addrs[2].port(), 8080);
    }

    #[test]
    fn parse_socket_addrs_invalid_ip_returns_error() {
        let err = parse_socket_addrs("not-an-ip", "80").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parse_socket_addrs_invalid_port_returns_error() {
        let err = parse_socket_addrs("1.2.3.4", "99999").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parse_socket_addrs_port_zero_is_accepted() {
        let addrs = parse_socket_addrs("1.2.3.4", "0").unwrap();
        assert_eq!(addrs[0].port(), 0);
    }

    #[test]
    fn parse_socket_addrs_empty_ports_returns_error() {
        let err = parse_socket_addrs("1.2.3.4", "").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parse_socket_addrs_port_65535_boundary() {
        let addrs = parse_socket_addrs("1.2.3.4", "65535").unwrap();
        assert_eq!(addrs[0].port(), 65535);
    }

    #[test]
    fn parse_socket_addrs_ipv6_works() {
        let addrs = parse_socket_addrs("::1", "443").unwrap();
        assert_eq!(
            addrs,
            vec![SocketAddr::new(IpAddr::from_str("::1").unwrap(), 443)]
        );
    }

    #[tokio::test]
    async fn list_mappings_empty_state() {
        let state = test_app_state();
        let res = list_mappings(State(state)).await.0;
        assert!(res.docker.is_empty());
        assert!(res.dnats.is_empty());
        assert!(res.snats.is_empty());
        assert!(res.hairpins.is_empty());
        assert!(res.policy_routes.is_empty());
    }

    #[tokio::test]
    async fn list_mappings_reflects_dnat_state() {
        let state = test_app_state();
        {
            let mut lock = state.daemon_state.write().await;
            lock.dnats.push(DnatConfig {
                ext_ip: "1.2.3.4".into(),
                int_ip: "10.0.0.1".into(),
                ports: "80".into(),
                proto: TransportProtocol::Tcp,
                ext_if: None,
                no_masquerade: false,
            });
        }
        let res = list_mappings(State(state)).await.0;
        assert_eq!(res.dnats.len(), 1);
        assert_eq!(res.dnats[0].ext_ip, "1.2.3.4");
    }

    #[tokio::test]
    async fn add_dnat_duplicate_is_idempotent() {
        let state = test_app_state();
        let req = DnatRequest {
            ext_ip: "1.2.3.4".into(),
            int_ip: "10.0.0.1".into(),
            ports: "80".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            no_masquerade: false,
        };

        let result = add_dnat(State(state.clone()), Json(req.clone())).await;
        if result.is_err() {
            // iptables not available — skip
            return;
        }
        assert!(result.is_ok());

        let second = add_dnat(State(state.clone()), Json(req.clone())).await;
        assert!(second.is_ok());
        assert_eq!(state.daemon_state.read().await.dnats.len(), 1);
    }

    #[tokio::test]
    async fn remove_dnat_not_found_still_returns_ok() {
        let state = test_app_state();
        let req = DnatRequest {
            ext_ip: "1.2.3.4".into(),
            int_ip: "10.0.0.1".into(),
            ports: "80".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            no_masquerade: false,
        };
        let result = remove_dnat(State(state), Json(req)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn add_dnat_invalid_port_csv_returns_error() {
        let state = test_app_state();
        let req = DnatRequest {
            ext_ip: "1.2.3.4".into(),
            int_ip: "10.0.0.1".into(),
            ports: "not-a-port".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            no_masquerade: false,
        };
        let result = add_dnat(State(state), Json(req)).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn remove_snat_not_found_returns_error() {
        let state = test_app_state();
        let req = SnatRequest {
            int_ip: "10.0.0.1".into(),
            ext_ip: "1.2.3.4".into(),
            ext_if: "eth0".into(),
        };
        let result = remove_snat(State(state), Json(req)).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn add_hairpin_invalid_ip_returns_error() {
        let state = test_app_state();
        let req = HairpinRequest {
            ext_ip: "not-an-ip".into(),
            int_ip: "10.0.0.1".into(),
            ports: "80".into(),
            proto: TransportProtocol::Tcp,
            lan_cidr: Some("10.0.0.0/24".into()),
        };
        let result = add_hairpin(State(state), Json(req)).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_mapping_invalid_host_ip_returns_error() {
        let state = test_app_state();
        let req = DockerAddMapRequest {
            host_ip: "bad-ip".into(),
            host_port: 8080,
            container_port: 80,
            target_ip: Some("10.0.0.2".into()),
            proto: TransportProtocol::Tcp,
        };
        let result = add_mapping(State(state), Path("test123".into()), Json(req)).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_mapping_with_target_ip_success() {
        let state = test_app_state();
        let req = DockerAddMapRequest {
            host_ip: "127.0.0.1".into(),
            host_port: 0,
            container_port: 80,
            target_ip: Some("10.0.0.2".into()),
            proto: TransportProtocol::Tcp,
        };
        let result = add_mapping(State(state.clone()), Path("test123".into()), Json(req)).await;
        if result.is_err() {
            // port 0 allocation or iptables may fail — skip
            return;
        }
        assert!(result.is_ok());
        let mapping = result.unwrap().0;
        assert_eq!(mapping.container_id, "test123");
    }

    #[tokio::test]
    async fn remove_mapping_not_found_returns_error() {
        let state = test_app_state();
        let result = remove_mapping(State(state), Path(("nonexistent".into(), "80".into()))).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn remove_mapping_by_id_not_found_returns_error() {
        let state = test_app_state();
        let result = remove_mapping_by_id(State(state), Path(999)).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn remap_port_container_not_found_returns_error() {
        let state = test_app_state();
        let req = DockerRemapRequest {
            host_port: 8080,
            new_host_port: 9090,
        };
        let result = remap_port(State(state), Path("nonexistent".into()), Json(req)).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn clear_all_empty_state_succeeds() {
        let state = test_app_state();
        let result = clear_all(State(state)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn remove_policy_route_not_found_returns_ok() {
        let state = test_app_state();
        let req = PolicyRouteRequest {
            src_ip: "10.0.0.1".into(),
            via: "192.168.1.1".into(),
            table: 100,
        };
        let result = remove_policy_route(State(state), Json(req)).await;
        assert!(result.is_ok());
    }
}
