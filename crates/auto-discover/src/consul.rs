//! Consul HTTP API client for service registration and KV storage.

use std::collections::HashMap;
use std::collections::HashSet;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::bail;
use lab_ops_lab_lib::TransportProtocol;
use serde_json::json;

use crate::config::ResolvedService;

/// A single key-value entry from Consul's KV store.
///
/// Values are automatically base64-decoded by the client.
#[derive(Debug, Clone)]
pub struct KvEntry {
    /// Full key path (e.g. `nginx-configs/sites/svc-123.conf`).
    pub key: String,
    /// Decoded plain-text value.
    pub value: String,
}

#[derive(serde::Deserialize)]
#[allow(non_snake_case)]
struct KvRawEntry {
    Key: String,
    Value: String,
}

fn base64_decode(s: &str) -> Option<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .ok()?;
    Some(String::from_utf8(bytes).unwrap_or_default())
}

/// Client for the Consul HTTP API (service registration, KV store, catalog queries).
pub struct ConsulClient {
    http_addr: String,
    client: reqwest::Client,
}

/// A service registration payload for the Consul agent API.
#[derive(Debug, Clone)]
pub struct ConsulServiceRegistration {
    /// Unique service instance ID (e.g. `node-name-domain-slug-port`).
    pub id: String,
    /// Service name (from `discovery.yaml`).
    pub name: String,
    /// IP address for the health check and service address.
    pub address: String,
    /// Port for the health check.
    pub port: u16,
    /// Arbitrary metadata (domain, template, protocol, forwarding info, etc.).
    pub meta: HashMap<String, String>,
    /// Health check definition (TCP or UDP netcat).
    pub check: serde_json::Value,
}

impl ConsulClient {
    /// Create a new client connected to the given Consul HTTP address.
    pub fn new(http_addr: String) -> Self {
        ConsulClient {
            http_addr,
            client: reqwest::Client::new(),
        }
    }

    /// Create a client using the `CONSUL_HTTP_ADDR` env var, defaulting to
    /// `http://127.0.0.1:8500`.
    pub fn from_env() -> Self {
        let addr = std::env::var("CONSUL_HTTP_ADDR")
            .unwrap_or_else(|_| "http://127.0.0.1:8500".to_string());
        Self::new(addr)
    }

