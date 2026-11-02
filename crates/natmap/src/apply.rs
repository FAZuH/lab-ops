//! Applies a node's `discovery.yaml` to the natmap daemon.
//!
//! Two entry points, one seam:
//!
//! - [`converge`] brings the daemon's mappings in line with a declaration —
//!   the declarative apply behind `natmap plan` and `natmap apply`.
//! - [`apply_service`] maps one already-resolved service, the older whole-node
//!   path that allocates a host port when the service does not name one.
//!
//! Both know nothing about Consul; registering a port with a consumer is a
//! separate concern.
//!
//! # Declarative apply
//!
//! A declaration names, per service, the host ports it owns (`ports:`). One
//! port produces at most one mapping: the node's tailnet address on that port
//! forwarded to the address the service answers on. A port whose service
//! already answers on the tailnet address needs no mapping and is reported as
//! covered.
//!
//! The daemon records which service owns each port ([`DockerPortMap::owner`]),
//! so the mapping set is also the ownership ledger. Three rules follow:
//!
//! - a port is handed to the service the declaration gives it to, and to no
//!   other: a port owned by another service is refused, naming both;
//! - a port is released only by the apply that removes its owner — [`Plan`]
//!   pairs the release with the create when one apply moves a port;
//! - only a mapping an apply created is ever pruned. A mapping without an owner
//!   was added by hand and survives, forever.

use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::str::FromStr;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::bail;
use color_eyre::eyre::eyre;
use lab_ops_lab_lib::TransportProtocol;
use lab_ops_lab_lib::discovery::DiscoveryConfig;
use lab_ops_lab_lib::discovery::ResolvedPortType;
use lab_ops_lab_lib::discovery::ResolvedService;
use serde::Serialize;

use crate::client::NatmapClient;
use crate::client::NatmapError;
use crate::models::DockerAddMapRequest;
use crate::models::DockerPortMap;
use crate::models::PolicyRouteConfig;
use crate::models::service_mapping_key;

/// A boxed future returned by the [`NatmapApply`] seam.
type ApplyFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The natmap operations applying a declaration needs.
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

    /// Every mapping the daemon holds, with the owner it recorded.
    fn list_mappings(&self) -> ApplyFuture<'_, Result<Vec<DockerPortMap>, NatmapError>>;

    /// Removes one mapping by the ID it was registered under and its host port.
    fn remove_mapping(&self, id: &str, port: u16) -> ApplyFuture<'_, Result<(), NatmapError>>;

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

    fn list_mappings(&self) -> ApplyFuture<'_, Result<Vec<DockerPortMap>, NatmapError>> {
        Box::pin(async move { NatmapClient::mappings(self).await })
    }

    fn remove_mapping(&self, id: &str, port: u16) -> ApplyFuture<'_, Result<(), NatmapError>> {
        let id = id.to_string();
        Box::pin(async move { NatmapClient::remove_mapping(self, &id, port).await })
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
        owner: None,
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

// --- Declarative apply ---

/// The mapping a declaration names for one port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeclaredMapping {
    /// Service that owns the port.
    pub service: String,
    /// Address the mapping answers on — the node's tailnet address.
    pub host_ip: String,
    /// The port the service owns; the same on both sides of the DNAT.
    pub port: u16,
    /// Address the service itself answers on.
    pub target_ip: String,
    /// Transport protocol. The ledger names ports, not protocols, so a declared
    /// port is TCP.
    pub proto: TransportProtocol,
}

/// A mapping the daemon holds, as the planner reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveMapping {
    /// The ID the mapping was registered under — what a removal names.
    pub id: String,
    /// Service that owns the port, or `None` for a mapping added by hand.
    pub owner: Option<String>,
    /// Address the mapping answers on.
    pub host_ip: String,
    /// Port the mapping answers on.
    pub host_port: u16,
    /// Address the mapping forwards to.
    pub target_ip: String,
    /// Port the mapping forwards to.
    pub target_port: u16,
    /// Transport protocol of the mapping.
    pub proto: TransportProtocol,
}

impl From<&DockerPortMap> for LiveMapping {
    fn from(mapping: &DockerPortMap) -> Self {
        LiveMapping {
            id: mapping.container_id.clone(),
            owner: mapping.owner.clone(),
            host_ip: mapping.request.host_addr.ip().to_string(),
            host_port: mapping.request.host_addr.port(),
            target_ip: mapping.request.container_addr.ip().to_string(),
            target_port: mapping.request.container_addr.port(),
            proto: mapping.request.proto,
        }
    }
}

