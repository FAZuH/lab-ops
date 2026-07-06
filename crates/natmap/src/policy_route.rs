use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;

use crate::models::PolicyRouteConfig;

pub struct PolicyRouteManager;

// ── Pure helpers (testable without ip commands) ──

/// Checks if `ip rule show` output contains an expected rule line.
fn rule_exists_in_output(output: &str, config: &PolicyRouteConfig) -> bool {
    let expected = format!("from {} lookup {}", config.src_ip, config.table);
    output.contains(&expected)
}

/// Checks if `ip route show table <N>` output contains an expected default route.
fn route_exists_in_output(output: &str, config: &PolicyRouteConfig) -> bool {
    let expected = format!("default via {}", config.via);
    output.contains(&expected)
}

/// Checks if a table's route output contains an exact route line.
fn route_line_matches_table(output: &str, route_line: &str) -> bool {
    output.lines().any(|l| l.trim() == route_line.trim())
}

/// Filters `ip route show table main` output to only routes that should be
/// cloned into a policy routing table.
fn filter_cloneable_routes(output: &str) -> Vec<String> {
    output
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
        .collect()
}

/// Builds `ip route add default via <via> table <table>` args.
fn build_route_add_args(config: &PolicyRouteConfig) -> Vec<String> {
    vec![
        "route".into(),
        "add".into(),
        "default".into(),
        "via".into(),
        config.via.clone(),
        "table".into(),
        config.table.to_string(),
    ]
}

/// Builds `ip rule add from <src_ip> table <table>` args.
fn build_rule_add_args(config: &PolicyRouteConfig) -> Vec<String> {
    vec![
        "rule".into(),
        "add".into(),
        "from".into(),
        config.src_ip.clone(),
        "table".into(),
        config.table.to_string(),
    ]
}

/// Builds `ip rule del from <src_ip> table <table>` args.
fn build_rule_del_args(config: &PolicyRouteConfig) -> Vec<String> {
    vec![
        "rule".into(),
        "del".into(),
        "from".into(),
        config.src_ip.clone(),
        "table".into(),
        config.table.to_string(),
    ]
}

/// Builds `ip route del default via <via> table <table>` args.
fn build_route_del_args(config: &PolicyRouteConfig) -> Vec<String> {
    vec![
        "route".into(),
        "del".into(),
        "default".into(),
        "via".into(),
        config.via.clone(),
        "table".into(),
        config.table.to_string(),
    ]
}

