//! Proxy-side nginx config synchronization from Consul KV.
//!
//! [`NginxDaemon`] watches the `nginx-configs/` Consul KV prefix with blocking
//! queries. On any change, it reads all `*.conf` entries, pipes them through
//! per-service postproc scripts and common postprocs from `/etc/auto-discover/postprocs.d/`,
//! writes the result to disk, creates symlinks in `sites-available` /
//! `streams-available`, and reloads nginx.
//!
//! Orphaned KV entries (where the corresponding service no longer exists in
//! the Consul catalog) are periodically garbage-collected every 5 minutes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::bail;

use crate::consul::ConsulClient;
use crate::consul::KvEntry;

/// Proxy-side daemon that watches Consul KV for nginx config changes and
/// applies them to the local nginx installation.
///
/// Uses Consul blocking queries (long-polling) to detect changes without
/// busy-waiting. Configs go through per-service postproc scripts followed
/// by lexicographically-sorted common postprocs from the `postproc_dir`.
pub struct NginxDaemon {
    consul: ConsulClient,
    tailscale_ip: String,
    tailscale_reachable: bool,
    configs_dir: PathBuf,
    sites_available: PathBuf,
    streams_available: PathBuf,
    postproc_dir: PathBuf,
    last_gc: Mutex<Instant>,
}

impl NginxDaemon {
    /// Create a new nginx daemon instance.
    ///
    /// Reads `TAILSCALE_IP` and `TAILSCALE_REACHABLE` from the environment
    /// for use by postproc scripts.
    pub fn new(
        consul_addr: String,
        configs_dir: PathBuf,
        sites_available: PathBuf,
        streams_available: PathBuf,
        postproc_dir: PathBuf,
    ) -> Self {
        let tailscale_ip = std::env::var("TAILSCALE_IP").unwrap_or_default();
        let tailscale_reachable =
            std::env::var("TAILSCALE_REACHABLE").unwrap_or_default() == "true";

        NginxDaemon {
            consul: ConsulClient::new(consul_addr),
            tailscale_ip,
            tailscale_reachable,
            configs_dir,
            sites_available,
            streams_available,
            postproc_dir,
            last_gc: Mutex::new(Instant::now()),
        }
    }

    pub fn new_default_paths(consul_addr: impl ToString) -> Self {
        Self::new(
            consul_addr.to_string(),
            PathBuf::from("/var/lib/auto-discover/nginx-configs"),
            PathBuf::from("/etc/nginx/sites-available"),
            PathBuf::from("/etc/nginx/streams-available"),
            PathBuf::from("/etc/auto-discover/postprocs.d"),
        )
    }

