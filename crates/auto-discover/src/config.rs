use std::collections::HashMap;
use std::path::Path;

use color_eyre::Result;
use lab_lib::TransportProtocol;
use serde::Deserialize;
use serde::Serialize;

use crate::consts::AD_NGINX_GEN;

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
    pub rproxylocal: Vec<RProxyLocalConfig>,

    #[serde(default)]
    pub rproxyremote: Vec<RProxyRemoteConfig>,

    #[serde(default)]
    pub forwardlocal: Vec<ForwardLocalConfig>,

    #[serde(default)]
    pub forwardremote: Vec<ForwardRemoteConfig>,

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
pub struct RProxyLocalConfig {
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
pub struct RProxyRemoteConfig {
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
pub struct ForwardLocalConfig {
    pub port: u16,
    #[serde(default)]
    pub proto: Option<TransportProtocol>,
    #[serde(default)]
    pub bind_ip: Option<String>,
    #[serde(default)]
    pub bind_interface: Option<String>,
    #[serde(default)]
    pub bind_port: Option<u16>,
    #[serde(default)]
    pub proxy_on: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForwardRemoteConfig {
    pub port: u16,
    #[serde(default)]
    pub proto: Option<TransportProtocol>,
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
    RProxyLocal {
        template: String,
        domains: Vec<String>,
        proxy_on: Option<String>,
        proxy_ip: Option<String>,
        nginx_generator: String,
        preprocess: String,
        postprocess: String,
    },
    RProxyRemote {
        template: String,
        domains: Vec<String>,
        proxy_on: String,
        proxy_ip: Option<String>,
        nginx_generator: String,
        preprocess: String,
        postprocess: String,
    },
    ForwardLocal {
        bind_port: Option<u16>,
    },
    ForwardRemote {
        ext_ip: String,
        ext_ports: Vec<u16>,
        hairpin: bool,
        proxy_on: Option<String>,
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
    pub protocol: TransportProtocol,
    pub port_type: ResolvedPortType,
    pub extra: HashMap<String, String>,
}

impl ResolvedService {
    pub fn primary_domain(&self) -> &str {
        match &self.port_type {
            ResolvedPortType::RProxyLocal { domains, .. }
            | ResolvedPortType::RProxyRemote { domains, .. } => {
                domains.first().map(|s| s.as_str()).unwrap_or("_")
            }
            _ => "_",
        }
    }

    pub fn domain_slug(&self) -> String {
        self.primary_domain().replace('.', "-")
    }
}

impl DiscoveryConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config = serde_yaml::from_str(&contents)?;
        Ok(config)
    }

    fn resolve_binding(
        &self,
        svc_ip: Option<&String>,
        svc_iface: Option<&String>,
        def_ip: Option<&String>,
        def_iface: Option<&String>,
    ) -> (Option<String>, Option<String>) {
        if let Some(ip) = svc_ip {
            (Some(ip.clone()), None)
        } else if let Some(iface) = svc_iface {
            (None, Some(iface.clone()))
        } else if let Some(ip) = def_ip {
            (Some(ip.clone()), None)
        } else if let Some(iface) = def_iface {
            (None, Some(iface.clone()))
        } else {
            (None, None)
        }
    }

