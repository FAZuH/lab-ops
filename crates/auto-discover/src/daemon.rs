//! Core discovery daemon: orchestrates container discovery, Consul registration,
//! natmap port mapping, and nginx config storage.
//!
//! [`DiscoveryDaemon`] is the central coordinator. It reads the discovery config,
//! queries Docker for running containers, matches them to configured services,
//! allocates ports, invokes natmap to set up iptables rules, registers services
//! with Consul, and stores nginx configs in Consul KV.

use std::collections::HashMap;
use std::path::PathBuf;

use color_eyre::eyre::bail;
use color_eyre::eyre::WrapErr;
use color_eyre::Result;
use sha2::Digest;
use sha2::Sha256;

use crate::config::DiscoveryConfig;
use crate::config::ResolvedService;
use crate::consul::build_consul_service;
use crate::consul::compute_generation_id;
use crate::consul::ConsulClient;
use crate::docker::ContainerInfo;
use crate::docker::DockerClient;
use crate::natmap::NatmapClient;
use crate::ports::allocate_port;
use crate::ports::port_is_free;
use crate::ports::PortAssignments;

/// Central orchestrator for the service discovery daemon.
///
/// Owns clients for Consul, natmap, Docker, and port assignments. Provides
/// the main [`sync`](DiscoveryDaemon::sync) method for reconciling the
/// desired state with reality, plus event handlers for live Docker events.
pub struct DiscoveryDaemon {
    config_path: PathBuf,
    consul: ConsulClient,
    natmap: NatmapClient,
    state_dir: PathBuf,
}

impl DiscoveryDaemon {
    /// Create a new daemon instance.
    ///
    /// `config_path` is the path to `discovery.yaml`. `state_dir` is the
    /// directory for `ports.json` persistence.
    pub fn new(config_path: PathBuf, state_dir: PathBuf) -> Self {
        DiscoveryDaemon {
            config_path,
            consul: ConsulClient::from_env(),
            natmap: NatmapClient::default_socket(),
            state_dir,
        }
    }

    /// Full reconciliation: scan all running Docker containers, match them
    /// to configured services, allocate ports, apply natmap rules, register
    /// with Consul, store nginx configs, and clean up stale registrations.
    ///
    /// Called on daemon startup and by the `sync` subcommand.
    pub async fn sync(&self) -> Result<()> {
        let config =
            DiscoveryConfig::load(&self.config_path).wrap_err("failed to load discovery config")?;

        let server_name = config.name.clone();

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

        for container in &containers {
            for resolved in self.match_services(&config, container) {
                let consul_ip = self.determine_consul_ip(&resolved, &container.id).await?;
                let natmap_bind_ip = self.get_natmap_bind_ip(&resolved);

                let port_key = format!("{}-{}", resolved.name, resolved.container_port);
                let (host_port, skip_natmap) = if let Some(ref fwd) = resolved.forwarding {
                    let p = fwd.ext_ports[0];
                    if !port_is_free("0.0.0.0", p) {
                        tracing::warn!(
                            "Forwarding port {} already in use for {} (host-published, skipping natmap)",
                            p,
                            resolved.name
                        );
                        (p, true)
                    } else {
                        (p, false)
                    }
                } else if let Some(p) = port_assignments.get(&port_key) {
                    (p, false)
                } else {
                    match allocate_port(&port_assignments) {
                        Some(p) => {
                            port_assignments.set(port_key.clone(), p);
                            (p, false)
                        }
                        None => {
                            tracing::warn!("No free ports available for {}", resolved.name);
                            continue;
                        }
                    }
                };

                if !skip_natmap {
                    self.natmap
                        .add_docker_mapping(
                            &container.id,
                            natmap_bind_ip.as_deref(),
                            host_port,
                            resolved.container_port,
                            &resolved.protocol,
                        )
                        .wrap_err("natmap command failed")?;
                }

                let registration = build_consul_service(
                    &resolved,
                    host_port,
                    &server_name,
                    &generation_id,
                    &container.id,
                    &consul_ip,
                );

                self.consul
                    .register_service(&registration)
                    .await
                    .wrap_err("Consul API error")?;

                if !resolved.template.is_empty() {
                    if let Err(e) = self
                        .store_nginx_config(&resolved, &registration.id, host_port, &consul_ip)
                        .await
                    {
                        tracing::warn!("Failed to store nginx config for {}: {}", resolved.name, e);
                    }
                }

                current_service_ids.push(registration.id.clone());
            }
        }

        port_assignments
            .save(&ports_path)
            .map_err(|e| {
                tracing::warn!("Failed to save port assignments: {}", e);
            })
            .ok();

        let stale_ids = self
            .consul
            .deregister_stale_services(&server_name, &current_service_ids)
            .await
            .unwrap_or_default();
        for id in &stale_ids {
            if let Err(e) = self.consul.delete_nginx_config_kv(id).await {
                tracing::warn!("Failed to delete nginx config KV for {}: {}", id, e);
            }
        }
        if !stale_ids.is_empty() {
            tracing::info!("Deregistered {} stale services", stale_ids.len());
        }

        tracing::info!(
            "Sync complete: {} services active, generation_id={}",
            current_service_ids.len(),
            generation_id
        );

        Ok(())
    }