/// A declared port that needs no mapping, with the address it already answers on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoveredPort {
    /// Service that owns the port.
    pub service: String,
    /// The port the service owns.
    pub port: u16,
    /// Address the service already answers on.
    pub address: String,
}

/// A declared port held by a mapping this apply will not remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    /// Service the declaration gives the port to.
    pub service: String,
    /// The contested port.
    pub port: u16,
    /// Service that owns the port now, or `None` when the holder was added by hand.
    pub owner: Option<String>,
    /// The ID the holding mapping is registered under.
    pub holder: String,
}

/// A mapping that exists but no longer matches the declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Update {
    /// The mapping as the daemon holds it.
    pub live: LiveMapping,
    /// The mapping the declaration names.
    pub wanted: DeclaredMapping,
}

/// What one apply would change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// Mappings to create.
    pub create: Vec<DeclaredMapping>,
    /// Mappings whose target address drifted.
    pub update: Vec<Update>,
    /// Mappings to release: stale ones, and ports this apply hands to another service.
    pub remove: Vec<LiveMapping>,
    /// Mappings that were added by hand. Never removed.
    pub keep: Vec<LiveMapping>,
    /// Mappings this node created, no longer declared, left because prune was off.
    pub stale: Vec<LiveMapping>,
    /// Declared ports that need no mapping.
    pub covered: Vec<CoveredPort>,
    /// Declared ports held by a mapping this apply will not remove.
    pub conflicts: Vec<Conflict>,
}

impl Plan {
    /// Whether the node already matches its declaration.
    ///
    /// Hand-added mappings and covered ports are not drift: nothing this apply
    /// owns disagrees with the declaration.
    pub fn is_clean(&self) -> bool {
        self.create.is_empty()
            && self.update.is_empty()
            && self.remove.is_empty()
            && self.stale.is_empty()
            && self.conflicts.is_empty()
    }

    /// One sentence per refused port, naming the requesting service and the holder.
    pub fn conflict_message(&self) -> Option<String> {
        if self.conflicts.is_empty() {
            return None;
        }
        let lines: Vec<String> = self
            .conflicts
            .iter()
            .map(|conflict| match &conflict.owner {
                Some(owner) => format!(
                    "port {} is owned by service {owner:?}; this declaration gives it to {:?} and this apply does not remove {owner:?} — rerun with --prune to release it",
                    conflict.port, conflict.service
                ),
                None => format!(
                    "port {} is held by a mapping this node did not create (registered as {:?}); this declaration gives it to {:?} — release it with \"natmap docker rm {} {}\", or stop whatever publishes on it",
                    conflict.port, conflict.holder, conflict.service, conflict.holder, conflict.port
                ),
            })
            .collect();
        Some(lines.join("\n"))
    }

    /// A one-line summary of what disagrees, or `None` when converged.
    pub fn drift_message(&self) -> Option<String> {
        if self.is_clean() {
            return None;
        }
        let mut parts = Vec::new();
        if !self.create.is_empty() {
            parts.push(format!("{} to create", self.create.len()));
        }
        if !self.update.is_empty() {
            parts.push(format!("{} to update", self.update.len()));
        }
        if !self.remove.is_empty() {
            parts.push(format!("{} to remove", self.remove.len()));
        }
        if !self.stale.is_empty() {
            parts.push(format!("{} stale (pass --prune)", self.stale.len()));
        }
        if !self.conflicts.is_empty() {
            parts.push(format!("{} conflicting", self.conflicts.len()));
        }
        Some(format!("drift: {}", parts.join(", ")))
    }
}

/// Expands a declaration into the mappings natmap must own.
///
/// One mapping per declared port that needs one: the node's tailnet address at
/// the declared port, forwarded to the address the service answers on. A port
/// whose service already answers on the tailnet address — or on every address —
/// is returned as covered instead. No port is ever allocated here: the port a
/// mapping uses is the port the declaration names.
pub fn declared_mappings(
    config: &DiscoveryConfig,
) -> Result<(Vec<DeclaredMapping>, Vec<CoveredPort>)> {
    let claims = config.port_claims();
    if claims.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let tailnet_ip = config
        .node
        .tailnet_ip
        .as_deref()
        .ok_or_else(|| eyre!("node.tailnet_ip is required to publish a declared port"))?;
    let tailnet_ip = IpAddr::from_str(tailnet_ip)
        .wrap_err_with(|| format!("node.tailnet_ip {tailnet_ip:?} is not an address"))?;

    let default_bind = config.default_bind_ip();
    let mut declared = Vec::new();
    let mut covered = Vec::new();
    let mut claimed_by: std::collections::HashMap<u16, &str> = std::collections::HashMap::new();

    for (service, claim) in claims {
        if claim.port == 0 {
            bail!("service {service:?} declares port 0, which it cannot own");
        }
        if let Some(previous) = claimed_by.insert(claim.port, service) {
            bail!(
                "port {} is claimed by both {previous:?} and {service:?}",
                claim.port
            );
        }

        let bind = claim.bind_ip.as_deref().unwrap_or(default_bind);
        let address = IpAddr::from_str(bind).wrap_err_with(|| {
            format!(
                "service {service:?} declares port {} on {bind:?}, which is not an address",
                claim.port
            )
        })?;
        if address.is_unspecified() || address == tailnet_ip {
            covered.push(CoveredPort {
                service: service.to_string(),
                port: claim.port,
                address: bind.to_string(),
            });
        } else {
            declared.push(DeclaredMapping {
                service: service.to_string(),
                host_ip: tailnet_ip.to_string(),
                port: claim.port,
                target_ip: bind.to_string(),
                proto: TransportProtocol::Tcp,
            });
        }
    }

    Ok((declared, covered))
}

