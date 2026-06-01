//! Docker API client for listing running containers.

use bollard::Docker;
use bollard::plugin::ContainerSummary;
use bollard::query_parameters::ListContainersOptionsBuilder;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;

use crate::model::ContainerInfo;

/// Wrapper around the Bollard Docker API client.
pub struct DockerClient {
    docker: Docker,
}

impl From<ContainerSummary> for ContainerInfo {
    fn from(c: ContainerSummary) -> Self {
        let id = c.id.unwrap_or_default();
        let name = c
            .names
            .unwrap_or_default()
            .first()
            .cloned()
            .map(|n| lab_ops_lab_lib::docker::trim_container_name(&n).to_string())
            .unwrap_or_default();
        let compose_project = c
            .labels
            .as_ref()
            .and_then(|labels| labels.get("com.docker.compose.project"))
            .cloned();

        Self {
            id,
            name,
            compose_project,
        }
    }
}

impl DockerClient {
    /// Connect to the local Docker daemon via the default socket or
    /// `DOCKER_HOST` env var.
    pub fn new() -> Result<Self> {
        let docker = lab_ops_lab_lib::docker::connect()?;
        Ok(DockerClient { docker })
    }

    /// Inspect all running containers.
    pub async fn list_running_containers(&self) -> Result<Vec<ContainerInfo>> {
        let options = ListContainersOptionsBuilder::default().all(false).build();

        let infos = self
            .docker
            .list_containers(Some(options))
            .await
            .wrap_err("failed to list Docker containers")?
            .into_iter()
            .map(|c| c.into())
            .collect();

        Ok(infos)
    }

    /// Inspect a single container.
    pub async fn inspect_container(&self, container_id: impl AsRef<str>) -> Result<ContainerInfo> {
        let id = container_id.as_ref();
        let info = self
            .docker
            .inspect_container(id, None)
            .await
            .wrap_err_with(|| format!("failed to inspect container {id}"))?;
        let name = info
            .name
            .as_deref()
            .map(lab_ops_lab_lib::docker::trim_container_name)
            .unwrap_or("")
            .to_string();
        let compose_project = info
            .config
            .as_ref()
            .and_then(|c| c.labels.as_ref())
            .and_then(|labels| labels.get("com.docker.compose.project"))
            .cloned();

        Ok(ContainerInfo {
            id: id.to_string(),
            name,
            compose_project,
        })
    }
}
