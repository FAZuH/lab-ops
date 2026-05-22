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