/// Whether a live mapping answers on the address a declaration names.
///
/// A mapping bound to a wildcard answers on every address, so it holds the
/// declared one too. The address matters: the same port may be answered on
/// loopback by a mapping the daemon discovered from Docker and on the tailnet
/// address by the one this apply creates, and they do not collide.
fn holds(mapping: &LiveMapping, host_ip: &str, port: u16) -> bool {
    mapping.host_port == port
        && (mapping.host_ip == host_ip
            || IpAddr::from_str(&mapping.host_ip).is_ok_and(|ip| ip.is_unspecified()))
}

/// Diffs a declaration against the mappings the daemon holds.
///
/// `prune` decides what happens to a mapping this node created that the
/// declaration no longer names: with it the mapping is released, without it the
/// mapping is reported stale and kept — and a port moving to another service is
/// refused, because the release is what authorizes the transfer.
pub fn plan(
    declared: &[DeclaredMapping],
    covered: &[CoveredPort],
    live: &[LiveMapping],
    prune: bool,
) -> Plan {
    let mut live = live.to_vec();
    live.sort_by(|a, b| (a.host_port, &a.host_ip).cmp(&(b.host_port, &b.host_ip)));

    let mut plan = Plan {
        covered: covered.to_vec(),
        ..Default::default()
    };

    for wanted in declared {
        match live.iter().find(|m| holds(m, &wanted.host_ip, wanted.port)) {
            None => plan.create.push(wanted.clone()),
            Some(m) if m.owner.as_deref() == Some(wanted.service.as_str()) => {
                if m.host_ip != wanted.host_ip
                    || m.target_ip != wanted.target_ip
                    || m.target_port != wanted.port
                    || m.proto != wanted.proto
                {
                    plan.update.push(Update {
                        live: m.clone(),
                        wanted: wanted.clone(),
                    });
                }
            }
            Some(m) if prune && m.owner.is_some() => {
                plan.remove.push(m.clone());
                plan.create.push(wanted.clone());
            }
            Some(m) => plan.conflicts.push(Conflict {
                service: wanted.service.clone(),
                port: wanted.port,
                owner: m.owner.clone(),
                holder: m.id.clone(),
            }),
        }
    }

    for mapping in &live {
        if declared
            .iter()
            .any(|wanted| holds(mapping, &wanted.host_ip, wanted.port))
        {
            continue;
        }
        if mapping.owner.is_none() {
            plan.keep.push(mapping.clone());
        } else if prune {
            plan.remove.push(mapping.clone());
        } else {
            plan.stale.push(mapping.clone());
        }
    }

    plan.remove
        .sort_by(|a, b| (a.host_port, &a.host_ip).cmp(&(b.host_port, &b.host_ip)));
    plan.create
        .sort_by(|a, b| (a.port, &a.service).cmp(&(b.port, &b.service)));
    plan
}

/// Signals that a plan found drift to converge.
///
/// Not a failure: the caller maps it to a non-zero exit without printing an
/// error report, because the diff is already on stdout. `natmap plan` and
/// `natmap apply --dry-run` return it when the node does not match its
/// declaration.
#[derive(Debug)]
pub struct Drift;

impl std::fmt::Display for Drift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the node does not match its declaration")
    }
}

impl std::error::Error for Drift {}

/// Options for one [`converge`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ConvergeOptions {
    /// Release the mappings this node created that the declaration no longer names.
    pub prune: bool,
    /// Report the plan without touching the daemon.
    pub dry_run: bool,
}

