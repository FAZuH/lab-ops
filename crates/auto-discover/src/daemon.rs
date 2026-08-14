use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::bail;
use lab_ops_lab_lib::port::PortAssignments;
use lab_ops_natmap::client::NatmapClient;
use lab_ops_natmap::client::NatmapError;
use lab_ops_natmap::models::DockerAddMapRequest;
use lab_ops_natmap::models::DockerPortMap;
use lab_ops_natmap::models::PolicyRouteConfig;
use sha2::Digest;
use sha2::Sha256;

use crate::config::DiscoveryConfig;
use crate::config::ResolvedPortType;
use crate::config::ResolvedService;
use crate::config::ServiceType;
use crate::consul::ConsulClient;
use crate::consul::ConsulServiceRegistration;
use crate::consul::compute_generation_id;
use crate::docker::DockerClient;
use crate::model::ContainerInfo;

// --- Adapter seams ---

/// Minimal seam over the natmap daemon's Docker-mapping operations.
trait NatmapOps: Send + Sync {
    fn add_mapping(
        &self,
        container_id: &str,
        req: DockerAddMapRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DockerPortMap, NatmapError>> + Send + '_>>;

    fn policy_route(
        &self,
        config: PolicyRouteConfig,
        delete: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Option<PolicyRouteConfig>, NatmapError>> + Send + '_>>;
}

impl NatmapOps for NatmapClient {
    fn add_mapping(
        &self,
        container_id: &str,
        req: DockerAddMapRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DockerPortMap, NatmapError>> + Send + '_>> {
        let container_id = container_id.to_string();
        Box::pin(async move { NatmapClient::add_mapping(self, &container_id, req).await })
    }

    fn policy_route(
        &self,
        config: PolicyRouteConfig,
        delete: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Option<PolicyRouteConfig>, NatmapError>> + Send + '_>>
    {
        Box::pin(async move { NatmapClient::policy_route(self, config, delete).await })
    }
}

/// Minimal seam over the Consul agent's registration operations.
trait ConsulOps: Send + Sync {
    fn register_service(
        &self,
        registration: &ConsulServiceRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    fn deregister_services_by_container(
        &self,
        container_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<serde_json::Value>>> + Send + '_>>;

    fn deregister_stale_services(
        &self,
        server_name: &str,
        current_ids: &[String],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + '_>>;
}

impl ConsulOps for ConsulClient {
    fn register_service(
        &self,
        registration: &ConsulServiceRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let registration = registration.clone();
        Box::pin(async move { ConsulClient::register_service(self, &registration).await })
    }

    fn deregister_services_by_container(
        &self,
        container_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<serde_json::Value>>> + Send + '_>> {
        let container_id = container_id.to_string();
        Box::pin(async move {
            ConsulClient::deregister_services_by_container(self, &container_id).await
        })
    }

    fn deregister_stale_services(
        &self,
        server_name: &str,
        current_ids: &[String],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + '_>> {
        let server_name = server_name.to_string();
        let current_ids = current_ids.to_vec();
        Box::pin(async move {
            ConsulClient::deregister_stale_services(self, &server_name, &current_ids).await
        })
    }
}

/// Minimal seam over the Docker daemon's container inspection.
trait DockerOps: Send + Sync {
    fn list_running_containers(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ContainerInfo>>> + Send + '_>>;

    fn inspect_container(
        &self,
        container_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<ContainerInfo>> + Send + '_>>;
}

impl DockerOps for DockerClient {
    fn list_running_containers(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ContainerInfo>>> + Send + '_>> {
        Box::pin(async move { DockerClient::list_running_containers(self).await })
    }

    fn inspect_container(
        &self,
        container_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<ContainerInfo>> + Send + '_>> {
        let container_id = container_id.to_string();
        Box::pin(async move { DockerClient::inspect_container(self, &container_id).await })
    }
}

pub struct DiscoveryDaemon {
    config_path: PathBuf,
    consul: Arc<dyn ConsulOps>,
    natmap: Arc<dyn NatmapOps>,
    docker: Option<Arc<dyn DockerOps>>,
    state_dir: PathBuf,
}

// --- Sync service types ---

/// What a sync pass targets: either a running container or a local service.
enum ServiceTarget<'a> {
    Container {
        info: &'a ContainerInfo,
    },
    Local {
        local_ip: String,
        service_id_prefix: &'a str,
    },
}

/// Outcome of the port-decision step. `Skip` means the service gets no
/// mapping and no registration this pass (e.g. no free ports available).
enum PortDecision {
    Map {
        host_port: u16,
        container_port: u16,
        skip_natmap: bool,
    },
    Skip,
}

impl DiscoveryDaemon {
    pub fn new(config_path: PathBuf, state_dir: PathBuf) -> Self {
        DiscoveryDaemon {
            config_path,
            consul: Arc::new(ConsulClient::from_env()),
            natmap: Arc::new(NatmapClient::default_socket()),
            docker: None,
            state_dir,
        }
    }

    pub async fn sync(&self) -> Result<()> {
        let config =
            DiscoveryConfig::load(&self.config_path).wrap_err("failed to load discovery config")?;

        let server_name = config.node.name.clone();

        let config_hash = self.compute_config_hash(&config);
        let generation_id = compute_generation_id(&server_name, &config_hash);

        let docker: Arc<dyn DockerOps> = match &self.docker {
            Some(d) => d.clone(),
            None => Arc::new(DockerClient::new().wrap_err("Docker API error")?),
        };

        let containers = docker
            .list_running_containers()
            .await
            .wrap_err("Docker API error")?;

        let ports_path = self.state_dir.join("ports.json");
        let mut port_assignments = PortAssignments::load(&ports_path);
        let mut current_service_ids = Vec::new();
        let mut sync_errors: usize = 0;

        let all_resolved = config.resolve_all();

        for resolved in &all_resolved {
            let result = match resolved.service_type {
                ServiceType::Docker => {
                    let matching_containers: Vec<&ContainerInfo> = containers
                        .iter()
                        .filter(|c| container_matches(c, resolved))
                        .collect();

                    let mut ids = Vec::new();
                    for container in matching_containers {
                        let target = ServiceTarget::Container { info: container };
                        match self
                            .sync_service(
                                &target,
                                resolved,
                                &server_name,
                                &generation_id,
                                &mut port_assignments,
                            )
                            .await
                        {
                            Ok(Some(id)) => ids.push(id),
                            Ok(None) => {}
                            Err(e) => {
                                sync_errors += 1;
                                tracing::error!(
                                    service.id_prefix = %resolved.service_id_prefix,
                                    error = %e,
                                    "failed to sync service"
                                );
                            }
                        }
                    }
                    Ok(ids)
                }
                ServiceType::Local => {
                    if resolved.local_address.is_none() {
                        tracing::warn!(
                            service.id_prefix = %resolved.service_id_prefix,
                            "local service missing address, skipping"
                        );
                        Ok(vec![])
                    } else {
                        let target = ServiceTarget::Local {
                            local_ip: resolved
                                .local_address
                                .as_deref()
                                .unwrap_or("127.0.0.1")
                                .to_string(),
                            service_id_prefix: &resolved.service_id_prefix,
                        };
                        self.sync_service(
                            &target,
                            resolved,
                            &server_name,
                            &generation_id,
                            &mut port_assignments,
                        )
                        .await
                        .map(|id| id.into_iter().collect::<Vec<_>>())
                        .map_err(|e| {
                            sync_errors += 1;
                            tracing::error!(
                                service.id_prefix = %resolved.service_id_prefix,
                                error = %e,
                                "failed to sync local service"
                            );
                            e
                        })
                    }
                }
            };
            if let Ok(ids) = result {
                current_service_ids.extend(ids);
            }
        }

        port_assignments
            .save(&ports_path)
            .map_err(|e| tracing::warn!(error = %e, "failed to save port assignments"))
            .ok();

        // The sweep guard is fail-closed by design: a non-total failure with
        // zero registrations (e.g. some docker services absent plus one errored
        // local service) blocks the sweep, deferring legitimately-stale cleanup
        // to the next clean pass so a partial failure never wipes the catalog.
        let sweep_stale = should_sweep_stale(current_service_ids.len(), sync_errors);
        if sweep_stale {
            let _ = self
                .consul
                .deregister_stale_services(&server_name, &current_service_ids)
                .await;
        } else {
            tracing::warn!(
                services.errors = sync_errors,
                "sync failed completely; skipping stale-service deregistration to protect existing registrations"
            );
        }

        if !sweep_stale {
            bail!("sync failed: {sync_errors} service(s) errored, 0 registered");
        }

        tracing::info!(
            services.active = current_service_ids.len(),
            generation.id = %generation_id,
            "sync complete"
        );

        Ok(())
    }

    #[tracing::instrument(skip_all, fields(service.id_prefix = %resolved.service_id_prefix))]
    async fn sync_service(
        &self,
        target: &ServiceTarget<'_>,
        resolved: &ResolvedService,
        server_name: &str,
        generation_id: &str,
        port_assignments: &mut PortAssignments,
    ) -> Result<Option<String>> {
        // Resolve the target's identity first. For containers this is the
        // Consul IP; for local services it is the configured local address.
        let (container_id, consul_ip, local_ip) = match target {
            ServiceTarget::Container { info } => {
                let consul_ip = self.determine_consul_ip(resolved, &info.id).await?;
                (info.id.as_str(), consul_ip, None)
            }
            ServiceTarget::Local {
                local_ip,
                service_id_prefix,
            } => (
                *service_id_prefix,
                local_ip.clone(),
                Some(local_ip.as_str()),
            ),
        };

        let decision = self.decide_ports(target, resolved, port_assignments)?;
        let PortDecision::Map {
            host_port,
            container_port,
            skip_natmap,
        } = decision
        else {
            return Ok(None);
        };

        if !skip_natmap {
            let natmap_bind_ip = get_natmap_bind_ip(resolved);
            self.ensure_docker_mapping(
                container_id,
                natmap_bind_ip.as_deref(),
                host_port,
                container_port,
                resolved.protocol,
                local_ip,
            )
            .await?;
        }

        if let ResolvedPortType::ForwardRemote {
            preserve_src_ip: true,
            preserve_src_ip_gateway: Some(gateway),
            preserve_src_ip_src,
            ..
        } = &resolved.port_type
        {
            let natmap_bind_ip = get_natmap_bind_ip(resolved);
            let src_ip = preserve_src_ip_src
                .clone()
                .unwrap_or_else(|| natmap_bind_ip.clone().unwrap_or_else(|| consul_ip.clone()));
            self.natmap
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

        let registration = ConsulServiceRegistration::new(
            resolved,
            host_port,
            server_name,
            generation_id,
            container_id,
            &consul_ip,
        );

        self.consul
            .register_service(&registration)
            .await
            .wrap_err("Consul API error")?;

        Ok(Some(registration.id))
    }

    /// Decide which host port (if any) a service gets this pass.
    ///
    /// This is the internal port-decision seam: a future single port
    /// authority will attach here.
    fn decide_ports(
        &self,
        target: &ServiceTarget<'_>,
        resolved: &ResolvedService,
        port_assignments: &mut PortAssignments,
    ) -> Result<PortDecision> {
        let port_key = format!("{}-{}", resolved.service_id_prefix, resolved.container_port);

        match &resolved.port_type {
            ResolvedPortType::ForwardRemote { ext_ports, .. } => {
                let host_port = ext_ports[0];
                // ForwardRemote needs a local natmap mapping so traffic from the
                // proxy's DNAT rule can reach the target. Always attempt to
                // create one — if another target has this port mapped, the
                // natmap daemon will return 409 (handled gracefully as a warning
                // by the client). For local targets, skip the mapping when the
                // host port is already taken on the natmap bind IP.
                let skip_natmap = match target {
                    ServiceTarget::Container { .. } => false,
                    ServiceTarget::Local { .. } => {
                        let natmap_bind_ip = get_natmap_bind_ip(resolved);
                        let check_ip = natmap_bind_ip.as_deref().unwrap_or("0.0.0.0");
                        !lab_ops_lab_lib::port::is_port_free(format!("{check_ip}:{host_port}"))
                    }
                };
                Ok(PortDecision::Map {
                    host_port,
                    container_port: resolved.container_port,
                    skip_natmap,
                })
            }
            ResolvedPortType::ForwardLocal {
                bind_port: Some(bp),
            } => Ok(PortDecision::Map {
                host_port: *bp,
                container_port: resolved.container_port,
                skip_natmap: false,
            }),
            ResolvedPortType::ForwardLocal { bind_port: None } => Ok(allocate_port_decision(
                port_assignments,
                &port_key,
                resolved,
            )),
            ResolvedPortType::RProxyLocal { .. } | ResolvedPortType::RProxyRemote { .. } => {
                match target {
                    // RProxy local targets need no natmap mapping — the proxy
                    // talks to the local service directly.
                    ServiceTarget::Local { .. } => Ok(PortDecision::Map {
                        host_port: resolved.container_port,
                        container_port: resolved.container_port,
                        skip_natmap: true,
                    }),
                    // RProxy container targets get a dynamic natmap mapping.
                    ServiceTarget::Container { .. } => Ok(allocate_port_decision(
                        port_assignments,
                        &port_key,
                        resolved,
                    )),
                }
            }
        }
    }

    /// Handles container start events.
    ///
    /// Span fields: `container.id`, `event.action`, `compose.project`.
    #[tracing::instrument(skip_all, fields(
        container.id = %container_id,
        event.action = %action,
        compose.project = %compose_project
    ))]
    pub async fn handle_container_start(
        &self,
        container_id: &str,
        compose_project: &str,
        action: &str,
    ) -> Result<()> {
        tracing::debug!("handling container start");
        let config =
            DiscoveryConfig::load(&self.config_path).wrap_err("failed to load discovery config")?;

        let docker: Arc<dyn DockerOps> = match &self.docker {
            Some(d) => d.clone(),
            None => Arc::new(DockerClient::new().wrap_err("Docker API error")?),
        };
        let cinfo = docker.inspect_container(container_id).await?;

        let mut resolved_services = Vec::new();
        for res in config.resolve_all() {
            if res.service_type != ServiceType::Docker {
                continue;
            }
            // Container matching is unified with the sync path: project,
            // container name and regex all come from the inspected container.
            if container_matches(&cinfo, &res) {
                resolved_services.push(res);
            }
        }

        if resolved_services.is_empty() {
            tracing::debug!(
                container.id = %&container_id[..12.min(container_id.len())],
                compose.project = %compose_project,
                "no matching services for container"
            );
            return Ok(());
        }

        tracing::info!(
            services.count = resolved_services.len(),
            compose.project = %compose_project,
            "port bindings for project"
        );

        let server_name = config.node.name.clone();
        let config_hash = self.compute_config_hash(&config);
        let generation_id = compute_generation_id(&server_name, &config_hash);

        let ports_path = self.state_dir.join("ports.json");
        let mut port_assignments = PortAssignments::load(&ports_path);

        for resolved in &resolved_services {
            let target = ServiceTarget::Container { info: &cinfo };
            if let Err(e) = self
                .sync_service(
                    &target,
                    resolved,
                    &server_name,
                    &generation_id,
                    &mut port_assignments,
                )
                .await
            {
                tracing::error!(
                    service.id_prefix = %resolved.service_id_prefix,
                    container.id = %&container_id[..12.min(container_id.len())],
                    error = %e,
                    "failed to sync service for container"
                );
            }
        }

        port_assignments.save(&ports_path).ok();
        Ok(())
    }

    /// Handles container die events.
    ///
    /// Span fields: `container.id`, `event.action`.
    #[tracing::instrument(skip_all, fields(
        container.id = %container_id,
        event.action = "die"
    ))]
    pub async fn handle_container_die(&self, container_id: &str) -> Result<()> {
        tracing::debug!("handling container die");
        let services = self
            .consul
            .deregister_services_by_container(container_id)
            .await
            .wrap_err("Consul API error")?;

        for svc in &services {
            if let Some(meta) = svc.get("Meta").and_then(|v| v.as_object())
                && meta.get("preserve_src_ip").and_then(|v| v.as_str()) == Some("true")
                && let Some(gateway) = meta.get("preserve_src_ip_gateway").and_then(|v| v.as_str())
                && let Some(src_ip) = meta.get("preserve_src_ip_src").and_then(|v| v.as_str())
                && let Err(e) = self
                    .natmap
                    .policy_route(
                        PolicyRouteConfig {
                            src_ip: src_ip.to_string(),
                            via: gateway.to_string(),
                            table: 100,
                        },
                        true,
                    )
                    .await
            {
                tracing::warn!(error = %e, "failed to remove policy route");
            }
        }

        tracing::debug!(
            services.count = services.len(),
            container.id = %container_id,
            "deregistered services for container"
        );
        Ok(())
    }

    /// Adds a Docker port mapping via the natmap daemon, treating an existing
    /// mapping (409) or a missing container (404) as non-fatal.
    ///
    /// Span fields: `host.port`, `container.port`, `proto`.
    #[tracing::instrument(skip_all, fields(host.port = %host_port, container.port = %container_port, proto = %proto))]
    async fn ensure_docker_mapping(
        &self,
        container_id: &str,
        host_ip: Option<&str>,
        host_port: u16,
        container_port: u16,
        proto: lab_ops_lab_lib::TransportProtocol,
        target_ip: Option<&str>,
    ) -> Result<()> {
        let req = DockerAddMapRequest {
            host_ip: host_ip.unwrap_or("0.0.0.0").to_string(),
            host_port,
            container_port,
            target_ip: target_ip.map(|s| s.to_string()),
            proto,
        };
        match self.natmap.add_mapping(container_id, req).await {
            Ok(_) => Ok(()),
            Err(NatmapError::Conflict(e)) => {
                tracing::warn!(error = %e, "natmap mapping already exists (409), continuing");
                Ok(())
            }
            Err(NatmapError::NotFound(e)) => {
                tracing::warn!(error = %e, "container not found, continuing");
                Ok(())
            }
            Err(e) => Err(e).wrap_err("natmap command failed"),
        }
    }

    async fn determine_consul_ip(
        &self,
        resolved: &ResolvedService,
        container_id: &str,
    ) -> Result<String> {
        if let Some(ref ip) = resolved.bind_ip {
            return Ok(ip.clone());
        }
        if let Some(ref iface) = resolved.bind_interface
            && let Some(ip) = resolve_interface_ip(iface)
        {
            return Ok(ip);
        }
        crate::docker::get_container_ip(container_id)
            .map(|ip| ip.to_string())
            .wrap_err("failed to get container IP")
    }

    fn compute_config_hash(&self, config: &DiscoveryConfig) -> String {
        let yaml = serde_yaml::to_string(config).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(yaml.as_bytes());
        let result = hasher.finalize();
        hex::encode(&result[..16])
    }
}

/// Allocate a dynamic host port for a service, or report `Skip` when the
/// port range is exhausted.
fn allocate_port_decision(
    port_assignments: &mut PortAssignments,
    port_key: &str,
    resolved: &ResolvedService,
) -> PortDecision {
    match port_assignments.get_or_allocate(port_key) {
        Some(host_port) => PortDecision::Map {
            host_port,
            container_port: resolved.container_port,
            skip_natmap: false,
        },
        None => {
            tracing::warn!(
                service.id_prefix = %resolved.service_id_prefix,
                "no free ports for service"
            );
            PortDecision::Skip
        }
    }
}

/// Decide whether the stale-service sweep should run after a sync pass.
///
/// A sync where every service errored and none registered (a total failure,
/// e.g. the natmap socket was not ready at startup) must not sweep, because
/// sweeping would deregister previously-registered services that merely
/// failed to re-register this pass.
fn should_sweep_stale(registered: usize, errors: usize) -> bool {
    !(errors > 0 && registered == 0)
}

/// Match a container against a resolved service definition.
///
/// Checks project, container name, and container_regex match criteria.
fn container_matches(container: &ContainerInfo, resolved: &ResolvedService) -> bool {
    let mc = match &resolved.match_cfg {
        Some(m) => m,
        None => return true,
    };

    if let Some(proj) = &mc.project
        && container.compose_project.as_deref() != Some(proj.as_str())
    {
        return false;
    }
    if let Some(c) = &mc.container
        && container.name != *c
    {
        return false;
    }
    if let Some(cr) = &mc.container_regex {
        if let Ok(re) = regex::Regex::new(cr) {
            if !re.is_match(&container.name) {
                return false;
            }
        } else {
            tracing::warn!("Invalid container_regex: {}", cr);
            return false;
        }
    }

    true
}

/// Resolve natmap bind IP from config.
fn get_natmap_bind_ip(resolved: &ResolvedService) -> Option<String> {
    if let Some(ref ip) = resolved.bind_ip {
        return Some(ip.clone());
    }
    if let Some(ref iface) = resolved.bind_interface {
        if let Some(ip) = resolve_interface_ip(iface) {
            return Some(ip);
        }
        tracing::warn!(
            "bind_interface {} configured but could not resolve IP (interface may be down)",
            iface
        );
    }
    None
}

/// Resolve the first IPv4 address on a network interface via `ip -j -4 addr show`.
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
    use std::collections::HashMap;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    use lab_ops_lab_lib::TransportProtocol;
    use lab_ops_natmap::models::DockerAddMapRequest;
    use lab_ops_natmap::models::DockerPortMap;
    use lab_ops_natmap::models::DockerPortMapRequest;
    use lab_ops_natmap::models::PolicyRouteConfig;
    use tracing_test::traced_test;

    use super::*;

    // ── should_sweep_stale ──

    #[test]
    fn sweep_runs_when_no_errors_and_nothing_registered() {
        assert!(should_sweep_stale(0, 0));
    }

    #[test]
    fn sweep_runs_when_sync_healthy() {
        assert!(should_sweep_stale(3, 0));
    }

    #[test]
    fn sweep_skipped_on_total_failure() {
        assert!(!should_sweep_stale(0, 5));
    }

    #[test]
    fn sweep_runs_on_partial_failure() {
        assert!(should_sweep_stale(2, 3));
    }

    #[tokio::test]
    #[traced_test]
    async fn handle_container_start_span_fields() {
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon =
            DiscoveryDaemon::new(PathBuf::from("/dev/null"), temp_dir.path().to_path_buf());

        let _ = daemon
            .handle_container_start("123456789012", "my_project", "start")
            .await;

        assert!(logs_contain("container.id=123456789012"));
        assert!(logs_contain("event.action=start"));
    }

    #[tokio::test]
    #[traced_test]
    async fn handle_container_die_logs_deregister() {
        let temp_dir = tempfile::tempdir().unwrap();
        let daemon =
            DiscoveryDaemon::new(PathBuf::from("/dev/null"), temp_dir.path().to_path_buf());

        let _ = daemon.handle_container_die("123456789012").await;

        // This will log a debug event or an error, but the span has the fields
        assert!(logs_contain("container.id=123456789012"));
    }

    // --- Fake adapters ---

    #[derive(Default)]
    struct FakeNatmap {
        add_mappings: Mutex<Vec<(String, DockerAddMapRequest)>>,
        policy_routes: Mutex<Vec<(PolicyRouteConfig, bool)>>,
        fail_add_mapping: AtomicBool,
        conflict_add_mapping: AtomicBool,
    }

    impl FakeNatmap {
        fn mappings(&self) -> Vec<(String, DockerAddMapRequest)> {
            self.add_mappings.lock().unwrap().clone()
        }

        fn policy_routes(&self) -> Vec<(PolicyRouteConfig, bool)> {
            self.policy_routes.lock().unwrap().clone()
        }
    }

    impl NatmapOps for FakeNatmap {
        fn add_mapping(
            &self,
            container_id: &str,
            req: DockerAddMapRequest,
        ) -> Pin<Box<dyn Future<Output = Result<DockerPortMap, NatmapError>> + Send + '_>> {
            let container_id = container_id.to_string();
            Box::pin(async move {
                if self.fail_add_mapping.load(Ordering::SeqCst) {
                    return Err(NatmapError::Internal("fake add_mapping failure".into()));
                }
                if self.conflict_add_mapping.load(Ordering::SeqCst) {
                    return Err(NatmapError::Conflict("fake conflict".into()));
                }
                self.add_mappings.lock().unwrap().push((container_id, req));
                Ok(DockerPortMap {
                    id: 1,
                    request: DockerPortMapRequest {
                        host_addr: "0.0.0.0:0".parse().unwrap(),
                        container_addr: "0.0.0.0:0".parse().unwrap(),
                        proto: TransportProtocol::Tcp,
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
        ) -> Pin<Box<dyn Future<Output = Result<Option<PolicyRouteConfig>, NatmapError>> + Send + '_>>
        {
            Box::pin(async move {
                self.policy_routes.lock().unwrap().push((config, delete));
                Ok(None)
            })
        }
    }

    #[derive(Default)]
    struct FakeConsul {
        registrations: Mutex<Vec<ConsulServiceRegistration>>,
    }

    impl FakeConsul {
        fn registrations(&self) -> Vec<ConsulServiceRegistration> {
            self.registrations.lock().unwrap().clone()
        }
    }

    impl ConsulOps for FakeConsul {
        fn register_service(
            &self,
            registration: &ConsulServiceRegistration,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
            let registration = registration.clone();
            Box::pin(async move {
                self.registrations.lock().unwrap().push(registration);
                Ok(())
            })
        }

        fn deregister_services_by_container(
            &self,
            _container_id: &str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<serde_json::Value>>> + Send + '_>> {
            Box::pin(async move { Ok(vec![]) })
        }

        fn deregister_stale_services(
            &self,
            _server_name: &str,
            _current_ids: &[String],
        ) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + '_>> {
            Box::pin(async move { Ok(vec![]) })
        }
    }

    struct FakeDocker {
        running: Vec<ContainerInfo>,
        inspect_result: Option<ContainerInfo>,
    }

    impl FakeDocker {
        fn with_running(containers: Vec<ContainerInfo>) -> Self {
            FakeDocker {
                running: containers,
                inspect_result: None,
            }
        }

        fn with_inspect(result: ContainerInfo) -> Self {
            FakeDocker {
                running: vec![],
                inspect_result: Some(result),
            }
        }
    }

    impl DockerOps for FakeDocker {
        fn list_running_containers(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ContainerInfo>>> + Send + '_>> {
            Box::pin(async move { Ok(self.running.clone()) })
        }

        fn inspect_container(
            &self,
            _container_id: &str,
        ) -> Pin<Box<dyn Future<Output = Result<ContainerInfo>> + Send + '_>> {
            Box::pin(async move {
                self.inspect_result
                    .clone()
                    .ok_or_else(|| color_eyre::eyre::eyre!("no fake container"))
            })
        }
    }

    // --- Fixtures ---

    fn make_daemon(
        dir: &tempfile::TempDir,
        natmap: Arc<dyn NatmapOps>,
        consul: Arc<dyn ConsulOps>,
        docker: Arc<dyn DockerOps>,
    ) -> DiscoveryDaemon {
        DiscoveryDaemon {
            config_path: dir.path().join("discovery.yaml"),
            consul,
            natmap,
            docker: Some(docker),
            state_dir: dir.path().to_path_buf(),
        }
    }

    fn make_container_info(id: &str, name: &str, project: Option<&str>) -> ContainerInfo {
        ContainerInfo {
            id: id.to_string(),
            name: name.to_string(),
            compose_project: project.map(str::to_string),
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
            service_type: ServiceType::Docker,
            match_cfg: None,
            local_address: None,
            container_port,
            proxy_on: None,
            bind_ip: None,
            bind_interface: None,
            protocol: TransportProtocol::Tcp,
            port_type,
            extra: HashMap::new(),
        }
    }

    fn make_forward_remote(
        ext_ports: Vec<u16>,
        preserve_src_ip: bool,
        gateway: Option<&str>,
        src: Option<&str>,
    ) -> ResolvedPortType {
        ResolvedPortType::ForwardRemote {
            ext_ip: "203.0.113.50".into(),
            ext_ports,
            hairpin: false,
            proxy_on: None,
            preserve_src_ip,
            preserve_src_ip_gateway: gateway.map(str::to_string),
            preserve_src_ip_src: src.map(str::to_string),
        }
    }

    fn make_container_target(info: &ContainerInfo) -> ServiceTarget<'_> {
        ServiceTarget::Container { info }
    }

    fn make_local_target(prefix: &'static str, local_ip: &str) -> ServiceTarget<'static> {
        ServiceTarget::Local {
            local_ip: local_ip.to_string(),
            service_id_prefix: prefix,
        }
    }

    // --- sync_service ---

    #[tokio::test]
    async fn sync_service_docker_target_maps_and_registers() {
        let dir = tempfile::tempdir().unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        let info = make_container_info("abc123def456", "web", Some("myproj"));
        let mut resolved = make_resolved(
            "web",
            8080,
            ResolvedPortType::ForwardLocal {
                bind_port: Some(38080),
            },
        );
        resolved.bind_ip = Some("10.0.0.5".into());

        let mut port_assignments = PortAssignments::default();
        let id = daemon
            .sync_service(
                &make_container_target(&info),
                &resolved,
                "test-node",
                "gen-1",
                &mut port_assignments,
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(id, "test-node-web-38080");
        let mappings = natmap.mappings();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, "abc123def456");
        assert_eq!(mappings[0].1.host_port, 38080);
        assert_eq!(mappings[0].1.container_port, 8080);
        assert_eq!(mappings[0].1.target_ip, None);
        let regs = consul.registrations();
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].address, "10.0.0.5");
        assert_eq!(regs[0].meta.get("container_id").unwrap(), "abc123def456");
        assert_eq!(regs[0].meta.get("server_name").unwrap(), "test-node");
    }

    #[tokio::test]
    async fn sync_service_local_target_maps_with_prefix_and_local_ip() {
        let dir = tempfile::tempdir().unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        let resolved = make_resolved(
            "loc",
            9090,
            ResolvedPortType::ForwardLocal {
                bind_port: Some(39090),
            },
        );

        let mut port_assignments = PortAssignments::default();
        let id = daemon
            .sync_service(
                &make_local_target("loc", "10.0.0.99"),
                &resolved,
                "test-node",
                "gen-1",
                &mut port_assignments,
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(id, "test-node-loc-39090");
        let mappings = natmap.mappings();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, "loc");
        assert_eq!(mappings[0].1.host_port, 39090);
        assert_eq!(mappings[0].1.container_port, 9090);
        assert_eq!(mappings[0].1.target_ip.as_deref(), Some("10.0.0.99"));
        let regs = consul.registrations();
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].address, "10.0.0.99");
        assert_eq!(regs[0].meta.get("container_id").unwrap(), "loc");
    }

    #[tokio::test]
    async fn sync_service_docker_forward_remote_always_maps() {
        let dir = tempfile::tempdir().unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        let info = make_container_info("abc123def456", "web", None);
        let mut resolved = make_resolved(
            "web",
            30000,
            make_forward_remote(vec![30000], false, None, None),
        );
        resolved.bind_ip = Some("10.0.0.5".into());

        let mut port_assignments = PortAssignments::default();
        let id = daemon
            .sync_service(
                &make_container_target(&info),
                &resolved,
                "test-node",
                "gen-1",
                &mut port_assignments,
            )
            .await
            .unwrap()
            .unwrap();

        assert!(!id.is_empty());
        let mappings = natmap.mappings();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].1.host_port, 30000);
        assert_eq!(mappings[0].1.target_ip, None);
    }

    #[tokio::test]
    async fn sync_service_local_forward_remote_maps_when_port_free() {
        let dir = tempfile::tempdir().unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        let resolved = make_resolved(
            "loc",
            39002,
            make_forward_remote(vec![39002], false, None, None),
        );

        let mut port_assignments = PortAssignments::default();
        let id = daemon
            .sync_service(
                &make_local_target("loc", "10.0.0.99"),
                &resolved,
                "test-node",
                "gen-1",
                &mut port_assignments,
            )
            .await
            .unwrap()
            .unwrap();

        assert!(!id.is_empty());
        let mappings = natmap.mappings();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].1.host_port, 39002);
        assert_eq!(mappings[0].1.target_ip.as_deref(), Some("10.0.0.99"));
    }

    #[tokio::test]
    async fn sync_service_local_forward_remote_skips_mapping_when_port_taken() {
        let _held = TcpListener::bind("0.0.0.0:39003").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        let resolved = make_resolved(
            "loc",
            39003,
            make_forward_remote(vec![39003], false, None, None),
        );

        let mut port_assignments = PortAssignments::default();
        let id = daemon
            .sync_service(
                &make_local_target("loc", "10.0.0.99"),
                &resolved,
                "test-node",
                "gen-1",
                &mut port_assignments,
            )
            .await
            .unwrap()
            .unwrap();

        assert!(!id.is_empty());
        assert!(natmap.mappings().is_empty());
        let regs = consul.registrations();
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].port, 39003);
    }

    #[tokio::test]
    async fn sync_service_local_rproxy_skips_natmap() {
        let dir = tempfile::tempdir().unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        let resolved = make_resolved(
            "web",
            8080,
            ResolvedPortType::RProxyLocal {
                template: "web.ctmpl".into(),
                domains: vec!["web.example.com".into()],
                proxy_on: None,
                proxy_ip: None,
            },
        );

        let mut port_assignments = PortAssignments::default();
        let id = daemon
            .sync_service(
                &make_local_target("web", "10.0.0.99"),
                &resolved,
                "test-node",
                "gen-1",
                &mut port_assignments,
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(id, "test-node-web-example-com-8080");
        assert!(natmap.mappings().is_empty());
        let regs = consul.registrations();
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].port, 8080);
        assert_eq!(regs[0].meta.get("container_id").unwrap(), "web");
    }

    #[tokio::test]
    async fn sync_service_docker_rproxy_allocates_and_maps() {
        let dir = tempfile::tempdir().unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        let info = make_container_info("abc123def456", "web", None);
        let mut resolved = make_resolved(
            "web",
            8080,
            ResolvedPortType::RProxyLocal {
                template: "web.ctmpl".into(),
                domains: vec!["web.example.com".into()],
                proxy_on: None,
                proxy_ip: None,
            },
        );
        resolved.bind_ip = Some("10.0.0.5".into());

        let mut port_assignments = PortAssignments::default();
        let id = daemon
            .sync_service(
                &make_container_target(&info),
                &resolved,
                "test-node",
                "gen-1",
                &mut port_assignments,
            )
            .await
            .unwrap()
            .unwrap();

        assert!(!id.is_empty());
        let mappings = natmap.mappings();
        assert_eq!(mappings.len(), 1);
        assert!((32768..=61000).contains(&mappings[0].1.host_port));
        let regs = consul.registrations();
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].port, mappings[0].1.host_port);
        assert_eq!(regs[0].address, "10.0.0.5");
    }

    #[tokio::test]
    async fn sync_service_policy_route_uses_explicit_src_ip() {
        let dir = tempfile::tempdir().unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        let info = make_container_info("abc123def456", "web", None);
        let mut resolved = make_resolved(
            "web",
            30001,
            make_forward_remote(vec![30001], true, Some("192.168.1.1"), Some("10.9.9.9")),
        );
        resolved.bind_ip = Some("10.0.0.5".into());

        let mut port_assignments = PortAssignments::default();
        let id = daemon
            .sync_service(
                &make_container_target(&info),
                &resolved,
                "test-node",
                "gen-1",
                &mut port_assignments,
            )
            .await
            .unwrap()
            .unwrap();

        assert!(!id.is_empty());
        let routes = natmap.policy_routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].0.src_ip, "10.9.9.9");
        assert_eq!(routes[0].0.via, "192.168.1.1");
        assert_eq!(routes[0].0.table, 100);
        assert!(!routes[0].1);
    }

    #[tokio::test]
    async fn sync_service_policy_route_local_falls_back_to_local_ip() {
        let dir = tempfile::tempdir().unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        let resolved = make_resolved(
            "loc",
            30002,
            make_forward_remote(vec![30002], true, Some("192.168.1.1"), None),
        );

        let mut port_assignments = PortAssignments::default();
        let id = daemon
            .sync_service(
                &make_local_target("loc", "10.0.0.99"),
                &resolved,
                "test-node",
                "gen-1",
                &mut port_assignments,
            )
            .await
            .unwrap()
            .unwrap();

        assert!(!id.is_empty());
        let routes = natmap.policy_routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].0.src_ip, "10.0.0.99");
    }

    #[tokio::test]
    async fn sync_service_propagates_mapping_failure() {
        let dir = tempfile::tempdir().unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        natmap.fail_add_mapping.store(true, Ordering::SeqCst);
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        let info = make_container_info("abc123def456", "web", None);
        let mut resolved = make_resolved(
            "web",
            8080,
            ResolvedPortType::ForwardLocal {
                bind_port: Some(38080),
            },
        );
        resolved.bind_ip = Some("10.0.0.5".into());

        let mut port_assignments = PortAssignments::default();
        let result = daemon
            .sync_service(
                &make_container_target(&info),
                &resolved,
                "test-node",
                "gen-1",
                &mut port_assignments,
            )
            .await;

        assert!(result.is_err());
        assert!(consul.registrations().is_empty());
    }

    #[tokio::test]
    #[traced_test]
    async fn sync_service_mapping_conflict_is_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        natmap.conflict_add_mapping.store(true, Ordering::SeqCst);
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        let info = make_container_info("abc123def456", "web", None);
        let mut resolved = make_resolved(
            "web",
            8080,
            ResolvedPortType::ForwardLocal {
                bind_port: Some(38080),
            },
        );
        resolved.bind_ip = Some("10.0.0.5".into());

        let mut port_assignments = PortAssignments::default();
        let id = daemon
            .sync_service(
                &make_container_target(&info),
                &resolved,
                "test-node",
                "gen-1",
                &mut port_assignments,
            )
            .await
            .unwrap()
            .unwrap();

        assert!(!id.is_empty());
        assert_eq!(consul.registrations().len(), 1);
        assert!(logs_contain("natmap mapping already exists"));
    }

    // --- Entry points ---

    #[tokio::test]
    async fn sync_command_path_syncs_local_service() {
        let dir = tempfile::tempdir().unwrap();
        let config = r#"
node:
  name: test-node
services:
  loc:
    type: local
    address: 10.0.0.99
    forwardlocal:
      - port: 9090
        bind_port: 39090
"#;
        std::fs::write(dir.path().join("discovery.yaml"), config).unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(vec![])),
        );

        daemon.sync().await.unwrap();

        let mappings = natmap.mappings();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, "loc");
        assert_eq!(mappings[0].1.target_ip.as_deref(), Some("10.0.0.99"));
        let regs = consul.registrations();
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].meta.get("container_id").unwrap(), "loc");
    }

    #[tokio::test]
    async fn sync_command_path_syncs_matching_containers() {
        let dir = tempfile::tempdir().unwrap();
        let config = r#"
node:
  name: test-node
services:
  web:
    type: docker
    bind_ip: 10.0.0.5
    match:
      project: myproj
    forwardlocal:
      - port: 8080
        bind_port: 38080
"#;
        std::fs::write(dir.path().join("discovery.yaml"), config).unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let running = vec![make_container_info("abc123def456", "web", Some("myproj"))];
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_running(running)),
        );

        daemon.sync().await.unwrap();

        let mappings = natmap.mappings();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, "abc123def456");
        assert_eq!(mappings[0].1.host_port, 38080);
        let regs = consul.registrations();
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].meta.get("container_id").unwrap(), "abc123def456");
    }

    #[tokio::test]
    async fn handle_container_start_syncs_matching_service() {
        let dir = tempfile::tempdir().unwrap();
        let config = r#"
node:
  name: test-node
services:
  web:
    type: docker
    bind_ip: 10.0.0.5
    match:
      project: myproj
    forwardlocal:
      - port: 8080
        bind_port: 38080
"#;
        std::fs::write(dir.path().join("discovery.yaml"), config).unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        let cinfo = make_container_info("abc123def456", "web", Some("myproj"));
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_inspect(cinfo)),
        );

        daemon
            .handle_container_start("abc123def456", "myproj", "start")
            .await
            .unwrap();

        let mappings = natmap.mappings();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, "abc123def456");
        assert_eq!(mappings[0].1.host_port, 38080);
        let regs = consul.registrations();
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].address, "10.0.0.5");
    }

    #[tokio::test]
    async fn handle_container_start_requires_inspect_project_for_match() {
        let dir = tempfile::tempdir().unwrap();
        let config = r#"
node:
  name: test-node
services:
  web:
    type: docker
    bind_ip: 10.0.0.5
    match:
      project: myproj
    forwardlocal:
      - port: 8080
        bind_port: 38080
"#;
        std::fs::write(dir.path().join("discovery.yaml"), config).unwrap();
        let natmap = Arc::new(FakeNatmap::default());
        let consul = Arc::new(FakeConsul::default());
        // After matching unification the container's own project (from
        // inspect) must match; the event's compose_project alone is not enough.
        let cinfo = make_container_info("abc123def456", "web", None);
        let daemon = make_daemon(
            &dir,
            natmap.clone(),
            consul.clone(),
            Arc::new(FakeDocker::with_inspect(cinfo)),
        );

        daemon
            .handle_container_start("abc123def456", "myproj", "start")
            .await
            .unwrap();

        assert!(natmap.mappings().is_empty());
        assert!(consul.registrations().is_empty());
    }
}
