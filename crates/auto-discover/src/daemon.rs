use std::path::PathBuf;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use lab_ops_lab_lib::port::PortAssignments;
use lab_ops_natmap::client::NatmapClient;
use lab_ops_natmap::client::NatmapError;
use lab_ops_natmap::models::DockerAddMapRequest;
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

pub struct DiscoveryDaemon {
    config_path: PathBuf,
    consul: ConsulClient,
    natmap: NatmapClient,
    state_dir: PathBuf,
}

impl DiscoveryDaemon {
    pub fn new(config_path: PathBuf, state_dir: PathBuf) -> Self {
        DiscoveryDaemon {
            config_path,
            consul: ConsulClient::from_env(),
            natmap: NatmapClient::default_socket(),
            state_dir,
        }
    }

    pub async fn sync(&self) -> Result<()> {
        let config =
            DiscoveryConfig::load(&self.config_path).wrap_err("failed to load discovery config")?;

        let server_name = config.node.name.clone();

        let config_hash = self.compute_config_hash(&config);
        let generation_id = compute_generation_id(&server_name, &config_hash);

        let docker = DockerClient::new().wrap_err("Docker API error")?;

        let containers = docker
            .list_running_containers()
            .await
            .wrap_err("Docker API error")?;

        let ports_path = self.state_dir.join("ports.json");
        let mut port_assignments = PortAssignments::load(&ports_path);
        let mut current_service_ids = Vec::new();

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
                        match self
                            .sync_docker(
                                resolved,
                                container,
                                &server_name,
                                &generation_id,
                                &mut port_assignments,
                            )
                            .await
                        {
                            Ok(Some(id)) => ids.push(id),
                            Ok(None) => {}
                            Err(e) => {
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
                        self.sync_local(
                            resolved,
                            &server_name,
                            &generation_id,
                            &mut port_assignments,
                        )
                        .await
                        .map(|id| id.into_iter().collect::<Vec<_>>())
                        .map_err(|e| {
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

        let _ = self
            .consul
            .deregister_stale_services(&server_name, &current_service_ids)
            .await;

        tracing::info!(
            services.active = current_service_ids.len(),
            generation.id = %generation_id,
            "sync complete"
        );

        Ok(())
    }

    async fn sync_docker(
        &self,
        resolved: &ResolvedService,
        container: &ContainerInfo,
        server_name: &str,
        generation_id: &str,
        port_assignments: &mut PortAssignments,
    ) -> Result<Option<String>> {
        let consul_ip = self.determine_consul_ip(resolved, &container.id).await?;
        let natmap_bind_ip = get_natmap_bind_ip(resolved);

        let port_key = format!("{}-{}", resolved.service_id_prefix, resolved.container_port);
        let (host_port, skip_natmap) = match &resolved.port_type {
            ResolvedPortType::ForwardRemote { ext_ports, .. } => {
                let p = ext_ports[0];
                // ForwardRemote needs a local natmap mapping so traffic from the
                // proxy's DNAT rule can reach the container. Always attempt to
                // create one — if another container has this port mapped, the
                // natmap daemon will return 409 (handled gracefully as a warning
                // by the client).
                (p, false)
            }
            ResolvedPortType::ForwardLocal {
                bind_port: Some(bp),
            } => (*bp, false),
            _ => match port_assignments.get_or_allocate(&port_key) {
                Some(p) => (p, false),
                None => {
                    tracing::warn!(service.id_prefix = %resolved.service_id_prefix, "no free ports for service");
                    return Ok(None);
                }
            },
        };

        if !skip_natmap {
            self.ensure_docker_mapping(
                &container.id,
                natmap_bind_ip.as_deref(),
                host_port,
                resolved.container_port,
                resolved.protocol,
                None,
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
            &container.id,
            &consul_ip,
        );

        self.consul
            .register_service(&registration)
            .await
            .wrap_err("Consul API error")?;

        Ok(Some(registration.id))
    }

    async fn sync_local(
        &self,
        resolved: &ResolvedService,
        server_name: &str,
        generation_id: &str,
        port_assignments: &mut PortAssignments,
    ) -> Result<Option<String>> {
        let local_ip = resolved.local_address.as_deref().unwrap_or("127.0.0.1");

        let port_key = format!("{}-{}", resolved.service_id_prefix, resolved.container_port);
        let (host_port, skip_natmap) = match &resolved.port_type {
            ResolvedPortType::ForwardRemote { ext_ports, .. } => {
                let p = ext_ports[0];
                let natmap_bind_ip = get_natmap_bind_ip(resolved);
                let check_ip = natmap_bind_ip.as_deref().unwrap_or("0.0.0.0");
                if lab_ops_lab_lib::port::is_port_free(format!("{check_ip}:{p}")) {
                    (p, false)
                } else {
                    (p, true)
                }
            }
            ResolvedPortType::ForwardLocal {
                bind_port: Some(bp),
            } => (*bp, false),
            ResolvedPortType::ForwardLocal { bind_port: None } => {
                match port_assignments.get_or_allocate(&port_key) {
                    Some(p) => (p, false),
                    None => {
                        tracing::warn!(service.id_prefix = %resolved.service_id_prefix, "no free ports for service");
                        return Ok(None);
                    }
                }
            }
            ResolvedPortType::RProxyLocal { .. } | ResolvedPortType::RProxyRemote { .. } => {
                (resolved.container_port, true)
            }
        };

        if !skip_natmap {
            let natmap_bind_ip = get_natmap_bind_ip(resolved);
            self.ensure_docker_mapping(
                &resolved.service_id_prefix,
                natmap_bind_ip.as_deref(),
                host_port,
                resolved.container_port,
                resolved.protocol,
                Some(local_ip),
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
            let src_ip = preserve_src_ip_src.clone().unwrap_or_else(|| {
                natmap_bind_ip
                    .clone()
                    .unwrap_or_else(|| local_ip.to_string())
            });
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

        let consul_ip = local_ip.to_string();
        let registration = ConsulServiceRegistration::new(
            resolved,
            host_port,
            server_name,
            generation_id,
            &resolved.service_id_prefix,
            &consul_ip,
        );

        self.consul
            .register_service(&registration)
            .await
            .wrap_err("Consul API error")?;

        Ok(Some(registration.id))
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

        let docker = DockerClient::new().wrap_err("Docker API error")?;
        let cinfo = docker.inspect_container(container_id).await?;

        let mut resolved_services = Vec::new();
        for res in config.resolve_all() {
            if res.service_type != ServiceType::Docker {
                continue;
            }
            // For project-only matching (most common), check against the event's
            // compose_project parameter since inspect may not always return labels.
            let project_matches = match &res.match_cfg {
                Some(mc) => mc.project.as_deref().is_none_or(|p| p == compose_project),
                None => true,
            };
            if !project_matches {
                continue;
            }
            // Check container name and regex match criteria
            if let Some(mc) = &res.match_cfg {
                if let Some(c) = &mc.container
                    && cinfo.name != *c
                {
                    continue;
                }
                if let Some(cr) = &mc.container_regex {
                    if let Ok(re) = regex::Regex::new(cr) {
                        if !re.is_match(&cinfo.name) {
                            continue;
                        }
                    } else {
                        tracing::warn!(regex = %cr, "invalid container_regex");
                        continue;
                    }
                }
            }
            resolved_services.push(res);
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
            if let Err(e) = self
                .sync_docker(
                    resolved,
                    &cinfo,
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
    use tracing_test::traced_test;

    use super::*;

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
}
