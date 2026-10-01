//! Axum HTTP handlers for the natmap daemon's Unix-socket API.

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
use crate::models::DockerAddMapRequest;
use crate::models::DockerPortMap;
use crate::models::DockerPortMapRequest;
use crate::models::DockerRemapRequest;
use crate::models::HairpinConfig;
use crate::models::ListResponse;
use crate::models::LiveRule;
use crate::models::PolicyRouteConfig;
use crate::models::RuleKind;
use crate::models::SnatConfig;
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

/// `GET /rules` — Returns all live NAT rules installed in iptables.
///
/// Parsed from the daemon's rule listing (all tables, natmap-commented lines
/// only). The daemon is the authority on what is actually installed.
/// Deterministic: rules are sorted and deduplicated.
#[tracing::instrument(skip_all)]
pub async fn list_rules(
    State(state): State<AppState>,
) -> Result<Json<Vec<LiveRule>>, (StatusCode, Json<ErrorResponse>)> {
    let lines = state
        .iptables
        .list_rules()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut rules: Vec<LiveRule> = lines.iter().filter_map(|l| parse_live_rule(l)).collect();
    rules.sort();
    rules.dedup();
    Ok(Json(rules))
}

// --- Static NAT handlers ---

/// `POST /dnat` — Adds a static DNAT rule.
///
/// Idempotent: if the exact same DNAT config already exists in the daemon
/// state (e.g. after restart reconciliation), returns OK without error.
///
/// Span fields: `ext.ip`, `int.ip`, `ports`, `proto`.
#[tracing::instrument(skip_all, fields(
    ext.ip = %config.ext_ip,
    int.ip = %config.int_ip,
    ports = %config.ports,
    proto = %config.proto
))]
pub async fn add_dnat(
    State(state): State<AppState>,
    Json(config): Json<DnatConfig>,
) -> Result<Json<DnatConfig>, (StatusCode, Json<ErrorResponse>)> {
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

    bind_ports(
        state.ports.clone(),
        &config.ext_ip,
        &config.ports,
        config.proto,
    )
    .await?;
    if let Err(e) = state.iptables.install_dnat(&config) {
        unbind_ports(state.ports, &config.ext_ip, &config.ports).await;
        return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }
    state.daemon_state.write().await.dnats.push(config.clone());
    state.persist().await;
    Ok(Json(config))
}

/// `DELETE /dnat` — Removes a static DNAT rule.
///
/// Span fields: `ext.ip`, `int.ip`, `ports`, `proto`.
#[tracing::instrument(skip_all, fields(
    ext.ip = %config.ext_ip,
    int.ip = %config.int_ip,
    ports = %config.ports,
    proto = %config.proto
))]
pub async fn remove_dnat(
    State(state): State<AppState>,
    Json(config): Json<DnatConfig>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;
    let idx = lock.dnats.iter().position(|d| {
        d.ext_ip == config.ext_ip && d.int_ip == config.int_ip && d.ports == config.ports
    });
    if let Some(i) = idx {
        let config = lock.dnats.remove(i);
        let _ = state.iptables.remove_dnat(&config);
        unbind_ports(state.ports.clone(), &config.ext_ip, &config.ports).await;
        drop(lock);
        state.persist().await;
        Ok(StatusCode::OK)
    } else {
        // Absent from state, but a previous instance may have left the rules and
        // port reservations behind, so clean those up too.
        let _ = state.iptables.remove_dnat(&config);
        unbind_ports(state.ports.clone(), &config.ext_ip, &config.ports).await;
        Ok(StatusCode::OK)
    }
}

/// `POST /snat` — Adds a static SNAT rule.
#[tracing::instrument(skip_all, fields(int.ip = %config.int_ip, ext.ip = %config.ext_ip, ext.iface = %config.ext_if))]
pub async fn add_snat(
    State(state): State<AppState>,
    Json(config): Json<SnatConfig>,
) -> Result<Json<SnatConfig>, (StatusCode, Json<ErrorResponse>)> {
    state
        .iptables
        .install_snat(&config)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state.daemon_state.write().await.snats.push(config.clone());
    state.persist().await;
    Ok(Json(config))
}

/// `DELETE /snat` — Removes a static SNAT rule.
#[tracing::instrument(skip_all, fields(int.ip = %config.int_ip, ext.ip = %config.ext_ip))]
pub async fn remove_snat(
    State(state): State<AppState>,
    Json(config): Json<SnatConfig>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;
    let idx = lock.snats.iter().position(|s| {
        s.int_ip == config.int_ip && s.ext_ip == config.ext_ip && s.ext_if == config.ext_if
    });
    if let Some(i) = idx {
        let config = lock.snats.remove(i);
        let _ = state.iptables.remove_snat(&config);
        drop(lock);
        state.persist().await;
        Ok(StatusCode::OK)
    } else {
        Err(err(StatusCode::NOT_FOUND, "SNAT rule not found"))
    }
}