    pub fn resolve_all(&self) -> Vec<ResolvedService> {
        let mut resolved = Vec::new();

        for (service_id_prefix, service) in &self.services {
            let (svc_bind_ip, svc_bind_interface) = self.resolve_binding(
                service.bind_ip.as_ref(),
                service.bind_interface.as_ref(),
                self.defaults.bind_ip.as_ref(),
                self.defaults.bind_interface.as_ref(),
            );

            for rp in &service.rproxylocal {
                resolved.push(ResolvedService {
                    service_id_prefix: service_id_prefix.clone(),
                    service_name: service_id_prefix.clone(),
                    service_type: service.service_type.clone(),
                    match_cfg: service.match_cfg.clone(),
                    local_address: service.address.clone(),
                    container_port: rp.port,
                    proxy_on: rp
                        .proxy_on
                        .clone()
                        .or_else(|| self.defaults.proxy_on.clone()),
                    bind_ip: svc_bind_ip.clone(),
                    bind_interface: svc_bind_interface.clone(),
                    protocol: TransportProtocol::default(),
                    extra: service.extra.clone(),
                    port_type: ResolvedPortType::RProxyLocal {
                        template: rp.template.clone(),
                        domains: rp.domains.clone(),
                        proxy_on: rp
                            .proxy_on
                            .clone()
                            .or_else(|| self.defaults.proxy_on.clone()),
                        proxy_ip: rp
                            .proxy_ip
                            .clone()
                            .or_else(|| self.defaults.proxy_ip.clone()),
                        nginx_generator: self
                            .resolve_nginx_generator(rp.nginx_generator.as_deref()),
                        preprocess: self.resolve_preprocess(rp.preprocess.as_deref()),
                        postprocess: self.resolve_postprocess(rp.postprocess.as_deref()),
                    },
                });
            }

            for rp in &service.rproxyremote {
                let proxy_on = rp
                    .proxy_on
                    .clone()
                    .or_else(|| self.defaults.proxy_on.clone());
                if proxy_on.is_none() {
                    tracing::warn!(
                        "rproxyremote entry for {} port {} has no proxy_on (required for remote proxy), skipping",
                        service_id_prefix,
                        rp.port
                    );
                    continue;
                }
                resolved.push(ResolvedService {
                    service_id_prefix: service_id_prefix.clone(),
                    service_name: service_id_prefix.clone(),
                    service_type: service.service_type.clone(),
                    match_cfg: service.match_cfg.clone(),
                    local_address: service.address.clone(),
                    container_port: rp.port,
                    proxy_on: proxy_on.clone(),
                    bind_ip: svc_bind_ip.clone(),
                    bind_interface: svc_bind_interface.clone(),
                    protocol: TransportProtocol::default(),
                    extra: service.extra.clone(),
                    port_type: ResolvedPortType::RProxyRemote {
                        template: rp.template.clone(),
                        domains: rp.domains.clone(),
                        proxy_on: proxy_on.unwrap_or_default(),
                        proxy_ip: rp
                            .proxy_ip
                            .clone()
                            .or_else(|| self.defaults.proxy_ip.clone()),
                        nginx_generator: self
                            .resolve_nginx_generator(rp.nginx_generator.as_deref()),
                        preprocess: self.resolve_preprocess(rp.preprocess.as_deref()),
                        postprocess: self.resolve_postprocess(rp.postprocess.as_deref()),
                    },
                });
            }

            for fl in &service.forwardlocal {
                let (final_ip, final_iface) = self.resolve_binding(
                    fl.bind_ip.as_ref(),
                    fl.bind_interface.as_ref(),
                    svc_bind_ip.as_ref(),
                    svc_bind_interface.as_ref(),
                );
                resolved.push(ResolvedService {
                    service_id_prefix: service_id_prefix.clone(),
                    service_name: service_id_prefix.clone(),
                    service_type: service.service_type.clone(),
                    match_cfg: service.match_cfg.clone(),
                    local_address: service.address.clone(),
                    container_port: fl.port,
                    proxy_on: fl
                        .proxy_on
                        .clone()
                        .or_else(|| self.defaults.proxy_on.clone()),
                    bind_ip: final_ip,
                    bind_interface: final_iface,
                    protocol: fl.proto.unwrap_or_default(),
                    extra: service.extra.clone(),
                    port_type: ResolvedPortType::ForwardLocal {
                        bind_port: fl.bind_port,
                    },
                });
            }

            for fr in &service.forwardremote {
                resolved.push(ResolvedService {
                    service_id_prefix: service_id_prefix.clone(),
                    service_name: service_id_prefix.clone(),
                    service_type: service.service_type.clone(),
                    match_cfg: service.match_cfg.clone(),
                    local_address: service.address.clone(),
                    container_port: fr.port,
                    proxy_on: fr
                        .proxy_on
                        .clone()
                        .or_else(|| self.defaults.proxy_on.clone()),
                    bind_ip: svc_bind_ip.clone(),
                    bind_interface: svc_bind_interface.clone(),
                    protocol: fr.proto.unwrap_or_default(),
                    extra: service.extra.clone(),
                    port_type: ResolvedPortType::ForwardRemote {
                        ext_ip: fr.ext_ip.clone().unwrap_or_default(),
                        ext_ports: fr.ext_ports.clone().unwrap_or_default(),
                        hairpin: fr.hairpin.unwrap_or(false),
                        proxy_on: fr
                            .proxy_on
                            .clone()
                            .or_else(|| self.defaults.proxy_on.clone()),
                    },
                });
            }
        }

        resolved.sort_by_key(|r| format!("{}-{}", r.service_id_prefix, r.container_port));
        resolved
    }

    fn resolve_nginx_generator(&self, override_val: Option<&str>) -> String {
        override_val
            .map(str::to_owned)
            .or_else(|| self.defaults.nginx_generator.clone())
            .unwrap_or_else(|| AD_NGINX_GEN.to_string())
    }

    fn resolve_preprocess(&self, override_val: Option<&str>) -> String {
        override_val
            .map(str::to_owned)
            .or_else(|| self.defaults.preprocess.clone())
            .unwrap_or_default()
    }

    fn resolve_postprocess(&self, override_val: Option<&str>) -> String {
        override_val
            .map(str::to_owned)
            .or_else(|| self.defaults.postprocess.clone())
            .unwrap_or_default()
    }
}