    /// Run the sync loop forever using Consul blocking queries.
    ///
    /// On each KV change, calls [`sync`](NginxDaemon::sync) to regenerate
    /// configs and reload nginx.
    pub async fn run_loop(&self) {
        let mut index: u64 = 0;
        loop {
            match self.sync().await {
                Ok(changed) => {
                    if changed {
                        tracing::info!("Nginx configs changed, reloading nginx");
                    } else {
                        tracing::debug!("No nginx config changes");
                    }
                }
                Err(e) => {
                    tracing::error!("Nginx sync failed: {}", e);
                }
            }

            match self
                .consul
                .list_kv_prefix_blocking("nginx-configs/", index)
                .await
            {
                Ok((_, new_index)) => {
                    index = new_index;
                }
                Err(e) => {
                    tracing::error!("Blocking query failed: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                }
            }
        }
    }

    /// One-shot sync: read all nginx configs from Consul KV, apply postprocs,
    /// write config files, create symlinks, and reload nginx.
    ///
    /// Also runs a periodic GC sweep (every 5 min) to delete orphaned KV
    /// entries whose registered services no longer exist in the Consul catalog.
    ///
    /// Returns `Ok(true)` if configs changed and nginx was reloaded.
    pub async fn sync(&self) -> Result<bool> {
        // Periodic GC: sweep orphaned nginx config KV entries every 5 min
        let should_gc = {
            let mut last_gc = self.last_gc.lock().unwrap();
            if last_gc.elapsed() >= Duration::from_secs(300) {
                *last_gc = Instant::now();
                true
            } else {
                false
            }
        };
        if should_gc && let Err(e) = self.gc_orphaned_kv_entries().await {
            tracing::warn!("nginx config GC failed: {}", e);
        }

        let entries = self
            .consul
            .list_kv_prefix("nginx-configs/")
            .await
            .wrap_err("failed to fetch nginx configs from Consul KV")?;

        let mut conf_entries: HashMap<String, &KvEntry> = HashMap::new();
        let mut postproc_entries: HashMap<String, &KvEntry> = HashMap::new();

        for entry in &entries {
            let rest = entry.key.strip_prefix("nginx-configs/").unwrap_or("");
            if rest.ends_with(".conf") {
                conf_entries.insert(entry.key.clone(), entry);
            } else if rest.ends_with(".postproc") {
                let base = entry.key.trim_end_matches(".postproc");
                postproc_entries.insert(base.to_string(), entry);
            }
        }

        let mut written = Vec::new();

        for (key, entry) in &conf_entries {
            let rest = key.strip_prefix("nginx-configs/").unwrap_or("");
            let is_stream = rest.starts_with("streams/");
            let target_dir = if is_stream {
                &self.streams_available
            } else {
                &self.sites_available
            };

            let filename = rest.split('/').next_back().unwrap_or("unknown.conf");

            let config = match self.process_config(key, entry, &postproc_entries).await {
                Ok(Some(c)) => c,
                Ok(None) => {
                    tracing::info!("Skipping nginx config {}", filename);
                    continue;
                }
                Err(e) => {
                    tracing::warn!("Failed to process {}: {}", key, e);
                    continue;
                }
            };

            let config_path = self.configs_dir.join(rest);
            if let Some(parent) = config_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&config_path, &config)?;

            let symlink_path = target_dir.join(filename);
            let _ = std::fs::remove_file(&symlink_path);
            std::os::unix::fs::symlink(&config_path, &symlink_path)?;

            written.push(filename.to_string());
        }

        let cleaned = self.cleanup_stale(&written)?;

        if written.is_empty() && !cleaned {
            return Ok(false);
        }

        self.validate_and_reload()?;

        Ok(true)
    }

    /// Process a single nginx config through per-service and common postprocs.
    ///
    /// Returns `Ok(None)` if a postproc exits non-zero (skip this service).
    async fn process_config(
        &self,
        key: &str,
        entry: &KvEntry,
        postproc_entries: &HashMap<String, &KvEntry>,
    ) -> Result<Option<String>> {
        let mut config = entry.value.clone();

        let base_key = key.trim_end_matches(".conf");
        if let Some(pp) = postproc_entries.get(base_key) {
            let script = &pp.value;
            if !script.is_empty() {
                let mut child = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(script)
                    .env("TAILSCALE_IP", &self.tailscale_ip)
                    .env(
                        "TAILSCALE_REACHABLE",
                        if self.tailscale_reachable {
                            "true"
                        } else {
                            "false"
                        },
                    )
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::inherit())
                    .spawn()?;

                let mut stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| color_eyre::eyre::eyre!("failed to take stdin"))?;
                use std::io::Write;
                stdin.write_all(config.as_bytes())?;
                drop(stdin);

                let result = child.wait_with_output()?;
                if result.status.success() {
                    config = String::from_utf8_lossy(&result.stdout).to_string();
                } else {
                    return Ok(None);
                }
            }
        }