    /// Register a service with the local Consul agent via `PUT /v1/agent/service/register`.
    ///
    /// Span fields: `consul.svc_id`, `consul.addr`.
    #[tracing::instrument(skip_all, fields(consul.svc_id = %registration.id, consul.addr = %registration.address))]
    pub async fn register_service(&self, registration: &ConsulServiceRegistration) -> Result<()> {
        let payload = json!({
            "ID": registration.id,
            "Name": registration.name,
            "Address": registration.address,
            "Port": registration.port,
            "Meta": registration.meta,
            "Check": registration.check,
        });

        let url = format!("{}/v1/agent/service/register", self.http_addr);
        let resp = self
            .client
            .put(&url)
            .json(&payload)
            .send()
            .await
            .wrap_err("Consul HTTP request failed")?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Consul API error: {}", body.trim());
        }
        Ok(())
    }

    /// Deregister a single service by its service ID.
    pub async fn deregister_service(&self, service_id: &str) -> Result<()> {
        let url = format!(
            "{}/v1/agent/service/deregister/{}",
            self.http_addr, service_id
        );
        let resp = self
            .client
            .put(&url)
            .send()
            .await
            .wrap_err("Consul HTTP request failed")?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Consul API error: {}", body.trim());
        }
        Ok(())
    }

    /// Find and deregister all services whose `Meta.container_id` matches
    /// the given container ID. Returns the list of deregistered service IDs.
    ///
    /// Span fields: `consul.svc_id`.
    #[tracing::instrument(skip_all, fields(consul.svc_id = tracing::field::Empty))]
    pub async fn deregister_services_by_container(
        &self,
        container_id: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let filter = format!("Meta.container_id==\"{container_id}\"");
        let services = self.get_agent_services_by_filter(&filter).await?;
        let mut deregistered = Vec::new();
        for (id, svc) in services {
            if let Err(e) = self.deregister_service(&id).await {
                tracing::warn!("Failed to deregister {}: {}", id, e);
            } else {
                deregistered.push(svc);
            }
        }
        Ok(deregistered)
    }

    /// Deregister services for a server whose `generation_id` or
    /// `server_name` no longer appears in `current_ids`.
    /// Returns the list of stale service IDs that were removed.
    pub async fn deregister_stale_services(
        &self,
        server_name: &str,
        current_ids: &[String],
    ) -> Result<Vec<String>> {
        let filter = format!("Meta.server_name==\"{server_name}\"");
        let services = self.get_agent_services_by_filter(&filter).await?;
        let stale: Vec<String> = services
            .keys()
            .filter(|id| !current_ids.contains(id))
            .cloned()
            .collect();
        for id in &stale {
            if let Err(e) = self.deregister_service(id).await {
                tracing::warn!("Failed to deregister stale {}: {}", id, e);
            }
        }
        Ok(stale)
    }

    /// Query the local Consul agent for services matching a filter expression.
    ///
    /// Filter syntax follows Consul's [agent/services filtering](https://developer.hashicorp.com/consul/api-docs/agent/service#filtering).
    pub async fn get_agent_services_by_filter(
        &self,
        filter: &str,
    ) -> Result<HashMap<String, serde_json::Value>> {
        let url = format!(
            "{}/v1/agent/services?filter={}",
            self.http_addr,
            urlencoding(filter)
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .wrap_err("Consul HTTP request failed")?;
        let body = resp
            .json::<HashMap<String, serde_json::Value>>()
            .await
            .wrap_err("Consul HTTP request failed")?;
        Ok(body)
    }

    /// Query the Consul catalog across all datacenters for services whose
    /// metadata key-value matches the given filter.
    ///
    /// Used by the forwarding daemon to discover services with
    /// `Meta.forwarding=="true"` regardless of which agent registered them.
    pub async fn get_catalog_services_by_meta(
        &self,
        meta_key: &str,
        meta_value: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let services_url = format!("{}/v1/catalog/services", self.http_addr);
        let resp = self
            .client
            .get(&services_url)
            .send()
            .await
            .wrap_err("Consul HTTP request failed")?;
        let catalog: HashMap<String, Vec<String>> =
            resp.json().await.wrap_err("Consul HTTP request failed")?;

        let mut results = Vec::new();

        for svc_name in catalog.keys() {
            let health_url = format!(
                "{}/v1/health/service/{}",
                self.http_addr,
                urlencoding(svc_name)
            );
            let resp = self
                .client
                .get(&health_url)
                .send()
                .await
                .wrap_err("Consul HTTP request failed")?;
            let instances: Vec<serde_json::Value> =
                resp.json().await.wrap_err("Consul HTTP request failed")?;

            for instance in instances {
                let Some(svc) = instance.get("Service") else {
                    continue;
                };
                let Some(meta) = svc.get("Meta") else {
                    continue;
                };
                let Some(val) = meta.get(meta_key).and_then(|v| v.as_str()) else {
                    continue;
                };
                if val == meta_value {
                    results.push(svc.clone());
                }
            }
        }

        Ok(results)
    }

    /// Store a value at the given KV key.
    pub async fn put_kv(&self, key: &str, value: &str) -> Result<()> {
        let url = format!("{}/v1/kv/{}", self.http_addr, key);
        let resp = self
            .client
            .put(&url)
            .body(value.to_owned())
            .send()
            .await
            .wrap_err("Consul HTTP request failed")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Consul API error: {}", body.trim());
        }
        Ok(())
    }

    /// List all KV entries under a prefix. Returns an empty vec if the
    /// prefix does not exist.
    pub async fn list_kv_prefix(&self, prefix: &str) -> Result<Vec<KvEntry>> {
        let url = format!("{}/v1/kv/{}?recurse=true", self.http_addr, prefix);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .wrap_err("Consul HTTP request failed")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        let entries: Vec<KvRawEntry> = resp.json().await.wrap_err("Consul HTTP request failed")?;
        Ok(entries
            .into_iter()
            .map(|e| KvEntry {
                key: e.Key,
                value: base64_decode(&e.Value).unwrap_or_default(),
            })
            .collect())
    }

    /// Long-poll the Consul KV prefix with a blocking query.
    ///
    /// Returns `(entries, new_index)` where `new_index` should be passed
    /// in the next call for continuous watching. Blocks up to 55 seconds.
    pub async fn list_kv_prefix_blocking(
        &self,
        prefix: &str,
        index: u64,
    ) -> Result<(Vec<KvEntry>, u64)> {
        let url = format!(
            "{}/v1/kv/{}?recurse=true&wait=55s&index={}",
            self.http_addr, prefix, index
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .wrap_err("Consul HTTP request failed")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok((vec![], index));
        }
        let new_index: u64 = resp
            .headers()
            .get("x-consul-index")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(index);
        let entries: Vec<KvRawEntry> = resp.json().await.wrap_err("Consul HTTP request failed")?;
        Ok((
            entries
                .into_iter()
                .map(|e| KvEntry {
                    key: e.Key,
                    value: base64_decode(&e.Value).unwrap_or_default(),
                })
                .collect(),
            new_index,
        ))
    }

    /// Delete a single KV key. 404 responses are silently ignored.
    pub async fn delete_kv(&self, key: &str) -> Result<()> {
        let url = format!("{}/v1/kv/{}", self.http_addr, key);
        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .wrap_err("Consul HTTP request failed")?;
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
            let body = resp.text().await.unwrap_or_default();
            bail!("Consul API error: {}", body.trim());
        }
        Ok(())
    }

    /// Delete all nginx config and postproc KV entries for a service ID
    /// (both `sites/` and `streams/` prefixes).
    pub async fn delete_nginx_config_kv(&self, service_id: &str) -> Result<()> {
        for prefix in &["sites", "streams"] {
            let _ = self
                .delete_kv(&format!("nginx-configs/{prefix}/{service_id}.conf"))
                .await;
            let _ = self
                .delete_kv(&format!("nginx-configs/{prefix}/{service_id}.postproc"))
                .await;
        }
        Ok(())
    }

    /// Query the Consul catalog cluster-wide for all registered service
    /// instance IDs.
    ///
    /// Iterates over all service names in the catalog and collects every
    /// `ServiceID`. Used by the nginx daemon GC to detect orphaned KV
    /// entries whose service no longer exists.
    pub async fn get_all_catalog_service_ids(&self) -> Result<HashSet<String>> {
        let services_url = format!("{}/v1/catalog/services", self.http_addr);
        let resp = self
            .client
            .get(&services_url)
            .send()
            .await
            .wrap_err("Consul HTTP request failed")?;
        let catalog: HashMap<String, Vec<String>> =
            resp.json().await.wrap_err("Consul HTTP request failed")?;

        let mut ids = HashSet::new();

        for svc_name in catalog.keys() {
            let svc_url = format!(
                "{}/v1/catalog/service/{}",
                self.http_addr,
                urlencoding(svc_name)
            );
            let resp = self
                .client
                .get(&svc_url)
                .send()
                .await
                .wrap_err("Consul HTTP request failed")?;
            let instances: Vec<serde_json::Value> =
                resp.json().await.wrap_err("Consul HTTP request failed")?;

            for instance in instances {
                if let Some(sid) = instance
                    .get("ServiceID")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                {
                    ids.insert(sid);
                }
            }
        }

        Ok(ids)
    }
}

