//! Discovery configuration types and YAML parsing.
//!
//! Parses `/etc/auto-discover/discovery.yaml` defining which Docker services
//! to discover, their port settings, and how to register them with Consul.
//! Fields cascade from defaults to per-network overrides through
//! [`DiscoveryConfig::resolve`].

use std::collections::HashMap;
use std::path::Path;

use color_eyre::Result;
use serde::Deserialize;
use serde::Serialize;

/// Root configuration parsed from `/etc/auto-discover/discovery.yaml`.
///
/// Defines node identity (`name`), cascade defaults, and the list of
/// Docker services to discover.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscoveryConfig {
    /// Node identity. Used for Consul service ID prefix and stale-service
    /// cleanup scoped to this server.
    pub name: String,
    /// Optional directory for additional config files.
    #[serde(default)]
    pub config_dir: Option<String>,
    /// Cascade defaults applied to all [`NetworkConfig`] entries that don't
    /// override them.
    #[serde(default)]
    pub defaults: Defaults,
    /// Service definitions to discover and register.
    pub networks: Vec<NetworkConfig>,
}

/// Default values that cascade to all network entries.
///
/// Per-network fields take precedence over these defaults.
/// The resolution chain is: network-specific value → defaults → hard-coded fallback.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Defaults {
    /// Default proxy server IP for nginx `listen` directives.
    #[serde(default)]
    pub proxy_ip: Option<String>,
    /// Default network interface for IP resolution.
    #[serde(default)]
    pub bind_interface: Option<String>,
    /// Default IP for natmap bind and Consul service address.
    #[serde(default)]
    pub bind_ip: Option<String>,
    /// Default transport protocol (`tcp` or `udp`).
    #[serde(default)]
    pub protocol: Option<String>,
    /// Default path to the nginx config generator script.
    #[serde(default)]
    pub nginx_generator: Option<String>,
    /// Default inline preprocess script (runs on service node before
    /// storing in Consul KV).
    #[serde(default)]
    pub preprocess: Option<String>,
    /// Default inline postprocess script (runs on proxy node during
    /// nginx config assembly).
    #[serde(default)]
    pub postprocess: Option<String>,
}

/// Kernel-level NAT configuration that bypasses the nginx reverse proxy.
///
/// When set, traffic is forwarded via iptables DNAT from the proxy server
/// directly to the service host, eliminating proxy latency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForwardingConfig {
    /// Public IP on the proxy server to forward FROM.
    pub ext_ip: String,
    /// Static port(s) on the public IP. The first port (`ext_ports[0]`) is
    /// used as the natmap host port instead of an ephemeral allocation.
    pub ext_ports: Vec<u16>,
    /// Protocol for the iptables DNAT rule. Defaults to `tcp`.
    #[serde(default)]
    pub proto: Option<String>,
    /// Whether to create hairpin NAT rules so internal hosts can reach
    /// themselves via the external IP. Defaults to `false`.
    #[serde(default)]
    pub hairpin: bool,
}

/// A single network/service definition from `discovery.yaml`.
///
/// Describes one Docker Compose project's port mapping, nginx routing,
/// and optional kernel-level forwarding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkConfig {
    /// Must match the `com.docker.compose.project` Docker label.
    /// One project can have multiple entries (e.g. TCP + UDP ports).
    pub name: String,
    /// Port the service listens on inside the container. The container must
    /// expose this port via Docker (EXPOSE directive).
    pub container_port: u16,
    /// Domain names for nginx `server_name`. First domain is the primary.
    #[serde(default)]
    pub domains: Vec<String>,
    /// Nginx template type: `REVERSE_PROXY`, `REVERSE_PROXY_PRIVATE`,
    /// `STREAM`, or `STREAM_PRIVATE`. Empty string when forwarding-only.
    #[serde(default)]
    pub template: String,
    /// Transport protocol. Cascades from defaults. Defaults to `tcp`.
    #[serde(default)]
    pub protocol: Option<String>,
    /// Kernel-level NAT config. When set, bypasses nginx/nginx-daemon
    /// and uses a static port from `ext_ports[0]`.
    #[serde(default)]
    pub forwarding: Option<ForwardingConfig>,
    /// Override for the proxy server IP.
    #[serde(default)]
    pub proxy_ip: Option<String>,
    /// IP to bind the natmap host port on.
    #[serde(default)]
    pub bind_ip: Option<String>,
    /// Interface name to resolve an IP from via `ip addr show`.
    #[serde(default)]
    pub bind_interface: Option<String>,
    /// Arbitrary key-value pairs passed to the generator script as
    /// `AUTO_DISCOVER_EXTRA_<key>` env vars.
    #[serde(default)]
    pub extra: HashMap<String, String>,
    /// Path to nginx config generator script. Cascades.
    #[serde(default)]
    pub nginx_generator: Option<String>,
    /// Inline shell script run on the service node after the generator.
    /// stdin = generator output, stdout = stored config.
    #[serde(default)]
    pub preprocess: Option<String>,
    /// Inline shell script stored in Consul KV, run on the proxy.
    /// stdin = config from KV, stdout = final nginx config.
    /// Exit 1 = skip this service.
    #[serde(default)]
    pub postprocess: Option<String>,
}

