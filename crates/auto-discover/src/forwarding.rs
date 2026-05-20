//! Proxy-side kernel-level forwarding (DNAT) rule synchronization.
//!
//! Queries the Consul catalog for services with `Meta.forwarding=="true"`,
//! groups them by `(ext_ip, int_ip, protocol)`, and applies iptables
//! DNAT + hairpin rules via `lab-ops natmap`. Handles cleanup of stale rules
//! for (ext_ip, int_ip) pairs no longer found in Consul.

use std::collections::HashMap;
use std::collections::HashSet;
use std::process::Command;

use color_eyre::eyre::bail;
use color_eyre::eyre::WrapErr;
use color_eyre::Result;

use crate::consul::ConsulClient;

/// Internal grouping of forwarding services by key `(ext_ip, int_ip, protocol)`.
#[derive(Debug, Clone)]
struct ForwardingGroup {
    ext_ip: String,
    int_ip: String,
    ports: Vec<u16>,
    proto: String,
    hairpin: bool,
}

/// One-shot sync of kernel-level forwarding rules from Consul.
///
/// Queries the Consul catalog (cross-agent) for services with `forwarding==true`,
/// applies DNAT and optional hairpin rules via `lab-ops natmap`, and removes
/// stale rules for (ext_ip, int_ip) pairs that no longer exist in Consul.
pub async fn sync_forwarding_rules(consul_addr: &str) -> Result<()> {
    let consul = ConsulClient::new(consul_addr.to_string());
    let groups = query_forwarding_services(&consul).await?;

    if groups.is_empty() {
        tracing::info!("No forwarding services found in Consul");
        return Ok(());
    }

    tracing::info!("Syncing {} forwarding groups", groups.len());

    let natmap_socket =
        std::env::var("NATMAP_SOCKET").unwrap_or_else(|_| "/run/natmap.sock".into());

    for group in &groups {
        let ports_csv: String = group
            .ports
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",");

        let delete_result = run_natmap_dnat(
            &natmap_socket,
            "dnat",
            &group.ext_ip,
            &group.int_ip,
            &ports_csv,
            &group.proto,
            true,
        );
        if let Err(e) = &delete_result {
            tracing::warn!(
                "Failed to delete old dnat rules for {}: {}",
                group.ext_ip,
                e
            );
        }

        let apply_result = run_natmap_dnat(
            &natmap_socket,
            "dnat",
            &group.ext_ip,
            &group.int_ip,
            &ports_csv,
            &group.proto,
            false,
        );
        if let Err(e) = apply_result {
            bail!("dnat for {} -> {} failed: {e}", group.ext_ip, group.int_ip);
        }

        if group.hairpin {
            let hairpin_delete = run_natmap_dnat(
                &natmap_socket,
                "hairpin",
                &group.ext_ip,
                &group.int_ip,
                &ports_csv,
                &group.proto,
                true,
            );
            if let Err(e) = &hairpin_delete {
                tracing::warn!(
                    "Failed to delete old hairpin rules for {}: {}",
                    group.ext_ip,
                    e
                );
            }

            let hairpin_apply = run_natmap_dnat(
                &natmap_socket,
                "hairpin",
                &group.ext_ip,
                &group.int_ip,
                &ports_csv,
                &group.proto,
                false,
            );
            if let Err(e) = hairpin_apply {
                bail!(
                    "hairpin for {} -> {} failed: {e}",
                    group.ext_ip,
                    group.int_ip
                );
            }
        }

        tracing::info!(
            "Applied forwarding: {} -> {} ports={} proto={} hairpin={}",
            group.ext_ip,
            group.int_ip,
            ports_csv,
            group.proto,
            group.hairpin
        );
    }

    let stale = find_stale_rules(&natmap_socket, &groups)?;
    for stale_group in stale {
        let ports_csv: String = stale_group
            .ports
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",");

        if let Err(e) = run_natmap_dnat(
            &natmap_socket,
            "dnat",
            &stale_group.ext_ip,
            &stale_group.int_ip,
            &ports_csv,
            &stale_group.proto,
            true,
        ) {
            tracing::warn!(
                "Failed to delete stale dnat for {}: {}",
                stale_group.ext_ip,
                e
            );
        }
        if stale_group.hairpin {
            if let Err(e) = run_natmap_dnat(
                &natmap_socket,
                "hairpin",
                &stale_group.ext_ip,
                &stale_group.int_ip,
                &ports_csv,
                &stale_group.proto,
                true,
            ) {
                tracing::warn!(
                    "Failed to delete stale hairpin for {}: {}",
                    stale_group.ext_ip,
                    e
                );
            }
        }
        tracing::info!(
            "Removed stale forwarding: {} -> {}",
            stale_group.ext_ip,
            stale_group.int_ip
        );
    }

    Ok(())
}

