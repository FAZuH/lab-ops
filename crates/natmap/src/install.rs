//! Systemd installation support for the natmap daemon and its converge timer.

use std::path::Path;
use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::bail;

/// Seconds between converge ticks when `--interval` is not given.
///
/// Short enough that an edited declaration is reflected without a manual
/// command, long enough that a converged node costs one read per tick.
pub const DEFAULT_CONVERGE_INTERVAL_SECS: u64 = 15;

/// Installs the natmap daemon as a systemd service, and optionally a timer that
/// converges this node to `declaration`.
///
/// Performs the following steps:
///
/// 1. Copies the current binary to `binary` (unless already at that path).
/// 2. Creates the `group` system group if it does not exist.
/// 3. Adds the current user to the group (requires re-login to take effect).
/// 4. Writes a systemd service unit to `/etc/systemd/system/natmap.service`.
/// 5. Runs `systemctl daemon-reload`, then `systemctl enable --now natmap`.
/// 6. With `declaration`, also writes `natmap-converge.service` and
///    `natmap-converge.timer`, and enables the timer.
pub fn install_systemd(
    binary: &str,
    group: &str,
    declaration: Option<&Path>,
    interval_secs: u64,
) -> Result<()> {
    install_binary(binary)?;
    create_group(group)?;
    add_user_to_grp(group);
    write_service(binary, group)?;
    if let Some(declaration) = declaration {
        write_converge_units(binary, declaration, interval_secs)?;
    }

    println!("Reloading systemd...");
    Command::new("systemctl").arg("daemon-reload").status()?;

    enable_service()?;
    if declaration.is_some() {
        enable_timer()?;
    }

    println!("natmap installed and running.");
    println!("Use `systemctl status natmap` to check.");
    println!("Use `lab-ops natmap ls` to see mappings (after re-login for group membership).");

    Ok(())
}

/// Renders the converge service and timer units for `declaration`.
///
/// `declaration` is an absolute path: the unit runs from `/`, so a relative path
/// would resolve somewhere else entirely.
pub fn render_converge_units(
    binary: &str,
    declaration: &Path,
    interval_secs: u64,
) -> Result<(String, String)> {
    if interval_secs == 0 {
        bail!("--interval must be at least 1 second");
    }

    let interval = format!("{interval_secs}s");
    let declaration = declaration.display().to_string();
    let service = include_str!("../assets/natmap-converge.service")
        .replace("{binary}", binary)
        .replace("{declaration}", &declaration);
    let timer = include_str!("../assets/natmap-converge.timer").replace("{interval}", &interval);
    Ok((service, timer))
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

fn enable_timer() -> Result<()> {
    println!("Enabling natmap converge timer...");
    let status = Command::new("systemctl")
        .args(["enable", "--now", "natmap-converge.timer"])
        .status()?;
    if !status.success() {
        bail!("Failed to enable natmap converge timer");
    }

    Ok(())
}

/// Writes the converge service and timer units for `declaration`.
fn write_converge_units(binary: &str, declaration: &Path, interval_secs: u64) -> Result<()> {
    let declaration = std::fs::canonicalize(declaration)
        .wrap_err_with(|| format!("the declaration {} does not exist", declaration.display()))?;
    let (service, timer) = render_converge_units(binary, &declaration, interval_secs)?;
    let unit_dir = Path::new("/etc/systemd/system");
    std::fs::write(unit_dir.join("natmap-converge.service"), service)?;
    std::fs::write(unit_dir.join("natmap-converge.timer"), timer)?;
    println!(
        "Wrote natmap-converge.service and natmap-converge.timer for {} (every {interval_secs}s).",
        declaration.display()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converge_service_unit_runs_apply_for_the_declaration() {
        let (service, _) = render_converge_units(
            "/usr/local/bin/lab-ops",
            Path::new("/etc/auto-discover/discovery.yaml"),
            15,
        )
        .unwrap();

        assert!(
            service.contains(
                "ExecStart=/usr/local/bin/lab-ops -v natmap apply --from /etc/auto-discover/discovery.yaml --prune"
            ),
            "{service}"
        );
        assert!(service.contains("Type=oneshot"), "{service}");
        assert!(service.contains("Requires=natmap.service"), "{service}");
    }

    #[test]
    fn converge_timer_unit_fires_on_the_interval() {
        let (_, timer) = render_converge_units(
            "lab-ops",
            Path::new("/etc/auto-discover/discovery.yaml"),
            30,
        )
        .unwrap();

        assert!(timer.contains("OnUnitActiveSec=30s"), "{timer}");
        assert!(timer.contains("Unit=natmap-converge.service"), "{timer}");
        assert!(timer.contains("WantedBy=timers.target"), "{timer}");
    }

    #[test]
    fn converge_units_refuse_a_zero_interval() {
        let result =
            render_converge_units("lab-ops", Path::new("/etc/auto-discover/discovery.yaml"), 0);

        assert!(result.is_err());
    }
}
