use color_eyre::eyre::bail;

pub fn install_systemd(binary: &str, group: &str) -> color_eyre::Result<()> {
    use std::process::Command;

    let current_exe = std::env::current_exe()?;
    let target = std::path::Path::new(binary);

    // Copy current binary to the target path (unless already there)
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

    let service_file = include_str!("../assets/natmap.service");

    // Create group if not exists
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

    // Add current user to the group
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

    // Write service file
    let path = std::path::Path::new("/etc/systemd/system/natmap.service");
    println!("Writing systemd service to {}", path.display());
    if path.exists() {
        println!("Service file already exists, overwriting.");
    }
    std::fs::write(path, service_file)?;

    // Reload systemd
    println!("Reloading systemd...");
    Command::new("systemctl").arg("daemon-reload").status()?;

    // Enable and start
    println!("Enabling natmap service...");
    let status = Command::new("systemctl")
        .args(["enable", "--now", "natmap"])
        .status()?;
    if !status.success() {
        bail!("Failed to enable natmap service");
    }

    println!("natmap installed and running.");
    println!("Use `systemctl status natmap` to check.");
    println!("Use `lab-ops natmap list` to see mappings (after re-login for group membership).");

    Ok(())
}
