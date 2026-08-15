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
use lab_ops_lab_lib::TransportProtocol;
use lab_ops_natmap::client::NatmapClient;
use lab_ops_natmap::client::NatmapError;
use lab_ops_natmap::models::DnatConfig;
use lab_ops_natmap::models::HairpinConfig;
use lab_ops_natmap::models::LiveRule;
use lab_ops_natmap::models::RuleKind;

use crate::consul::ConsulClient;

/// Interface for the natmap operations forwarding sync relies on.
///
/// Local to this crate (plan decision #2): auto-discover does not share
/// primitives with natmap, only the reported [`LiveRule`] wire format.
trait NatmapOps {
    /// Fetches the daemon-reported live rules.
    async fn rules(&self) -> Result<Vec<LiveRule>, NatmapError>;
    /// Installs or deletes a static DNAT rule.
    async fn dnat(
        &self,
        config: DnatConfig,
        delete: bool,
    ) -> Result<Option<DnatConfig>, NatmapError>;
    /// Installs or deletes a static hairpin rule.
    async fn hairpin(
        &self,
        config: HairpinConfig,
        delete: bool,
    ) -> Result<Option<HairpinConfig>, NatmapError>;
}

impl NatmapOps for NatmapClient {
    async fn rules(&self) -> Result<Vec<LiveRule>, NatmapError> {
        self.rules().await
    }

    async fn dnat(
        &self,
        config: DnatConfig,
        delete: bool,
    ) -> Result<Option<DnatConfig>, NatmapError> {
        self.dnat(config, delete).await
    }

    async fn hairpin(
        &self,
        config: HairpinConfig,
        delete: bool,
    ) -> Result<Option<HairpinConfig>, NatmapError> {
        self.hairpin(config, delete).await
    }
}

/// Parses a forwarding group's protocol string into a typed
/// [`TransportProtocol`].
///
/// Returns an error for invalid values so a misconfigured service fails the
/// sync instead of silently installing a TCP rule, matching the natmap
/// daemon's rejection of unknown protocols.
fn parse_group_proto(proto: &str) -> Result<TransportProtocol> {
    proto
        .parse()
        .wrap_err_with(|| format!("invalid transport protocol for forwarding group: {proto}"))
}

/// Determine the LAN CIDR that contains the given IP by querying the routing
/// table on the proxy host. Returns the network address in CIDR notation
/// (e.g. `"10.10.10.0/24"`).
fn get_lan_cidr(ip: &str) -> Result<String> {
    // Find the interface for this IP
    let route_out = Command::new("ip")
        .args(["-o", "route", "get", ip])
        .output()
        .wrap_err("failed to run ip route get")?;
    if !route_out.status.success() {
        bail!("ip route get failed for {ip}");
    }
    let route_stdout = String::from_utf8_lossy(&route_out.stdout);
    let dev = route_stdout
        .split_whitespace()
        .position(|w| w == "dev")
        .and_then(|pos| route_stdout.split_whitespace().nth(pos + 1))
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("could not parse interface from ip route get output")
        })?
        .to_string();

    // Find the kernel subnet route on that interface (e.g. "10.10.10.0/24 dev vmbr1 ...")
    let link_out = Command::new("ip")
        .args([
            "-o", "route", "show", "dev", &dev, "proto", "kernel", "scope", "link",
        ])
        .output()
        .wrap_err("failed to run ip route show")?;
    let link_stdout = String::from_utf8_lossy(&link_out.stdout);
    let cidr = link_stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("no kernel link route found on {dev}"))?
        .to_string();
    Ok(cidr)
}

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
    reconcile_forwarding_rules(&natmap, &groups).await
}