/// Builds `ip route show table <table>` args.
fn build_route_show_args(table: u32) -> Vec<String> {
    vec![
        "route".into(),
        "show".into(),
        "table".into(),
        table.to_string(),
    ]
}

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
        Ok(rule_exists_in_output(&stdout, config))
    }

    fn check_route_exists(&self, config: &PolicyRouteConfig) -> Result<bool> {
        let output = Command::new("ip")
            .args(build_route_show_args(config.table))
            .output()
            .wrap_err("Failed to execute ip route show")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(route_exists_in_output(&stdout, config))
    }

    fn route_line_in_table(&self, route_line: &str, table: u32) -> Result<bool> {
        let output = Command::new("ip")
            .args(build_route_show_args(table))
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(route_line_matches_table(&stdout, route_line))
    }

    fn get_cloneable_routes(&self) -> Result<Vec<String>> {
        let output = Command::new("ip")
            .args(["route", "show", "table", "main"])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(filter_cloneable_routes(&stdout))
    }

    pub fn install(&self, config: &PolicyRouteConfig) -> Result<()> {
        if !self.check_route_exists(config)? {
            let status = Command::new("ip")
                .args(build_route_add_args(config))
                .status()
                .wrap_err("Failed to execute ip route add")?;
            if !status.success() {
                color_eyre::eyre::bail!("ip route add failed with status: {}", status);
            }
        }

        if !self.check_rule_exists(config)? {
            let status = Command::new("ip")
                .args(build_rule_add_args(config))
                .status()
                .wrap_err("Failed to execute ip rule add")?;
            if !status.success() {
                color_eyre::eyre::bail!("ip rule add failed with status: {}", status);
            }
        }

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
        let _ = Command::new("ip")
            .args(build_rule_del_args(config))
            .status();
        let _ = Command::new("ip")
            .args(build_route_del_args(config))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PolicyRouteConfig {
        PolicyRouteConfig {
            src_ip: "10.0.0.1".into(),
            via: "192.168.1.1".into(),
            table: 100,
        }
    }

    // ── rule_exists_in_output ──

    #[test]
    fn rule_exists_in_output_matches() {
        let cfg = test_config();
        let output = "32765:\tfrom 10.0.0.1 lookup 100\n32766:\tfrom all lookup main\n";
        assert!(rule_exists_in_output(output, &cfg));
    }

    #[test]
    fn rule_exists_in_output_no_match() {
        let cfg = test_config();
        let output = "32766:\tfrom all lookup main\n";
        assert!(!rule_exists_in_output(output, &cfg));
    }

    #[test]
    fn rule_exists_in_output_different_ip() {
        let cfg = test_config();
        let output = "32765:\tfrom 10.0.0.2 lookup 100\n";
        assert!(!rule_exists_in_output(output, &cfg));
    }

    #[test]
    fn rule_exists_in_output_different_table() {
        let cfg = test_config();
        let output = "32765:\tfrom 10.0.0.1 lookup 200\n";
        assert!(!rule_exists_in_output(output, &cfg));
    }

    // ── route_exists_in_output ──

    #[test]
    fn route_exists_in_output_matches() {
        let cfg = test_config();
        let output = "default via 192.168.1.1 dev eth0\n10.0.0.0/24 dev eth0 scope link\n";
        assert!(route_exists_in_output(output, &cfg));
    }

    #[test]
    fn route_exists_in_output_no_match() {
        let cfg = test_config();
        let output = "10.0.0.0/24 dev eth0 scope link\n";
        assert!(!route_exists_in_output(output, &cfg));
    }

    #[test]
    fn route_exists_in_output_different_via() {
        let cfg = test_config();
        let output = "default via 10.0.0.1 dev eth0\n";
        assert!(!route_exists_in_output(output, &cfg));
    }

    // ── route_line_matches_table ──

    #[test]
    fn route_line_matches_table_exact_match() {
        let output = "10.10.10.0/24 dev vmbr1 proto kernel scope link src 10.10.10.1\n";
        assert!(route_line_matches_table(
            output,
            "10.10.10.0/24 dev vmbr1 proto kernel scope link src 10.10.10.1"
        ));
    }

    #[test]
    fn route_line_matches_table_trimmed_match() {
        let output = "  10.10.10.0/24 dev vmbr1 proto kernel scope link src 10.10.10.1  \n";
        assert!(route_line_matches_table(
            output,
            "10.10.10.0/24 dev vmbr1 proto kernel scope link src 10.10.10.1"
        ));
    }

    #[test]
    fn route_line_matches_table_no_match() {
        let output = "10.10.20.0/24 dev vmbr2 proto kernel scope link src 10.10.20.1\n";
        assert!(!route_line_matches_table(
            output,
            "10.10.10.0/24 dev vmbr1 proto kernel scope link src 10.10.10.1"
        ));
    }

    // ── filter_cloneable_routes ──

    #[test]
    fn filter_cloneable_routes_excludes_default() {
        let output = "default via 192.168.1.1 dev eth0\n10.0.0.0/24 dev eth0 scope link\n";
        let routes = filter_cloneable_routes(output);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0], "10.0.0.0/24 dev eth0 scope link");
    }

    #[test]
    fn filter_cloneable_routes_excludes_broadcast_local_unreachable() {
        let output = "broadcast 10.0.0.0 dev eth0\nlocal 10.0.0.1 dev eth0\nunreachable 10.0.0.0/8\n10.0.0.0/24 dev eth0 scope link\n";
        let routes = filter_cloneable_routes(output);
        assert_eq!(routes.len(), 1);
    }

    #[test]
    fn filter_cloneable_routes_excludes_ipv6_link_local_and_multicast() {
        let output = "fe80::/64 dev eth0 proto kernel metric 256\nff00::/8 dev eth0 metric 256\n2001:db8::/64 dev eth0 metric 256\n";
        let routes = filter_cloneable_routes(output);
        assert_eq!(routes.len(), 1);
        assert!(routes[0].contains("2001:db8::"));
    }

    #[test]
    fn filter_cloneable_routes_empty_output() {
        assert!(filter_cloneable_routes("").is_empty());
    }

    #[test]
    fn filter_cloneable_routes_skips_empty_lines() {
        let output = "\n\n10.0.0.0/24 dev eth0 scope link\n\n";
        let routes = filter_cloneable_routes(output);
        assert_eq!(routes.len(), 1);
    }

    // ── build_route_add_args ──

    #[test]
    fn route_add_args_format() {
        let cfg = test_config();
        let args = build_route_add_args(&cfg);
        assert_eq!(
            args,
            vec![
                "route",
                "add",
                "default",
                "via",
                "192.168.1.1",
                "table",
                "100"
            ]
        );
    }

    // ── build_rule_add_args ──

    #[test]
    fn rule_add_args_format() {
        let cfg = test_config();
        let args = build_rule_add_args(&cfg);
        assert_eq!(
            args,
            vec!["rule", "add", "from", "10.0.0.1", "table", "100"]
        );
    }

    // ── build_rule_del_args ──

    #[test]
    fn rule_del_args_format() {
        let cfg = test_config();
        let args = build_rule_del_args(&cfg);
        assert_eq!(
            args,
            vec!["rule", "del", "from", "10.0.0.1", "table", "100"]
        );
    }

    // ── build_route_del_args ──

    #[test]
    fn route_del_args_format() {
        let cfg = test_config();
        let args = build_route_del_args(&cfg);
        assert_eq!(
            args,
            vec![
                "route",
                "del",
                "default",
                "via",
                "192.168.1.1",
                "table",
                "100"
            ]
        );
    }

    // ── build_route_show_args ──

    #[test]
    fn route_show_args_format() {
        let args = build_route_show_args(100);
        assert_eq!(args, vec!["route", "show", "table", "100"]);
    }

    #[test]
    fn route_show_args_zero_table() {
        let args = build_route_show_args(0);
        assert_eq!(args, vec!["route", "show", "table", "0"]);
    }

    #[test]
    fn route_show_args_max_table() {
        let args = build_route_show_args(4294967295);
        assert_eq!(args, vec!["route", "show", "table", "4294967295"]);
    }
}
