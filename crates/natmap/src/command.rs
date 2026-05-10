use std::process::Command;

use color_eyre::eyre::Result;
use hyper::Method;

use crate::models::*;
use crate::utils::request_json;

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

    println!("\n── Docker port mappings ──");
    let _ = try_list(socket, container_id, json).await;

    Ok(())
}

pub fn handle_dnat(
    ext_ip: String,
    int_ip: String,
    proto: String,
    ports: String,
    ext_if: Option<String>,
    delete: bool,
) -> Result<()> {
    let action = if delete { "-D" } else { "-A" };
    let multiport = ports.contains(',');
    let port_args = if multiport {
        vec!["-m", "multiport", "--dports", &ports]
    } else {
        vec!["--dport", &ports]
    };

    let mut pre_args = vec!["-t", "nat", action, "PREROUTING"];
    if let Some(ref iface) = ext_if {
        pre_args.extend(vec!["-i", iface]);
    }
    pre_args.extend(vec!["-d", &ext_ip, "-p", &proto]);
    pre_args.extend(port_args.clone());

    let dest = if multiport {
        int_ip.clone()
    } else {
        format!("{}:{}", int_ip, ports)
    };
    pre_args.extend(vec!["-j", "DNAT", "--to-destination", &dest]);

    run_iptables(&pre_args, delete)?;

    let mut fwd_args = vec![action, "FORWARD", "-p", &proto, "-d", &int_ip];
    fwd_args.extend(port_args);
    fwd_args.extend(vec!["-j", "ACCEPT"]);

    run_iptables(&fwd_args, delete)?;
    Ok(())
}

pub fn handle_snat(int_ip: String, ext_if: String, ext_ip: String, delete: bool) -> Result<()> {
    let action = if delete { "-D" } else { "-A" };
    let args = vec![
        "-t",
        "nat",
        action,
        "POSTROUTING",
        "-s",
        &int_ip,
        "-o",
        &ext_if,
        "-j",
        "SNAT",
        "--to-source",
        &ext_ip,
    ];
    run_iptables(&args, delete)?;
    Ok(())
}

pub fn handle_hairpin(
    ext_ip: String,
    int_ip: String,
    proto: String,
    ports: String,
    delete: bool,
) -> Result<()> {
    let action = if delete { "-D" } else { "-A" };
    let multiport = ports.contains(',');
    let port_args = if multiport {
        vec!["-m", "multiport", "--dports", &ports]
    } else {
        vec!["--dport", &ports]
    };

    let mut pre_args = vec![
        "-t",
        "nat",
        action,
        "PREROUTING",
        "-s",
        &int_ip,
        "-d",
        &ext_ip,
        "-p",
        &proto,
    ];
    pre_args.extend(port_args.clone());
    pre_args.extend(vec!["-j", "DNAT", "--to-destination", &int_ip]);
    run_iptables(&pre_args, delete)?;

    let mut post_args = vec![
        "-t",
        "nat",
        action,
        "POSTROUTING",
        "-s",
        &int_ip,
        "-d",
        &int_ip,
        "-p",
        &proto,
    ];
    post_args.extend(port_args);
    post_args.extend(vec!["-j", "MASQUERADE"]);
    run_iptables(&post_args, delete)?;
    Ok(())
}

fn run_iptables(args: &[&str], ignore_error: bool) -> Result<()> {
    let output = match Command::new("iptables").args(args).output() {
        Ok(o) => o,
        Err(e) => {
            if ignore_error {
                return Ok(());
            } else {
                return Err(e.into());
            }
        }
    };
    if !output.status.success() && !ignore_error {
        let stderr = String::from_utf8_lossy(&output.stderr);
        color_eyre::eyre::bail!(
            "iptables command failed: iptables {}\n{}",
            args.join(" "),
            stderr
        );
    }
    Ok(())
}

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

pub async fn try_list(socket: &str, container_id: Option<String>, json: bool) -> Result<()> {
    match list(container_id, socket, json).await {
        Ok(()) => Ok(()),
        Err(_) => {
            println!("  (daemon not running)");
            Ok(())
        }
    }
}

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

pub async fn add(container_id: String, mapping: String, socket: &str, json: bool) -> Result<()> {
    // Parse proto from trailing /proto suffix
    let (mapping_part, proto) = match mapping.split_once('/') {
        Some((m, p)) => (m, p.to_string()),
        None => (mapping.as_str(), "tcp".to_string()),
    };

    // Split by : to determine if IP is provided
    let parts: Vec<&str> = mapping_part.split(':').collect();
    let (host_ip, host_port, container_port) = match parts.len() {
        3 => (parts[0].to_string(), parts[1].parse()?, parts[2].parse()?),
        2 => ("0.0.0.0".to_string(), parts[0].parse()?, parts[1].parse()?),
        _ => color_eyre::eyre::bail!(
            "Invalid mapping format. Use [HOST_IP:]HOST_PORT:CONTAINER_PORT[/PROTO] (e.g., 8080:80 or 10.0.0.1:8080:80/tcp)"
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