/// Applies the desired forwarding groups and deletes rules reported by the
/// natmap daemon that no longer match any desired group.
///
/// The daemon's reported live rules are the source of truth for what exists;
/// a rule is stale when its `(ext_ip, int_ip, protocol)` is not desired.
///
/// Span fields: `rule.count` (desired groups + stale rules).
async fn reconcile_forwarding_rules(
    natmap: &impl NatmapOps,
    groups: &[ForwardingGroup],
) -> Result<()> {
    let reported = natmap
        .rules()
        .await
        .wrap_err("failed to fetch live rules from natmap daemon")?;

    // Validates every group's protocol up-front so a misconfigured service
    // aborts the sync before any rule is applied.
    let desired_keys: HashSet<(String, String, TransportProtocol)> = groups
        .iter()
        .map(|g| {
            Ok((
                g.ext_ip.clone(),
                g.int_ip.clone(),
                parse_group_proto(&g.proto)?,
            ))
        })
        .collect::<Result<_>>()?;

    let stale = stale_forwarding_rules(&reported, &desired_keys);
    tracing::Span::current().record("rule.count", groups.len() + stale.len());

    if groups.is_empty() {
        tracing::info!("no forwarding services found in Consul; cleaning up stale rules");
    }

    for group in groups {
        apply_forwarding_group(natmap, group).await?;
    }
    for rule in stale {
        delete_stale_rule(natmap, rule).await;
    }
    Ok(())
}

/// Returns the reported rules that are no longer desired.
///
/// Only DNAT and hairpin rules are forwarding-managed; SNAT and Docker
/// mapping rules are never considered stale here.
fn stale_forwarding_rules<'a>(
    reported: &'a [LiveRule],
    desired_keys: &HashSet<(String, String, TransportProtocol)>,
) -> Vec<&'a LiveRule> {
    reported
        .iter()
        .filter(|r| {
            matches!(r.kind, RuleKind::Dnat | RuleKind::Hairpin)
                && !desired_keys.contains(&(r.ext_ip.clone(), r.int_ip.clone(), r.proto))
        })
        .collect()
}

