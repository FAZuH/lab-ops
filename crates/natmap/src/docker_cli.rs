use clap::Parser;
use clap::Subcommand;
use color_eyre::Result;
use comfy_table::Table;
use hyper::Method;

use crate::install::install_systemd;
use crate::models::ActivePortMapping;
use crate::models::AddMappingRequest;
use crate::models::RemapRequest;
use crate::utils::request_json;

#[derive(Parser)]
pub struct Cli {
    #[arg(long, default_value = "/run/dockernatmap.sock")]
    pub socket: String,

    #[arg(long)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// List all active port mappings
    List {
        /// Filter by container ID or name
        #[arg(value_name = "CONTAINER_ID")]
        container_id: Option<String>,
    },
    /// Remap a host port for a running container
    Remap {
        /// Target container ID or name
        #[arg(value_name = "CONTAINER_ID")]
        container_id: String,
        /// Port mapping (e.g., 8080:9090)
        #[arg(value_name = "OLD_PORT:NEW_PORT")]
        mapping: String,
    },
    /// Add a new port mapping to a running container
    Add {
        /// Target container ID or name
        #[arg(value_name = "CONTAINER_ID")]
        container_id: String,
        /// Port mapping (e.g., 8443:443/tcp or 10.0.0.1:8443:443/tcp)
        #[arg(value_name = "[HOST_IP:]HOST_PORT:CONTAINER_PORT[/PROTO]")]
        mapping: String,
    },
    /// Remove a specific port mapping
    Remove {
        /// Target container ID or name
        #[arg(value_name = "CONTAINER_ID")]
        container_id: Option<String>,
        /// Port mapping to remove (e.g., 8080/tcp)
        #[arg(value_name = "PORT[/PROTO]")]
        port: Option<String>,
        /// Remove all port mappings for the container
        #[arg(long)]
        all: bool,
        /// Remove by mapping ID
        #[arg(long)]
        id: Option<u64>,
    },
    /// Run the dockernatmap daemon
    Daemon {
        /// State file path
        #[arg(long, default_value = "/var/lib/dockernatmap/state.json")]
        state_dir: String,
        /// Unix socket path
        #[arg(long, default_value = "/run/dockernatmap.sock")]
        socket: String,
        /// Group to own the Unix socket
        #[arg(long, default_value = "dockernatmap")]
        socket_group: String,
    },
    /// Install dockernatmap as a systemd service
    Install {
        /// Group for socket access (user will be added to this group)
        #[arg(long, default_value = "dockernatmap")]
        group: String,
        /// Path to the daemon binary
        #[arg(long, default_value = "/usr/local/bin/lab-ops")]
        binary: String,
    },
}

pub async fn run_cli() -> Result<()> {
    run_cli_with_args(Cli::parse()).await
}

pub async fn run_cli_with_args(cli: Cli) -> Result<()> {
    match cli.command {
        Command::List { container_id } => {
            list(container_id, &cli.socket, cli.json).await?;
        }
        Command::Remap {
            container_id,
            mapping,
        } => {
            remap(container_id, mapping, &cli.socket, cli.json).await?;
        }
        Command::Add {
            container_id,
            mapping,
        } => {
            add(container_id, mapping, &cli.socket, cli.json).await?;
        }
        Command::Remove {
            container_id,
            port,
            all,
            id,
        } => {
            remove(container_id, port, all, id, &cli.socket, cli.json).await?;
        }
        Command::Daemon {
            state_dir,
            socket,
            socket_group,
        } => {
            crate::daemon::run_daemon_with_paths(&socket, &state_dir, &socket_group).await?;
        }
        Command::Install { binary, group } => {
            install_systemd(&binary, &group)?;
        }
    }

    Ok(())
}

