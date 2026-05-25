use std::path::PathBuf;

use bollard::query_parameters::EventsOptions;
use clap::Parser;
use clap::Subcommand;
use color_eyre::Result;
use color_eyre::eyre::bail;
use futures_util::StreamExt;
use tracing::Instrument;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::config::DiscoveryConfig;
use crate::daemon::DiscoveryDaemon;
use crate::nginx_daemon::NginxDaemon;

/// Service discovery daemon: watches Docker events, manages port forwarding
/// via `lab-ops natmap`, registers services with Consul, generates nginx
/// configs, and syncs forwarding/KV rules on the proxy server.
#[derive(Parser)]
#[command(name = "auto-discover")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Service discovery daemon with Consul integration and nginx config generation")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run all enabled daemon components (discovery, forwarding, nginx)
    Daemon {
        /// Path to discovery.yaml
        #[arg(default_value = "/etc/auto-discover/discovery.yaml")]
        config: PathBuf,
        /// State directory for port assignments
        #[arg(long, default_value = "/var/lib/auto-discover")]
        state_dir: PathBuf,
        /// Consul HTTP address
        #[arg(long, default_value = "http://127.0.0.1:8500")]
        consul_addr: String,
        /// Disable the discovery component (Docker event watching)
        #[arg(long)]
        no_discovery: bool,
        /// Disable the forwarding component (kernel DNAT sync)
        #[arg(long)]
        no_forwarding: bool,
        /// Disable the nginx component (KV config sync)
        #[arg(long)]
        no_nginx: bool,
        /// Directory for generated nginx site configs
        #[arg(long, default_value = crate::consts::NGINX_SITEENABLED)]
        nginx_sites_dir: PathBuf,
        /// Directory for generated nginx stream configs
        #[arg(long, default_value = crate::consts::NGINX_STREAMENABLED)]
        nginx_streams_dir: PathBuf,
    },
    /// Run a single sync pass and exit
    Sync {
        /// Path to discovery.yaml
        #[arg(default_value = "/etc/auto-discover/discovery.yaml")]
        config: PathBuf,
        /// State directory for port assignments
        #[arg(long, default_value = "/var/lib/auto-discover")]
        state_dir: PathBuf,
    },
    /// Validate the discovery configuration
    Check {
        /// Path to discovery.yaml
        #[arg(default_value = "/etc/auto-discover/discovery.yaml")]
        config: PathBuf,
    },
    /// Run on proxy server: sync DNAT rules from Consul (one-shot)
    ForwardingSync {
        /// Consul HTTP address
        #[arg(default_value = "http://127.0.0.1:8500")]
        consul_addr: String,
    },
    /// Run on proxy server: sync nginx configs from Consul KV (one-shot)
    NginxSync {
        /// Consul HTTP address
        #[arg(default_value = "http://127.0.0.1:8500")]
        consul_addr: String,
        /// Directory for generated nginx site configs
        #[arg(long, default_value = crate::consts::NGINX_SITEENABLED)]
        nginx_sites_dir: PathBuf,
        /// Directory for generated nginx stream configs
        #[arg(long, default_value = crate::consts::NGINX_STREAMENABLED)]
        nginx_streams_dir: PathBuf,
    },
}