impl ConsulServiceRegistration {
    /// Create a new [`ConsulServiceRegistration`] from a resolved service definition.
    ///
    /// Constructs the service ID from `server_name`, the primary domain slug,
    /// and the host port. Creates TCP or UDP health checks based on the
    /// protocol. Populates metadata including domain, template, proxy_ip,
    /// and optional forwarding info.
    pub fn new(
        service: &ResolvedService,
        host_port: u16,
        server_name: &str,
        generation_id: &str,
        container_id: &str,
        bind_ip: &str,
    ) -> Self {
        let domain = service.primary_domain().to_string();
        let domain_slug = service.domain_slug();
        let service_id = if domain_slug == "_" || domain_slug.is_empty() {
            format!("{}-{}-{}", server_name, service.service_name, host_port)
        } else {
            format!("{server_name}-{domain_slug}-{host_port}")
        };
        let protocol = service.protocol;

        let mut meta = HashMap::new();
        meta.insert("domain".into(), domain);
        meta.insert("protocol".into(), protocol.to_string());
        meta.insert("server_name".into(), server_name.to_string());
        meta.insert("generation_id".into(), generation_id.to_string());
        meta.insert("container_id".into(), container_id.to_string());

        if let Some(ref proxy_on) = service.proxy_on {
            meta.insert("proxy_on".into(), proxy_on.clone());
        }

        for (k, v) in &service.extra {
            meta.insert(k.clone(), v.clone());
        }

        use crate::config::ResolvedPortType::*;
        match &service.port_type {
            RProxyLocal {
                template, proxy_ip, ..
            }
            | RProxyRemote {
                template, proxy_ip, ..
            } => {
                if !template.is_empty() {
                    meta.insert("template".into(), template.clone());
                }
                if let Some(proxy_ip) = proxy_ip {
                    meta.insert("proxy_ip".into(), proxy_ip.clone());
                }
            }
            ForwardLocal { .. } => {
                meta.insert("forwarding".into(), "true".into());
                meta.insert("forwarding_type".into(), "local".into());
            }
            ForwardRemote {
                ext_ip,
                ext_ports,
                hairpin,
                preserve_src_ip,
                preserve_src_ip_gateway,
                preserve_src_ip_src,
                ..
            } => {
                meta.insert("forwarding".into(), "true".into());
                meta.insert("forwarding_type".into(), "remote".into());
                meta.insert("ext_ip".into(), ext_ip.clone());
                meta.insert(
                    "ext_ports".into(),
                    ext_ports
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                );
                if *hairpin {
                    meta.insert("hairpin".into(), "true".into());
                }
                if *preserve_src_ip {
                    meta.insert("preserve_src_ip".into(), "true".into());
                    if let Some(gw) = preserve_src_ip_gateway {
                        meta.insert("preserve_src_ip_gateway".into(), gw.clone());
                    }
                    let mut src_ip = preserve_src_ip_src.clone();
                    if src_ip.is_none() {
                        if let Some(ref ip) = service.bind_ip {
                            src_ip = Some(ip.clone());
                        } else if let Some(ref iface) = service.bind_interface {
                            src_ip = crate::daemon::resolve_interface_ip(iface);
                        }
                        if src_ip.is_none() {
                            src_ip = Some(bind_ip.to_string());
                        }
                    }
                    if let Some(src) = src_ip {
                        meta.insert("preserve_src_ip_src".into(), src);
                    }
                }
            }
        }

        let check = match protocol {
            TransportProtocol::Tcp => {
                json!({
                    "TCP": format!("{}:{}", bind_ip, host_port),
                    "Interval": "30s",
                    "Timeout": "10s",
                    "DeregisterCriticalServiceAfter": "5m"
                })
            }
            TransportProtocol::Udp => {
                json!({
                    "Name": format!("UDP check for {}", service.service_name),
                    "Args": ["/usr/bin/nc", "-uz", bind_ip, &host_port.to_string()],
                    "Interval": "30s",
                    "Timeout": "10s",
                    "DeregisterCriticalServiceAfter": "5m"
                })
            }
        };

        Self {
            id: service_id,
            name: service.service_name.clone(),
            address: bind_ip.to_string(),
            port: host_port,
            meta,
            check,
        }
    }
}

