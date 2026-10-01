//! `discovery.yaml` schema: node identity, defaults, and per-service definitions.

use std::collections::HashMap;
use std::path::Path;

use color_eyre::Result;
use lab_ops_lab_lib::TransportProtocol;
use serde::Deserialize;
use serde::Serialize;

/// Root of `discovery.yaml`: this node's identity, the defaults every
/// service inherits, and the per-service map.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscoveryConfig {
    pub node: NodeConfig,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub services: HashMap<String, ServiceConfig>,
}

/// Identity this node registers its own Consul agent under.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeConfig {
    pub name: String,
}

/// Fallback values for any service that does not set them itself.
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
    pub preserve_src_ip: Option<bool>,
    #[serde(default)]
    pub preserve_src_ip_gateway: Option<String>,
    #[serde(default)]
    pub preserve_src_ip_src: Option<String>,
}

/// Whether a service is a Docker container or a fixed local address.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    Docker,
    Local,
}

/// One entry under `services:` in `discovery.yaml`.
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

/// Container match criteria; a service with no match registers once, statically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MatchConfig {
    pub project: Option<String>,
    pub container: Option<String>,
    pub container_regex: Option<String>,
}

/// A Consul environment-variable registration for a local service.
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
}

/// A Consul environment-variable registration for a container's port.
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
}

/// A natmap DNAT mapping onto a local address.
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

/// A natmap DNAT mapping published on an external IP and port set.
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
    #[serde(default)]
    pub preserve_src_ip: Option<bool>,
    #[serde(default)]
    pub preserve_src_ip_gateway: Option<String>,
    #[serde(default)]
    pub preserve_src_ip_src: Option<String>,
}

/// The single port registration a service resolves to, after defaults merge.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedPortType {
    RProxyLocal {
        template: String,
        domains: Vec<String>,
        proxy_ip: Option<String>,
    },
    RProxyRemote {
        template: String,
        domains: Vec<String>,
        proxy_ip: Option<String>,
    },
    ForwardLocal {
        bind_port: Option<u16>,
    },
    ForwardRemote {
        ext_ip: String,
        ext_ports: Vec<u16>,
        hairpin: bool,
        preserve_src_ip: bool,
        preserve_src_ip_gateway: Option<String>,
        preserve_src_ip_src: Option<String>,
    },
}

/// One fully-resolved registration, with defaults applied and ports flattened.
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
    /// First configured domain, or `"_"` for a non-rproxy service.
    pub fn primary_domain(&self) -> &str {
        match &self.port_type {
            ResolvedPortType::RProxyLocal { domains, .. }
            | ResolvedPortType::RProxyRemote { domains, .. } => {
                domains.first().map(|s| s.as_str()).unwrap_or("_")
            }
            _ => "_",
        }
    }

    /// The primary domain with dots replaced by dashes, for use in an ID.
    pub fn domain_slug(&self) -> String {
        self.primary_domain().replace('.', "-")
    }

    /// Every configured domain; empty for a non-rproxy service.
    pub fn domains(&self) -> Vec<&str> {
        match &self.port_type {
            ResolvedPortType::RProxyLocal { domains, .. }
            | ResolvedPortType::RProxyRemote { domains, .. } => {
                domains.iter().map(|s| s.as_str()).collect()
            }
            _ => vec![],
        }
    }
}

impl DiscoveryConfig {
    /// Reads and deserializes a `discovery.yaml` from disk.
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

