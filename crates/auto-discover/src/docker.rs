//! Docker API client for listing running containers.

use bollard::query_parameters::ListContainersOptions;
use bollard::Docker;
use color_eyre::eyre::WrapErr;
use color_eyre::Result;

/// Wrapper around the Bollard Docker API client.
pub struct DockerClient {
    docker: Docker,
}

/// Metadata about a running Docker container.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    /// Full container ID.
    pub id: String,
    /// Container name (leading `/` stripped).
    pub name: String,
    /// Docker Compose project name from the `com.docker.compose.project` label.
    pub compose_project: Option<String>,
}

impl DockerClient {
    /// Connect to the local Docker daemon via the default socket or
    /// `DOCKER_HOST` env var.
    pub fn new() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()?;
        Ok(DockerClient { docker })
    }

    pub async fn list_running_containers(&self) -> Result<Vec<ContainerInfo>> {
        let options = ListContainersOptions {
            all: false,
            ..Default::default()
        };

        let containers = self
            .docker
            .list_containers(Some(options))
            .await
            .wrap_err("failed to list Docker containers")?;

        let infos: Vec<ContainerInfo> = containers
            .into_iter()
            .map(|c| {
                let id = c.id.unwrap_or_default();
                let names = c.names.unwrap_or_default();
                let name = names.first().cloned().unwrap_or_default();
                let compose_project = c
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get("com.docker.compose.project"))
                    .cloned();

                ContainerInfo {
                    id,
                    name: name.trim_start_matches('/').to_string(),
                    compose_project,
                }
            })
            .collect();

        Ok(infos)
    }

    /// Inspect a single container and return full metadata.
    pub async fn inspect_container(&self, container_id: &str) -> Result<ContainerInfo> {
        let info = self
            .docker
            .inspect_container(container_id, None)
            .await
            .wrap_err_with(|| format!("failed to inspect container {container_id}"))?;
        let name = info
            .name
            .as_deref()
            .unwrap_or("")
            .trim_start_matches('/')
            .to_string();
        let compose_project = info
            .config
            .as_ref()
            .and_then(|c| c.labels.as_ref())
            .and_then(|labels| labels.get("com.docker.compose.project"))
            .cloned();

        Ok(ContainerInfo {
            id: container_id.to_string(),
            name,
            compose_project,
        })
    }
}