    /// Handle a Docker `container start` event.
    ///
    /// Matches the container's Compose project name and exposed ports against
    /// the discovery config, then registers the service if there's a match.
    pub async fn handle_container_start(
        &self,
        container_id: &str,
        compose_project: &str,
    ) -> Result<()> {
        let config =
            DiscoveryConfig::load(&self.config_path).wrap_err("failed to load discovery config")?;

        let resolved_services: Vec<ResolvedService> = config
            .networks
            .iter()
            .filter(|s| s.name == compose_project)
            .map(|s| config.resolve(s))
            .collect();

        if resolved_services.is_empty() {
            return Ok(());
        }

        let docker = DockerClient::new().wrap_err("Docker API error")?;
        let exposed_ports = docker
            .get_exposed_ports(container_id)
            .await
            .wrap_err("Docker API error")?;

        let resolved_services: Vec<&ResolvedService> = resolved_services
            .iter()
            .filter(|s| exposed_ports.contains(&s.container_port))
            .collect();

        if resolved_services.is_empty() {
            return Ok(());
        }

        tracing::info!(
            "handle_container_start: {} services for project {}",
            resolved_services.len(),
            compose_project
        );

        let server_name = config.name.clone();

        let config_hash = self.compute_config_hash(&config);
        let generation_id = compute_generation_id(&server_name, &config_hash);

        let ports_path = self.state_dir.join("ports.json");
        let mut port_assignments = PortAssignments::load(&ports_path);

        for resolved in &resolved_services {
            let consul_ip = self.determine_consul_ip(resolved, container_id).await?;
            let natmap_bind_ip = self.get_natmap_bind_ip(resolved);

            let port_key = format!("{}-{}", resolved.name, resolved.container_port);
            let (host_port, skip_natmap) = if let Some(ref fwd) = resolved.forwarding {
                let p = fwd.ext_ports[0];
                if !port_is_free("0.0.0.0", p) {
                    tracing::warn!(
                        "Forwarding port {} already in use for {} (host-published, skipping natmap)",
                        p,
                        resolved.name
                    );
                    (p, true)
                } else {
                    (p, false)
                }
            } else if let Some(p) = port_assignments.get(&port_key) {
                (p, false)
            } else {
                match allocate_port(&port_assignments) {
                    Some(p) => {
                        port_assignments.set(port_key.clone(), p);
                        port_assignments.save(&ports_path).ok();
                        (p, false)
                    }
                    None => bail!("no free ports"),
                }
            };

            if !skip_natmap {
                self.natmap
                    .add_docker_mapping(
                        container_id,
                        natmap_bind_ip.as_deref(),
                        host_port,
                        resolved.container_port,
                        &resolved.protocol,
                    )
                    .wrap_err("natmap command failed")?;
            }

            let registration = build_consul_service(
                resolved,
                host_port,
                &server_name,
                &generation_id,
                container_id,
                &consul_ip,
            );

            self.consul
                .register_service(&registration)
                .await
                .wrap_err("Consul API error")?;

            if !resolved.template.is_empty() {
                if let Err(e) = self
                    .store_nginx_config(resolved, &registration.id, host_port, &consul_ip)
                    .await
                {
                    tracing::warn!("Failed to store nginx config for {}: {}", resolved.name, e);
                }
            }

            tracing::info!(
                "Registered {} at {}:{} (container {})",
                resolved.name,
                consul_ip,
                host_port,
                container_id
            );
        }

        Ok(())
    }