/// `POST /hairpin` — Adds a static hairpin NAT rule.
#[tracing::instrument(skip_all, fields(ext.ip = %config.ext_ip, int.ip = %config.int_ip, ports = %config.ports, proto = %config.proto))]
pub async fn add_hairpin(
    State(state): State<AppState>,
    Json(config): Json<HairpinConfig>,
) -> Result<Json<HairpinConfig>, (StatusCode, Json<ErrorResponse>)> {
    bind_ports(
        state.ports.clone(),
        &config.ext_ip,
        &config.ports,
        config.proto,
    )
    .await?;
    if let Err(e) = state.iptables.install_hairpin(&config) {
        unbind_ports(state.ports, &config.ext_ip, &config.ports).await;
        return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
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
#[tracing::instrument(skip_all, fields(ext.ip = %config.ext_ip, int.ip = %config.int_ip, ports = %config.ports))]
pub async fn remove_hairpin(
    State(state): State<AppState>,
    Json(config): Json<HairpinConfig>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;
    let idx = lock.hairpins.iter().position(|h| {
        h.ext_ip == config.ext_ip && h.int_ip == config.int_ip && h.ports == config.ports
    });
    if let Some(i) = idx {
        let config = lock.hairpins.remove(i);
        let _ = state.iptables.remove_hairpin(&config);
        unbind_ports(state.ports.clone(), &config.ext_ip, &config.ports).await;
        drop(lock);
        state.persist().await;
        Ok(StatusCode::OK)
    } else {
        // Absent from state, but a previous instance may have left the rules and
        // port reservations behind, so clean those up too.
        let _ = state.iptables.remove_hairpin(&config);
        unbind_ports(state.ports, &config.ext_ip, &config.ports).await;
        Ok(StatusCode::OK)
    }
}

// --- Policy Route handlers ---

/// `POST /policy-route` — Adds a policy route rule.
///
/// Installs an `ip rule` + `ip route` entry for source IP preservation.
/// Idempotent if the same policy route already exists.
#[tracing::instrument(skip_all, fields(src.ip = %config.src_ip, via = %config.via, table = config.table))]
pub async fn add_policy_route(
    State(state): State<AppState>,
    Json(config): Json<PolicyRouteConfig>,
) -> Result<Json<PolicyRouteConfig>, (StatusCode, Json<ErrorResponse>)> {
    if let Err(e) = state.policy_route.install(&config) {
        return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
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
#[tracing::instrument(skip_all, fields(src.ip = %config.src_ip, via = %config.via, table = config.table))]
pub async fn remove_policy_route(
    State(state): State<AppState>,
    Json(config): Json<PolicyRouteConfig>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;
    let idx = lock
        .policy_routes
        .iter()
        .position(|r| r.src_ip == config.src_ip && r.via == config.via && r.table == config.table);
    if let Some(i) = idx {
        let config = lock.policy_routes.remove(i);
        let _ = state.policy_route.remove(&config);
        drop(lock);
        state.persist().await;
        Ok(StatusCode::OK)
    } else {
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
    let container_mappings = lock
        .mapping
        .get_mut(&container_id)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "Container not found"))?;

    let mut to_replace = Vec::new();
    for (i, m) in container_mappings.iter().enumerate() {
        if m.request.host_addr.port() == req.host_port {
            to_replace.push(i);
        }
    }
    if to_replace.is_empty() {
        return Err(err(StatusCode::NOT_FOUND, "Port mapping not found"));
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
        if let Err(e) = state
            .ports
            .allocate(new_mapping.request.host_addr, old.request.proto)
            .await
        {
            return Err(err(StatusCode::CONFLICT, e.to_string()));
        }
        let _ = state.iptables.remove_mapping(old);
        if let Err(e) = state.iptables.install_dockermap(&new_mapping) {
            let _ = state.iptables.install_dockermap(old);
            state.ports.deallocate(new_mapping.request.host_addr).await;
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
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
        let ip = IpAddr::from_str(target_ip_str)
            .map_err(|e| err(StatusCode::BAD_REQUEST, format!("Invalid target IP: {e}")))?;
        (ip, container_id.clone())
    } else {
        let docker = state
            .docker
            .as_ref()
            .ok_or_else(|| err(StatusCode::SERVICE_UNAVAILABLE, "Docker not available"))?;
        let inspect = docker
            .inspect_container(&container_id, None)
            .await
            .map_err(|e| err(StatusCode::NOT_FOUND, format!("Container not found: {e}")))?;
        let container_name = inspect
            .name
            .as_deref()
            .map(lab_ops_lab_lib::docker::trim_container_name)
            .unwrap_or("unknown")
            .to_string();
        let network_settings = inspect
            .network_settings
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "Container has no network settings"))?;
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
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "Container has no IP address"))?;
        (container_ip, container_name)
    };

    let proto = req.proto;
    let host_ip = IpAddr::from_str(&req.host_ip)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("Invalid host IP: {e}")))?;
    let container_addr = SocketAddr::new(container_ip, req.container_port);
    let host_addr = if req.host_port == 0 {
        allocate_free_port(&state.ports, host_ip, proto).await?
    } else {
        let addr = SocketAddr::new(host_ip, req.host_port);
        state
            .ports
            .allocate(addr, proto)
            .await
            .map_err(|e| err(StatusCode::CONFLICT, e.to_string()))?;
        addr
    };

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

    if let Err(e) = state.iptables.install_dockermap(&mapping) {
        state.ports.deallocate(host_addr).await;
        return Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("iptables error: {e}"),
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
    let container_mappings = lock
        .mapping
        .get_mut(&container_id)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "Container not found"))?;
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
        Err(err(StatusCode::NOT_FOUND, "Port mapping not found"))
    }
}

