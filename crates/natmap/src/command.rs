//! CLI command implementations that communicate with the natmap daemon.
//!
//! Each `handle_*` function constructs the appropriate HTTP request to the
//! daemon's Unix socket API and formats the response for display.

use std::path::Path;
use std::process::Command;

use color_eyre::eyre::Result;
use color_eyre::eyre::bail;
use comfy_table::Attribute;
use comfy_table::Color;
use hyper::Method;
use lab_ops_lab_lib::TransportProtocol;

use crate::models::DnatConfig;
use crate::models::DnatRequest;
use crate::models::DockerAddMapRequest;
use crate::models::DockerPortMap;
use crate::models::DockerRemapRequest;
use crate::models::HairpinConfig;
use crate::models::HairpinRequest;
use crate::models::ListResponse;
use crate::models::SnatConfig;
use crate::models::SnatRequest;
use crate::utils::request_json;

/// Displays a combined listing of static iptables NAT rules and daemon-managed state.
///
/// Reads live iptables rules via `iptables-save` (filtering to natmap-commented
/// rules only) and queries the daemon at `GET /mappings` for managed DNAT, SNAT,
/// hairpin, and Docker mappings.
pub async fn handle_list(
    socket: impl AsRef<Path>,
    container_id: Option<String>,
    json: bool,
    use_color: bool,
) -> Result<()> {
    println!("── Static iptables NAT rules (natmap-managed) ──");
    let output = Command::new("iptables-save").output();
    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let rules: Vec<&str> = stdout.lines().filter(|l| l.contains("natmap:")).collect();
            if rules.is_empty() {
                println!("  (none)");
            } else {
                let mut table = comfy_table::Table::new();
                if use_color {
                    table.set_header(vec![
                        comfy_table::Cell::new("CHAIN")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                        comfy_table::Cell::new("RULE")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                    ]);
                } else {
                    table.set_header(vec!["CHAIN", "RULE"]);
                }
                for r in &rules {
                    let rest = r.strip_prefix("-A ").unwrap_or(r);
                    let (chain, rule) = rest.split_once(' ').unwrap_or((rest, ""));
                    table.add_row(vec![chain.to_string(), rule.to_string()]);
                }
                println!("{table}");
            }
        }
        Err(_) => println!("  (could not read iptables rules)"),
    }

    println!("\n── Daemon-managed state ──");
    match request_json::<ListResponse, ()>(socket, Method::GET, "/mappings", None).await {
        Ok(resp) => {
            if !resp.dnats.is_empty() {
                let mut table = comfy_table::Table::new();
                if use_color {
                    table.set_header(vec![
                        comfy_table::Cell::new("EXT IP")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                        comfy_table::Cell::new("INT IP")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                        comfy_table::Cell::new("PORTS")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                        comfy_table::Cell::new("PROTO")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                        comfy_table::Cell::new("IFACE")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                    ]);
                } else {
                    table.set_header(vec!["EXT IP", "INT IP", "PORTS", "PROTO", "IFACE"]);
                }
                for d in &resp.dnats {
                    let if_info = d.ext_if.as_deref().unwrap_or("-");
                    table.add_row(vec![
                        d.ext_ip.clone(),
                        d.int_ip.clone(),
                        d.ports.clone(),
                        d.proto.to_string(),
                        if_info.to_string(),
                    ]);
                }
                println!("  DNAT rules:\n{table}");
            }
            if !resp.snats.is_empty() {
                let mut table = comfy_table::Table::new();
                if use_color {
                    table.set_header(vec![
                        comfy_table::Cell::new("INT IP")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                        comfy_table::Cell::new("EXT IP")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                        comfy_table::Cell::new("IFACE")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                    ]);
                } else {
                    table.set_header(vec!["INT IP", "EXT IP", "IFACE"]);
                }
                for s in &resp.snats {
                    table.add_row(vec![s.int_ip.clone(), s.ext_ip.clone(), s.ext_if.clone()]);
                }
                println!("  SNAT rules:\n{table}");
            }
            if !resp.hairpins.is_empty() {
                let mut table = comfy_table::Table::new();
                if use_color {
                    table.set_header(vec![
                        comfy_table::Cell::new("EXT IP")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                        comfy_table::Cell::new("INT IP")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                        comfy_table::Cell::new("PORTS")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                        comfy_table::Cell::new("PROTO")
                            .fg(Color::Cyan)
                            .add_attribute(Attribute::Bold),
                    ]);
                } else {
                    table.set_header(vec!["EXT IP", "INT IP", "PORTS", "PROTO"]);
                }
                for h in &resp.hairpins {
                    table.add_row(vec![
                        h.ext_ip.clone(),
                        h.int_ip.clone(),
                        h.ports.clone(),
                        h.proto.to_string(),
                    ]);
                }
                println!("  Hairpin rules:\n{table}");
            }
            if !resp.docker.is_empty() {
                println!("  Docker mappings:");
                if json {
                    println!("{}", serde_json::to_string_pretty(&resp.docker)?);
                } else {
                    let mut table = comfy_table::Table::new();
                    if use_color {
                        table.set_header(vec![
                            comfy_table::Cell::new("ID")
                                .fg(Color::Cyan)
                                .add_attribute(Attribute::Bold),
                            comfy_table::Cell::new("CONTAINER")
                                .fg(Color::Cyan)
                                .add_attribute(Attribute::Bold),
                            comfy_table::Cell::new("CONTAINER ID")
                                .fg(Color::Cyan)
                                .add_attribute(Attribute::Bold),
                            comfy_table::Cell::new("HOST ADDR")
                                .fg(Color::Cyan)
                                .add_attribute(Attribute::Bold),
                            comfy_table::Cell::new("CONTAINER ADDR")
                                .fg(Color::Cyan)
                                .add_attribute(Attribute::Bold),
                            comfy_table::Cell::new("PROTO")
                                .fg(Color::Cyan)
                                .add_attribute(Attribute::Bold),
                        ]);
                    } else {
                        table.set_header(vec![
                            "ID",
                            "CONTAINER",
                            "CONTAINER ID",
                            "HOST ADDR",
                            "CONTAINER ADDR",
                            "PROTO",
                        ]);
                    }
                    for m in resp.docker {
                        if let Some(ref cid) = container_id
                            && !m.container_id.starts_with(cid)
                            && m.container_name != *cid
                        {
                            continue;
                        }
                        table.add_row(vec![
                            m.id.to_string(),
                            m.container_name,
                            m.container_id.chars().take(12).collect::<String>(),
                            m.request.host_addr.to_string(),
                            m.request.container_addr.to_string(),
                            m.request.proto.to_string(),
                        ]);
                    }
                    println!("{table}");
                }
            }
        }
        Err(_) => {
            println!("  (daemon not running — use `natmap daemon` to start)");
        }
    }

    Ok(())
}