impl DiscoveryConfig {
    /// Load and parse a `discovery.yaml` file from the given path.
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: DiscoveryConfig = serde_yaml::from_str(&contents)?;
        Ok(config)
    }

    /// Resolve a [`NetworkConfig`] into a [`ResolvedService`] by cascading
    /// defaults and filling in fallback values.
    ///
    /// Resolution order (last wins):
    /// 1. Hard-coded fallback
    /// 2. `defaults.*`
    /// 3. Per-network field
    pub fn resolve(&self, service: &NetworkConfig) -> ResolvedService {
        let protocol = service
            .protocol
            .clone()
            .or_else(|| service.forwarding.as_ref().and_then(|f| f.proto.clone()))
            .or_else(|| self.defaults.protocol.clone())
            .unwrap_or_else(|| "tcp".to_string());

        let proxy_ip = service
            .proxy_ip
            .clone()
            .or_else(|| self.defaults.proxy_ip.clone());

        let bind_ip = service
            .bind_ip
            .clone()
            .or_else(|| self.defaults.bind_ip.clone());

        let nginx_generator = service
            .nginx_generator
            .clone()
            .or_else(|| self.defaults.nginx_generator.clone())
            .unwrap_or_else(|| "/usr/local/bin/auto-discover-gen-nginx".to_string());

        let preprocess = service
            .preprocess
            .clone()
            .or_else(|| self.defaults.preprocess.clone())
            .unwrap_or_default();

        let postprocess = service
            .postprocess
            .clone()
            .or_else(|| self.defaults.postprocess.clone())
            .unwrap_or_default();

        ResolvedService {
            name: service.name.clone(),
            container_port: service.container_port,
            domains: service.domains.clone(),
            template: service.template.clone(),
            protocol,
            forwarding: service.forwarding.clone(),
            proxy_ip,
            bind_ip,
            bind_interface: service
                .bind_interface
                .clone()
                .or_else(|| self.defaults.bind_interface.clone()),
            extra: service.extra.clone(),
            nginx_generator,
            preprocess,
            postprocess,
        }
    }
}

/// A fully resolved service definition with all defaults applied.
///
/// This is the type used throughout the daemon for Consul registration,
/// natmap port mapping, and nginx config generation.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedService {
    /// Service name (from the `name` field in `discovery.yaml`).
    pub name: String,
    /// Container port to forward traffic to.
    pub container_port: u16,
    /// Domain names for nginx routing. The first domain is the primary.
    pub domains: Vec<String>,
    /// Nginx template identifier.
    pub template: String,
    /// Transport protocol (`tcp` or `udp`). Always set after resolution.
    pub protocol: String,
    /// Kernel-level forwarding configuration, if any.
    pub forwarding: Option<ForwardingConfig>,
    /// Proxy server IP for nginx `listen` directive.
    pub proxy_ip: Option<String>,
    /// IP to bind the natmap host port on.
    pub bind_ip: Option<String>,
    /// Network interface for IP resolution.
    pub bind_interface: Option<String>,
    /// Extra metadata passed to the nginx generator script.
    pub extra: HashMap<String, String>,
    /// Path to the nginx config generator script.
    pub nginx_generator: String,
    /// Preprocess script content (runs on service node).
    pub preprocess: String,
    /// Postprocess script content (runs on proxy node).
    pub postprocess: String,
}