/// Compute a deterministic generation ID combining `server_name` and a
/// hex config hash. Used for stale-service cleanup during reconfiguration.
pub fn compute_generation_id(server_name: &str, config_hash: &str) -> String {
    format!("{server_name}-{config_hash}")
}

fn urlencoding(s: &str) -> String {
    s.replace('\"', "%22")
        .replace(' ', "%20")
        .replace('=', "%3D")
        .replace('{', "%7B")
        .replace('}', "%7D")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedPortType;
    use crate::config::ServiceType;

    #[test]
    fn build_consul_service() {
        let mut extra = HashMap::new();
        extra.insert("client_max_body_size".into(), "50M".into());

        let service = ResolvedService {
            service_id_prefix: "example-drive".into(),
            service_name: "example-drive".into(),
            service_type: ServiceType::Docker,
            match_cfg: None,
            local_address: None,
            container_port: 80,
            proxy_on: None,
            bind_ip: None,
            bind_interface: None,
            protocol: TransportProtocol::Tcp,
            extra,
            port_type: ResolvedPortType::RProxyLocal {
                template: "example-drive.ctmpl".into(),
                domains: vec!["drive.example.com".into()],
                proxy_on: None,
                proxy_ip: Some("203.0.113.43".into()),
                nginx_generator: "/usr/local/bin/auto-discover-gen-nginx".into(),
                preprocess: String::new(),
                postprocess: String::new(),
            },
        };

        let reg = ConsulServiceRegistration::new(
            &service,
            32000,
            "service-node-1",
            "gen-123",
            "abcdef",
            "10.0.0.101",
        );

        assert_eq!(reg.id, "service-node-1-drive-example-com-32000");
        assert_eq!(reg.name, "example-drive");
        assert_eq!(reg.address, "10.0.0.101");
        assert_eq!(reg.port, 32000);
        assert_eq!(reg.meta.get("domain").unwrap(), "drive.example.com");
        assert_eq!(reg.meta.get("template").unwrap(), "example-drive.ctmpl");
        assert_eq!(reg.meta.get("proxy_ip").unwrap(), "203.0.113.43");
        assert_eq!(reg.meta.get("server_name").unwrap(), "service-node-1");
        assert_eq!(reg.meta.get("generation_id").unwrap(), "gen-123");
        assert_eq!(reg.meta.get("container_id").unwrap(), "abcdef");
        assert_eq!(reg.meta.get("client_max_body_size").unwrap(), "50M");
        assert!(reg.check.get("TCP").is_some());
    }

    #[test]
    fn build_consul_service_udp_check() {
        let service = ResolvedService {
            service_id_prefix: "dns".into(),
            service_name: "dns".into(),
            service_type: ServiceType::Docker,
            match_cfg: None,
            local_address: None,
            container_port: 53,
            proxy_on: None,
            bind_ip: None,
            bind_interface: None,
            protocol: TransportProtocol::Udp,
            extra: HashMap::new(),
            port_type: ResolvedPortType::RProxyLocal {
                template: "dns.ctmpl".into(),
                domains: vec!["dns.example.com".into()],
                proxy_on: None,
                proxy_ip: None,
                nginx_generator: "/usr/local/bin/auto-discover-gen-nginx".into(),
                preprocess: String::new(),
                postprocess: String::new(),
            },
        };

        let reg = ConsulServiceRegistration::new(
            &service,
            53530,
            "service-node-1",
            "gen-1",
            "xyz",
            "10.0.0.101",
        );

        assert_eq!(reg.meta.get("protocol").unwrap(), "udp");
        assert!(reg.check.get("Args").is_some());
        assert!(reg.check.get("TCP").is_none());
    }

    #[test]
    fn generation_id_format() {
        let id = compute_generation_id("service-node-1", "a1b2c3");
        assert_eq!(id, "service-node-1-a1b2c3");
    }

    #[test]
    fn url_encoding_format() {
        let encoded = urlencoding("\"key\"=value");
        assert_eq!(encoded, "%22key%22%3Dvalue");
    }

    #[test]
    fn build_consul_service_with_forwarding() {
        let service = ResolvedService {
            service_id_prefix: "example-mc".into(),
            service_name: "example-mc".into(),
            service_type: ServiceType::Docker,
            match_cfg: None,
            local_address: None,
            container_port: 25565,
            proxy_on: None,
            bind_ip: None,
            bind_interface: None,
            protocol: TransportProtocol::Tcp,
            extra: HashMap::new(),
            port_type: ResolvedPortType::ForwardRemote {
                ext_ip: "203.0.113.43".into(),
                ext_ports: vec![25565],
                hairpin: true,
                proxy_on: None,
                preserve_src_ip: true,
                preserve_src_ip_gateway: None,
                preserve_src_ip_src: None,
            },
        };

        let reg = ConsulServiceRegistration::new(
            &service,
            25565,
            "service-node-2",
            "gen-1",
            "abcdef",
            "10.0.0.102",
        );

        assert_eq!(reg.meta.get("forwarding").unwrap(), "true");
        assert_eq!(reg.meta.get("forwarding_type").unwrap(), "remote");
        assert_eq!(reg.meta.get("ext_ip").unwrap(), "203.0.113.43");
        assert_eq!(reg.meta.get("ext_ports").unwrap(), "25565");
        assert_eq!(reg.meta.get("hairpin").unwrap(), "true");
        assert_eq!(reg.meta.get("preserve_src_ip").unwrap(), "true");
    }
}