    /// Handle a Docker `container die` event.
    ///
    /// Deregisters all Consul services for the container and removes their
    /// nginx config KV entries.
    pub async fn handle_container_die(&self, container_id: &str) -> Result<()> {
        let ids = self
            .consul
            .deregister_services_by_container(container_id)
            .await
            .wrap_err("Consul API error")?;

        for id in &ids {
            if let Err(e) = self.consul.delete_nginx_config_kv(id).await {
                tracing::warn!("Failed to delete nginx config KV for {}: {}", id, e);
            }
        }

        tracing::info!(
            "Deregistered {} services for container {}",
            ids.len(),
            container_id
        );
        Ok(())
    }

    /// Match a container against the discovery config using a two-level filter:
    /// 1. Compose project name matches `networks[].name`
    /// 2. Container exposes `networks[].container_port`
    fn match_services(
        &self,
        config: &DiscoveryConfig,
        container: &ContainerInfo,
    ) -> Vec<ResolvedService> {
        let project = match container.compose_project.as_deref() {
            Some(p) => p,
            None => return vec![],
        };
        config
            .networks
            .iter()
            .filter(|s| s.name == project)
            .filter(|s| container.exposed_ports.contains(&s.container_port))
            .map(|s| config.resolve(s))
            .collect()
    }

    /// Determine the IP address to register in Consul as the service address.
    ///
    /// Resolution order: `bind_ip` → `bind_interface` → container Docker IP.
    async fn determine_consul_ip(
        &self,
        resolved: &ResolvedService,
        container_id: &str,
    ) -> Result<String> {
        tracing::info!("determine_consul_ip: resolved={:?}", resolved);
        if let Some(ref ip) = resolved.bind_ip {
            return Ok(ip.clone());
        }
        if let Some(ref iface) = resolved.bind_interface {
            if let Some(ip) = resolve_interface_ip(iface) {
                return Ok(ip);
            }
        }
        self.natmap
            .get_container_ip(container_id)
            .map(|ip| ip.to_string())
            .wrap_err("failed to get container IP")
    }

    /// Determine the bind IP to pass to natmap for port mapping.
    ///
    /// Resolution order: `bind_ip` → `bind_interface` → `None` (natmap
    /// defaults to all interfaces).
    fn get_natmap_bind_ip(&self, resolved: &ResolvedService) -> Option<String> {
        if let Some(ref ip) = resolved.bind_ip {
            return Some(ip.clone());
        }
        if let Some(ref iface) = resolved.bind_interface {
            tracing::info!("Resolving interface: {}", iface);
            if let Some(ip) = resolve_interface_ip(iface) {
                tracing::info!("Resolved {} to {}", iface, ip);
                return Some(ip);
            }
        }
        None
    }

    /// Compute a SHA-256 hash of the discovery config (first 16 bytes),
    /// returned as a hex string. Used to detect config changes for
    /// stale-service cleanup.
    fn compute_config_hash(&self, config: &DiscoveryConfig) -> String {
        let yaml = serde_yaml::to_string(config).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(yaml.as_bytes());
        let result = hasher.finalize();
        hex::encode(&result[..16])
    }