/// `DELETE /mapping/by-id/:id` — Removes a port mapping by its numeric ID.
#[tracing::instrument(skip_all, fields(mapping.id = id))]
pub async fn remove_mapping_by_id(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut lock = state.daemon_state.write().await;
    for mappings in lock.mapping.values_mut() {
        if let Some(pos) = mappings.iter().position(|m| m.id == id) {
            let m = mappings.remove(pos);
            let _ = state.iptables.remove_mapping(&m);
            state.ports.deallocate(m.request.host_addr).await;
            drop(lock);
            state.persist().await;
            return Ok(StatusCode::OK);
        }
    }
    Err(err(
        StatusCode::NOT_FOUND,
        format!("No mapping found with id {id}"),
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

/// Builds the `(StatusCode, Json<ErrorResponse>)` tuple every handler returns on failure.
fn err(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
}

/// Lower bound of the ephemeral port range the daemon allocates from.
///
/// The natmap daemon is the single authority for this range.
pub(crate) const EPHEMERAL_PORT_START: u16 = 32768;
/// Upper bound of the ephemeral port range the daemon allocates from.
pub(crate) const EPHEMERAL_PORT_END: u16 = 61000;

/// Reserves the first free host port on `host_ip` from the ephemeral range.
///
/// Each candidate is reserved directly so a concurrent claim of the same port
/// is detected by the bind itself; on success the port is held by the caller
/// until it deallocates it.
async fn allocate_free_port(
    ports: &PortAllocator,
    host_ip: IpAddr,
    proto: TransportProtocol,
) -> Result<SocketAddr, (StatusCode, Json<ErrorResponse>)> {
    for port in EPHEMERAL_PORT_START..=EPHEMERAL_PORT_END {
        let addr = SocketAddr::new(host_ip, port);
        if ports.allocate(addr, proto).await.is_ok() {
            return Ok(addr);
        }
    }
    Err(err(
        StatusCode::SERVICE_UNAVAILABLE,
        "no free host port available in ephemeral range",
    ))
}

/// Reserves every port in `ports_csv` on `ip`, or returns 409 naming the first
/// port another holder already owns. A partial failure releases what it took.
pub async fn bind_ports(
    ports: Arc<PortAllocator>,
    ip: &str,
    ports_csv: &str,
    proto: TransportProtocol,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    for addr in parse_socket_addrs(ip, ports_csv)? {
        ports
            .allocate(addr, proto)
            .await
            .map_err(|e| err(StatusCode::CONFLICT, e.to_string()))?;
    }
    Ok(())
}

/// Releases every reservation `bind_ports` took for `ip`.
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
    let ip: IpAddr = ip
        .parse()
        .map_err(|_| err(StatusCode::BAD_REQUEST, format!("Invalid IP: {ip}")))?;

    ports_csv
        .split(',')
        .map(|p| {
            let port = p
                .trim()
                .parse::<u16>()
                .map_err(|_| err(StatusCode::BAD_REQUEST, format!("Invalid port: {p}")))?;
            Ok(SocketAddr::new(ip, port))
        })
        .collect()
}

/// Parses a natmap-commented `iptables-save` line into a [`LiveRule`].
///
/// Returns `None` for lines that are not natmap-managed NAT rules (e.g.
/// FORWARD ACCEPT rules for Docker mappings) or that lack the fields a rule
/// kind requires. Rule kind is attributed from the `natmap:*` comment prefix.
fn parse_live_rule(line: &str) -> Option<LiveRule> {
    let comment = line.split_once("--comment ")?;
    let comment = comment.1.trim();
    let comment = match comment.strip_prefix('"') {
        Some(inner) => inner.split('"').next().unwrap_or(comment),
        None => comment,
    };

    let (kind, rest) = if let Some(rest) = comment.strip_prefix("natmap:dnat:") {
        (RuleKind::Dnat, rest)
    } else if let Some(rest) = comment.strip_prefix("natmap:hairpin:") {
        (RuleKind::Hairpin, rest)
    } else if let Some(rest) = comment.strip_prefix("natmap:snat:") {
        (RuleKind::Snat, rest)
    } else {
        let rest = comment.strip_prefix("natmap:")?;
        (RuleKind::Mapping, rest)
    };

    let proto = match line.split(" -p ").nth(1) {
        Some(rest) => rest
            .split_whitespace()
            .next()?
            .parse::<TransportProtocol>()
            .ok()?,
        // SNAT rules carry no protocol; the daemon defaults to TCP.
        None => TransportProtocol::Tcp,
    };

    match kind {
        RuleKind::Dnat => {
            if !line.contains("-j DNAT") {
                return None;
            }
            let mut fields = rest.split(':');
            let ext_ip = fields.next()?.to_string();
            let ports = parse_ports_csv(fields.next()?)?;
            let int_ip = line
                .split("--to-destination ")
                .nth(1)?
                .split_whitespace()
                .next()?
                .split(':')
                .next()?
                .to_string();
            Some(LiveRule {
                kind,
                ext_ip,
                int_ip,
                ports,
                proto,
            })
        }
        RuleKind::Hairpin => {
            if !(line.contains("-j DNAT") || line.contains("-j MASQUERADE")) {
                return None;
            }
            let mut fields = rest.split(':');
            let ext_ip = fields.next()?.to_string();
            let int_ip = fields.next()?.to_string();
            let ports = parse_ports_csv(fields.next()?)?;
            Some(LiveRule {
                kind,
                ext_ip,
                int_ip,
                ports,
                proto,
            })
        }
        RuleKind::Snat => {
            if !line.contains("-j SNAT") {
                return None;
            }
            let int_ip = line
                .split(" -s ")
                .nth(1)?
                .split_whitespace()
                .next()?
                .split('/')
                .next()?
                .to_string();
            let ext_ip = line
                .split("--to-source ")
                .nth(1)?
                .split_whitespace()
                .next()?
                .to_string();
            Some(LiveRule {
                kind,
                ext_ip,
                int_ip,
                ports: Vec::new(),
                proto,
            })
        }
        RuleKind::Mapping => {
            if !line.contains("-j DNAT") {
                return None;
            }
            let ext_ip = line
                .split(" -d ")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .map(|ip| ip.split('/').next().unwrap_or("0.0.0.0").to_string())
                .unwrap_or_else(|| "0.0.0.0".to_string());
            let int_ip = line
                .split("--to-destination ")
                .nth(1)?
                .split_whitespace()
                .next()?
                .split(':')
                .next()?
                .to_string();
            let ports = parse_ports(line)?;
            Some(LiveRule {
                kind,
                ext_ip,
                int_ip,
                ports,
                proto,
            })
        }
    }
}

/// Extracts ports from an iptables line, preferring multiport `--dports`.
///
/// `--dports ` (with the trailing space) is not a substring of `--dport `,
/// so checking multiport first is unambiguous.
fn parse_ports(line: &str) -> Option<Vec<u16>> {
    if let Some(rest) = line.split(" --dports ").nth(1) {
        return parse_ports_csv(rest.split_whitespace().next()?);
    }
    if let Some(rest) = line.split(" --dport ").nth(1) {
        return parse_ports_csv(rest.split_whitespace().next()?);
    }
    None
}

/// Parses a comma-separated port list into `u16`s, skipping invalid entries.
fn parse_ports_csv(csv: &str) -> Option<Vec<u16>> {
    Some(csv.split(',').filter_map(|p| p.parse().ok()).collect())
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::str::FromStr;
    use std::sync::Arc;

    use super::*;
    use crate::daemon::tests::FakeIptables;
    use crate::daemon::tests::test_app_state_with;
    use crate::daemon::tests::test_port;
    use crate::models::*;

    /// An [`AppState`] on the iptables fake, in a temp dir of its own. The dir
    /// is returned because the state file and socket path live inside it.
    fn test_app_state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let state = test_app_state_with(&dir, Arc::new(FakeIptables::default()));
        (dir, state)
    }

    fn make_addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::from([127, 0, 0, 1]), port)
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
        let (_dir, state) = test_app_state();
        let res = list_mappings(State(state)).await.0;
        assert!(res.docker.is_empty());
        assert!(res.dnats.is_empty());
        assert!(res.snats.is_empty());
        assert!(res.hairpins.is_empty());
        assert!(res.policy_routes.is_empty());
    }

    #[tokio::test]
    async fn list_mappings_reflects_dnat_state() {
        let (_dir, state) = test_app_state();
        {
            let mut lock = state.daemon_state.write().await;
            lock.dnats.push(DnatConfig {
                ext_ip: "1.2.3.4".into(),
                int_ip: "10.0.0.1".into(),
                ports: "80".into(),
                proto: TransportProtocol::Tcp,
                ext_if: None,
                preserve_src_ip: false,
            });
        }
        let res = list_mappings(State(state)).await.0;
        assert_eq!(res.dnats.len(), 1);
        assert_eq!(res.dnats[0].ext_ip, "1.2.3.4");
    }

    #[tokio::test]
    async fn add_dnat_duplicate_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeIptables::default());
        let state = test_app_state_with(&dir, fake.clone());
        let req = DnatConfig {
            ext_ip: "127.0.0.1".into(),
            int_ip: "10.0.0.1".into(),
            // `add_dnat` reserves the ext port with a real bind, so it has to be
            // a loopback port below 1024 to run unprivileged.
            ports: test_port().to_string(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        };

        let first = add_dnat(State(state.clone()), Json(req.clone()))
            .await
            .unwrap()
            .0;
        let second = add_dnat(State(state.clone()), Json(req.clone()))
            .await
            .unwrap()
            .0;

        assert_eq!(first, second);
        assert_eq!(state.daemon_state.read().await.dnats.len(), 1);
        assert_eq!(
            fake.installed_dnats(),
            vec![req],
            "a duplicate must not install a second rule"
        );
    }

    #[tokio::test]
    async fn remove_dnat_not_found_still_returns_ok() {
        let (_dir, state) = test_app_state();
        let req = DnatConfig {
            ext_ip: "1.2.3.4".into(),
            int_ip: "10.0.0.1".into(),
            ports: "80".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        };
        let result = remove_dnat(State(state), Json(req)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn add_dnat_invalid_port_csv_returns_error() {
        let (_dir, state) = test_app_state();
        let req = DnatConfig {
            ext_ip: "1.2.3.4".into(),
            int_ip: "10.0.0.1".into(),
            ports: "not-a-port".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        };
        let result = add_dnat(State(state), Json(req)).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn remove_snat_not_found_returns_error() {
        let (_dir, state) = test_app_state();
        let req = SnatConfig {
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
        let (_dir, state) = test_app_state();
        let req = HairpinConfig {
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
        let (_dir, state) = test_app_state();
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
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeIptables::default());
        let state = test_app_state_with(&dir, fake.clone());
        let req = DockerAddMapRequest {
            host_ip: "127.0.0.1".into(),
            host_port: 39050,
            container_port: 80,
            target_ip: Some("10.0.0.2".into()),
            proto: TransportProtocol::Tcp,
        };
        let mapping = add_mapping(State(state.clone()), Path("test123".into()), Json(req))
            .await
            .unwrap()
            .0;

        assert_eq!(mapping.container_id, "test123");
        assert_eq!(mapping.request.host_addr.to_string(), "127.0.0.1:39050");
        assert_eq!(
            mapping.request.container_addr.to_string(),
            "10.0.0.2:80",
            "the mapping must DNAT to the requested target IP"
        );
        let installed = fake.installed_mappings();
        assert_eq!(installed, vec![mapping.clone()]);
        assert!(state.ports.is_allocated(mapping.request.host_addr).await);
    }

    #[tokio::test]
    async fn add_mapping_no_host_port_allocates_and_returns_port() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeIptables::default());
        let state = test_app_state_with(&dir, fake.clone());
        let req = DockerAddMapRequest {
            host_ip: "127.0.0.2".into(),
            host_port: 0,
            container_port: 80,
            target_ip: Some("10.0.0.2".into()),
            proto: TransportProtocol::Tcp,
        };

        let result = add_mapping(State(state.clone()), Path("c1".into()), Json(req)).await;
        let mapping = result.unwrap().0;

        let host_addr = mapping.request.host_addr;
        assert_eq!(host_addr.ip(), IpAddr::from_str("127.0.0.2").unwrap());
        assert!(
            (super::EPHEMERAL_PORT_START..=super::EPHEMERAL_PORT_END).contains(&host_addr.port())
        );
        assert_eq!(fake.installed_mappings(), vec![mapping.clone()]);
        assert!(state.ports.is_allocated(host_addr).await);
    }

    #[tokio::test]
    async fn add_mapping_taken_host_port_returns_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeIptables::default());
        let state = test_app_state_with(&dir, fake.clone());
        let host_port = test_port();
        let addr = make_addr(host_port);
        state
            .ports
            .allocate(addr, TransportProtocol::Tcp)
            .await
            .unwrap();
        let req = DockerAddMapRequest {
            host_ip: "127.0.0.1".into(),
            host_port,
            container_port: 80,
            target_ip: Some("10.0.0.2".into()),
            proto: TransportProtocol::Tcp,
        };

        let result = add_mapping(State(state.clone()), Path("c1".into()), Json(req)).await;

        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
        assert!(fake.installed_mappings().is_empty());
    }

    #[tokio::test]
    async fn add_mapping_install_failure_releases_allocated_port() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeIptables::default());
        fake.set_fail_dockermap(true);
        let state = test_app_state_with(&dir, fake.clone());
        let req = DockerAddMapRequest {
            host_ip: "127.0.0.4".into(),
            host_port: 0,
            container_port: 80,
            target_ip: Some("10.0.0.2".into()),
            proto: TransportProtocol::Tcp,
        };

        let result = add_mapping(State(state.clone()), Path("c1".into()), Json(req.clone())).await;
        assert_eq!(result.unwrap_err().0, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(fake.installed_mappings().is_empty());

        // Asserted against the allocator map, not a bind: the OS ephemeral range
        // overlaps the scan range, so an external bind would race.
        let leaked: Vec<u16> = {
            let mut leaked = Vec::new();
            for port in super::EPHEMERAL_PORT_START..=super::EPHEMERAL_PORT_START + 12 {
                let addr = SocketAddr::new(IpAddr::from_str("127.0.0.4").unwrap(), port);
                if state.ports.is_allocated(addr).await {
                    leaked.push(port);
                }
            }
            leaked
        };
        assert!(
            leaked.is_empty(),
            "allocated port must be released on install failure; still held: {leaked:?}"
        );

        fake.set_fail_dockermap(false);
        let mapping = add_mapping(State(state.clone()), Path("c1".into()), Json(req))
            .await
            .unwrap()
            .0;
        // Released and immediately re-allocatable, so the retry gets the same port.
        assert_eq!(
            mapping.request.host_addr.ip(),
            IpAddr::from_str("127.0.0.4").unwrap()
        );
        assert!(
            (super::EPHEMERAL_PORT_START..=super::EPHEMERAL_PORT_END)
                .contains(&mapping.request.host_addr.port())
        );
        assert!(state.ports.is_allocated(mapping.request.host_addr).await);
    }

    #[tokio::test]
    async fn add_mapping_explicit_port_install_failure_releases_allocated_port() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeIptables::default());
        fake.set_fail_dockermap(true);
        let state = test_app_state_with(&dir, fake.clone());
        let req = DockerAddMapRequest {
            host_ip: "127.0.0.4".into(),
            host_port: 39040,
            container_port: 80,
            target_ip: Some("10.0.0.2".into()),
            proto: TransportProtocol::Tcp,
        };

        let result = add_mapping(State(state.clone()), Path("c1".into()), Json(req.clone())).await;
        assert_eq!(result.unwrap_err().0, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(fake.installed_mappings().is_empty());

        // The explicitly requested port must be released on install failure.
        // Asserted against the allocator map, not a bind: the OS ephemeral range
        // overlaps, so an external bind would race.
        let addr = SocketAddr::new(IpAddr::from_str("127.0.0.4").unwrap(), 39040);
        assert!(
            !state.ports.is_allocated(addr).await,
            "explicitly requested port must be released on install failure"
        );

        fake.set_fail_dockermap(false);
        let mapping = add_mapping(State(state.clone()), Path("c1".into()), Json(req))
            .await
            .unwrap()
            .0;
        // Released and immediately re-allocatable: 39040 was the requested port.
        assert_eq!(mapping.request.host_addr.port(), 39040);
        assert!(state.ports.is_allocated(mapping.request.host_addr).await);
    }

    #[tokio::test]
    async fn remove_mapping_not_found_returns_error() {
        let (_dir, state) = test_app_state();
        let result = remove_mapping(State(state), Path(("nonexistent".into(), "80".into()))).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn remove_mapping_by_id_not_found_returns_error() {
        let (_dir, state) = test_app_state();
        let result = remove_mapping_by_id(State(state), Path(999)).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn remap_port_container_not_found_returns_error() {
        let (_dir, state) = test_app_state();
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
        let (_dir, state) = test_app_state();
        let result = clear_all(State(state)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn remove_policy_route_not_found_returns_ok() {
        let (_dir, state) = test_app_state();
        let req = PolicyRouteConfig {
            src_ip: "10.0.0.1".into(),
            via: "192.168.1.1".into(),
            table: 100,
        };
        let result = remove_policy_route(State(state), Json(req)).await;
        assert!(result.is_ok());
    }

    #[test]
    fn parse_ports_csv_skips_invalid_entries() {
        assert_eq!(parse_ports_csv("80,abc,443"), Some(vec![80, 443]));
        assert_eq!(parse_ports_csv("abc"), Some(vec![]));
    }

    #[test]
    fn parse_live_rule_single_port_dnat() {
        let line = r#"-A PREROUTING -d 203.0.113.50/32 -p tcp -m tcp --dport 36000 -j DNAT --to-destination 10.0.0.99:36000 -m comment --comment "natmap:dnat:203.0.113.50:36000""#;
        let rule = parse_live_rule(line).unwrap();
        assert_eq!(rule.kind, RuleKind::Dnat);
        assert_eq!(rule.ext_ip, "203.0.113.50");
        assert_eq!(rule.int_ip, "10.0.0.99");
        assert_eq!(rule.ports, vec![36000]);
        assert_eq!(rule.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_live_rule_multiport_dnat() {
        let line = r#"-A PREROUTING -d 203.0.113.50/32 -p tcp -m multiport --dports 80,443 -j DNAT --to-destination 10.0.0.99 -m comment --comment "natmap:dnat:203.0.113.50:80,443""#;
        let rule = parse_live_rule(line).unwrap();
        assert_eq!(rule.kind, RuleKind::Dnat);
        assert_eq!(rule.ext_ip, "203.0.113.50");
        assert_eq!(rule.int_ip, "10.0.0.99");
        assert_eq!(rule.ports, vec![80, 443]);
        assert_eq!(rule.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_live_rule_hairpin_dnat_form() {
        let line = r#"-A PREROUTING -s 10.0.0.99/32 -d 203.0.113.50/32 -p tcp -m tcp --dport 80 -j DNAT --to-destination 10.0.0.99 -m comment --comment "natmap:hairpin:203.0.113.50:10.0.0.99:80""#;
        let rule = parse_live_rule(line).unwrap();
        assert_eq!(rule.kind, RuleKind::Hairpin);
        assert_eq!(rule.ext_ip, "203.0.113.50");
        assert_eq!(rule.int_ip, "10.0.0.99");
        assert_eq!(rule.ports, vec![80]);
        assert_eq!(rule.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_live_rule_hairpin_masquerade_form() {
        let line = r#"-A POSTROUTING -s 10.0.0.0/24 -d 10.0.0.99/32 -p tcp -m tcp --dport 80 -j MASQUERADE -m comment --comment "natmap:hairpin:203.0.113.50:10.0.0.99:80""#;
        let rule = parse_live_rule(line).unwrap();
        assert_eq!(rule.kind, RuleKind::Hairpin);
        assert_eq!(rule.ext_ip, "203.0.113.50");
        assert_eq!(rule.int_ip, "10.0.0.99");
        assert_eq!(rule.ports, vec![80]);
        assert_eq!(rule.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_live_rule_docker_mapping() {
        let line = r#"-A NATMAP -p tcp -d 100.64.0.10/32 -m tcp --dport 8080 -j DNAT --to-destination 10.0.0.2:80 -m comment --comment "natmap:c1:8080""#;
        let rule = parse_live_rule(line).unwrap();
        assert_eq!(rule.kind, RuleKind::Mapping);
        assert_eq!(rule.ext_ip, "100.64.0.10");
        assert_eq!(rule.int_ip, "10.0.0.2");
        assert_eq!(rule.ports, vec![8080]);
        assert_eq!(rule.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_live_rule_snat() {
        let line = r#"-A POSTROUTING -s 10.0.0.1/32 -o eth0 -j SNAT --to-source 203.0.113.50 -m comment --comment "natmap:snat:10.0.0.1:203.0.113.50""#;
        let rule = parse_live_rule(line).unwrap();
        assert_eq!(rule.kind, RuleKind::Snat);
        assert_eq!(rule.ext_ip, "203.0.113.50");
        assert_eq!(rule.int_ip, "10.0.0.1");
        assert!(rule.ports.is_empty());
        assert_eq!(rule.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_live_rule_forward_accept_returns_none() {
        let line = r#"-A NATMAP -d 172.17.0.3/32 -p tcp -m tcp --dport 8080 -j ACCEPT -m comment --comment "natmap:c1:8080""#;
        assert!(parse_live_rule(line).is_none());
    }

    #[test]
    fn parse_live_rule_non_natmap_comment_returns_none() {
        let line = r#"-A PREROUTING -p tcp -m tcp --dport 22 -j DNAT --to-destination 10.0.0.5:22 -m comment --comment "docker:custom""#;
        assert!(parse_live_rule(line).is_none());
    }

    #[test]
    fn parse_live_rule_single_port_dnat_iptables_save_order() {
        // Real `iptables-save` renders the comment BEFORE the jump target.
        let line = r#"-A PREROUTING -d 203.0.113.50/32 -p tcp -m tcp --dport 36002 -m comment --comment "natmap:dnat:203.0.113.50:36002" -j DNAT --to-destination 10.0.0.99:36002"#;
        let rule = parse_live_rule(line).unwrap();
        assert_eq!(rule.kind, RuleKind::Dnat);
        assert_eq!(rule.ext_ip, "203.0.113.50");
        assert_eq!(rule.int_ip, "10.0.0.99");
        assert_eq!(rule.ports, vec![36002]);
        assert_eq!(rule.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_live_rule_multiport_dnat_iptables_save_order() {
        let line = r#"-A PREROUTING -d 203.0.113.50/32 -p tcp -m multiport --dports 80,443 -m comment --comment "natmap:dnat:203.0.113.50:80,443" -j DNAT --to-destination 10.0.0.99"#;
        let rule = parse_live_rule(line).unwrap();
        assert_eq!(rule.kind, RuleKind::Dnat);
        assert_eq!(rule.ext_ip, "203.0.113.50");
        assert_eq!(rule.int_ip, "10.0.0.99");
        assert_eq!(rule.ports, vec![80, 443]);
        assert_eq!(rule.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_live_rule_hairpin_dnat_iptables_save_order() {
        let line = r#"-A PREROUTING -s 10.0.0.99/32 -d 203.0.113.50/32 -p tcp -m tcp --dport 80 -m comment --comment "natmap:hairpin:203.0.113.50:10.0.0.99:80" -j DNAT --to-destination 10.0.0.99"#;
        let rule = parse_live_rule(line).unwrap();
        assert_eq!(rule.kind, RuleKind::Hairpin);
        assert_eq!(rule.ext_ip, "203.0.113.50");
        assert_eq!(rule.int_ip, "10.0.0.99");
        assert_eq!(rule.ports, vec![80]);
        assert_eq!(rule.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_live_rule_docker_mapping_iptables_save_order() {
        let line = r#"-A NATMAP -p tcp -d 100.64.0.10/32 -m tcp --dport 8080 -m comment --comment "natmap:c1:8080" -j DNAT --to-destination 10.0.0.2:80"#;
        let rule = parse_live_rule(line).unwrap();
        assert_eq!(rule.kind, RuleKind::Mapping);
        assert_eq!(rule.ext_ip, "100.64.0.10");
        assert_eq!(rule.int_ip, "10.0.0.2");
        assert_eq!(rule.ports, vec![8080]);
        assert_eq!(rule.proto, TransportProtocol::Tcp);
    }

    #[tokio::test]
    async fn list_rules_dedups_parsed_rules() {
        let fake = Arc::new(FakeIptables::default());
        fake.set_rules_lines(vec![
            r#"-A PREROUTING -d 203.0.113.50/32 -p tcp -m tcp --dport 36000 -j DNAT --to-destination 10.0.0.99:36000 -m comment --comment "natmap:dnat:203.0.113.50:36000""#.into(),
            r#"-A PREROUTING -d 203.0.113.50/32 -p tcp -m tcp --dport 36000 -j DNAT --to-destination 10.0.0.99:36000 -m comment --comment "natmap:dnat:203.0.113.50:36000""#.into(),
            r#"-A NATMAP -d 172.17.0.3/32 -p tcp -m tcp --dport 8080 -j ACCEPT -m comment --comment "natmap:c1:8080""#.into(),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let state = test_app_state_with(&dir, fake);
        let res = list_rules(State(state)).await.unwrap();
        assert_eq!(res.0.len(), 1);
        assert_eq!(res.0[0].kind, RuleKind::Dnat);
        assert_eq!(res.0[0].ext_ip, "203.0.113.50");
        assert_eq!(res.0[0].int_ip, "10.0.0.99");
    }
}
