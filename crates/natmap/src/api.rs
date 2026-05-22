use std::net::IpAddr;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use color_eyre::Result;
use lab_lib::port::PortAllocator;

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
use crate::models::SnatConfig;
use crate::models::SnatRequest;
use crate::models::TransportProtocol;

/// `GET /mappings` — Returns all managed DNAT, SNAT, hairpin, and Docker mappings.
pub async fn list_mappings(State(state): State<AppState>) -> Json<ListResponse> {
    let state = state.daemon_state.read().await;
    Json(ListResponse {
        docker: state.mapping.values().flatten().cloned().collect(),
        dnats: state.dnats.clone(),
        snats: state.snats.clone(),
        hairpins: state.hairpins.clone(),
    })
}

// --- Static NAT handlers ---

/// `POST /dnat` — Adds a static DNAT rule.
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
    };
    bind_ports(state.ports.clone(), &config.ext_ip, &config.ports).await?;
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
        Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "DNAT rule not found".into(),
            }),
        ))
    }
}

/// `POST /snat` — Adds a static SNAT rule.
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
pub async fn add_hairpin(
    State(state): State<AppState>,
    Json(req): Json<HairpinRequest>,
) -> Result<Json<HairpinConfig>, (StatusCode, Json<ErrorResponse>)> {
    let config = HairpinConfig {
        ext_ip: req.ext_ip.clone(),
        int_ip: req.int_ip.clone(),
        ports: req.ports.clone(),
        proto: req.proto,
    };
    bind_ports(state.ports.clone(), &config.ext_ip, &config.ports).await?;
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
        if let Err(e) = state.ports.allocate(new_mapping.request.host_addr).await {
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
            .map(lab_lib::docker::trim_container_name)
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
    let request = DockerPortMapRequest {
        host_addr,
        container_addr,
        proto,
    };
    let id = state.allocate_id();
    let mapping = DockerPortMap::new(id, request, container_id.clone(), container_name);

    state.ports.allocate(host_addr).await.map_err(|e| {
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

    drop(lock);
    state.persist().await;
    Ok(StatusCode::OK)
}

pub async fn bind_ports(
    ports: Arc<PortAllocator>,
    ip: &str,
    ports_csv: &str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    for addr in parse_socket_addrs(ip, ports_csv)? {
        ports.allocate(addr).await.map_err(|e| {
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