    /// Run the nginx config generator script, apply preprocess, and store
    /// the result in Consul KV under `nginx-configs/sites/` or `nginx-configs/streams/`.
    ///
    /// If `postprocess` is configured, the script content is stored in a
    /// separate `.postproc` KV key for the proxy-side nginx-daemon to apply.
    async fn store_nginx_config(
        &self,
        service: &ResolvedService,
        service_id: &str,
        host_port: u16,
        consul_ip: &str,
    ) -> Result<()> {
        let kv_prefix = if service.template.starts_with("STREAM") {
            "nginx-configs/streams"
        } else {
            "nginx-configs/sites"
        };

        let mut envs: HashMap<String, String> = HashMap::new();
        envs.insert("AUTO_DISCOVER_SERVICE_NAME".into(), service.name.clone());
        envs.insert("AUTO_DISCOVER_SERVICE_ID".into(), service_id.to_string());
        envs.insert(
            "AUTO_DISCOVER_DOMAIN".into(),
            service.primary_domain().to_string(),
        );
        envs.insert(
            "AUTO_DISCOVER_ALL_DOMAINS".into(),
            service.domains.join(" "),
        );
        if let Some(ref proxy_ip) = service.proxy_ip {
            envs.insert("AUTO_DISCOVER_PROXY_IP".into(), proxy_ip.clone());
        }
        envs.insert("AUTO_DISCOVER_BIND_IP".into(), consul_ip.to_string());
        envs.insert("AUTO_DISCOVER_HOST_PORT".into(), host_port.to_string());
        envs.insert(
            "AUTO_DISCOVER_CONTAINER_PORT".into(),
            service.container_port.to_string(),
        );
        envs.insert("AUTO_DISCOVER_TEMPLATE".into(), service.template.clone());
        envs.insert("AUTO_DISCOVER_PROTOCOL".into(), service.protocol.clone());
        for (k, v) in &service.extra {
            envs.insert(format!("AUTO_DISCOVER_EXTRA_{k}"), v.clone());
        }

        let output = std::process::Command::new(&service.nginx_generator)
            .envs(&envs)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(
                "Generator {} failed for {}: stderr={}",
                service.nginx_generator,
                service.name,
                stderr
            );
            bail!(
                "generator {} failed for {}",
                service.nginx_generator,
                service.name
            );
        }

        let mut config = String::from_utf8_lossy(&output.stdout).to_string();

        if !service.preprocess.is_empty() {
            let mut child = std::process::Command::new("sh")
                .arg("-c")
                .arg(&service.preprocess)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::inherit())
                .spawn()?;

            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| color_eyre::eyre::eyre!("failed to take preprocess stdin"))?;
            use std::io::Write;
            stdin.write_all(config.as_bytes())?;
            drop(stdin);

            let result = child.wait_with_output()?;
            if result.status.success() {
                config = String::from_utf8_lossy(&result.stdout).to_string();
            } else {
                let stderr = String::from_utf8_lossy(&result.stderr);
                tracing::warn!(
                    "Preprocess failed for {}, using base config: {}",
                    service.name,
                    stderr
                );
            }
        }

        let conf_key = format!("{kv_prefix}/{service_id}.conf");
        self.consul
            .put_kv(&conf_key, &config)
            .await
            .wrap_err("Consul API error")?;

        if !service.postprocess.is_empty() {
            let postproc_key = format!("{kv_prefix}/{service_id}.postproc");
            self.consul
                .put_kv(&postproc_key, &service.postprocess)
                .await
                .wrap_err("Consul API error")?;
        }

        tracing::info!("Stored nginx config for {} at {}", service.name, conf_key);
        Ok(())
    }
}

/// Resolve the first IPv4 address on a network interface via `ip -j -4 addr show`.
fn resolve_interface_ip(iface_name: &str) -> Option<String> {
    let output = match std::process::Command::new("ip")
        .args(["-j", "-4", "addr", "show", iface_name])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("ip command failed for {}: {}", iface_name, e);
            return None;
        }
    };

    if !output.status.success() {
        tracing::warn!("Interface {} not found or ip command failed", iface_name);
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to parse ip command json for {}: {}", iface_name, e);
            return None;
        }
    };

    if let Some(interfaces) = parsed.as_array() {
        if let Some(interface) = interfaces.first() {
            if let Some(addr_info) = interface.get("addr_info").and_then(|a| a.as_array()) {
                if let Some(first_addr) = addr_info.first() {
                    if let Some(ip) = first_addr.get("local").and_then(|l| l.as_str()) {
                        return Some(ip.to_string());
                    }
                }
            }
        }
    }

    tracing::warn!(
        "Interface {} has no IPv4 address. Parsed JSON: {:?}",
        iface_name,
        parsed
    );
    None
}
