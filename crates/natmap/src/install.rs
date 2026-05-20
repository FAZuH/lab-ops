//! Systemd installation support for the natmap daemon.

use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::bail;

/// Installs the natmap daemon as a systemd service.
///
/// Performs the following steps:
///
/// 1. Copies the current binary to `binary` (unless already at that path).
/// 2. Creates the `group` system group if it does not exist.
/// 3. Adds the current user to the group (requires re-login to take effect).
/// 4. Writes a systemd service unit to `/etc/systemd/system/natmap.service`.
/// 5. Runs `systemctl daemon-reload`, then `systemctl enable --now natmap`.
pub fn install_systemd(binary: &str, group: &str) -> Result<()> {
    install_binary(binary)?;
    create_group(group)?;
    add_user_to_grp(group);
    write_service(binary, group)?;

    // Reload systemd
    println!("Reloading systemd...");
    Command::new("systemctl").arg("daemon-reload").status()?;

    enable_service()?;

    println!("natmap installed and running.");
    println!("Use `systemctl status natmap` to check.");
    println!("Use `lab-ops natmap ls` to see mappings (after re-login for group membership).");

    Ok(())
}

/// Copy current binary to the target path (unless already there)
fn install_binary(binary: &str) -> Result<()> {
    let current_exe = std::env::current_exe()?;
    let target = std::path::Path::new(binary);

    if std::fs::canonicalize(&current_exe).ok().as_ref()
        != std::fs::canonicalize(target).ok().as_ref()
    {
        println!(
            "Installing binary: {} -> {}",
            current_exe.display(),
            target.display()
        );
        std::fs::copy(&current_exe, target)?;
        let _ = Command::new("chmod").args(["755", binary]).status();
    } else {
        println!("Binary already at {}, skipping copy.", target.display());
    }

    Ok(())
}

/// Create group if not exists
fn create_group(group: &str) -> Result<()> {
    let group_exists = Command::new("getent")
        .args(["group", group])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !group_exists {
        println!("Creating group '{group}'...");
        let status = Command::new("groupadd")
            .args(["--system", group])
            .status()?;
        if !status.success() {
            bail!("Failed to create group '{group}'");
        }
        println!("Group '{group}' created.");
    }

    Ok(())
}

/// Add current user to group
fn add_user_to_grp(group: &str) {
    if let Ok(user) = std::env::var("USER")
        && !user.is_empty()
        && user != "root"
    {
        println!("Adding user '{user}' to group '{group}'...");
        let _ = Command::new("usermod")
            .args(["-a", "-G", group, &user])
            .status();
        println!(
            "User '{user}' added to group '{group}'. You may need to re-login for this to take effect."
        );
    }
}

/// Write service file with placeholder substitution
fn write_service(binary: &str, group: &str) -> Result<()> {
    let state_dir = "/var/lib/natmap/state.json";

    let rendered = include_str!("../assets/natmap.service")
        .replace("{binary}", binary)
        .replace("{state_dir}", state_dir)
        .replace("{group}", group);
    let path = std::path::Path::new("/etc/systemd/system/natmap.service");
    println!("Writing systemd service to {}", path.display());
    if path.exists() {
        println!("Service file already exists, overwriting.");
    }
    std::fs::write(path, rendered)?;

    Ok(())
}

/// Enable and start
fn enable_service() -> Result<()> {
    println!("Enabling natmap service...");
    let status = Command::new("systemctl")
        .args(["enable", "--now", "natmap"])
        .status()?;
    if !status.success() {
        bail!("Failed to enable natmap service");
    }

    Ok(())
}