/// What one converge did.
///
/// Refusing is a value, not an error: the plan that names the conflicting
/// services is what the caller has to show, and it was computed before
/// anything was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Converged {
    /// The declared mappings are in place. The plan says what changed.
    Applied(Plan),
    /// Nothing was written: a declared port is held by a mapping this apply
    /// will not remove. `plan.conflicts` says which, and names both services.
    Refused(Plan),
}

/// Brings the daemon's mappings in line with a declaration.
///
/// Refuses — writing nothing and returning [`Converged::Refused`] — when a
/// declared port is held by a mapping this apply will not remove. Otherwise
/// creates and updates the declared mappings, then releases the stale ones —
/// the reverse would drop a port before its replacement exists. The one
/// exception is a port this apply hands to another service: that release runs
/// first, because the daemon arbitrates the port and would answer 409 to the
/// create.
#[tracing::instrument(skip_all, fields(node = %config.node.name, prune = options.prune, dry_run = options.dry_run))]
pub async fn converge(
    apply: &dyn NatmapApply,
    config: &DiscoveryConfig,
    options: ConvergeOptions,
) -> Result<Converged> {
    let (declared, covered) = declared_mappings(config)?;
    let live: Vec<LiveMapping> = apply
        .list_mappings()
        .await
        .map_err(|e| eyre!("cannot read the daemon's mappings: {e}"))?
        .iter()
        .map(LiveMapping::from)
        .collect();

    let plan = plan(&declared, &covered, &live, options.prune);
    if !plan.conflicts.is_empty() {
        return Ok(Converged::Refused(plan));
    }
    if options.dry_run {
        return Ok(Converged::Applied(plan));
    }

    let (handing_over, releasing): (Vec<&LiveMapping>, Vec<&LiveMapping>) =
        plan.remove.iter().partition(|mapping| {
            plan.create
                .iter()
                .any(|wanted| wanted.port == mapping.host_port)
        });

    for mapping in handing_over {
        release(apply, mapping).await?;
    }
    for update in &plan.update {
        if update.live.host_ip != update.wanted.host_ip {
            release(apply, &update.live).await?;
        }
        create(apply, &update.wanted).await?;
    }
    for wanted in &plan.create {
        create(apply, wanted).await?;
    }
    for mapping in releasing {
        release(apply, mapping).await?;
    }

    Ok(Converged::Applied(plan))
}

/// Creates or rewrites one declared mapping, recording its owner.
///
/// The mapping is stored under a service key, not the service name: a
/// container id keys container mappings, and no container event may match a
/// mapping the declaration owns.
async fn create(apply: &dyn NatmapApply, wanted: &DeclaredMapping) -> Result<()> {
    let request = DockerAddMapRequest {
        host_ip: wanted.host_ip.clone(),
        host_port: wanted.port,
        container_port: wanted.port,
        target_ip: Some(wanted.target_ip.clone()),
        proto: wanted.proto,
        owner: Some(wanted.service.clone()),
    };
    apply
        .add_mapping(&service_mapping_key(&wanted.service), request)
        .await
        .map_err(|e| {
            eyre!(
                "cannot map port {} for {:?}: {e}",
                wanted.port,
                wanted.service
            )
        })?;
    Ok(())
}