pub async fn list(container_id: Option<String>, socket: &str, json: bool) -> Result<()> {
    let res: Vec<ActivePortMapping> =
        request_json(socket, Method::GET, "/mappings", None::<()>).await?;
    let res = if let Some(cid) = container_id {
        res.into_iter()
            .filter(|m| m.container_id.starts_with(&cid) || m.container_name == cid)
            .collect()
    } else {
        res
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&res)?);
    } else {
        let mut table = Table::new();
        table.set_header(vec![
            "ID",
            "CONTAINER",
            "CONTAINER ID",
            "HOST ADDR",
            "CONTAINER ADDR",
            "PROTO",
        ]);
        for m in res {
            table.add_row(vec![
                m.id.to_string(),
                m.container_name,
                m.container_id.chars().take(12).collect::<String>(),
                m.request.host_addr.to_string(),
                m.request.container_addr.to_string(),
                m.request.proto.to_string(),
            ]);
        }
        println!("{table}");
    }

    Ok(())
}

pub async fn remap(container_id: String, mapping: String, socket: &str, json: bool) -> Result<()> {
    let parts: Vec<&str> = mapping.split(':').collect();
    if parts.len() != 2 {
        color_eyre::eyre::bail!("Invalid mapping format. Use <old_host_port>:<new_host_port>");
    }
    let req = RemapRequest {
        host_port: parts[0].parse()?,
        new_host_port: parts[1].parse()?,
    };
    let uri = format!("/remap/{}", container_id);
    let res: Vec<ActivePortMapping> = request_json(socket, Method::PUT, &uri, Some(req)).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&res)?);
    } else {
        println!("Successfully remapped {} rules", res.len());
    }

    Ok(())
}

pub async fn add(container_id: String, mapping: String, socket: &str, json: bool) -> Result<()> {
    // Parse proto from trailing /proto suffix
    let (mapping_part, proto) = match mapping.split_once('/') {
        Some((m, p)) => (m, p.to_string()),
        None => (mapping.as_str(), "tcp".to_string()),
    };

    // Split by : to determine if IP is provided
    let parts: Vec<&str> = mapping_part.split(':').collect();
    let (host_ip, host_port, container_port) = match parts.len() {
        3 => (parts[0].to_string(), parts[1].parse()?, parts[2].parse()?),
        2 => ("0.0.0.0".to_string(), parts[0].parse()?, parts[1].parse()?),
        _ => color_eyre::eyre::bail!(
            "Invalid mapping format. Use [HOST_IP:]HOST_PORT:CONTAINER_PORT[/PROTO] (e.g., 8080:80 or 10.0.0.1:8080:80/tcp)"
        ),
    };

    let req = AddMappingRequest {
        host_ip,
        host_port,
        container_port,
        proto,
    };
    let uri = format!("/mapping/{}", container_id);
    let res: ActivePortMapping = request_json(socket, Method::POST, &uri, Some(req)).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&res)?);
    } else {
        println!("Successfully added mapping.");
    }

    Ok(())
}

pub async fn remove(
    container_id: Option<String>,
    port: Option<String>,
    all: bool,
    id: Option<u64>,
    socket: &str,
    json: bool,
) -> Result<()> {
    if let Some(mapping_id) = id {
        let uri = format!("/mapping/by-id/{}", mapping_id);
        let _res: () = request_json(socket, Method::DELETE, &uri, None::<()>).await?;
        if !json {
            println!("Successfully removed mapping {mapping_id}.");
        }
    } else if all {
        color_eyre::eyre::bail!("--all not implemented yet");
    } else if let (Some(cid), Some(p)) = (container_id, port) {
        let port_num: u16 = p.split('/').next().unwrap().parse()?;
        let uri = format!("/mapping/{}/{}", cid, port_num);
        let _res: () = request_json(socket, Method::DELETE, &uri, None::<()>).await?;
        if !json {
            println!("Successfully removed mapping.");
        }
    } else {
        color_eyre::eyre::bail!("Specify either --id <ID>, or <CONTAINER_ID> <PORT>, or --all");
    }

    Ok(())
}