/// Query the Consul catalog for all services with `Meta.forwarding=="true"`,
/// parse their forwarding metadata, and group by `(ext_ip, address, protocol)`.
async fn query_forwarding_services(consul: &ConsulClient) -> Result<Vec<ForwardingGroup>> {
    let services = consul
        .get_catalog_services_by_meta("forwarding", "true")
        .await
        .wrap_err("failed to query forwarding services from Consul")?;

    let mut group_map: HashMap<(String, String, String), (Vec<u16>, bool)> = HashMap::new();

    for svc in services {
        let meta = svc.get("Meta").and_then(|m| m.as_object());
        let meta = match meta {
            Some(m) => m,
            None => continue,
        };

        let ext_ip = match meta.get("ext_ip").and_then(|v| v.as_str()) {
            Some(ip) => ip.to_string(),
            None => continue,
        };

        let address = match svc.get("Address").and_then(|v| v.as_str()) {
            Some(addr) => addr.to_string(),
            None => continue,
        };

        let protocol = meta
            .get("protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("tcp")
            .to_string();

        let ports: Vec<u16> = meta
            .get("ext_ports")
            .and_then(|v| v.as_str())
            .map(|s| {
                s.split(',')
                    .filter_map(|p| p.trim().parse::<u16>().ok())
                    .collect()
            })
            .unwrap_or_default();

        let hairpin = meta
            .get("hairpin")
            .and_then(|v| v.as_str())
            .map(|s| s == "true")
            .unwrap_or(false);

        if ports.is_empty() {
            continue;
        }

        let key = (ext_ip.clone(), address.clone(), protocol.clone());
        let entry = group_map
            .entry(key)
            .or_insert_with(|| (Vec::new(), hairpin));
        entry.0.extend(ports);
        entry.1 = entry.1 || hairpin;
    }

    let groups: Vec<ForwardingGroup> = group_map
        .into_iter()
        .map(|((ext_ip, int_ip, proto), (ports, hairpin))| {
            let mut sorted = ports;
            sorted.sort();
            sorted.dedup();
            ForwardingGroup {
                ext_ip,
                int_ip,
                ports: sorted,
                proto,
                hairpin,
            }
        })
        .collect();

    Ok(groups)
}

/// Invoke `lab-ops natmap dnat` (or `hairpin`) to apply or delete a group of rules.
fn run_natmap_dnat(
    socket: &str,
    subcmd: &str,
    ext_ip: &str,
    int_ip: &str,
    ports: &str,
    proto: &str,
    delete: bool,
) -> Result<()> {
    let mut args = vec![
        "natmap", "--socket", socket, subcmd, "--ext-ip", ext_ip, "--int-ip", int_ip, "--ports",
        ports,
    ];

    if !proto.is_empty() && proto != "tcp" {
        args.push("--proto");
        args.push(proto);
    }

    if delete {
        args.push("--delete");
    }

    let output = Command::new("lab-ops")
        .args(&args)
        .output()
        .wrap_err("failed to run lab-ops")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("lab-ops natmap {} failed: {}", subcmd, stderr.trim());
    }
    Ok(())
}

/// Compare existing natmap DNAT rules against the desired state and return
/// groups that should be deleted.
fn find_stale_rules(socket: &str, current: &[ForwardingGroup]) -> Result<Vec<ForwardingGroup>> {
    let desired: HashSet<(String, String, String)> = current
        .iter()
        .map(|g| (g.ext_ip.clone(), g.int_ip.clone(), g.proto.clone()))
        .collect();

    let output = Command::new("lab-ops")
        .args(["natmap", "--socket", socket, "--json", "ls"])
        .output()
        .wrap_err("failed to run lab-ops natmap ls")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("lab-ops natmap ls failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).wrap_err("failed to parse natmap ls output")?;

    let mut existing: HashSet<(String, String, String)> = HashSet::new();
    if let Some(rules) = parsed.as_array() {
        for rule in rules {
            let rule_type = rule.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if rule_type != "dnat" {
                continue;
            }
            let ext_ip = rule.get("ext_ip").and_then(|v| v.as_str()).unwrap_or("");
            let int_ip = rule.get("int_ip").and_then(|v| v.as_str()).unwrap_or("");
            let proto = rule.get("proto").and_then(|v| v.as_str()).unwrap_or("tcp");
            if !ext_ip.is_empty() && !int_ip.is_empty() {
                existing.insert((ext_ip.to_string(), int_ip.to_string(), proto.to_string()));
            }
        }
    }

    let stale: Vec<ForwardingGroup> = existing
        .difference(&desired)
        .map(|(ext_ip, int_ip, proto)| ForwardingGroup {
            ext_ip: ext_ip.clone(),
            int_ip: int_ip.clone(),
            ports: vec![],
            proto: proto.clone(),
            hairpin: true,
        })
        .collect();

    Ok(stale)
}
