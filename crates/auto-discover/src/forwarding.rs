//! Proxy-side kernel-level forwarding (DNAT) rule synchronization.
//!
//! Queries the Consul catalog for services with `Meta.forwarding=="true"`,
//! groups them by `(ext_ip, int_ip, protocol)`, and applies iptables
//! DNAT + hairpin rules via the natmap daemon. Handles cleanup of stale
//! rules for (ext_ip, int_ip) pairs no longer found in Consul.

use std::collections::HashMap;
use std::collections::HashSet;
use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::bail;

use crate::consul::ConsulClient;
use crate::natmap::NatmapClient;

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
/// applies DNAT and optional hairpin rules via the natmap daemon, and removes
/// stale DNAT rules for (ext_ip, int_ip) pairs that no longer exist in Consul.
///
/// Requires the natmap daemon to be running on the Unix socket at
/// `NATMAP_SOCKET` (default: [`lab_lib::NATMAP_SOCKET`]).
///
/// Span fields: `rule.count`.
#[tracing::instrument(skip_all, fields(rule.count = tracing::field::Empty))]
pub async fn sync_forwarding_rules(consul_addr: &str) -> Result<()> {
    let consul = ConsulClient::new(consul_addr.to_string());
    let groups = query_forwarding_services(&consul).await?;
    let natmap = NatmapClient::default_socket();

    if groups.is_empty() {
        tracing::info!("No forwarding services found in Consul; cleaning up stale rules");
        let stale = find_stale_rules(&groups)?;
        tracing::Span::current().record("rule.count", stale.len());
        for stale_group in stale {
            let ports_csv = stale_group
                .ports
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(",");
            natmap
                .dnat(
                    &stale_group.ext_ip,
                    &stale_group.int_ip,
                    &ports_csv,
                    &stale_group.proto,
                    true,
                )
                .await
                .ok();
        }
        return Ok(());
    }

    let stale = find_stale_rules(&groups)?;
    tracing::Span::current().record("rule.count", groups.len() + stale.len());

    for group in &groups {
        let ports_csv: String = group
            .ports
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",");

        natmap
            .dnat(&group.ext_ip, &group.int_ip, &ports_csv, &group.proto, true)
            .await
            .ok();

        natmap
            .dnat(
                &group.ext_ip,
                &group.int_ip,
                &ports_csv,
                &group.proto,
                false,
            )
            .await
            .wrap_err_with(|| format!("dnat for {} -> {} failed", group.ext_ip, group.int_ip))?;

        if group.hairpin {
            natmap
                .hairpin(&group.ext_ip, &group.int_ip, &ports_csv, &group.proto, true)
                .await
                .ok();

            if let Err(e) = natmap
                .hairpin(
                    &group.ext_ip,
                    &group.int_ip,
                    &ports_csv,
                    &group.proto,
                    false,
                )
                .await
            {
                tracing::warn!(
                    "hairpin for {} -> {} failed (non-fatal): {}",
                    group.ext_ip,
                    group.int_ip,
                    e
                );
            }
        }

        tracing::info!(
            ext.ip = %group.ext_ip,
            int.ip = %group.int_ip,
            ports = %ports_csv,
            proto = %group.proto,
            hairpin = group.hairpin,
            "applied forwarding"
        );
    }

    for stale_group in stale {
        let ports_csv = stale_group
            .ports
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",");
        natmap
            .dnat(
                &stale_group.ext_ip,
                &stale_group.int_ip,
                &ports_csv,
                &stale_group.proto,
                true,
            )
            .await
            .ok();
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
        let Some(meta) = svc.get("Meta").and_then(|m| m.as_object()) else {
            continue;
        };

        let Some(ext_ip) = meta.get("ext_ip").and_then(|v| v.as_str()) else {
            continue;
        };

        let Some(address) = svc.get("Address").and_then(|v| v.as_str()) else {
            continue;
        };

        let protocol = meta
            .get("protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("tcp");

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

        let entry = group_map
            .entry((
                ext_ip.to_string(),
                address.to_string(),
                protocol.to_string(),
            ))
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

/// Compare existing natmap DNAT rules against the desired state and return
/// groups that should be deleted.
fn find_stale_rules(current: &[ForwardingGroup]) -> Result<Vec<ForwardingGroup>> {
    let desired: HashSet<(String, String, String)> = current
        .iter()
        .map(|g| (g.ext_ip.clone(), g.int_ip.clone(), g.proto.clone()))
        .collect();

    let output = Command::new("iptables-save")
        .arg("-t")
        .arg("nat")
        .output()
        .wrap_err("failed to run iptables-save")?;

    if !output.status.success() {
        bail!("iptables-save failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut stale_groups: HashMap<(String, String, String), Vec<u16>> = HashMap::new();

    for line in stdout.lines() {
        if let Some((ext_ip, int_ip, port, proto)) = parse_dnat_rule(line) {
            let key = (ext_ip, int_ip, proto);
            if !desired.contains(&key) {
                stale_groups.entry(key).or_default().push(port);
            }
        }
    }

    let stale: Vec<ForwardingGroup> = stale_groups
        .into_iter()
        .map(|((ext_ip, int_ip, proto), ports)| ForwardingGroup {
            ext_ip,
            int_ip,
            ports,
            proto,
            hairpin: true,
        })
        .collect();

    Ok(stale)
}

/// Parse a DNAT rule from `iptables-save -t nat` output.
///
/// Format: `-A PREROUTING -d <ext_ip>/32 -p <proto> ... -j DNAT --to-destination <int_ip>:<port>`
fn parse_dnat_rule(line: &str) -> Option<(String, String, u16, String)> {
    let line = line.trim();
    if !line.starts_with("-A PREROUTING") || !line.contains("-j DNAT") {
        return None;
    }

    let ext_ip = line.split(" -d ").nth(1)?.split('/').next()?.to_string();

    let proto = line
        .split(" -p ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .to_string();

    let port = line
        .split("--dport ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse::<u16>()
        .ok()?;

    let int_ip = line
        .split("--to-destination ")
        .nth(1)?
        .split(':')
        .next()?
        .to_string();

    Some((ext_ip, int_ip, port, proto))
}
