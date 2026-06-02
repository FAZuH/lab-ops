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

type GroupedServices = (Vec<u16>, bool, bool);

/// Internal grouping of forwarding services by key `(ext_ip, int_ip, protocol)`.
#[derive(Debug, Clone)]
struct ForwardingGroup {
    ext_ip: String,
    int_ip: String,
    ports: Vec<u16>,
    proto: String,
    hairpin: bool,
    preserve_src_ip: bool,
}

/// One-shot sync of kernel-level forwarding rules from Consul.
///
/// Queries the Consul catalog (cross-agent) for services with `forwarding==true`,
/// applies DNAT and optional hairpin rules via the natmap daemon, and removes
/// stale DNAT rules for (ext_ip, int_ip) pairs that no longer exist in Consul.
///
/// Requires the natmap daemon to be running on the Unix socket at
/// `NATMAP_SOCKET` (default: [`lab_ops_lab_lib::NATMAP_SOCKET`]).
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
                    stale_group.preserve_src_ip,
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
            .dnat(
                &group.ext_ip,
                &group.int_ip,
                &ports_csv,
                &group.proto,
                true,
                group.preserve_src_ip,
            )
            .await
            .ok();

        natmap
            .dnat(
                &group.ext_ip,
                &group.int_ip,
                &ports_csv,
                &group.proto,
                false,
                group.preserve_src_ip,
            )
            .await
            .wrap_err_with(|| format!("dnat for {} -> {} failed", group.ext_ip, group.int_ip))?;

        if group.hairpin && !group.preserve_src_ip {
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
                stale_group.preserve_src_ip,
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
    Ok(group_forwarding_services(services))
}