        let mut scripts: Vec<PathBuf> = Vec::new();
        if let Ok(dir) = std::fs::read_dir(&self.postproc_dir) {
            for entry in dir.flatten() {
                if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    scripts.push(entry.path());
                }
            }
        }
        scripts.sort();

        for script in &scripts {
            let mut child = std::process::Command::new(script)
                .env("TAILSCALE_IP", &self.tailscale_ip)
                .env(
                    "TAILSCALE_REACHABLE",
                    if self.tailscale_reachable {
                        "true"
                    } else {
                        "false"
                    },
                )
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::inherit())
                .spawn()?;

            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| color_eyre::eyre::eyre!("failed to take stdin"))?;
            use std::io::Write;
            stdin.write_all(config.as_bytes())?;
            drop(stdin);

            let result = child.wait_with_output()?;
            if result.status.success() {
                config = String::from_utf8_lossy(&result.stdout).to_string();
            } else {
                return Ok(None);
            }
        }

        Ok(Some(config))
    }

    /// Remove symlinks and config files that are no longer in the active set.
    /// Skips `_maps.conf` (nginx-ui managed). Only removes files under
    /// `auto-discover/nginx-configs`.
    fn cleanup_stale(&self, active: &[String]) -> Result<bool> {
        let mut removed = false;

        for dir in [&self.sites_available, &self.streams_available] {
            if !dir.exists() {
                continue;
            }
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();

                if !path.is_symlink() {
                    continue;
                }

                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };

                if name == "_maps.conf" {
                    continue;
                }

                if active.contains(&name.to_string()) {
                    continue;
                };

                let Ok(target) = std::fs::read_link(&path) else {
                    continue;
                };

                let Some(target_str) = target.to_str() else {
                    continue;
                };

                if !target_str.contains("auto-discover/nginx-configs") {
                    continue;
                };

                let _ = std::fs::remove_file(&path);
                tracing::info!("Removed stale symlink: {}", name);
                removed = true;

                let configs_root = self.configs_dir.to_string_lossy().to_string();
                let configs_root = configs_root.trim_end_matches('/');
                let relative = target_str.trim_start_matches(configs_root);
                let relative = relative.trim_start_matches('/');
                let target_path = self.configs_dir.join(relative);
                let _ = std::fs::remove_file(&target_path);
            }
        }

        Ok(removed)
    }

    /// GC sweep: delete orphaned nginx config KV entries whose registered
    /// services no longer exist in the Consul catalog.
    ///
    /// Lists all KV entries under `nginx-configs/`, queries the catalog for
    /// all known service instance IDs, and deletes any KV entry whose
    /// service ID is not found. Handles both `.conf` and `.postproc` entries
    /// under both `sites/` and `streams/` prefixes.
    async fn gc_orphaned_kv_entries(&self) -> Result<()> {
        let entries = self
            .consul
            .list_kv_prefix("nginx-configs/")
            .await
            .wrap_err("failed to list nginx config KV entries for GC")?;

        let catalog_ids = self
            .consul
            .get_all_catalog_service_ids()
            .await
            .wrap_err("failed to query Consul catalog for GC")?;

        let mut orphaned = Vec::new();

        for entry in &entries {
            let Some(service_id) = extract_service_id(&entry.key) else {
                continue;
            };

            if !catalog_ids.contains(service_id) {
                orphaned.push(service_id.to_string());
            }
        }

        orphaned.sort();
        orphaned.dedup();

        for id in &orphaned {
            tracing::info!("GC removing orphaned nginx config KV for {}", id);
            if let Err(e) = self.consul.delete_nginx_config_kv(id).await {
                tracing::warn!("GC failed to delete KV for {}: {}", id, e);
            }
        }

        if !orphaned.is_empty() {
            tracing::info!("GC removed {} orphaned nginx config(s)", orphaned.len());
        }

        Ok(())
    }

    /// Run `nginx -t` to validate config syntax, then `systemctl reload nginx`
    /// to apply changes.
    fn validate_and_reload(&self) -> Result<()> {
        let test = std::process::Command::new("nginx").arg("-t").output()?;

        if !test.status.success() {
            let stderr = String::from_utf8_lossy(&test.stderr);
            tracing::error!("nginx -t failed: {}", stderr);
            bail!("nginx -t failed: {}", stderr.trim());
        }

        let reload = std::process::Command::new("systemctl")
            .args(["reload", "nginx"])
            .output()?;

        if !reload.status.success() {
            let stderr = String::from_utf8_lossy(&reload.stderr);
            tracing::warn!("nginx reload failed (may not be running): {}", stderr);
        } else {
            tracing::info!("nginx reloaded successfully");
        }

        Ok(())
    }
}

/// Extract the service ID from a Consul KV key under `nginx-configs/`.
///
/// Returns the service ID only for `.conf` entries (skipping `.postproc`).
///
/// # Examples
///
/// ```ignore
/// assert_eq!(extract_service_id("nginx-configs/sites/svc-123.conf"), Some("svc-123"));
/// assert_eq!(extract_service_id("nginx-configs/streams/mysvc-8080.conf"), Some("mysvc-8080"));
/// assert_eq!(extract_service_id("nginx-configs/sites/svc-123.postproc"), None);
/// ```
fn extract_service_id(kv_key: &str) -> Option<&str> {
    let rest = kv_key.strip_prefix("nginx-configs/")?;
    if !rest.ends_with(".conf") {
        return None;
    }
    rest.strip_suffix(".conf")?.split('/').next_back()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_service_id_sites_conf() {
        assert_eq!(
            extract_service_id("nginx-configs/sites/node1-example-com-32000.conf"),
            Some("node1-example-com-32000"),
        );
    }

    #[test]
    fn test_extract_service_id_streams_conf() {
        assert_eq!(
            extract_service_id("nginx-configs/streams/node2-dns-53530.conf"),
            Some("node2-dns-53530"),
        );
    }

    #[test]
    fn test_extract_service_id_postproc_is_none() {
        assert_eq!(
            extract_service_id("nginx-configs/sites/svc-123.postproc"),
            None,
        );
    }

    #[test]
    fn test_extract_service_id_unrelated_key_is_none() {
        assert_eq!(extract_service_id("some-other-prefix/foo.conf"), None);
    }

    #[test]
    fn test_extract_service_id_with_hyphens_and_numbers() {
        assert_eq!(
            extract_service_id("nginx-configs/sites/service-node-1-drive-example-com-32000.conf"),
            Some("service-node-1-drive-example-com-32000"),
        );
    }
}