/// Adds or removes a static DNAT rule via the daemon API.
pub async fn handle_dnat(
    ext_ip: String,
    int_ip: String,
    proto: String,
    ports: String,
    ext_if: Option<String>,
    delete: bool,
    socket: impl AsRef<Path>,
) -> Result<()> {
    let req = DnatRequest {
        ext_ip,
        int_ip,
        ports,
        proto: proto.parse()?,
        ext_if,
    };
    if delete {
        let _: () = request_json(socket, Method::DELETE, "/dnat", Some(req)).await?;
        tracing::info!("dnat rule removed");
    } else {
        let _res: DnatConfig = request_json(socket, Method::POST, "/dnat", Some(req)).await?;
        tracing::info!("dnat rule added");
    }
    Ok(())
}

/// Adds or removes a static SNAT rule via the daemon API.
pub async fn handle_snat(
    int_ip: String,
    ext_if: String,
    ext_ip: String,
    delete: bool,
    socket: impl AsRef<Path>,
) -> Result<()> {
    let req = SnatRequest {
        int_ip,
        ext_if,
        ext_ip,
    };
    if delete {
        let _: () = request_json(socket, Method::DELETE, "/snat", Some(req)).await?;
        tracing::info!("snat rule removed");
    } else {
        let _res: SnatConfig = request_json(socket, Method::POST, "/snat", Some(req)).await?;
        tracing::info!("snat rule added");
    }
    Ok(())
}

/// Adds or removes a static hairpin NAT rule via the daemon API.
pub async fn handle_hairpin(
    ext_ip: String,
    int_ip: String,
    proto: String,
    ports: String,
    delete: bool,
    socket: impl AsRef<Path>,
) -> Result<()> {
    let req = HairpinRequest {
        ext_ip,
        int_ip,
        ports,
        proto: proto.parse()?,
    };
    if delete {
        let _: () = request_json(socket, Method::DELETE, "/hairpin", Some(req)).await?;
        tracing::info!("hairpin rule removed");
    } else {
        let _res: HairpinConfig = request_json(socket, Method::POST, "/hairpin", Some(req)).await?;
        tracing::info!("hairpin rule added");
    }
    Ok(())
}

/// Sends a clear-all request to the daemon, removing all managed rules and resetting state.
pub async fn handle_clear(socket: impl AsRef<Path>) -> Result<()> {
    let _: () = request_json(socket, Method::DELETE, "/clear", None::<()>).await?;
    tracing::info!("all nat rules cleared");
    Ok(())
}

/// Lists Docker port mappings from the daemon (active mappings only).
pub async fn list(container_id: Option<String>, socket: &str, json: bool) -> Result<()> {
    let res: Vec<DockerPortMap> =
        request_json(socket, Method::GET, "/mappings", None::<()>).await?;
    let res = if let Some(cid) = container_id {
        res.into_iter()
            .filter(|m| m.container_id.starts_with(&cid) || m.container_name == cid)
            .collect()
    } else {
        res
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&res)?);
    } else {
        let mut table = comfy_table::Table::new();
        table.set_header(vec![
            "ID",
            "CONTAINER",
            "CONTAINER ID",
            "HOST ADDR",
            "CONTAINER ADDR",
            "PROTO",
        ]);
        for m in res {
            table.add_row(vec![
                m.id.to_string(),
                m.container_name,
                m.container_id.chars().take(12).collect::<String>(),
                m.request.host_addr.to_string(),
                m.request.container_addr.to_string(),
                m.request.proto.to_string(),
            ]);
        }
        println!("{table}");
    }

    Ok(())
}

