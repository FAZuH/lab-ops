/// The lab-ops binary installed on the host system.
pub const BIN: &str = "/usr/local/bin/lab-ops";

/// The lab-ops CLI command name (for subprocess invocations).
pub const CMD: &str = "lab-ops";

/// Subcommand name for the DNS zone-to-Ansible converter.
pub const CMD_CF2ANSIBLE: &str = "cf2ansible";
/// Subcommand name for the DNS zone-to-Terraform converter.
pub const CMD_CF2TERRA: &str = "cf2terra";
/// Subcommand name for the Docker network viewer.
pub const CMD_DOCKERNET: &str = "dockernet";
/// Subcommand name for the NAT mapping tool.
pub const CMD_NATMAP: &str = "natmap";
/// Subcommand name for the service discovery daemon.
pub const CMD_AUTO_DISCOVER: &str = "auto-discover";

