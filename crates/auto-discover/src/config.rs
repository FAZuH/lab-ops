use std::collections::HashMap;
use std::path::Path;

use color_eyre::Result;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscoveryConfig {
    pub node: NodeConfig,
    #[serde(default)]
    pub config_dir: Option<String>,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub services: HashMap<String, ServiceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeConfig {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Defaults {
    #[serde(default)]
    pub proxy_on: Option<String>,
    #[serde(default)]
    pub proxy_ip: Option<String>,
    #[serde(default)]
    pub bind_interface: Option<String>,
    #[serde(default)]
    pub bind_ip: Option<String>,
    #[serde(default)]
    pub nginx_generator: Option<String>,
    #[serde(default)]
    pub preprocess: Option<String>,
    #[serde(default)]
    pub postprocess: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    Docker,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceConfig {
    #[serde(rename = "type")]
    pub service_type: ServiceType,

    #[serde(rename = "match")]
    #[serde(default)]
    pub match_cfg: Option<MatchConfig>,

    #[serde(default)]
    pub address: Option<String>,

    #[serde(default)]
    pub bind_ip: Option<String>,

    #[serde(default)]
    pub bind_interface: Option<String>,

    #[serde(default)]
    pub rproxy: Vec<RProxyConfig>,

    #[serde(default)]
    pub forwarding: Vec<ForwardingConfig>,

    #[serde(default)]
    pub extra: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MatchConfig {
    pub project: Option<String>,
    pub container: Option<String>,
    pub container_regex: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RProxyConfig {
    pub port: u16,
    pub template: String,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub proxy_on: Option<String>,
    #[serde(default)]
    pub proxy_ip: Option<String>,
    #[serde(default)]
    pub nginx_generator: Option<String>,
    #[serde(default)]
    pub preprocess: Option<String>,
    #[serde(default)]
    pub postprocess: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ForwardingType {
    Local,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForwardingConfig {
    #[serde(rename = "type")]
    pub fwd_type: ForwardingType,
    pub port: u16,
    #[serde(default)]
    pub proto: Option<String>,
    #[serde(default)]
    pub bind_ip: Option<String>,
    #[serde(default)]
    pub bind_interface: Option<String>,
    #[serde(default)]
    pub bind_port: Option<u16>,
    #[serde(default)]
    pub ext_ip: Option<String>,
    #[serde(default)]
    pub ext_ports: Option<Vec<u16>>,
    #[serde(default)]
    pub hairpin: Option<bool>,
    #[serde(default)]
    pub proxy_on: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedPortType {
    RProxy {
        template: String,
        domains: Vec<String>,
        proxy_ip: Option<String>,
        nginx_generator: String,
        preprocess: String,
        postprocess: String,
    },
    ForwardingLocal {
        bind_port: Option<u16>,
        template: String,
        domains: Vec<String>,
        proxy_ip: Option<String>,
        nginx_generator: String,
        preprocess: String,
        postprocess: String,
    },
    ForwardingRemote {
        ext_ip: String,
        ext_ports: Vec<u16>,
        hairpin: bool,
        template: String,
        domains: Vec<String>,
        proxy_ip: Option<String>,
        nginx_generator: String,
        preprocess: String,
        postprocess: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedService {
    pub service_id_prefix: String,
    pub service_name: String,
    pub service_type: ServiceType,
    pub match_cfg: Option<MatchConfig>,
    pub local_address: Option<String>,
    pub container_port: u16,
    pub proxy_on: Option<String>,
    pub bind_ip: Option<String>,
    pub bind_interface: Option<String>,
    pub protocol: String,
    pub port_type: ResolvedPortType,
    pub extra: HashMap<String, String>,
}

impl ResolvedService {
    /// Returns the primary domain for routing and ID generation.
    pub fn primary_domain(&self) -> &str {
        match &self.port_type {
            ResolvedPortType::RProxy { domains, .. } => {
                domains.first().map(|s| s.as_str()).unwrap_or("_")
            }
            _ => "_",
        }
    }

    /// Returns a slugified domain for ID generation.
    pub fn domain_slug(&self) -> String {
        self.primary_domain().replace('.', "-")
    }
}

impl DiscoveryConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;

        // Try new format first, fall back to legacy format
        if let Ok(config) = serde_yaml::from_str::<DiscoveryConfig>(&contents) {
            return Ok(config);
        }

        // Legacy format (old `name` + `networks` top-level keys)
        #[derive(Deserialize)]
        struct LegacyForwardingConfig {
            ext_ip: String,
            ext_ports: Vec<u16>,
            #[serde(default)]
            proto: Option<String>,
            #[serde(default)]
            hairpin: bool,
        }

        #[derive(Deserialize)]
        struct LegacyNetworkConfig {
            name: String,
            container_port: Option<u16>,
            local_ip: Option<String>,
            local_port: Option<u16>,
            #[serde(default)]
            domains: Vec<String>,
            #[serde(default)]
            template: String,
            #[serde(default)]
            protocol: Option<String>,
            forwarding: Option<LegacyForwardingConfig>,
            #[serde(default)]
            proxy_ip: Option<String>,
            #[serde(default)]
            bind_ip: Option<String>,
            #[serde(default)]
            bind_interface: Option<String>,
            #[serde(default)]
            extra: HashMap<String, String>,
            #[serde(default)]
            nginx_generator: Option<String>,
            #[serde(default)]
            preprocess: Option<String>,
            #[serde(default)]
            postprocess: Option<String>,
        }

        #[derive(Deserialize)]
        struct LegacyConfig {
            name: String,
            #[serde(default)]
            config_dir: Option<String>,
            #[serde(default)]
            defaults: Defaults,
            networks: Vec<LegacyNetworkConfig>,
        }

        let legacy: LegacyConfig = serde_yaml::from_str(&contents)?;
        let mut services = HashMap::new();

        for net in legacy.networks {
            let is_local = net.local_ip.is_some() || net.local_port.is_some();
            let port = net.local_port.or(net.container_port);
            let _proto = net.protocol.unwrap_or_else(|| "tcp".to_string());

            let entry = services
                .entry(net.name.clone())
                .or_insert_with(|| ServiceConfig {
                    service_type: if is_local {
                        ServiceType::Local
                    } else {
                        ServiceType::Docker
                    },
                    match_cfg: Some(MatchConfig {
                        project: Some(net.name.clone()),
                        container: None,
                        container_regex: None,
                    }),
                    address: net.local_ip.clone(),
                    bind_ip: net.bind_ip.clone(),
                    bind_interface: net.bind_interface.clone(),
                    rproxy: Vec::new(),
                    forwarding: Vec::new(),
                    extra: HashMap::new(),
                });

            // Legacy forwarding uses ext_ports[0] as the host port directly
            if let Some(f) = net.forwarding
                && let Some(p) = port
            {
                entry.forwarding.push(ForwardingConfig {
                    fwd_type: ForwardingType::Remote,
                    port: p,
                    proto: f.proto,
                    bind_ip: None,
                    bind_interface: None,
                    bind_port: None,
                    ext_ip: Some(f.ext_ip),
                    ext_ports: Some(f.ext_ports),
                    hairpin: Some(f.hairpin),
                    proxy_on: None,
                });
            }

            // RProxy entries: only create when no forwarding OR when template is non-empty
            let has_forwarding_in_entry = !entry.forwarding.is_empty();
            if (!has_forwarding_in_entry || (!net.template.is_empty() && !net.template.is_empty()))
                && let Some(p) = port
            {
                entry.rproxy.push(RProxyConfig {
                    port: p,
                    template: net.template.clone(),
                    domains: net.domains.clone(),
                    proxy_on: None,
                    proxy_ip: net.proxy_ip.clone(),
                    nginx_generator: net.nginx_generator.clone(),
                    preprocess: net.preprocess.clone(),
                    postprocess: net.postprocess.clone(),
                });
            }

            entry.extra.extend(net.extra);
        }

        Ok(DiscoveryConfig {
            node: NodeConfig { name: legacy.name },
            config_dir: legacy.config_dir,
            defaults: legacy.defaults,
            services,
        })
    }

    pub fn resolve_all(&self) -> Vec<ResolvedService> {
        let mut resolved = Vec::new();

        for (service_id_prefix, service) in &self.services {
            let svc_bind_ip = service
                .bind_ip
                .clone()
                .or_else(|| self.defaults.bind_ip.clone());
            let svc_bind_interface = service
                .bind_interface
                .clone()
                .or_else(|| self.defaults.bind_interface.clone());

            for proxy in &service.rproxy {
                // Skip rproxy entries that have a matching forwarding entry (merged above)
                if service.forwarding.iter().any(|f| f.port == proxy.port) {
                    continue;
                }
                resolved.push(ResolvedService {
                    service_id_prefix: service_id_prefix.clone(),
                    service_name: service_id_prefix.clone(),
                    service_type: service.service_type.clone(),
                    match_cfg: service.match_cfg.clone(),
                    local_address: service.address.clone(),
                    container_port: proxy.port,
                    proxy_on: proxy
                        .proxy_on
                        .clone()
                        .or_else(|| self.defaults.proxy_on.clone()),
                    bind_ip: svc_bind_ip.clone(),
                    bind_interface: svc_bind_interface.clone(),
                    protocol: "tcp".to_string(), // rproxy is always tcp implicitly
                    extra: service.extra.clone(),
                    port_type: ResolvedPortType::RProxy {
                        template: proxy.template.clone(),
                        domains: proxy.domains.clone(),
                        proxy_ip: proxy
                            .proxy_ip
                            .clone()
                            .or_else(|| self.defaults.proxy_ip.clone()),
                        nginx_generator: proxy
                            .nginx_generator
                            .clone()
                            .or_else(|| self.defaults.nginx_generator.clone())
                            .unwrap_or_else(|| {
                                "/usr/local/bin/auto-discover-gen-nginx".to_string()
                            }),
                        preprocess: proxy
                            .preprocess
                            .clone()
                            .or_else(|| self.defaults.preprocess.clone())
                            .unwrap_or_default(),
                        postprocess: proxy
                            .postprocess
                            .clone()
                            .or_else(|| self.defaults.postprocess.clone())
                            .unwrap_or_default(),
                    },
                });
            }

            for fwd in &service.forwarding {
                let matching_rproxy = service.rproxy.iter().find(|r| r.port == fwd.port);
                let port_type = match fwd.fwd_type {
                    ForwardingType::Local => ResolvedPortType::ForwardingLocal {
                        bind_port: fwd.bind_port,
                        template: matching_rproxy
                            .map(|r| r.template.clone())
                            .unwrap_or_default(),
                        domains: matching_rproxy
                            .map(|r| r.domains.clone())
                            .unwrap_or_default(),
                        proxy_ip: matching_rproxy
                            .and_then(|r| r.proxy_ip.clone())
                            .or_else(|| self.defaults.proxy_ip.clone()),
                        nginx_generator: matching_rproxy
                            .and_then(|r| r.nginx_generator.clone())
                            .or_else(|| self.defaults.nginx_generator.clone())
                            .unwrap_or_else(|| {
                                "/usr/local/bin/auto-discover-gen-nginx".to_string()
                            }),
                        preprocess: matching_rproxy
                            .and_then(|r| r.preprocess.clone())
                            .or_else(|| self.defaults.preprocess.clone())
                            .unwrap_or_default(),
                        postprocess: matching_rproxy
                            .and_then(|r| r.postprocess.clone())
                            .or_else(|| self.defaults.postprocess.clone())
                            .unwrap_or_default(),
                    },
                    ForwardingType::Remote => ResolvedPortType::ForwardingRemote {
                        ext_ip: fwd.ext_ip.clone().unwrap_or_default(),
                        ext_ports: fwd.ext_ports.clone().unwrap_or_default(),
                        hairpin: fwd.hairpin.unwrap_or(false),
                        template: matching_rproxy
                            .map(|r| r.template.clone())
                            .unwrap_or_default(),
                        domains: matching_rproxy
                            .map(|r| r.domains.clone())
                            .unwrap_or_default(),
                        proxy_ip: matching_rproxy
                            .and_then(|r| r.proxy_ip.clone())
                            .or_else(|| self.defaults.proxy_ip.clone()),
                        nginx_generator: matching_rproxy
                            .and_then(|r| r.nginx_generator.clone())
                            .or_else(|| self.defaults.nginx_generator.clone())
                            .unwrap_or_else(|| {
                                "/usr/local/bin/auto-discover-gen-nginx".to_string()
                            }),
                        preprocess: matching_rproxy
                            .and_then(|r| r.preprocess.clone())
                            .or_else(|| self.defaults.preprocess.clone())
                            .unwrap_or_default(),
                        postprocess: matching_rproxy
                            .and_then(|r| r.postprocess.clone())
                            .or_else(|| self.defaults.postprocess.clone())
                            .unwrap_or_default(),
                    },
                };

                resolved.push(ResolvedService {
                    service_id_prefix: service_id_prefix.clone(),
                    service_name: service_id_prefix.clone(),
                    service_type: service.service_type.clone(),
                    match_cfg: service.match_cfg.clone(),
                    local_address: service.address.clone(),
                    container_port: fwd.port,
                    proxy_on: fwd
                        .proxy_on
                        .clone()
                        .or_else(|| self.defaults.proxy_on.clone()),
                    bind_ip: fwd.bind_ip.clone().or(svc_bind_ip.clone()),
                    bind_interface: fwd.bind_interface.clone().or(svc_bind_interface.clone()),
                    protocol: fwd.proto.clone().unwrap_or_else(|| "tcp".to_string()),
                    extra: service.extra.clone(),
                    port_type,
                });
            }
        }

        // Sort resolved services for deterministic output
        resolved.sort_by_key(|r| format!("{}-{}", r.service_id_prefix, r.container_port));
        resolved
    }
}