impl ResolvedService {
    /// Returns the first domain name, or an empty string if none configured.
    ///
    /// This is used as the nginx `server_name` and as a discriminator in
    /// the Consul service ID to prevent collisions when multiple entries
    /// share the same name and port.
    pub fn primary_domain(&self) -> &str {
        self.domains.first().map(|s| s.as_str()).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let yaml = r#"
name: service-node-1
networks:
  - name: example-drive
    container_port: 80
    template: example-drive.ctmpl
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.name, "service-node-1");
        assert_eq!(config.networks.len(), 1);
        assert_eq!(config.networks[0].name, "example-drive");
        assert_eq!(config.networks[0].container_port, 80);
        assert!(config.networks[0].domains.is_empty());
    }

    #[test]
    fn parse_full_config() {
        let yaml = r#"
name: service-node-1
defaults:
  proxy_ip: 203.0.113.43
  protocol: tcp

networks:
  - name: example-drive
    container_port: 80
    domains:
      - drive.example.com
    template: example-drive.ctmpl
    protocol: tcp
    proxy_ip: 203.0.113.43
    extra:
      client_max_body_size: "50M"

  - name: example-mail
    container_port: 443
    domains:
      - mail.example.com
    template: example-mail.ctmpl
    bind_ip: 10.0.0.101
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.networks.len(), 2);
        assert_eq!(config.defaults.proxy_ip.as_deref(), Some("203.0.113.43"));

        let svc = &config.networks[0];
        assert_eq!(svc.extra.get("client_max_body_size").unwrap(), "50M");

        let svc2 = &config.networks[1];
        assert_eq!(svc2.bind_ip.as_deref(), Some("10.0.0.101"));
    }

    #[test]
    fn resolve_uses_defaults() {
        let yaml = r#"
name: service-node-1
defaults:
  proxy_ip: 203.0.113.43
  protocol: tcp

networks:
  - name: example-drive
    container_port: 80
    template: example-drive.ctmpl
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        let resolved = config.resolve(&config.networks[0]);
        assert_eq!(resolved.protocol, "tcp");
        assert_eq!(resolved.proxy_ip.as_deref(), Some("203.0.113.43"));
    }

    #[test]
    fn resolve_service_overrides_default() {
        let yaml = r#"
name: service-node-1
defaults:
  proxy_ip: 10.0.0.1
  protocol: tcp

networks:
  - name: example-drive
    container_port: 80
    template: example-drive.ctmpl
    protocol: udp
    proxy_ip: 203.0.113.43
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        let resolved = config.resolve(&config.networks[0]);
        assert_eq!(resolved.protocol, "udp");
        assert_eq!(resolved.proxy_ip.as_deref(), Some("203.0.113.43"));
    }

    #[test]
    fn primary_domain() {
        let resolved = ResolvedService {
            name: "test".into(),
            container_port: 80,
            domains: vec!["drive.example.com".into(), "www.example.com".into()],
            template: "t.ctmpl".into(),
            protocol: "tcp".into(),
            forwarding: None,
            proxy_ip: None,
            bind_ip: None,
            bind_interface: None,
            extra: HashMap::new(),
            nginx_generator: "/usr/local/bin/auto-discover-gen-nginx".into(),
            preprocess: String::new(),
            postprocess: String::new(),
        };
        assert_eq!(resolved.primary_domain(), "drive.example.com");
    }

    #[test]
    fn resolve_nginx_generator_default() {
        let yaml = r#"
name: service-node-1
networks:
  - name: test
    container_port: 80
    template: REVERSE_PROXY
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        let resolved = config.resolve(&config.networks[0]);
        assert_eq!(
            resolved.nginx_generator,
            "/usr/local/bin/auto-discover-gen-nginx"
        );
    }

    #[test]
    fn resolve_nginx_generator_override() {
        let yaml = r#"
name: service-node-1
defaults:
  nginx_generator: /usr/local/bin/custom-gen
networks:
  - name: test
    container_port: 80
    template: REVERSE_PROXY
    nginx_generator: /usr/local/bin/per-service-gen
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        let resolved = config.resolve(&config.networks[0]);
        assert_eq!(resolved.nginx_generator, "/usr/local/bin/per-service-gen");
    }

    #[test]
    fn resolve_preprocess_postprocess_defaults() {
        let yaml = r#"
name: service-node-1
defaults:
  preprocess: "sed s/a/b/"
  postprocess: "sed s/c/d/"
networks:
  - name: test
    container_port: 80
    template: REVERSE_PROXY
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        let resolved = config.resolve(&config.networks[0]);
        assert_eq!(resolved.preprocess, "sed s/a/b/");
        assert_eq!(resolved.postprocess, "sed s/c/d/");
    }

    #[test]
    fn resolve_preprocess_postprocess_override() {
        let yaml = r#"
name: service-node-1
networks:
  - name: test
    container_port: 80
    template: REVERSE_PROXY
    preprocess: "per-service-pre"
    postprocess: "per-service-post"
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        let resolved = config.resolve(&config.networks[0]);
        assert_eq!(resolved.preprocess, "per-service-pre");
        assert_eq!(resolved.postprocess, "per-service-post");
    }

    #[test]
    fn parse_bind_interface() {
        let yaml = r#"
name: service-node-1
networks:
  - name: test-iface
    container_port: 80
    bind_interface: dummy0
    template: t.ctmpl
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.networks[0].bind_interface.as_deref(), Some("dummy0"));
    }

    #[test]
    fn resolve_bind_ip_from_defaults() {
        let yaml = r#"
name: service-node-2
defaults:
  bind_ip: 10.0.0.102
  proxy_ip: 203.0.113.43

networks:
  - name: example-mc
    container_port: 25565
    template: t.ctmpl
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        let resolved = config.resolve(&config.networks[0]);
        assert_eq!(resolved.bind_ip.as_deref(), Some("10.0.0.102"));
        assert_eq!(resolved.proxy_ip.as_deref(), Some("203.0.113.43"));
    }

    #[test]
    fn resolve_bind_ip_override() {
        let yaml = r#"
name: service-node-2
defaults:
  bind_ip: 10.0.0.102

networks:
  - name: public-service
    container_port: 80
    template: t.ctmpl
    bind_ip: 0.0.0.0
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        let resolved = config.resolve(&config.networks[0]);
        assert_eq!(resolved.bind_ip.as_deref(), Some("0.0.0.0"));
    }

    #[test]
    fn primary_domain_empty() {
        let resolved = ResolvedService {
            name: "test".into(),
            container_port: 80,
            domains: vec![],
            template: "t.ctmpl".into(),
            protocol: "tcp".into(),
            forwarding: None,
            proxy_ip: None,
            bind_ip: None,
            bind_interface: None,
            extra: HashMap::new(),
            nginx_generator: "/usr/local/bin/auto-discover-gen-nginx".into(),
            preprocess: String::new(),
            postprocess: String::new(),
        };
        assert_eq!(resolved.primary_domain(), "");
    }

    #[test]
    fn parse_forwarding_config() {
        let yaml = r#"
name: service-node-2
networks:
  - name: example-mc
    container_port: 25565
    forwarding:
      ext_ip: 203.0.113.43
      ext_ports: [25565]
      proto: tcp
      hairpin: true
    template: ""
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        let fwd = config.networks[0].forwarding.as_ref().unwrap();
        assert_eq!(fwd.ext_ip, "203.0.113.43");
        assert_eq!(fwd.ext_ports, vec![25565]);
        assert_eq!(fwd.proto.as_deref(), Some("tcp"));
        assert!(fwd.hairpin);
    }

    #[test]
    fn resolve_passes_forwarding_through() {
        let yaml = r#"
name: service-node-2
networks:
  - name: example-mc
    container_port: 25565
    forwarding:
      ext_ip: 203.0.113.43
      ext_ports: [25565]
      proto: tcp
    template: ""
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        let resolved = config.resolve(&config.networks[0]);
        let fwd = resolved.forwarding.unwrap();
        assert_eq!(fwd.ext_ip, "203.0.113.43");
        assert_eq!(fwd.ext_ports, vec![25565]);
    }

    #[test]
    fn forwarding_hairpin_default_false() {
        let yaml = r#"
name: service-node-2
networks:
  - name: test
    container_port: 80
    forwarding:
      ext_ip: 203.0.113.43
      ext_ports: [80]
    template: ""
"#;
        let config: DiscoveryConfig = serde_yaml::from_str(yaml).unwrap();
        let fwd = config.networks[0].forwarding.as_ref().unwrap();
        assert!(!fwd.hairpin);
    }
}
