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

    /// Check if an exact route line already exists in a given table.
    fn route_line_in_table(&self, route_line: &str, table: u32) -> Result<bool> {
        let output = Command::new("ip")
            .args(["route", "show", "table", &table.to_string()])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.lines().any(|l| l.trim() == route_line.trim()))
    }

    /// Collect routes from the main table that should be cloned into the
    /// policy routing table: non-default, non-local, non-broadcast routes
    /// for local subnets (Docker bridges, LAN, etc.).
    fn get_cloneable_routes(&self) -> Result<Vec<String>> {
        let output = Command::new("ip")
            .args(["route", "show", "table", "main"])
            .output()?;
        let routes = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty()
                    && !t.starts_with("default ")
                    && !t.starts_with("broadcast ")
                    && !t.starts_with("local ")
                    && !t.starts_with("unreachable ")
                    && !t.starts_with("fe80::")
                    && !t.starts_with("ff00::")
            })
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        Ok(routes)
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

        // Clone local-subnet routes from the main table so traffic from
        // src_ip to Docker bridges, the LAN, etc. uses the correct interface
        // instead of the proxy gateway (which would break local connectivity).
        let table = config.table;
        for route_line in self.get_cloneable_routes()? {
            if !self.route_line_in_table(&route_line, table)? {
                let status = Command::new("sh")
                    .args(["-c", &format!("ip route add {route_line} table {table}")])
                    .status()
                    .wrap_err("Failed to execute ip route add")?;
                if !status.success() {
                    tracing::warn!("failed to clone route to table {table}: {route_line}");
                }
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