pub async fn run_cli(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Daemon {
            config,
            state_dir,
            consul_addr,
            no_discovery,
            no_forwarding,
            no_nginx,
            nginx_sites_dir,
            nginx_streams_dir,
        } => {
            run_unified_daemon(
                config,
                state_dir,
                consul_addr,
                no_discovery,
                no_forwarding,
                no_nginx,
                nginx_sites_dir,
                nginx_streams_dir,
            )
            .await
        }
        Commands::Sync { config, state_dir } => run_sync(config, state_dir).await,
        Commands::Check { config } => check_config(config),
        Commands::ForwardingSync { consul_addr } => run_forwarding_sync(&consul_addr).await,
        Commands::NginxSync {
            consul_addr,
            nginx_sites_dir,
            nginx_streams_dir,
        } => run_nginx_sync(&consul_addr, nginx_sites_dir, nginx_streams_dir).await,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_unified_daemon(
    config_path: PathBuf,
    state_dir: PathBuf,
    consul_addr: String,
    no_discovery: bool,
    no_forwarding: bool,
    no_nginx: bool,
    nginx_sites_dir: PathBuf,
    nginx_streams_dir: PathBuf,
) -> Result<()> {
    if no_discovery && no_forwarding && no_nginx {
        bail!("All components disabled, nothing to do");
    }

    info!("Starting auto-discover daemon");

    if !no_discovery {
        let config = config_path.clone();
        let state = state_dir.clone();
        tokio::spawn(async move {
            info!("Discovery component started");
            run_daemon(config, state).await;
            info!("Discovery component exited");
        });
    }

    if !no_forwarding {
        let addr = consul_addr.clone();
        tokio::spawn(async move {
            info!("Forwarding component started");
            run_forwarding_daemon(addr).await;
            info!("Forwarding component exited");
        });
    }

    if !no_nginx {
        let addr = consul_addr.clone();
        let sites = nginx_sites_dir.clone();
        let streams = nginx_streams_dir.clone();
        tokio::spawn(async move {
            info!("Nginx component started");
            run_nginx_daemon(addr, sites, streams).await;
            info!("Nginx component exited");
        });
    }

    tokio::signal::ctrl_c().await?;
    info!("Shutdown signal received");
    Ok(())
}

async fn run_daemon(config_path: PathBuf, state_dir: PathBuf) {
    info!("Config: {}", config_path.display());
    info!("State dir: {}", state_dir.display());

    let daemon = DiscoveryDaemon::new(config_path.clone(), state_dir);

    let mut retries = 0u32;
    loop {
        match daemon.sync().await {
            Ok(()) => {
                info!("Initial sync succeeded");
                break;
            }
            Err(e) => {
                if retries < 10 {
                    retries += 1;
                    let delay =
                        std::time::Duration::from_secs(2u64.saturating_mul(retries as u64).min(30));
                    warn!(
                        "Initial sync failed (attempt {}/10, retrying in {:?}): {}",
                        retries, delay, e
                    );
                    tokio::time::sleep(delay).await;
                } else {
                    error!("Initial sync failed after {} attempts: {}", retries, e);
                    break;
                }
            }
        }
    }

    let docker_api = match lab_lib::docker::connect() {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to connect to Docker: {}", e);
            return;
        }
    };

    let mut stream = docker_api.events(Some(EventsOptions {
        since: None,
        until: None,
        filters: Some(
            vec![
                ("type".to_string(), vec!["container".to_string()]),
                (
                    "event".to_string(),
                    vec!["start".to_string(), "die".to_string()],
                ),
            ]
            .into_iter()
            .collect(),
        ),
    }));

    info!("Listening for Docker events...");

    let span = tracing::info_span!("event_loop", daemon = "auto-discover");
    async {
        while let Some(event) = stream.next().await {
            let e = match event {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!(error = %e, "docker event error");
                    continue;
                }
            };

            tracing::trace!(raw_event = ?e, "raw docker event");

            let actor = e.actor.as_ref();
            let attrs = actor.and_then(|a| a.attributes.as_ref());

            let container_id = actor.map(|a| a.id.as_deref().unwrap_or("")).unwrap_or("");
            let action = e.action.as_deref().unwrap_or("");
            let compose_project = attrs
                .and_then(|a| a.get("com.docker.compose.project"))
                .cloned();

            tracing::debug!(
                container.id = container_id,
                event.action = action,
                "docker event"
            );

            match action {
                "start" => {
                    if let Some(ref project) = compose_project
                        && let Err(e) = daemon
                            .handle_container_start(container_id, project, action)
                            .await
                    {
                        tracing::error!(error = %e, "container start error");
                    }
                }
                "die" => {
                    if let Err(e) = daemon.handle_container_die(container_id).await {
                        tracing::error!(error = %e, "container die error");
                    }
                }
                _ => {}
            }
        }
    }
    .instrument(span)
    .await;
}

pub async fn run_sync(config_path: PathBuf, state_dir: PathBuf) -> Result<()> {
    info!("Running sync...");
    let daemon = DiscoveryDaemon::new(config_path, state_dir);
    match daemon.sync().await {
        Ok(()) => info!("Sync completed successfully"),
        Err(e) => bail!("sync failed: {e}"),
    }
    Ok(())
}

pub fn check_config(config_path: PathBuf) -> Result<()> {
    let config = DiscoveryConfig::load(&config_path)
        .map_err(|e| color_eyre::eyre::eyre!("configuration error: {e}"))?;
    println!("Configuration is valid.");
    let resolved = config.resolve_all();
    println!("Services defined: {}", resolved.len());
    for svc in &resolved {
        let kind = if svc.local_address.is_some() {
            format!("local ({})", svc.local_address.as_deref().unwrap())
        } else {
            "docker".into()
        };
        let template = match &svc.port_type {
            crate::config::ResolvedPortType::RProxyLocal { template, .. }
            | crate::config::ResolvedPortType::RProxyRemote { template, .. } => template.clone(),
            crate::config::ResolvedPortType::ForwardLocal { .. } => "forwardlocal".into(),
            crate::config::ResolvedPortType::ForwardRemote { .. } => "forwardremote".into(),
        };
        println!(
            "  - {} (port {}, protocol {}, template {}, type {})",
            svc.service_name, svc.container_port, svc.protocol, template, kind
        );
    }
    Ok(())
}

pub async fn run_forwarding_sync(consul_addr: &str) -> Result<()> {
    info!("Running forwarding sync...");
    crate::forwarding::sync_forwarding_rules(consul_addr).await?;
    info!("Forwarding sync completed successfully");
    Ok(())
}

async fn run_forwarding_daemon(consul_addr: String) {
    info!("Forwarding daemon started");
    loop {
        match crate::forwarding::sync_forwarding_rules(&consul_addr).await {
            Ok(()) => info!("Forwarding daemon sync completed"),
            Err(e) => error!("Forwarding daemon sync failed: {}", e),
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

pub async fn run_nginx_sync(
    consul_addr: &str,
    nginx_sites_dir: PathBuf,
    nginx_streams_dir: PathBuf,
) -> Result<()> {
    info!("Running nginx sync...");
    let daemon = NginxDaemon::new(
        consul_addr.to_string(),
        PathBuf::from(crate::consts::AD_NGINX),
        nginx_sites_dir,
        nginx_streams_dir,
        PathBuf::from(crate::consts::AD_POSTPROC),
    );
    let changed = daemon.sync().await?;
    info!("Nginx sync completed, changed={}", changed);
    Ok(())
}

async fn run_nginx_daemon(
    consul_addr: String,
    nginx_sites_dir: PathBuf,
    nginx_streams_dir: PathBuf,
) {
    info!("Nginx daemon started");
    let daemon = NginxDaemon::new(
        consul_addr,
        PathBuf::from(crate::consts::AD_NGINX),
        nginx_sites_dir,
        nginx_streams_dir,
        PathBuf::from(crate::consts::AD_POSTPROC),
    );
    daemon.run_loop().await;
}