/// Parse raw Consul service JSON into grouped forwarding entries.
///
/// Extracted as a pure function to enable unit testing of hairpin and
/// preserve_src_ip flag propagation without a live Consul instance.
fn group_forwarding_services(services: Vec<serde_json::Value>) -> Vec<ForwardingGroup> {
    let mut group_map: HashMap<(String, String, String), GroupedServices> = HashMap::new();

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

        let preserve_src_ip = meta
            .get("preserve_src_ip")
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
            .or_insert_with(|| (Vec::new(), hairpin, preserve_src_ip));
        entry.0.extend(ports);
        entry.1 = entry.1 || hairpin;
        entry.2 = entry.2 || preserve_src_ip;
    }

    group_map
        .into_iter()
        .map(
            |((ext_ip, int_ip, proto), (ports, hairpin, preserve_src_ip))| {
                let mut sorted = ports;
                sorted.sort();
                sorted.dedup();
                ForwardingGroup {
                    ext_ip,
                    int_ip,
                    ports: sorted,
                    proto,
                    hairpin,
                    preserve_src_ip,
                }
            },
        )
        .collect()
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
            hairpin: true,          // conservative default for cleanup
            preserve_src_ip: false, // conservative default for cleanup
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn make_service(
        ext_ip: &str,
        address: &str,
        ext_ports: &str,
        protocol: &str,
        hairpin: bool,
        preserve_src_ip: bool,
    ) -> serde_json::Value {
        let mut meta = serde_json::Map::new();
        meta.insert("ext_ip".into(), json!(ext_ip));
        meta.insert("ext_ports".into(), json!(ext_ports));
        meta.insert("protocol".into(), json!(protocol));
        if hairpin {
            meta.insert("hairpin".into(), json!("true"));
        }
        if preserve_src_ip {
            meta.insert("preserve_src_ip".into(), json!("true"));
        }
        json!({
            "Address": address,
            "Meta": meta,
        })
    }

    #[test]
    fn hairpin_true_preserve_src_false() {
        let services = vec![make_service(
            "1.2.3.4", "10.0.0.1", "8080", "tcp", true, false,
        )];
        let groups = group_forwarding_services(services);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].hairpin);
        assert!(!groups[0].preserve_src_ip);
        assert_eq!(groups[0].ext_ip, "1.2.3.4");
        assert_eq!(groups[0].int_ip, "10.0.0.1");
        assert_eq!(groups[0].ports, vec![8080]);
        assert_eq!(groups[0].proto, "tcp");
    }

    #[test]
    fn hairpin_true_preserve_src_true() {
        let services = vec![make_service(
            "1.2.3.4", "10.0.0.1", "25565", "tcp", true, true,
        )];
        let groups = group_forwarding_services(services);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].hairpin);
        assert!(groups[0].preserve_src_ip);
    }

    #[test]
    fn hairpin_false_preserve_src_true() {
        let services = vec![make_service(
            "1.2.3.4", "10.0.0.1", "25565", "tcp", false, true,
        )];
        let groups = group_forwarding_services(services);
        assert_eq!(groups.len(), 1);
        assert!(!groups[0].hairpin);
        assert!(groups[0].preserve_src_ip);
    }

    #[test]
    fn both_flags_false_when_absent() {
        let mut meta = serde_json::Map::new();
        meta.insert("ext_ip".into(), json!("1.2.3.4"));
        meta.insert("ext_ports".into(), json!("80"));
        let services = vec![json!({
            "Address": "10.0.0.1",
            "Meta": meta,
        })];
        let groups = group_forwarding_services(services);
        assert_eq!(groups.len(), 1);
        assert!(!groups[0].hairpin);
        assert!(!groups[0].preserve_src_ip);
        assert_eq!(groups[0].proto, "tcp");
    }

    #[test]
    fn merge_multiple_services_same_key() {
        let services = vec![
            make_service("1.2.3.4", "10.0.0.1", "80", "tcp", true, false),
            make_service("1.2.3.4", "10.0.0.1", "443", "tcp", false, true),
        ];
        let groups = group_forwarding_services(services);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].hairpin);
        assert!(groups[0].preserve_src_ip);
        assert_eq!(groups[0].ports, vec![80, 443]);
    }

    #[test]
    fn different_keys_not_merged() {
        let services = vec![
            make_service("1.2.3.4", "10.0.0.1", "80", "tcp", true, false),
            make_service("1.2.3.4", "10.0.0.2", "80", "tcp", false, true),
        ];
        let groups = group_forwarding_services(services);
        assert_eq!(groups.len(), 2);
        let g1 = groups.iter().find(|g| g.int_ip == "10.0.0.1").unwrap();
        let g2 = groups.iter().find(|g| g.int_ip == "10.0.0.2").unwrap();
        assert!(g1.hairpin);
        assert!(!g1.preserve_src_ip);
        assert!(!g2.hairpin);
        assert!(g2.preserve_src_ip);
    }

    #[test]
    fn skips_service_with_no_ports() {
        let services = vec![make_service("1.2.3.4", "10.0.0.1", "", "tcp", true, true)];
        let groups = group_forwarding_services(services);
        assert!(groups.is_empty());
    }

    #[test]
    fn skips_service_without_ext_ip() {
        let services = vec![json!({
            "Address": "10.0.0.1",
            "Meta": { "ext_ports": "80" },
        })];
        let groups = group_forwarding_services(services);
        assert!(groups.is_empty());
    }

    #[test]
    fn skips_service_without_address() {
        let services = vec![json!({
            "Meta": { "ext_ip": "1.2.3.4", "ext_ports": "80" },
        })];
        let groups = group_forwarding_services(services);
        assert!(groups.is_empty());
    }

    #[test]
    fn protocol_defaults_to_tcp() {
        let mut meta = serde_json::Map::new();
        meta.insert("ext_ip".into(), json!("1.2.3.4"));
        meta.insert("ext_ports".into(), json!("80"));
        let services = vec![json!({
            "Address": "10.0.0.1",
            "Meta": meta,
        })];
        let groups = group_forwarding_services(services);
        assert_eq!(groups[0].proto, "tcp");
    }

    #[test]
    fn udp_protocol_preserved() {
        let services = vec![make_service(
            "1.2.3.4", "10.0.0.1", "19132", "udp", true, true,
        )];
        let groups = group_forwarding_services(services);
        assert_eq!(groups[0].proto, "udp");
    }

    #[test]
    fn deduplicates_ports() {
        let services = vec![
            make_service("1.2.3.4", "10.0.0.1", "80,443", "tcp", false, false),
            make_service("1.2.3.4", "10.0.0.1", "80,8080", "tcp", false, false),
        ];
        let groups = group_forwarding_services(services);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].ports, vec![80, 443, 8080]);
    }

    #[test]
    fn preserve_src_ip_false_even_as_string() {
        let mut meta = serde_json::Map::new();
        meta.insert("ext_ip".into(), json!("1.2.3.4"));
        meta.insert("ext_ports".into(), json!("80"));
        meta.insert("hairpin".into(), json!("true"));
        meta.insert("preserve_src_ip".into(), json!("false"));
        let services = vec![json!({
            "Address": "10.0.0.1",
            "Meta": meta,
        })];
        let groups = group_forwarding_services(services);
        assert!(groups[0].hairpin);
        assert!(!groups[0].preserve_src_ip);
    }

    // ── parse_dnat_rule tests ──

    #[test]
    fn parse_dnat_rule_tcp_basic() {
        let line = "-A PREROUTING -d 203.0.113.50/32 -p tcp -m tcp --dport 36000 -j DNAT --to-destination 10.0.0.99:36000";
        let (ext_ip, int_ip, port, proto) = parse_dnat_rule(line).unwrap();
        assert_eq!(ext_ip, "203.0.113.50");
        assert_eq!(int_ip, "10.0.0.99");
        assert_eq!(port, 36000);
        assert_eq!(proto, "tcp");
    }

    #[test]
    fn parse_dnat_rule_udp() {
        let line = "-A PREROUTING -d 10.10.10.102/32 -p udp -m udp --dport 19132 -j DNAT --to-destination 10.10.10.102:19132";
        let (ext_ip, int_ip, port, proto) = parse_dnat_rule(line).unwrap();
        assert_eq!(ext_ip, "10.10.10.102");
        assert_eq!(int_ip, "10.10.10.102");
        assert_eq!(port, 19132);
        assert_eq!(proto, "udp");
    }

    #[test]
    fn parse_dnat_rule_non_dnat_returns_none() {
        assert!(parse_dnat_rule("-A POSTROUTING -s 10.0.0.0/24 -j MASQUERADE").is_none());
        assert!(
            parse_dnat_rule("-A PREROUTING -d 1.2.3.4/32 -p tcp --dport 80 -j ACCEPT").is_none()
        );
        assert!(parse_dnat_rule("").is_none());
    }

    #[test]
    fn parse_dnat_rule_missing_fields() {
        assert!(parse_dnat_rule("-A PREROUTING -j DNAT").is_none());
        assert!(
            parse_dnat_rule("-A PREROUTING -p tcp -j DNAT --to-destination 10.0.0.1:80").is_none()
        );
    }
}