/// Lists Docker mappings, silently returning a message when the daemon is not reachable.
pub async fn try_list(socket: &str, container_id: Option<String>, json: bool) -> Result<()> {
    match list(container_id, socket, json).await {
        Ok(()) => Ok(()),
        Err(_) => {
            println!("  (daemon not running)");
            Ok(())
        }
    }
}

/// Remaps a container's host port to a new port without restarting the container.
pub async fn remap(
    container_id: String,
    mapping: String,
    socket: impl AsRef<Path>,
    json: bool,
) -> Result<()> {
    let parts: Vec<&str> = mapping.split(':').collect();
    if parts.len() != 2 {
        bail!("Invalid mapping format. Use <old_host_port>:<new_host_port>");
    }
    let req = DockerRemapRequest {
        host_port: parts[0].parse()?,
        new_host_port: parts[1].parse()?,
    };
    let uri = format!("/remap/{container_id}");
    let res: Vec<DockerPortMap> = request_json(socket, Method::PUT, &uri, Some(req)).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&res)?);
    } else {
        tracing::info!(count = res.len(), "successfully remapped rules");
    }
    Ok(())
}

/// Adds a new port mapping via the daemon API.
pub async fn add(
    container_id: String,
    mapping_opt: Option<String>,
    name: Option<String>,
    socket: impl AsRef<Path>,
    json: bool,
) -> Result<()> {
    let (container_id, mapping) = match (name, mapping_opt) {
        (Some(n), Some(m)) => (n, m),
        (Some(n), None) => (n, container_id),
        (None, Some(m)) => (container_id, m),
        (None, None) => bail!(
            "Missing mapping. Usage: docker add <CONTAINER_ID> <MAPPING> or docker add <MAPPING> --name <NAME>"
        ),
    };

    let (mapping_part, proto) = match mapping.split_once('/') {
        Some((m, p)) => (m, p.parse()?),
        None => (mapping.as_str(), TransportProtocol::default()),
    };

    let parts: Vec<&str> = mapping_part.split(':').collect();

    let mut host_ip = "0.0.0.0".to_string();
    let host_port: u16;
    let mut target_ip = None;
    let container_port: u16;

    match parts.len() {
        1 => {
            host_port = parts[0].parse()?;
            container_port = host_port;
        }
        2 => {
            if let Ok(ip) = parts[0].parse::<std::net::IpAddr>() {
                host_ip = ip.to_string();
                host_port = parts[1].parse()?;
                container_port = host_port;
            } else {
                host_port = parts[0].parse()?;
                container_port = parts[1].parse()?;
            }
        }
        3 => {
            if let Ok(ip) = parts[0].parse::<std::net::IpAddr>() {
                host_ip = ip.to_string();
                host_port = parts[1].parse()?;
                container_port = parts[2].parse()?;
            } else {
                host_port = parts[0].parse()?;
                target_ip = Some(parts[1].to_string());
                container_port = parts[2].parse()?;
            }
        }
        4 => {
            host_ip = parts[0].to_string();
            host_port = parts[1].parse()?;
            target_ip = Some(parts[2].to_string());
            container_port = parts[3].parse()?;
        }
        _ => bail!(
            "Invalid mapping format. Use [HOST_IP:]HOST_PORT[:[TARGET_IP:]TARGET_PORT][/PROTO]"
        ),
    }

    let req = DockerAddMapRequest {
        host_ip,
        host_port,
        container_port,
        target_ip,
        proto,
    };
    let uri = format!("/mapping/{container_id}");
    let res: DockerPortMap = request_json(socket, Method::POST, &uri, Some(req)).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&res)?);
    } else {
        tracing::info!("successfully added mapping");
    }
    Ok(())
}

/// Removes one or more Docker port mappings via the daemon API.
pub async fn remove(
    container_id: Option<String>,
    port: Option<String>,
    all: bool,
    id: Option<u64>,
    name: Option<String>,
    socket: impl AsRef<Path>,
    json: bool,
) -> Result<()> {
    if let Some(mapping_id) = id {
        let uri = format!("/mapping/by-id/{mapping_id}");
        let _res: () = request_json(socket, Method::DELETE, &uri, None::<()>).await?;
        if !json {
            tracing::info!(mapping.id = mapping_id, "successfully removed mapping");
        }
    } else if all {
        bail!("--all not implemented yet");
    } else {
        let cid = name
            .or(container_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("Missing container ID or --name"))?;
        let p = port.ok_or_else(|| color_eyre::eyre::eyre!("Missing port to remove"))?;
        let port_num: u16 = p.split('/').next().unwrap().parse()?;
        let uri = format!("/mapping/{cid}/{port_num}");
        let _res: () = request_json(socket, Method::DELETE, &uri, None::<()>).await?;
        if !json {
            tracing::info!("successfully removed mapping");
        }
    }
    Ok(())
}
