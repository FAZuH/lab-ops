//! Applies a resolved `discovery.yaml` service to the natmap daemon.
//!
//! This is the mapping half of a discovery pass: resolve the target's bind
//! address, decide the host port, create the DNAT mapping through the daemon,
//! and install the `preserve_src_ip` policy route. It knows nothing about
//! Consul; registering the resulting port is a separate concern.
//!
//! [`apply_service`] is the entry point the planned `natmap apply --from
//! discovery.yaml` builds on.

use std::future::Future;
use std::pin::Pin;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use lab_ops_lab_lib::TransportProtocol;
use lab_ops_lab_lib::discovery::ResolvedPortType;
use lab_ops_lab_lib::discovery::ResolvedService;

use crate::client::NatmapClient;
use crate::client::NatmapError;
use crate::models::DockerAddMapRequest;
use crate::models::DockerPortMap;
use crate::models::PolicyRouteConfig;

/// A boxed future returned by the [`NatmapApply`] seam.
type ApplyFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The natmap operations applying a discovered service needs.
///
/// [`NatmapClient`] is the production implementation; tests fake it so port
/// decisions and rule wiring can be exercised without a daemon.
pub trait NatmapApply: Send + Sync {
    /// Creates one Docker mapping (DNAT) for a service port.
    fn add_mapping(
        &self,
        id: &str,
        req: DockerAddMapRequest,
    ) -> ApplyFuture<'_, Result<DockerPortMap, NatmapError>>;

    /// Installs or deletes a policy routing rule.
    fn policy_route(
        &self,
        config: PolicyRouteConfig,
        delete: bool,
    ) -> ApplyFuture<'_, Result<Option<PolicyRouteConfig>, NatmapError>>;
}

impl NatmapApply for NatmapClient {
    fn add_mapping(
        &self,
        id: &str,
        req: DockerAddMapRequest,
    ) -> ApplyFuture<'_, Result<DockerPortMap, NatmapError>> {
        let id = id.to_string();
        Box::pin(async move { NatmapClient::add_mapping(self, &id, req).await })
    }

    fn policy_route(
        &self,
        config: PolicyRouteConfig,
        delete: bool,
    ) -> ApplyFuture<'_, Result<Option<PolicyRouteConfig>, NatmapError>> {
        Box::pin(async move { NatmapClient::policy_route(self, config, delete).await })
    }
}

/// The identity and address of the thing a resolved service points at.
#[derive(Debug, Clone, Copy)]
pub struct ApplyTarget<'a> {
    /// Container ID, or the service key for a local (non-Docker) service.
    pub id: &'a str,
    /// The container's primary IP, when the target is a container.
    pub container_ip: Option<&'a str>,
    /// The local service address, when the target is a local service.
    pub local_ip: Option<&'a str>,
}

impl ApplyTarget<'_> {
    /// Whether this target is a local (non-Docker) service.
    pub fn is_local(&self) -> bool {
        self.local_ip.is_some()
    }
}

/// The mapping a resolved service asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortDecision {
    /// Host port to request; `0` asks the daemon to allocate one.
    pub host_port: u16,
    /// Port on the container or local service.
    pub container_port: u16,
}

/// Translates a resolved service's intent into a mapping request.
///
/// Returns `None` when the service needs no mapping at all: an rproxy service
/// on a local target is reached by the proxy directly. Availability is not
/// decided here — an explicit port is sent as requested and the daemon
/// arbitrates (409 on conflict); a dynamic port is sent as `host_port: 0` and
/// the daemon reports the chosen port.
pub fn decide_ports(resolved: &ResolvedService, local_target: bool) -> Option<PortDecision> {
    let container_port = resolved.container_port;
    match &resolved.port_type {
        ResolvedPortType::ForwardRemote { ext_ports, .. } => Some(PortDecision {
            host_port: ext_ports.first().copied().unwrap_or(0),
            container_port,
        }),
        ResolvedPortType::ForwardLocal { bind_port } => Some(PortDecision {
            host_port: bind_port.unwrap_or(0),
            container_port,
        }),
        ResolvedPortType::RProxyLocal { .. } | ResolvedPortType::RProxyRemote { .. } => {
            if local_target {
                None
            } else {
                Some(PortDecision {
                    host_port: 0,
                    container_port,
                })
            }
        }
    }
}

