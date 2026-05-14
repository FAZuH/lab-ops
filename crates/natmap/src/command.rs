//! CLI command implementations that communicate with the natmap daemon.
//!
//! Each `handle_*` function constructs the appropriate HTTP request to the
//! daemon's Unix socket API and formats the response for display.

use std::process::Command;

use color_eyre::eyre::Result;
use hyper::Method;

use crate::models::*;
use crate::utils::request_json;

/// Displays a combined listing of static iptables NAT rules and daemon-managed state.
///
/// Reads live iptables rules via `iptables-save` and queries the daemon at
/// `GET /mappings` for managed DNAT, SNAT, hairpin, and Docker mappings.
pub async fn handle_list(socket: &str, container_id: Option<String>, json: bool) -> Result<()> {
    println!("── Static iptables NAT rules ──");
    let output = Command::new("iptables-save").output();
    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let rules: Vec<&str> = stdout
                .lines()
                .filter(|l| {
                    l.starts_with("-A PREROUTING")
                        || l.starts_with("-A POSTROUTING")
                        || l.starts_with("-A FORWARD")
                })
                .collect();
            if rules.is_empty() {
                println!("  (none)");
            } else {
                for r in rules {
                    println!("  {r}");
                }
            }
        }
        Err(_) => println!("  (could not read iptables rules)"),
    }

    println!("\n── Daemon-managed state ──");
    match request_json::<ListResponse, ()>(socket, Method::GET, "/mappings", None).await {
        Ok(resp) => {
            if !resp.dnats.is_empty() {
                println!("\n  DNAT rules:");
                for d in &resp.dnats {
                    let if_info = d.ext_if.as_deref().unwrap_or("-");
                    println!(
                        "    {} -> {} (ports: {}, proto: {}, if: {})",
                        d.ext_ip, d.int_ip, d.ports, d.proto, if_info
                    );
                }
            }
            if !resp.snats.is_empty() {
                println!("\n  SNAT rules:");
                for s in &resp.snats {
                    println!("    {} -> {} (if: {})", s.int_ip, s.ext_ip, s.ext_if);
                }
            }
            if !resp.hairpins.is_empty() {
                println!("\n  Hairpin rules:");
                for h in &resp.hairpins {
                    println!(
                        "    {} <-> {} (ports: {}, proto: {})",
                        h.ext_ip, h.int_ip, h.ports, h.proto
                    );
                }
            }
            if !resp.docker.is_empty() {
                println!("\n  Docker mappings:");
                if json {
                    println!("{}", serde_json::to_string_pretty(&resp.docker)?);
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
    socket: &str,
) -> Result<()> {
    let req = DnatRequest {
        ext_ip,
        int_ip,
        ports,
        proto,
        ext_if,
    };
    if delete {
        let _: () = request_json(socket, Method::DELETE, "/dnat", Some(req)).await?;
        println!("DNAT rule removed.");
    } else {
        let _res: DnatConfig = request_json(socket, Method::POST, "/dnat", Some(req)).await?;
        println!("DNAT rule added.");
    }
    Ok(())
}

/// Adds or removes a static SNAT rule via the daemon API.
pub async fn handle_snat(
    int_ip: String,
    ext_if: String,
    ext_ip: String,
    delete: bool,
    socket: &str,
) -> Result<()> {
    let req = SnatRequest {
        int_ip,
        ext_if,
        ext_ip,
    };
    if delete {
        let _: () = request_json(socket, Method::DELETE, "/snat", Some(req)).await?;
        println!("SNAT rule removed.");
    } else {
        let _res: SnatConfig = request_json(socket, Method::POST, "/snat", Some(req)).await?;
        println!("SNAT rule added.");
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
    socket: &str,
) -> Result<()> {
    let req = HairpinRequest {
        ext_ip,
        int_ip,
        ports,
        proto,
    };
    if delete {
        let _: () = request_json(socket, Method::DELETE, "/hairpin", Some(req)).await?;
        println!("Hairpin rule removed.");
    } else {
        let _res: HairpinConfig = request_json(socket, Method::POST, "/hairpin", Some(req)).await?;
        println!("Hairpin rule added.");
    }
    Ok(())
}

/// Lists Docker port mappings from the daemon (active mappings only).
pub async fn list(container_id: Option<String>, socket: &str, json: bool) -> Result<()> {
    let res: Vec<ActivePortMapping> =
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
pub async fn remap(container_id: String, mapping: String, socket: &str, json: bool) -> Result<()> {
    let parts: Vec<&str> = mapping.split(':').collect();
    if parts.len() != 2 {
        color_eyre::eyre::bail!("Invalid mapping format. Use <old_host_port>:<new_host_port>");
    }
    let req = RemapRequest {
        host_port: parts[0].parse()?,
        new_host_port: parts[1].parse()?,
    };
    let uri = format!("/remap/{}", container_id);
    let res: Vec<ActivePortMapping> = request_json(socket, Method::PUT, &uri, Some(req)).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&res)?);
    } else {
        println!("Successfully remapped {} rules", res.len());
    }
    Ok(())
}

/// Adds a new port mapping to a running container via the daemon API.
pub async fn add(container_id: String, mapping: String, socket: &str, json: bool) -> Result<()> {
    let (mapping_part, proto) = match mapping.split_once('/') {
        Some((m, p)) => (m, p.to_string()),
        None => (mapping.as_str(), "tcp".to_string()),
    };

    let parts: Vec<&str> = mapping_part.split(':').collect();
    let (host_ip, host_port, container_port) = match parts.len() {
        3 => (parts[0].to_string(), parts[1].parse()?, parts[2].parse()?),
        2 => ("0.0.0.0".to_string(), parts[0].parse()?, parts[1].parse()?),
        _ => color_eyre::eyre::bail!(
            "Invalid mapping format. Use [HOST_IP:]HOST_PORT:CONTAINER_PORT[/PROTO]"
        ),
    };

    let req = AddMappingRequest {
        host_ip,
        host_port,
        container_port,
        proto,
    };
    let uri = format!("/mapping/{}", container_id);
    let res: ActivePortMapping = request_json(socket, Method::POST, &uri, Some(req)).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&res)?);
    } else {
        println!("Successfully added mapping.");
    }
    Ok(())
}

/// Removes one or more Docker port mappings via the daemon API.
pub async fn remove(
    container_id: Option<String>,
    port: Option<String>,
    all: bool,
    id: Option<u64>,
    socket: &str,
    json: bool,
) -> Result<()> {
    if let Some(mapping_id) = id {
        let uri = format!("/mapping/by-id/{}", mapping_id);
        let _res: () = request_json(socket, Method::DELETE, &uri, None::<()>).await?;
        if !json {
            println!("Successfully removed mapping {mapping_id}.");
        }
    } else if all {
        color_eyre::eyre::bail!("--all not implemented yet");
    } else if let (Some(cid), Some(p)) = (container_id, port) {
        let port_num: u16 = p.split('/').next().unwrap().parse()?;
        let uri = format!("/mapping/{}/{}", cid, port_num);
        let _res: () = request_json(socket, Method::DELETE, &uri, None::<()>).await?;
        if !json {
            println!("Successfully removed mapping.");
        }
    } else {
        color_eyre::eyre::bail!("Specify either --id <ID>, or <CONTAINER_ID> <PORT>, or --all");
    }
    Ok(())
}