    /// Expands every service into one [`ResolvedService`] per port registration.
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
                        proxy_ip: rp
                            .proxy_ip
                            .clone()
                            .or_else(|| self.defaults.proxy_ip.clone()),
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
                        proxy_ip: rp
                            .proxy_ip
                            .clone()
                            .or_else(|| self.defaults.proxy_ip.clone()),
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
                        preserve_src_ip: fr
                            .preserve_src_ip
                            .unwrap_or_else(|| self.defaults.preserve_src_ip.unwrap_or(false)),
                        preserve_src_ip_gateway: fr
                            .preserve_src_ip_gateway
                            .clone()
                            .or_else(|| self.defaults.preserve_src_ip_gateway.clone()),
                        preserve_src_ip_src: fr
                            .preserve_src_ip_src
                            .clone()
                            .or_else(|| self.defaults.preserve_src_ip_src.clone()),
                    },
                });
            }
        }

        resolved.sort_by_key(|r| format!("{}-{}", r.service_id_prefix, r.container_port));
        resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_with_forwardremote(
        _port: u16,
        forwardremote: Vec<ForwardRemoteConfig>,
    ) -> ServiceConfig {
        ServiceConfig {
            service_type: ServiceType::Docker,
            match_cfg: None,
            address: None,
            bind_ip: None,
            bind_interface: None,
            rproxylocal: vec![],
            rproxyremote: vec![],
            forwardlocal: vec![],
            forwardremote,
            extra: HashMap::new(),
        }
    }

    fn forwardremote_with_preserve_ip(
        port: u16,
        preserve_src_ip: Option<bool>,
        preserve_src_ip_gateway: Option<String>,
        preserve_src_ip_src: Option<String>,
    ) -> ForwardRemoteConfig {
        ForwardRemoteConfig {
            port,
            proto: None,
            ext_ip: Some("1.2.3.4".into()),
            ext_ports: Some(vec![port]),
            hairpin: None,
            proxy_on: None,
            preserve_src_ip,
            preserve_src_ip_gateway,
            preserve_src_ip_src,
        }
    }

    fn config_with_defaults(services: HashMap<String, ServiceConfig>) -> DiscoveryConfig {
        DiscoveryConfig {
            node: NodeConfig {
                name: "test-node".into(),
            },
            defaults: Defaults {
                preserve_src_ip: Some(true),
                preserve_src_ip_gateway: Some("192.168.1.1".into()),
                preserve_src_ip_src: None,
                ..Default::default()
            },
            services,
        }
    }

    #[test]
    fn preserve_src_ip_falls_back_to_defaults() {
        let svc = service_with_forwardremote(
            80,
            vec![forwardremote_with_preserve_ip(80, None, None, None)],
        );
        let mut services = HashMap::new();
        services.insert("svc".into(), svc);

        let resolved = config_with_defaults(services).resolve_all();

        assert_eq!(resolved.len(), 1);
        let res = &resolved[0];
        let ResolvedPortType::ForwardRemote {
            preserve_src_ip,
            preserve_src_ip_gateway,
            preserve_src_ip_src,
            ..
        } = &res.port_type
        else {
            panic!("Expected ForwardRemote");
        };
        assert!(*preserve_src_ip);
        assert_eq!(preserve_src_ip_gateway.as_deref(), Some("192.168.1.1"));
        assert_eq!(preserve_src_ip_src.as_deref(), None);
    }

    #[test]
    fn preserve_src_ip_overrides_defaults() {
        let svc = service_with_forwardremote(
            81,
            vec![forwardremote_with_preserve_ip(
                81,
                Some(false),
                Some("10.10.10.1".into()),
                Some("10.10.10.10".into()),
            )],
        );
        let mut services = HashMap::new();
        services.insert("svc".into(), svc);

        let resolved = config_with_defaults(services).resolve_all();

        assert_eq!(resolved.len(), 1);
        let res = &resolved[0];
        let ResolvedPortType::ForwardRemote {
            preserve_src_ip,
            preserve_src_ip_gateway,
            preserve_src_ip_src,
            ..
        } = &res.port_type
        else {
            panic!("Expected ForwardRemote");
        };
        assert!(!(*preserve_src_ip));
        assert_eq!(preserve_src_ip_gateway.as_deref(), Some("10.10.10.1"));
        assert_eq!(preserve_src_ip_src.as_deref(), Some("10.10.10.10"));
    }

    #[test]
    fn discovery_config_parses_full_yaml() {
        let yaml = r#"
node:
  name: homelab-ünïcode
defaults:
  proxy_on: https://proxy.example.com
services:
  nginx:
    type: docker
    match:
      project: web
    rproxylocal:
      - port: 80
        template: "{service}-{port}"
        domains: ["example.com", "www.example.com"]
    forwardlocal:
      - port: 443
        proto: udp
        bind_port: 8443
    forwardremote:
      - port: 8080
        ext_ip: 203.0.113.50
        ext_ports: [80, 443]
        hairpin: true
"#;
        let cfg: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.node.name, "homelab-ünïcode");
        let svc = &cfg.services["nginx"];
        assert_eq!(svc.service_type, ServiceType::Docker);
        assert_eq!(
            svc.match_cfg.as_ref().unwrap().project.as_deref(),
            Some("web")
        );
        assert_eq!(svc.rproxylocal[0].template, "{service}-{port}");
        assert_eq!(
            svc.rproxylocal[0].domains,
            ["example.com", "www.example.com"]
        );
        assert_eq!(svc.forwardlocal[0].proto, Some(TransportProtocol::Udp));
        assert_eq!(svc.forwardlocal[0].bind_port, Some(8443));
        assert_eq!(
            svc.forwardremote[0].ext_ports.as_deref(),
            Some(&[80u16, 443][..])
        );
    }

    #[test]
    fn discovery_config_defaults_missing_optional_sections() {
        let yaml = "node:\n  name: homelab\n";
        let cfg: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.services.is_empty());
        assert_eq!(cfg.defaults, Defaults::default());
    }

    #[test]
    fn discovery_config_rejects_missing_node_name() {
        let yaml = "node: {}\nservices: {}\n";
        let err = serde_yaml::from_str::<DiscoveryConfig>(yaml).unwrap_err();
        assert!(err.to_string().contains("name"), "{err}");
    }

    #[test]
    fn service_config_rejects_unknown_type() {
        let yaml = "node:\n  name: homelab\nservices:\n  api:\n    type: kubernetes\n";
        let err = serde_yaml::from_str::<DiscoveryConfig>(yaml).unwrap_err();
        assert!(err.to_string().contains("unknown variant"), "{err}");
    }

    #[test]
    fn service_config_rejects_missing_type() {
        let yaml = "node:\n  name: homelab\nservices:\n  api:\n    address: 127.0.0.1\n";
        let err = serde_yaml::from_str::<DiscoveryConfig>(yaml).unwrap_err();
        assert!(err.to_string().contains("type"), "{err}");
    }

    #[test]
    fn rproxy_local_config_rejects_missing_template() {
        let yaml = "node:\n  name: homelab\nservices:\n  api:\n    type: docker\n    rproxylocal:\n      - port: 80\n";
        let err = serde_yaml::from_str::<DiscoveryConfig>(yaml).unwrap_err();
        assert!(err.to_string().contains("template"), "{err}");
    }

    #[test]
    fn forward_remote_config_rejects_missing_port() {
        let yaml = "node:\n  name: homelab\nservices:\n  api:\n    type: docker\n    forwardremote:\n      - ext_ip: 203.0.113.50\n";
        let err = serde_yaml::from_str::<DiscoveryConfig>(yaml).unwrap_err();
        assert!(err.to_string().contains("port"), "{err}");
    }

    #[test]
    fn forward_local_config_rejects_port_above_u16_max() {
        let yaml = "node:\n  name: homelab\nservices:\n  api:\n    type: docker\n    forwardlocal:\n      - port: 65536\n";
        let err = serde_yaml::from_str::<DiscoveryConfig>(yaml).unwrap_err();
        assert!(err.to_string().contains("65536"), "{err}");
    }

    #[test]
    fn forward_remote_config_rejects_port_above_u16_max() {
        let yaml = "node:\n  name: homelab\nservices:\n  api:\n    type: docker\n    forwardremote:\n      - port: 65536\n";
        let err = serde_yaml::from_str::<DiscoveryConfig>(yaml).unwrap_err();
        assert!(err.to_string().contains("65536"), "{err}");
    }

    #[test]
    fn forward_remote_config_rejects_ext_ports_above_u16_max() {
        let yaml = "node:\n  name: homelab\nservices:\n  api:\n    type: docker\n    forwardremote:\n      - port: 8080\n        ext_ports: [80, 65536]\n";
        let err = serde_yaml::from_str::<DiscoveryConfig>(yaml).unwrap_err();
        assert!(err.to_string().contains("65536"), "{err}");
    }

    #[test]
    fn forward_local_config_accepts_u16_boundary_ports() {
        let yaml = "node:\n  name: homelab\nservices:\n  api:\n    type: docker\n    forwardlocal:\n      - port: 65535\n        bind_port: 0\n";
        let cfg: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.services["api"].forwardlocal[0].port, 65535);
        assert_eq!(cfg.services["api"].forwardlocal[0].bind_port, Some(0));
    }
}