/// The address natmap should bind the host port to.
///
/// An explicit `bind_ip` wins; otherwise the first IPv4 address of
/// `bind_interface`; otherwise the daemon's default (`0.0.0.0`).
pub fn natmap_bind_ip(resolved: &ResolvedService) -> Option<String> {
    if let Some(ip) = &resolved.bind_ip {
        return Some(ip.clone());
    }
    if let Some(iface) = &resolved.bind_interface {
        if let Some(ip) = resolve_interface_ip(iface) {
            return Some(ip);
        }
        tracing::warn!(
            interface = %iface,
            "bind_interface has no IPv4 address (interface may be down)"
        );
    }
    None
}

/// The address of the service on this node.
///
/// Precedence: explicit `bind_ip`, the bind interface's IPv4 address, then the
/// target's own address (container IP or local service address). Used as the
/// source of the `preserve_src_ip` policy route when the service does not name
/// one.
pub fn service_address(resolved: &ResolvedService, target: &ApplyTarget<'_>) -> Option<String> {
    natmap_bind_ip(resolved).or_else(|| target.container_ip.or(target.local_ip).map(str::to_string))
}

/// Applies one resolved service and returns the host port it is published on.
///
/// The steps are, in order: decide the port, ask the daemon for the DNAT
/// mapping (which allocates the port when the decision is dynamic), and
/// install the `preserve_src_ip` policy route when the service asks for one.
///
/// A conflict or a missing container on an explicit port is non-fatal: the
/// requested port is returned, mirroring the daemon's own arbitration.
///
/// Span fields: `service.id_prefix`, `host.port`, `container.port`.
#[tracing::instrument(skip_all, fields(
    service.id_prefix = %resolved.service_id_prefix,
    host.port = tracing::field::Empty,
    container.port = %resolved.container_port
))]
pub async fn apply_service(
    apply: &dyn NatmapApply,
    resolved: &ResolvedService,
    target: ApplyTarget<'_>,
) -> Result<u16> {
    let host_port = match decide_ports(resolved, target.is_local()) {
        Some(decision) => {
            let bind_ip = natmap_bind_ip(resolved);
            ensure_mapping(
                apply,
                target.id,
                bind_ip.as_deref(),
                decision.host_port,
                decision.container_port,
                resolved.protocol,
                target.local_ip,
            )
            .await?
        }
        None => resolved.container_port,
    };
    tracing::Span::current().record("host.port", host_port);

    if let ResolvedPortType::ForwardRemote {
        preserve_src_ip: true,
        preserve_src_ip_gateway: Some(gateway),
        preserve_src_ip_src,
        ..
    } = &resolved.port_type
    {
        let src_ip = match preserve_src_ip_src.clone() {
            Some(src) => src,
            None => service_address(resolved, &target).ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "service {} preserves its source IP but has no bind address to route",
                    resolved.service_id_prefix
                )
            })?,
        };
        apply
            .policy_route(
                PolicyRouteConfig {
                    src_ip,
                    via: gateway.clone(),
                    table: 100,
                },
                false,
            )
            .await
            .wrap_err("policy_route command failed")?;
    }

    Ok(host_port)
}

/// Creates the mapping and returns the effective host port.
///
/// On success this is the port reported by the daemon
/// (`mapping.request.host_addr.port()`); for a dynamic request (`host_port: 0`)
/// the daemon allocates and reports it. On an explicit port, an existing
/// mapping (409), a missing container (404), or a daemon still recovering its
/// Docker integration (503) is non-fatal — the requested host port is returned.
/// A dynamic request that the daemon rejects fails, because there is no port to
/// report.
#[allow(clippy::too_many_arguments)]
async fn ensure_mapping(
    apply: &dyn NatmapApply,
    id: &str,
    host_ip: Option<&str>,
    host_port: u16,
    container_port: u16,
    proto: TransportProtocol,
    target_ip: Option<&str>,
) -> Result<u16> {
    let req = DockerAddMapRequest {
        host_ip: host_ip.unwrap_or("0.0.0.0").to_string(),
        host_port,
        container_port,
        target_ip: target_ip.map(str::to_string),
        proto,
    };
    match apply.add_mapping(id, req).await {
        Ok(mapping) => Ok(mapping.request.host_addr.port()),
        Err(e @ NatmapError::Conflict(_)) if host_port == 0 => {
            Err(e).wrap_err("natmap rejected dynamic port allocation")
        }
        Err(e @ NatmapError::NotFound(_)) if host_port == 0 => {
            Err(e).wrap_err("container not found for dynamic port allocation")
        }
        Err(e @ NatmapError::Unavailable(_)) if host_port == 0 => {
            Err(e).wrap_err("natmap unavailable")
        }
        Err(NatmapError::Conflict(e)) => {
            tracing::warn!(
                error = %format!("{e:#}"),
                "natmap mapping already exists (409), keeping the requested port"
            );
            Ok(host_port)
        }
        Err(NatmapError::NotFound(e)) => {
            tracing::warn!(
                error = %format!("{e:#}"),
                "container not found, keeping the requested port"
            );
            Ok(host_port)
        }
        Err(NatmapError::Unavailable(e)) => {
            tracing::warn!(
                error = %e,
                "natmap mapping unavailable (503), keeping the requested port"
            );
            Ok(host_port)
        }
        Err(e) => Err(e).wrap_err("natmap command failed"),
    }
}