/// Applies one forwarding group's DNAT (and optional hairpin) rules.
///
/// Delete-before-create avoids duplicates. Deletes and creates are both
/// non-fatal (warn + continue) — a failed DNAT create does NOT skip the
/// hairpin install, and a successful sync still returns `Ok`.
async fn apply_forwarding_group(natmap: &impl NatmapOps, group: &ForwardingGroup) -> Result<()> {
    let proto = parse_group_proto(&group.proto)?;
    let ports_csv = ports_csv(&group.ports);

    if let Err(e) = natmap
        .dnat(
            DnatConfig {
                ext_ip: group.ext_ip.clone(),
                int_ip: group.int_ip.clone(),
                ports: ports_csv.clone(),
                proto,
                ext_if: None,
                preserve_src_ip: group.preserve_src_ip,
            },
            true,
        )
        .await
    {
        tracing::warn!(
            ext.ip = %group.ext_ip,
            int.ip = %group.int_ip,
            ports = %ports_csv,
            proto = %group.proto,
            error = %e,
            "failed to delete existing dnat rule"
        );
    }

    if let Err(e) = natmap
        .dnat(
            DnatConfig {
                ext_ip: group.ext_ip.clone(),
                int_ip: group.int_ip.clone(),
                ports: ports_csv.clone(),
                proto,
                ext_if: None,
                preserve_src_ip: group.preserve_src_ip,
            },
            false,
        )
        .await
    {
        tracing::warn!(
            ext.ip = %group.ext_ip,
            int.ip = %group.int_ip,
            ports = %ports_csv,
            proto = %group.proto,
            error = %e,
            "failed to create dnat rule"
        );
    }

    if group.hairpin {
        let lan_cidr = if group.preserve_src_ip {
            match get_lan_cidr(&group.int_ip) {
                Ok(cidr) => Some(cidr),
                Err(e) => {
                    tracing::warn!(
                        int.ip = %group.int_ip,
                        error = %e,
                        "failed to detect LAN CIDR, skipping hairpin source restriction"
                    );
                    None
                }
            }
        } else {
            None
        };

        if let Err(e) = natmap
            .hairpin(
                HairpinConfig {
                    ext_ip: group.ext_ip.clone(),
                    int_ip: group.int_ip.clone(),
                    ports: ports_csv.clone(),
                    proto,
                    lan_cidr: lan_cidr.clone(),
                },
                true,
            )
            .await
        {
            tracing::warn!(
                ext.ip = %group.ext_ip,
                int.ip = %group.int_ip,
                ports = %ports_csv,
                proto = %group.proto,
                error = %e,
                "failed to delete existing hairpin rule"
            );
        }

        if let Err(e) = natmap
            .hairpin(
                HairpinConfig {
                    ext_ip: group.ext_ip.clone(),
                    int_ip: group.int_ip.clone(),
                    ports: ports_csv.clone(),
                    proto,
                    lan_cidr: lan_cidr.clone(),
                },
                false,
            )
            .await
        {
            tracing::warn!(
                ext.ip = %group.ext_ip,
                int.ip = %group.int_ip,
                ports = %ports_csv,
                proto = %group.proto,
                error = %e,
                "hairpin creation failed (non-fatal)"
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
    Ok(())
}

/// Deletes one stale forwarding rule reported by the daemon.
///
/// SNAT and Docker mapping rules are not forwarding-managed and are skipped.
/// A failed delete is logged (warn) but does not fail the sync — the stale
/// rule will be retried on the next sync.
async fn delete_stale_rule(natmap: &impl NatmapOps, rule: &LiveRule) {
    let ports_csv = ports_csv(&rule.ports);
    let result = match rule.kind {
        RuleKind::Dnat => natmap
            .dnat(
                DnatConfig {
                    ext_ip: rule.ext_ip.clone(),
                    int_ip: rule.int_ip.clone(),
                    ports: ports_csv.clone(),
                    proto: rule.proto,
                    ext_if: None,
                    preserve_src_ip: false,
                },
                true,
            )
            .await
            .map(|_| ()),
        RuleKind::Hairpin => natmap
            .hairpin(
                HairpinConfig {
                    ext_ip: rule.ext_ip.clone(),
                    int_ip: rule.int_ip.clone(),
                    ports: ports_csv.clone(),
                    proto: rule.proto,
                    lan_cidr: None,
                },
                true,
            )
            .await
            .map(|_| ()),
        RuleKind::Snat | RuleKind::Mapping => return,
    };
    if let Err(e) = result {
        tracing::warn!(
            kind = ?rule.kind,
            ext.ip = %rule.ext_ip,
            int.ip = %rule.int_ip,
            ports = %ports_csv,
            proto = %rule.proto,
            error = %e,
            "failed to delete stale forwarding rule"
        );
    }
}

/// Joins ports into the comma-separated form used by natmap configs.
fn ports_csv(ports: &[u16]) -> String {
    ports
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",")
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    use proptest::prelude::*;
    use serde_json::json;
    use tracing_test::traced_test;

    use super::*;

    // --- parse_group_proto ---

    #[test]
    fn parse_group_proto_accepts_tcp() {
        assert_eq!(parse_group_proto("tcp").unwrap(), TransportProtocol::Tcp);
    }

    #[test]
    fn parse_group_proto_accepts_udp() {
        assert_eq!(parse_group_proto("udp").unwrap(), TransportProtocol::Udp);
    }

    #[test]
    fn parse_group_proto_rejects_invalid_protocol() {
        let err = parse_group_proto("bogus").unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }

    fn arb_ipv4() -> impl Strategy<Value = String> {
        (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>())
            .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}"))
    }

    fn arb_forwarding_service() -> impl Strategy<Value = serde_json::Value> {
        (
            arb_ipv4(),
            arb_ipv4(),
            prop::sample::select(&["tcp", "udp"]),
            any::<bool>(),
            any::<bool>(),
        )
            .prop_flat_map(|(ext_ip, int_ip, proto, hairpin, preserve_src_ip)| {
                let port_count = 1..=5usize;
                (
                    Just((ext_ip, int_ip, proto, hairpin, preserve_src_ip)),
                    prop::collection::vec(any::<u16>(), port_count),
                )
            })
            .prop_map(
                |((ext_ip, int_ip, proto, hairpin, preserve_src_ip), ports)| {
                    let ports_str = ports
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    let mut meta = serde_json::Map::new();
                    meta.insert("ext_ip".into(), json!(ext_ip));
                    meta.insert("ext_ports".into(), json!(ports_str));
                    meta.insert("protocol".into(), json!(proto));
                    if hairpin {
                        meta.insert("hairpin".into(), json!("true"));
                    }
                    if preserve_src_ip {
                        meta.insert("preserve_src_ip".into(), json!("true"));
                    }
                    json!({
                        "Address": int_ip,
                        "Meta": meta,
                    })
                },
            )
    }

    fn service_matches_group(svc: &serde_json::Value, g: &ForwardingGroup) -> Option<bool> {
        let meta = svc.get("Meta")?.as_object()?;
        let ext_ip = meta.get("ext_ip")?.as_str()?;
        let address = svc.get("Address")?.as_str()?;
        let proto = meta
            .get("protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("tcp");
        if ext_ip != g.ext_ip || address != g.int_ip || proto != g.proto {
            return Some(false);
        }
        Some(true)
    }

    proptest! {
        #[test]
        fn no_duplicate_keys(services in prop::collection::vec(arb_forwarding_service(), 0..=10)) {
            let groups = group_forwarding_services(services);
            let mut seen = std::collections::HashSet::new();
            for g in &groups {
                let key = (g.ext_ip.clone(), g.int_ip.clone(), g.proto.clone());
                prop_assert!(seen.insert(key), "duplicate key ({}, {}, {})", g.ext_ip, g.int_ip, g.proto);
            }
        }

        #[test]
        fn ports_sorted_and_unique(services in prop::collection::vec(arb_forwarding_service(), 0..=10)) {
            let groups = group_forwarding_services(services);
            for g in &groups {
                let mut expected = g.ports.clone();
                expected.sort();
                expected.dedup();
                prop_assert_eq!(&g.ports, &expected, "ports not sorted/deduped for ({}, {})", g.ext_ip, g.int_ip);
            }
        }

        #[test]
        fn all_ports_in_group(services in prop::collection::vec(arb_forwarding_service(), 0..=10)) {
            let groups = group_forwarding_services(services.clone());
            for svc in &services {
                let svc_addr = match svc.get("Address").and_then(|v| v.as_str()) {
                    Some(a) => a,
                    None => continue,
                };
                let meta = match svc.get("Meta").and_then(|v| v.as_object()) {
                    Some(m) => m,
                    None => continue,
                };
                let ext_ip = match meta.get("ext_ip").and_then(|v| v.as_str()) {
                    Some(e) => e,
                    None => continue,
                };
                let ports_str = match meta.get("ext_ports").and_then(|v| v.as_str()) {
                    Some(p) => p,
                    None => continue,
                };
                let ports: Vec<u16> = ports_str.split(',').filter_map(|p| p.parse().ok()).collect();
                if ports.is_empty() {
                    continue;
                }
                let proto = meta.get("protocol").and_then(|v| v.as_str()).unwrap_or("tcp");
                let group = groups.iter().find(|g| {
                    g.ext_ip == ext_ip && g.int_ip == svc_addr && g.proto == proto
                });
                prop_assert!(group.is_some(),
                    "service ({}, {}, {}) has no matching group", ext_ip, svc_addr, proto);
                let group = group.unwrap();
                for p in &ports {
                    prop_assert!(group.ports.contains(p),
                        "port {} from service ({}, {}, {}) not in group", p, ext_ip, svc_addr, proto);
                }
            }
        }

        #[test]
        fn hairpin_or_ed(services in prop::collection::vec(arb_forwarding_service(), 0..=10)) {
            let groups = group_forwarding_services(services.clone());
            for g in &groups {
                let any_hairpin = services.iter().filter(|svc| {
                    service_matches_group(svc, g).unwrap_or(false)
                }).any(|svc| {
                    svc.get("Meta")
                        .and_then(|m| m.get("hairpin"))
                        .and_then(|v| v.as_str())
                        == Some("true")
                });
                prop_assert_eq!(g.hairpin, any_hairpin,
                    "hairpin mismatch for ({}, {}): group={}, any_service={}", g.ext_ip, g.int_ip, g.hairpin, any_hairpin);
            }
        }

        #[test]
        fn preserve_src_ip_or_ed(services in prop::collection::vec(arb_forwarding_service(), 0..=10)) {
            let groups = group_forwarding_services(services.clone());
            for g in &groups {
                let any_preserve = services.iter().filter(|svc| {
                    service_matches_group(svc, g).unwrap_or(false)
                }).any(|svc| {
                    svc.get("Meta")
                        .and_then(|m| m.get("preserve_src_ip"))
                        .and_then(|v| v.as_str())
                        == Some("true")
                });
                prop_assert_eq!(g.preserve_src_ip, any_preserve,
                    "preserve_src_ip mismatch for ({}, {}): group={}, any_service={}", g.ext_ip, g.int_ip, g.preserve_src_ip, any_preserve);
            }
        }
    }

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
    fn empty_input_returns_empty() {
        let groups = group_forwarding_services(vec![]);
        assert!(groups.is_empty());
    }

    #[test]
    fn port_zero_in_ext_ports() {
        let services = vec![make_service(
            "1.2.3.4", "10.0.0.1", "0", "tcp", false, false,
        )];
        let groups = group_forwarding_services(services);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].ports, vec![0]);
    }

    #[test]
    fn different_protocols_grouped_separately() {
        let services = vec![
            make_service("1.2.3.4", "10.0.0.1", "80", "tcp", false, false),
            make_service("1.2.3.4", "10.0.0.1", "80", "udp", false, false),
        ];
        let groups = group_forwarding_services(services);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().any(|g| g.proto == "tcp"));
        assert!(groups.iter().any(|g| g.proto == "udp"));
    }

    #[test]
    fn meta_non_object_skipped() {
        let services = vec![json!({
            "Address": "10.0.0.1",
            "Meta": "not-an-object",
        })];
        assert!(group_forwarding_services(services).is_empty());
    }

    #[test]
    fn meta_null_skipped() {
        let services = vec![json!({
            "Address": "10.0.0.1",
            "Meta": null,
        })];
        assert!(group_forwarding_services(services).is_empty());
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

    // --- FakeNatmap in-memory adapter ---

    /// In-memory [`NatmapOps`] recording every call for assertions.
    struct FakeNatmap {
        reported: Mutex<Vec<LiveRule>>,
        dnat_deletes: Mutex<Vec<DnatConfig>>,
        dnat_creates: Mutex<Vec<DnatConfig>>,
        hairpin_deletes: Mutex<Vec<HairpinConfig>>,
        hairpin_creates: Mutex<Vec<HairpinConfig>>,
        fail_dnat_create: AtomicBool,
        fail_hairpin_create: AtomicBool,
        fail_dnat_delete: AtomicBool,
    }

    impl FakeNatmap {
        fn new(reported: Vec<LiveRule>) -> Self {
            Self {
                reported: Mutex::new(reported),
                dnat_deletes: Mutex::new(Vec::new()),
                dnat_creates: Mutex::new(Vec::new()),
                hairpin_deletes: Mutex::new(Vec::new()),
                hairpin_creates: Mutex::new(Vec::new()),
                fail_dnat_create: AtomicBool::new(false),
                fail_hairpin_create: AtomicBool::new(false),
                fail_dnat_delete: AtomicBool::new(false),
            }
        }

        fn set_fail_dnat_create(&self, fail: bool) {
            self.fail_dnat_create.store(fail, Ordering::SeqCst);
        }

        fn set_fail_dnat_delete(&self, fail: bool) {
            self.fail_dnat_delete.store(fail, Ordering::SeqCst);
        }
    }

    impl NatmapOps for FakeNatmap {
        async fn rules(&self) -> Result<Vec<LiveRule>, NatmapError> {
            Ok(self.reported.lock().unwrap().clone())
        }

        async fn dnat(
            &self,
            config: DnatConfig,
            delete: bool,
        ) -> Result<Option<DnatConfig>, NatmapError> {
            if delete {
                if self.fail_dnat_delete.load(Ordering::SeqCst) {
                    return Err(NatmapError::Internal("fake dnat delete failure".into()));
                }
                self.dnat_deletes.lock().unwrap().push(config);
            } else {
                if self.fail_dnat_create.load(Ordering::SeqCst) {
                    return Err(NatmapError::Internal("fake dnat create failure".into()));
                }
                self.dnat_creates.lock().unwrap().push(config);
            }
            Ok(None)
        }

        async fn hairpin(
            &self,
            config: HairpinConfig,
            delete: bool,
        ) -> Result<Option<HairpinConfig>, NatmapError> {
            if delete {
                self.hairpin_deletes.lock().unwrap().push(config);
            } else {
                if self.fail_hairpin_create.load(Ordering::SeqCst) {
                    return Err(NatmapError::Internal("fake hairpin create failure".into()));
                }
                self.hairpin_creates.lock().unwrap().push(config);
            }
            Ok(None)
        }
    }

    fn make_live_dnat(ext_ip: &str, int_ip: &str, ports: &[u16]) -> LiveRule {
        LiveRule {
            kind: RuleKind::Dnat,
            ext_ip: ext_ip.to_string(),
            int_ip: int_ip.to_string(),
            ports: ports.to_vec(),
            proto: TransportProtocol::Tcp,
        }
    }

    fn make_live_hairpin(ext_ip: &str, int_ip: &str, ports: &[u16]) -> LiveRule {
        LiveRule {
            kind: RuleKind::Hairpin,
            ext_ip: ext_ip.to_string(),
            int_ip: int_ip.to_string(),
            ports: ports.to_vec(),
            proto: TransportProtocol::Tcp,
        }
    }

    fn make_group(ext_ip: &str, int_ip: &str, ports: &[u16], hairpin: bool) -> ForwardingGroup {
        ForwardingGroup {
            ext_ip: ext_ip.to_string(),
            int_ip: int_ip.to_string(),
            ports: ports.to_vec(),
            proto: "tcp".to_string(),
            hairpin,
            preserve_src_ip: false,
        }
    }

    // --- stale_forwarding_rules tests ---

    #[test]
    fn stale_forwarding_rules_marks_multiport_dnat_when_not_desired() {
        let reported = vec![make_live_dnat("203.0.113.50", "10.0.0.99", &[80, 443])];
        let desired: HashSet<(String, String, TransportProtocol)> = HashSet::new();
        let stale = stale_forwarding_rules(&reported, &desired);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].ports, vec![80, 443]);
    }

    #[test]
    fn stale_forwarding_rules_keeps_desired_rules() {
        let reported = vec![
            make_live_dnat("203.0.113.50", "10.0.0.99", &[80]),
            make_live_hairpin("203.0.113.50", "10.0.0.99", &[80]),
        ];
        let desired: HashSet<(String, String, TransportProtocol)> = [(
            "203.0.113.50".to_string(),
            "10.0.0.99".to_string(),
            TransportProtocol::Tcp,
        )]
        .into();
        assert!(stale_forwarding_rules(&reported, &desired).is_empty());
    }

    #[test]
    fn stale_forwarding_rules_ignores_mapping_and_snat() {
        let mapping = LiveRule {
            kind: RuleKind::Mapping,
            ext_ip: "203.0.113.50".to_string(),
            int_ip: "10.0.0.99".to_string(),
            ports: vec![80],
            proto: TransportProtocol::Tcp,
        };
        let snat = LiveRule {
            kind: RuleKind::Snat,
            ext_ip: "10.0.0.99".to_string(),
            int_ip: "10.0.0.99".to_string(),
            ports: vec![],
            proto: TransportProtocol::Tcp,
        };
        let reported = vec![mapping, snat];
        let desired: HashSet<(String, String, TransportProtocol)> = HashSet::new();
        assert!(stale_forwarding_rules(&reported, &desired).is_empty());
    }

    // --- reconcile_forwarding_rules tests ---

    #[tokio::test]
    async fn reconcile_forwarding_rules_deletes_stale_with_real_attributes() {
        let natmap = FakeNatmap::new(vec![make_live_dnat(
            "203.0.113.50",
            "10.0.0.99",
            &[80, 443],
        )]);
        let groups = vec![make_group("198.51.100.7", "10.0.0.99", &[8080], false)];
        reconcile_forwarding_rules(&natmap, &groups).await.unwrap();

        let deletes = natmap.dnat_deletes.lock().unwrap();
        // Delete-first for the desired group + one stale delete.
        assert_eq!(deletes.len(), 2);
        let stale = deletes.iter().find(|c| c.ext_ip == "203.0.113.50").unwrap();
        assert_eq!(stale.int_ip, "10.0.0.99");
        assert_eq!(stale.ports, "80,443");
        assert_eq!(stale.proto, TransportProtocol::Tcp);
    }

    #[tokio::test]
    async fn reconcile_forwarding_rules_keeps_desired_rules() {
        let natmap = FakeNatmap::new(vec![make_live_dnat("198.51.100.7", "10.0.0.99", &[8080])]);
        let groups = vec![make_group("198.51.100.7", "10.0.0.99", &[8080], false)];
        reconcile_forwarding_rules(&natmap, &groups).await.unwrap();

        // Only the group's own delete-first delete; no stale delete.
        let deletes = natmap.dnat_deletes.lock().unwrap();
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].ext_ip, "198.51.100.7");
        assert_eq!(natmap.dnat_creates.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    #[traced_test]
    async fn reconcile_forwarding_rules_create_failure_does_not_skip_hairpin() {
        let natmap = FakeNatmap::new(vec![]);
        natmap.set_fail_dnat_create(true);
        let groups = vec![make_group("198.51.100.7", "10.0.0.99", &[8080], true)];
        reconcile_forwarding_rules(&natmap, &groups).await.unwrap();

        assert!(logs_contain("failed to create dnat rule"));
        assert_eq!(natmap.hairpin_creates.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    #[traced_test]
    async fn reconcile_forwarding_rules_delete_failure_is_logged_not_swallowed() {
        let natmap = FakeNatmap::new(vec![make_live_dnat("203.0.113.50", "10.0.0.99", &[80])]);
        natmap.set_fail_dnat_delete(true);
        let groups = vec![make_group("198.51.100.7", "10.0.0.99", &[8080], false)];
        reconcile_forwarding_rules(&natmap, &groups).await.unwrap();

        assert!(logs_contain("failed to delete stale forwarding rule"));
        // The sync must still apply the desired group despite the failed delete.
        assert_eq!(natmap.dnat_creates.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    #[traced_test]
    async fn reconcile_forwarding_rules_empty_groups_deletes_all() {
        let natmap = FakeNatmap::new(vec![
            make_live_dnat("203.0.113.50", "10.0.0.99", &[80]),
            make_live_hairpin("203.0.113.51", "10.0.0.98", &[443]),
        ]);
        reconcile_forwarding_rules(&natmap, &[]).await.unwrap();

        assert!(logs_contain("no forwarding services found in Consul"));
        assert_eq!(natmap.dnat_deletes.lock().unwrap().len(), 1);
        assert_eq!(natmap.hairpin_deletes.lock().unwrap().len(), 1);
        assert!(natmap.dnat_creates.lock().unwrap().is_empty());
    }
}
