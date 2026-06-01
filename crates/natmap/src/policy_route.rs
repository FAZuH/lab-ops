use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;

use crate::models::PolicyRouteConfig;

pub struct PolicyRouteManager;

impl PolicyRouteManager {
    pub fn new() -> Self {
        Self
    }

    fn check_rule_exists(&self, config: &PolicyRouteConfig) -> Result<bool> {
        let output = Command::new("ip")
            .args(["rule", "show"])
            .output()
            .wrap_err("Failed to execute ip rule show")?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        // Format: "32765:  from <src_ip> lookup <table>"
        let expected = format!("from {} lookup {}", config.src_ip, config.table);
        Ok(stdout.contains(&expected))
    }

    fn check_route_exists(&self, config: &PolicyRouteConfig) -> Result<bool> {
        let output = Command::new("ip")
            .args(["route", "show", "table", &config.table.to_string()])
            .output()
            .wrap_err("Failed to execute ip route show")?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let expected = format!("default via {}", config.via);
        Ok(stdout.contains(&expected))
    }

    pub fn install(&self, config: &PolicyRouteConfig) -> Result<()> {
        if !self.check_route_exists(config)? {
            let status = Command::new("ip")
                .args([
                    "route",
                    "add",
                    "default",
                    "via",
                    &config.via,
                    "table",
                    &config.table.to_string(),
                ])
                .status()
                .wrap_err("Failed to execute ip route add")?;

            if !status.success() {
                color_eyre::eyre::bail!("ip route add failed with status: {}", status);
            }
        }

        if !self.check_rule_exists(config)? {
            let status = Command::new("ip")
                .args([
                    "rule",
                    "add",
                    "from",
                    &config.src_ip,
                    "table",
                    &config.table.to_string(),
                ])
                .status()
                .wrap_err("Failed to execute ip rule add")?;

            if !status.success() {
                color_eyre::eyre::bail!("ip rule add failed with status: {}", status);
            }
        }

        Ok(())
    }

    pub fn remove(&self, config: &PolicyRouteConfig) -> Result<()> {
        // We ignore errors on remove, in case the rules are already gone.
        let _ = Command::new("ip")
            .args([
                "rule",
                "del",
                "from",
                &config.src_ip,
                "table",
                &config.table.to_string(),
            ])
            .status();

        let _ = Command::new("ip")
            .args([
                "route",
                "del",
                "default",
                "via",
                &config.via,
                "table",
                &config.table.to_string(),
            ])
            .status();

        Ok(())
    }

    pub fn flush_all(&self, policy_routes: &[PolicyRouteConfig]) -> Result<()> {
        for config in policy_routes {
            self.remove(config)?;
        }
        Ok(())
    }
}
