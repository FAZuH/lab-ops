//! Docker client helpers used by both natmap and auto-discover.

use bollard::Docker;
use color_eyre::eyre::Result;

/// Connect to the local Docker daemon via the default socket or `DOCKER_HOST` env var.
pub fn connect() -> Result<Docker> {
    Ok(Docker::connect_with_local_defaults()?)
}

/// Strip the leading `/` that Docker prepends to container names.
///
/// Docker names come in the format `"/example-drive"`. This returns `"example-drive"`.
pub fn trim_container_name(name: &str) -> &str {
    name.trim_start_matches('/')
}