/// Releases one mapping.
async fn release(apply: &dyn NatmapApply, mapping: &LiveMapping) -> Result<()> {
    apply
        .remove_mapping(&mapping.id, mapping.host_port)
        .await
        .map_err(|e| {
            eyre!(
                "cannot release port {} from {:?}: {e}",
                mapping.host_port,
                mapping.id
            )
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use lab_ops_lab_lib::discovery::Defaults;
    use lab_ops_lab_lib::discovery::NodeConfig;
    use lab_ops_lab_lib::discovery::PortClaim;
    use lab_ops_lab_lib::discovery::ServiceConfig;
    use lab_ops_lab_lib::discovery::ServiceType;

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
        live: Mutex<Vec<DockerPortMap>>,
        removed: Mutex<Vec<(String, u16)>>,
        order: Mutex<Vec<String>>,
    }

    impl FakeNatmap {
        fn fail_with(error: NatmapError) -> Self {
            FakeNatmap {
                fail_with: Mutex::new(Some(error)),
                ..Default::default()
            }
        }

        /// A fake daemon already holding these mappings.
        fn holding(mappings: Vec<DockerPortMap>) -> Self {
            FakeNatmap {
                live: Mutex::new(mappings),
                ..Default::default()
            }
        }

        fn mappings(&self) -> Vec<(String, DockerAddMapRequest)> {
            self.mappings.lock().unwrap().clone()
        }

        fn policy_routes(&self) -> Vec<(PolicyRouteConfig, bool)> {
            self.policy_routes.lock().unwrap().clone()
        }

        fn removed(&self) -> Vec<(String, u16)> {
            self.removed.lock().unwrap().clone()
        }

        fn order(&self) -> Vec<String> {
            self.order.lock().unwrap().clone()
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
                self.order
                    .lock()
                    .unwrap()
                    .push(format!("add:{}", req.host_port));
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
                    container_id: id,
                    container_name: String::new(),
                    rule_comment: String::new(),
                    owner: req.owner.clone(),
                })
            })
        }

        fn list_mappings(&self) -> ApplyFuture<'_, Result<Vec<DockerPortMap>, NatmapError>> {
            Box::pin(async move {
                if let Some(error) = self.fail_with.lock().unwrap().take() {
                    return Err(error);
                }
                Ok(self.live.lock().unwrap().clone())
            })
        }

        fn remove_mapping(&self, id: &str, port: u16) -> ApplyFuture<'_, Result<(), NatmapError>> {
            let id = id.to_string();
            Box::pin(async move {
                if let Some(error) = self.fail_with.lock().unwrap().take() {
                    return Err(error);
                }
                self.order.lock().unwrap().push(format!("remove:{port}"));
                self.removed.lock().unwrap().push((id, port));
                Ok(())
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

    // --- Declarative apply ---

    const TAILNET: &str = "100.64.0.17";

    fn claim(port: u16, bind_ip: Option<&str>) -> PortClaim {
        PortClaim {
            port,
            bind_ip: bind_ip.map(str::to_string),
        }
    }

    fn config(services: &[(&str, Vec<PortClaim>)]) -> DiscoveryConfig {
        let services = services
            .iter()
            .map(|(name, ports)| {
                (
                    name.to_string(),
                    ServiceConfig {
                        service_type: ServiceType::Docker,
                        match_cfg: None,
                        address: None,
                        bind_ip: None,
                        bind_interface: None,
                        ports: ports.clone(),
                        rproxylocal: vec![],
                        rproxyremote: vec![],
                        forwardlocal: vec![],
                        forwardremote: vec![],
                        extra: HashMap::new(),
                    },
                )
            })
            .collect();
        DiscoveryConfig {
            node: NodeConfig {
                name: "test-node".into(),
                tailnet_ip: Some(TAILNET.into()),
            },
            defaults: Defaults {
                bind_ip: Some("127.0.0.1".into()),
                ..Default::default()
            },
            services,
        }
    }

    fn addr(text: &str) -> std::net::SocketAddr {
        text.parse().unwrap()
    }

    fn live(
        id: &str,
        owner: Option<&str>,
        host_ip: &str,
        port: u16,
        target_ip: &str,
    ) -> LiveMapping {
        LiveMapping {
            id: id.to_string(),
            owner: owner.map(str::to_string),
            host_ip: host_ip.to_string(),
            host_port: port,
            target_ip: target_ip.to_string(),
            target_port: port,
            proto: TransportProtocol::Tcp,
        }
    }

    fn mapping(
        id: &str,
        owner: Option<&str>,
        host_ip: &str,
        port: u16,
        target_ip: &str,
    ) -> DockerPortMap {
        DockerPortMap {
            id: 1,
            request: DockerPortMapRequest {
                host_addr: addr(&format!("{host_ip}:{port}")),
                container_addr: addr(&format!("{target_ip}:{port}")),
                proto: TransportProtocol::Tcp,
            },
            container_id: id.to_string(),
            container_name: id.to_string(),
            rule_comment: String::new(),
            owner: owner.map(str::to_string),
        }
    }

    /// The plan a converge produced, whichever way it ended.
    fn plan_of(converged: Converged) -> Plan {
        match converged {
            Converged::Applied(plan) | Converged::Refused(plan) => plan,
        }
    }

    #[test]
    fn declared_mappings_names_one_mapping_per_declared_port() {
        let cfg = config(&[
            ("fazuh-uptime", vec![claim(3001, None)]),
            ("fazuh-registry", vec![claim(5000, Some("127.0.0.1"))]),
        ]);

        let (declared, covered) = declared_mappings(&cfg).unwrap();

        assert_eq!(
            declared,
            vec![
                DeclaredMapping {
                    service: "fazuh-uptime".into(),
                    host_ip: TAILNET.into(),
                    port: 3001,
                    target_ip: "127.0.0.1".into(),
                    proto: TransportProtocol::Tcp,
                },
                DeclaredMapping {
                    service: "fazuh-registry".into(),
                    host_ip: TAILNET.into(),
                    port: 5000,
                    target_ip: "127.0.0.1".into(),
                    proto: TransportProtocol::Tcp,
                },
            ]
        );
        assert!(covered.is_empty());
    }

    #[test]
    fn declared_mappings_uses_the_port_bind_ip_as_the_target() {
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, Some("10.0.0.5"))])]);

        let (declared, _) = declared_mappings(&cfg).unwrap();

        assert_eq!(declared[0].target_ip, "10.0.0.5");
        assert_eq!(declared[0].host_ip, TAILNET);
    }

    #[test]
    fn declared_mappings_covers_a_port_already_on_the_tailnet_address() {
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, Some(TAILNET))])]);

        let (declared, covered) = declared_mappings(&cfg).unwrap();

        assert!(declared.is_empty());
        assert_eq!(
            covered,
            vec![CoveredPort {
                service: "fazuh-uptime".into(),
                port: 3001,
                address: TAILNET.into(),
            }]
        );
    }

    #[test]
    fn declared_mappings_covers_a_port_bound_to_every_address() {
        let cfg = config(&[("fazuh-mail", vec![claim(443, Some("0.0.0.0"))])]);

        let (declared, covered) = declared_mappings(&cfg).unwrap();

        assert!(declared.is_empty());
        assert_eq!(covered[0].port, 443);
    }

    #[test]
    fn declared_mappings_refuses_a_port_claimed_by_two_services() {
        let cfg = config(&[
            ("fazuh-uptime", vec![claim(3001, None)]),
            ("fazuh-grafana", vec![claim(3001, None)]),
        ]);

        let error = declared_mappings(&cfg).unwrap_err().to_string();

        assert!(error.contains("\"fazuh-uptime\""), "{error}");
        assert!(error.contains("\"fazuh-grafana\""), "{error}");
        assert!(error.contains("3001"), "{error}");
    }

    #[test]
    fn declared_mappings_requires_a_tailnet_ip_when_a_port_is_declared() {
        let mut cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);
        cfg.node.tailnet_ip = None;

        let error = declared_mappings(&cfg).unwrap_err().to_string();

        assert!(error.contains("tailnet_ip"), "{error}");
    }

    #[test]
    fn declared_mappings_needs_no_tailnet_ip_without_a_declared_port() {
        let mut cfg = config(&[("fazuh-uptime", vec![])]);
        cfg.node.tailnet_ip = None;

        let (declared, covered) = declared_mappings(&cfg).unwrap();

        assert!(declared.is_empty());
        assert!(covered.is_empty());
    }

    #[test]
    fn plan_creates_a_mapping_the_daemon_does_not_hold() {
        let (declared, covered) =
            declared_mappings(&config(&[("fazuh-uptime", vec![claim(3001, None)])])).unwrap();

        let plan = plan(&declared, &covered, &[], false);

        assert_eq!(plan.create, declared);
        assert!(!plan.is_clean());
    }

    #[test]
    fn plan_is_clean_when_the_mapping_matches() {
        let (declared, covered) =
            declared_mappings(&config(&[("fazuh-uptime", vec![claim(3001, None)])])).unwrap();
        let live = [live(
            "fazuh-uptime",
            Some("fazuh-uptime"),
            TAILNET,
            3001,
            "127.0.0.1",
        )];

        let plan = plan(&declared, &covered, &live, false);

        assert!(plan.is_clean(), "{plan:?}");
    }

    #[test]
    fn plan_updates_a_mapping_whose_target_moved() {
        let (declared, covered) =
            declared_mappings(&config(&[("fazuh-uptime", vec![claim(3001, None)])])).unwrap();
        let live = [live(
            "fazuh-uptime",
            Some("fazuh-uptime"),
            TAILNET,
            3001,
            "10.0.0.5",
        )];

        let plan = plan(&declared, &covered, &live, false);

        assert_eq!(plan.update.len(), 1);
        assert_eq!(plan.update[0].wanted.target_ip, "127.0.0.1");
        assert!(plan.create.is_empty());
    }

    #[test]
    fn plan_refuses_a_port_owned_by_another_service() {
        let (declared, covered) =
            declared_mappings(&config(&[("fazuh-grafana", vec![claim(3001, None)])])).unwrap();
        let live = [live(
            "fazuh-uptime",
            Some("fazuh-uptime"),
            TAILNET,
            3001,
            "127.0.0.1",
        )];

        let plan = plan(&declared, &covered, &live, false);

        assert!(plan.create.is_empty());
        let message = plan.conflict_message().unwrap();
        assert!(message.contains("\"fazuh-uptime\""), "{message}");
        assert!(message.contains("\"fazuh-grafana\""), "{message}");
        assert!(message.contains("3001"), "{message}");
        assert!(message.contains("--prune"), "{message}");
    }

    #[test]
    fn plan_refuses_a_hand_added_mapping_on_a_declared_port() {
        let (declared, covered) =
            declared_mappings(&config(&[("fazuh-grafana", vec![claim(3001, None)])])).unwrap();
        let live = [live("hand", None, "0.0.0.0", 3001, "10.0.0.9")];

        let plan = plan(&declared, &covered, &live, false);

        let message = plan.conflict_message().unwrap();
        assert!(message.contains("did not create"), "{message}");
        assert!(message.contains("\"hand\""), "{message}");
    }

    #[test]
    fn plan_updates_a_mapping_whose_protocol_drifted() {
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);
        let (declared, covered) = declared_mappings(&cfg).unwrap();
        let mut udp = live(
            "fazuh-uptime",
            Some("fazuh-uptime"),
            TAILNET,
            3001,
            "127.0.0.1",
        );
        udp.proto = TransportProtocol::Udp;

        let plan = plan(&declared, &covered, &[udp], false);

        assert_eq!(plan.update.len(), 1, "{plan:?}");
    }

    #[test]
    fn plan_creates_beside_a_docker_mapping_on_the_same_port() {
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);
        let (declared, covered) = declared_mappings(&cfg).unwrap();
        // The daemon discovered the container's loopback publish: same port,
        // different address, no owner.
        let live = [live(
            "fazuh-uptime-1",
            None,
            "127.0.0.1",
            3001,
            "172.17.0.2",
        )];

        let plan = plan(&declared, &covered, &live, true);

        assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
        assert_eq!(plan.create.len(), 1);
        assert_eq!(plan.keep.len(), 1);
        assert!(plan.remove.is_empty());
    }

    #[test]
    fn plan_transfers_a_port_in_one_apply_when_pruning() {
        let (declared, covered) =
            declared_mappings(&config(&[("fazuh-grafana", vec![claim(3001, None)])])).unwrap();
        let live = [live(
            "fazuh-uptime",
            Some("fazuh-uptime"),
            TAILNET,
            3001,
            "127.0.0.1",
        )];

        let plan = plan(&declared, &covered, &live, true);

        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.create.len(), 1);
        assert_eq!(plan.remove.len(), 1);
        assert_eq!(plan.remove[0].owner.as_deref(), Some("fazuh-uptime"));
    }

    #[test]
    fn plan_never_prunes_a_hand_added_mapping_even_with_prune() {
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);
        let (declared, covered) = declared_mappings(&cfg).unwrap();
        let live = [live("hand", None, "0.0.0.0", 8080, "10.0.0.9")];

        let plan = plan(&declared, &covered, &live, true);

        assert!(plan.remove.is_empty(), "{plan:?}");
        assert_eq!(plan.keep.len(), 1);
        assert_eq!(plan.keep[0].id, "hand");
    }

    #[test]
    fn plan_marks_a_removed_services_mapping_stale_without_prune() {
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);
        let (declared, covered) = declared_mappings(&cfg).unwrap();
        let live = [live(
            "fazuh-registry",
            Some("fazuh-registry"),
            TAILNET,
            5000,
            "127.0.0.1",
        )];

        let plan = plan(&declared, &covered, &live, false);

        assert!(plan.remove.is_empty());
        assert_eq!(plan.stale.len(), 1);
        assert!(plan.drift_message().unwrap().contains("--prune"));
    }

    #[test]
    fn plan_prunes_a_removed_services_mapping() {
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);
        let (declared, covered) = declared_mappings(&cfg).unwrap();
        let live = [live(
            "fazuh-registry",
            Some("fazuh-registry"),
            TAILNET,
            5000,
            "127.0.0.1",
        )];

        let plan = plan(&declared, &covered, &live, true);

        assert_eq!(plan.remove.len(), 1);
        assert!(plan.stale.is_empty());
    }

    #[test]
    fn drift_message_is_none_when_converged() {
        let plan = Plan::default();

        assert_eq!(plan.drift_message(), None);
    }

    #[tokio::test]
    async fn converge_creates_the_declared_port_with_its_owner() {
        let fake = FakeNatmap::default();
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);

        converge(&fake, &cfg, ConvergeOptions::default())
            .await
            .unwrap();

        let mappings = fake.mappings();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, service_mapping_key("fazuh-uptime"));
        assert_eq!(mappings[0].1.host_port, 3001);
        assert_eq!(mappings[0].1.container_port, 3001);
        assert_eq!(mappings[0].1.host_ip, TAILNET);
        assert_eq!(mappings[0].1.target_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(mappings[0].1.owner.as_deref(), Some("fazuh-uptime"));
    }

    #[tokio::test]
    async fn converge_refuses_a_port_owned_by_another_service_and_writes_nothing() {
        let fake = FakeNatmap::holding(vec![mapping(
            "fazuh-uptime",
            Some("fazuh-uptime"),
            TAILNET,
            3001,
            "127.0.0.1",
        )]);
        let cfg = config(&[("fazuh-grafana", vec![claim(3001, None)])]);

        let converged = converge(&fake, &cfg, ConvergeOptions::default())
            .await
            .unwrap();
        let Converged::Refused(plan) = &converged else {
            panic!("a port owned by another service must be refused: {converged:?}");
        };
        let message = plan.conflict_message().unwrap();
        assert!(message.contains("\"fazuh-uptime\""), "{message}");
        assert!(message.contains("\"fazuh-grafana\""), "{message}");
        assert!(fake.mappings().is_empty());
        assert!(fake.removed().is_empty());
    }

    #[tokio::test]
    async fn converge_keeps_a_hand_added_mapping_when_pruning() {
        let fake = FakeNatmap::holding(vec![mapping("hand", None, "0.0.0.0", 8080, "10.0.0.9")]);
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);

        converge(
            &fake,
            &cfg,
            ConvergeOptions {
                prune: true,
                dry_run: false,
            },
        )
        .await
        .unwrap();

        assert!(fake.removed().is_empty(), "pruned a hand-added mapping");
        assert_eq!(fake.mappings().len(), 1);
    }

    #[tokio::test]
    async fn converge_releases_a_removed_service_in_the_same_apply() {
        let fake = FakeNatmap::holding(vec![mapping(
            "fazuh-uptime",
            Some("fazuh-uptime"),
            TAILNET,
            3001,
            "127.0.0.1",
        )]);
        let cfg = config(&[("fazuh-registry", vec![claim(5000, None)])]);

        converge(
            &fake,
            &cfg,
            ConvergeOptions {
                prune: true,
                dry_run: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(fake.removed(), vec![("fazuh-uptime".to_string(), 3001)]);
    }

    #[tokio::test]
    async fn converge_hands_a_port_over_by_releasing_it_before_creating() {
        let fake = FakeNatmap::holding(vec![mapping(
            "fazuh-uptime",
            Some("fazuh-uptime"),
            TAILNET,
            3001,
            "127.0.0.1",
        )]);
        let cfg = config(&[("fazuh-grafana", vec![claim(3001, None)])]);

        converge(
            &fake,
            &cfg,
            ConvergeOptions {
                prune: true,
                dry_run: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(fake.order(), vec!["remove:3001", "add:3001"]);
    }

    #[tokio::test]
    async fn converge_creates_before_releasing_a_stale_mapping() {
        let fake = FakeNatmap::holding(vec![mapping(
            "fazuh-registry",
            Some("fazuh-registry"),
            TAILNET,
            5000,
            "127.0.0.1",
        )]);
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);

        converge(
            &fake,
            &cfg,
            ConvergeOptions {
                prune: true,
                dry_run: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(fake.order(), vec!["add:3001", "remove:5000"]);
    }

    #[tokio::test]
    async fn converge_dry_run_writes_nothing_and_reports_drift() {
        let fake = FakeNatmap::default();
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);

        let plan = plan_of(
            converge(
                &fake,
                &cfg,
                ConvergeOptions {
                    prune: true,
                    dry_run: true,
                },
            )
            .await
            .unwrap(),
        );

        assert!(fake.mappings().is_empty());
        assert!(fake.removed().is_empty());
        assert_eq!(plan.create.len(), 1);
        assert!(plan.drift_message().is_some());
    }

    #[tokio::test]
    async fn converge_writes_nothing_when_already_converged() {
        let fake = FakeNatmap::holding(vec![mapping(
            "fazuh-uptime",
            Some("fazuh-uptime"),
            TAILNET,
            3001,
            "127.0.0.1",
        )]);
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);

        let plan = plan_of(
            converge(
                &fake,
                &cfg,
                ConvergeOptions {
                    prune: true,
                    dry_run: false,
                },
            )
            .await
            .unwrap(),
        );

        assert!(plan.is_clean());
        assert!(fake.order().is_empty(), "{:?}", fake.order());
    }

    #[tokio::test]
    async fn converge_fails_when_the_daemon_cannot_be_read() {
        let fake = FakeNatmap::fail_with(NatmapError::Connect("no socket".into()));
        let cfg = config(&[("fazuh-uptime", vec![claim(3001, None)])]);

        assert!(
            converge(&fake, &cfg, ConvergeOptions::default())
                .await
                .is_err()
        );
    }
}