/// Resolves the first IPv4 address on a network interface via `ip -j -4 addr show`.
pub fn resolve_interface_ip(iface_name: &str) -> Option<String> {
    let output = std::process::Command::new("ip")
        .args(["-j", "-4", "addr", "show", iface_name])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    if let Some(interfaces) = parsed.as_array()
        && let Some(interface) = interfaces.first()
        && let Some(addr_info) = interface.get("addr_info").and_then(|a| a.as_array())
        && let Some(first_addr) = addr_info.first()
        && let Some(ip) = first_addr.get("local").and_then(|l| l.as_str())
    {
        return Some(ip.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::models::DockerPortMapRequest;

    /// Port the fake daemon reports when asked to allocate (`host_port: 0`).
    const FAKE_ALLOCATED_PORT: u16 = 40000;

    /// Records the calls the apply path makes and answers like the daemon.
    #[derive(Default)]
    struct FakeNatmap {
        mappings: Mutex<Vec<(String, DockerAddMapRequest)>>,
        policy_routes: Mutex<Vec<(PolicyRouteConfig, bool)>>,
        fail_with: Mutex<Option<NatmapError>>,
    }

    impl FakeNatmap {
        fn fail_with(error: NatmapError) -> Self {
            FakeNatmap {
                fail_with: Mutex::new(Some(error)),
                ..Default::default()
            }
        }

        fn mappings(&self) -> Vec<(String, DockerAddMapRequest)> {
            self.mappings.lock().unwrap().clone()
        }

        fn policy_routes(&self) -> Vec<(PolicyRouteConfig, bool)> {
            self.policy_routes.lock().unwrap().clone()
        }
    }

    impl NatmapApply for FakeNatmap {
        fn add_mapping(
            &self,
            id: &str,
            req: DockerAddMapRequest,
        ) -> ApplyFuture<'_, Result<DockerPortMap, NatmapError>> {
            let id = id.to_string();
            Box::pin(async move {
                if let Some(error) = self.fail_with.lock().unwrap().take() {
                    return Err(error);
                }
                self.mappings
                    .lock()
                    .unwrap()
                    .push((id.clone(), req.clone()));
                let host_port = if req.host_port == 0 {
                    FAKE_ALLOCATED_PORT
                } else {
                    req.host_port
                };
                let container_ip = req.target_ip.clone().unwrap_or_else(|| "0.0.0.0".into());
                Ok(DockerPortMap {
                    id: 1,
                    request: DockerPortMapRequest {
                        host_addr: format!("{}:{}", req.host_ip, host_port).parse().unwrap(),
                        container_addr: format!("{}:{}", container_ip, req.container_port)
                            .parse()
                            .unwrap(),
                        proto: req.proto,
                    },
                    container_id: String::new(),
                    container_name: String::new(),
                    rule_comment: String::new(),
                })
            })
        }

        fn policy_route(
            &self,
            config: PolicyRouteConfig,
            delete: bool,
        ) -> ApplyFuture<'_, Result<Option<PolicyRouteConfig>, NatmapError>> {
            Box::pin(async move {
                self.policy_routes.lock().unwrap().push((config, delete));
                Ok(None)
            })
        }
    }

    fn make_resolved(
        prefix: &str,
        container_port: u16,
        port_type: ResolvedPortType,
    ) -> ResolvedService {
        ResolvedService {
            service_id_prefix: prefix.to_string(),
            service_name: prefix.to_string(),
            service_type: lab_ops_lab_lib::discovery::ServiceType::Docker,
            match_cfg: None,
            local_address: None,
            container_port,
            proxy_on: None,
            bind_ip: None,
            bind_interface: None,
            protocol: TransportProtocol::Tcp,
            port_type,
            extra: std::collections::HashMap::new(),
        }
    }

    fn forward_remote(
        ext_ports: Vec<u16>,
        preserve_src_ip: bool,
        gateway: Option<&str>,
        src: Option<&str>,
    ) -> ResolvedPortType {
        ResolvedPortType::ForwardRemote {
            ext_ip: "203.0.113.50".into(),
            ext_ports,
            hairpin: false,
            preserve_src_ip,
            preserve_src_ip_gateway: gateway.map(str::to_string),
            preserve_src_ip_src: src.map(str::to_string),
        }
    }

    fn local_target<'a>(id: &'a str, ip: &'a str) -> ApplyTarget<'a> {
        ApplyTarget {
            id,
            container_ip: None,
            local_ip: Some(ip),
        }
    }

    fn container_target<'a>(id: &'a str, ip: Option<&'a str>) -> ApplyTarget<'a> {
        ApplyTarget {
            id,
            container_ip: ip,
            local_ip: None,
        }
    }

    #[test]
    fn decide_ports_forward_local_explicit_port() {
        let resolved = make_resolved(
            "web",
            8080,
            ResolvedPortType::ForwardLocal {
                bind_port: Some(38080),
            },
        );
        assert_eq!(
            decide_ports(&resolved, false),
            Some(PortDecision {
                host_port: 38080,
                container_port: 8080,
            })
        );
    }

    #[test]
    fn decide_ports_forward_local_unbound_allocates() {
        let resolved = make_resolved(
            "web",
            8080,
            ResolvedPortType::ForwardLocal { bind_port: None },
        );
        assert_eq!(
            decide_ports(&resolved, false),
            Some(PortDecision {
                host_port: 0,
                container_port: 8080,
            })
        );
    }

    #[test]
    fn decide_ports_forward_remote_uses_the_first_external_port() {
        let resolved = make_resolved(
            "web",
            30000,
            forward_remote(vec![30001, 30002], false, None, None),
        );
        assert_eq!(
            decide_ports(&resolved, false),
            Some(PortDecision {
                host_port: 30001,
                container_port: 30000,
            })
        );
    }

    #[test]
    fn decide_ports_forward_remote_without_external_ports_allocates() {
        let resolved = make_resolved("web", 30000, forward_remote(vec![], false, None, None));
        assert_eq!(
            decide_ports(&resolved, false),
            Some(PortDecision {
                host_port: 0,
                container_port: 30000,
            })
        );
    }

    #[test]
    fn decide_ports_rproxy_local_target_needs_no_mapping() {
        let resolved = make_resolved(
            "app",
            80,
            ResolvedPortType::RProxyLocal {
                template: "HTTP_PROXY".into(),
                domains: vec!["app.test".into()],
                proxy_ip: None,
            },
        );
        assert_eq!(decide_ports(&resolved, true), None);
    }

    #[test]
    fn decide_ports_rproxy_container_target_allocates() {
        let resolved = make_resolved(
            "app",
            80,
            ResolvedPortType::RProxyLocal {
                template: "HTTP_PROXY".into(),
                domains: vec!["app.test".into()],
                proxy_ip: None,
            },
        );
        assert_eq!(
            decide_ports(&resolved, false),
            Some(PortDecision {
                host_port: 0,
                container_port: 80,
            })
        );
    }

    #[tokio::test]
    async fn apply_service_local_target_passes_the_local_ip_as_target() {
        let fake = FakeNatmap::default();
        let mut resolved = make_resolved(
            "loc",
            9090,
            ResolvedPortType::ForwardLocal {
                bind_port: Some(39090),
            },
        );
        resolved.bind_ip = Some("10.0.0.5".into());

        let host_port = apply_service(&fake, &resolved, local_target("loc", "127.0.0.1"))
            .await
            .unwrap();

        assert_eq!(host_port, 39090);
        let mappings = fake.mappings();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, "loc");
        assert_eq!(mappings[0].1.host_ip, "10.0.0.5");
        assert_eq!(mappings[0].1.host_port, 39090);
        assert_eq!(mappings[0].1.container_port, 9090);
        assert_eq!(mappings[0].1.target_ip.as_deref(), Some("127.0.0.1"));
    }

    #[tokio::test]
    async fn apply_service_container_target_allocates_and_omits_the_target_ip() {
        let fake = FakeNatmap::default();
        let resolved = make_resolved(
            "web",
            8080,
            ResolvedPortType::ForwardLocal { bind_port: None },
        );

        let host_port = apply_service(&fake, &resolved, container_target("abc123", None))
            .await
            .unwrap();

        assert_eq!(host_port, FAKE_ALLOCATED_PORT);
        let mappings = fake.mappings();
        assert_eq!(mappings[0].0, "abc123");
        assert_eq!(mappings[0].1.host_port, 0);
        assert_eq!(mappings[0].1.target_ip, None);
    }

    #[tokio::test]
    async fn apply_service_dynamic_conflict_is_fatal() {
        let fake = FakeNatmap::fail_with(NatmapError::Conflict("port taken".into()));
        let resolved = make_resolved(
            "web",
            8080,
            ResolvedPortType::ForwardLocal { bind_port: None },
        );

        assert!(
            apply_service(&fake, &resolved, container_target("abc123", None))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn apply_service_explicit_conflict_keeps_the_requested_port() {
        let fake = FakeNatmap::fail_with(NatmapError::Conflict("port taken".into()));
        let resolved = make_resolved(
            "web",
            8080,
            ResolvedPortType::ForwardLocal {
                bind_port: Some(38080),
            },
        );

        let host_port = apply_service(&fake, &resolved, container_target("abc123", None))
            .await
            .unwrap();

        assert_eq!(host_port, 38080);
    }

    #[tokio::test]
    async fn apply_service_rproxy_local_target_creates_no_mapping() {
        let fake = FakeNatmap::default();
        let resolved = make_resolved(
            "app",
            80,
            ResolvedPortType::RProxyLocal {
                template: "HTTP_PROXY".into(),
                domains: vec!["app.test".into()],
                proxy_ip: None,
            },
        );

        let host_port = apply_service(&fake, &resolved, local_target("app", "10.0.0.9"))
            .await
            .unwrap();

        assert_eq!(host_port, 80);
        assert!(fake.mappings().is_empty());
    }

    #[tokio::test]
    async fn apply_service_policy_route_uses_the_explicit_source() {
        let fake = FakeNatmap::default();
        let resolved = make_resolved(
            "web",
            80,
            forward_remote(vec![30000], true, Some("192.168.1.1"), Some("10.0.0.10")),
        );

        apply_service(
            &fake,
            &resolved,
            container_target("abc123", Some("172.17.0.2")),
        )
        .await
        .unwrap();

        let routes = fake.policy_routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].0.src_ip, "10.0.0.10");
        assert_eq!(routes[0].0.via, "192.168.1.1");
        assert_eq!(routes[0].0.table, 100);
        assert!(!routes[0].1);
    }

    #[tokio::test]
    async fn apply_service_policy_route_falls_back_to_the_service_address() {
        let fake = FakeNatmap::default();
        let resolved = make_resolved(
            "web",
            80,
            forward_remote(vec![30000], true, Some("192.168.1.1"), None),
        );

        apply_service(
            &fake,
            &resolved,
            container_target("abc123", Some("172.17.0.2")),
        )
        .await
        .unwrap();

        let routes = fake.policy_routes();
        assert_eq!(routes[0].0.src_ip, "172.17.0.2");
    }

    #[tokio::test]
    async fn apply_service_policy_route_requires_an_address() {
        let fake = FakeNatmap::default();
        let resolved = make_resolved(
            "web",
            80,
            forward_remote(vec![30000], true, Some("192.168.1.1"), None),
        );

        assert!(
            apply_service(&fake, &resolved, container_target("abc123", None))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn apply_service_installs_no_policy_route_without_a_gateway() {
        let fake = FakeNatmap::default();
        let resolved = make_resolved("web", 80, forward_remote(vec![30000], true, None, None));

        apply_service(
            &fake,
            &resolved,
            container_target("abc123", Some("172.17.0.2")),
        )
        .await
        .unwrap();

        assert!(fake.policy_routes().is_empty());
    }
}
