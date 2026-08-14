//! iptables rule management for DNAT, SNAT, hairpin, and Docker mappings.
//!
//! All rules are installed in the `NATMAP` chain (a sub-chain of `PREROUTING`
//! in the `nat` table and `DOCKER-USER` in the `filter` table). This keeps
//! natmap rules separate from Docker's own rules and ensures clean crash
//! recovery via chain flush.

use std::ffi::OsStr;
use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::bail;

use crate::models::DnatConfig;
use crate::models::DockerPortMap;
use crate::models::HairpinConfig;
use crate::models::SnatConfig;

const NATMAP: &str = "NATMAP";

/// Determines the destination IP for the OUTPUT DNAT rule.
///
/// When the host IP is unspecified (`0.0.0.0` or `::`), the loopback
/// address (`127.0.0.1` / `::1`) is used so localhost-sourced traffic
/// is also DNATed. For specific host IPs, the IP itself is used.
///
/// ```
/// use std::net::IpAddr;
/// use std::str::FromStr;
/// use lab_ops_natmap::iptables::output_dnat_destination;
///
/// assert_eq!(output_dnat_destination(IpAddr::from_str("0.0.0.0").unwrap(), false), "127.0.0.1");
/// assert_eq!(output_dnat_destination(IpAddr::from_str("::").unwrap(), true), "::1");
/// assert_eq!(output_dnat_destination(IpAddr::from_str("100.64.0.10").unwrap(), false), "100.64.0.10");
/// ```
pub fn output_dnat_destination(host_ip: std::net::IpAddr, is_ipv6: bool) -> String {
    if host_ip.is_unspecified() {
        if is_ipv6 {
            "::1".to_string()
        } else {
            "127.0.0.1".to_string()
        }
    } else {
        host_ip.to_string()
    }
}

/// Manages the lifecycle of iptables rules used by natmap.
///
/// Creates the `NATMAP` chain in both the `nat` and `filter` tables,
/// inserts jumps from `PREROUTING` and `DOCKER-USER`, and provides
/// methods to install/remove individual rules.
pub struct IptablesManager;

// ── Pure argument builders (testable without iptables) ──

/// Builds iptables args for a docker mapping DNAT rule (nat/NATMAP).
fn build_dnat_rule_args(map: &DockerPortMap) -> Vec<String> {
    let req = &map.request;
    let host_ip = req.host_addr.ip();
    let mut args = vec![
        "-t".into(),
        "nat".into(),
        "-A".into(),
        NATMAP.into(),
        "-p".into(),
        req.proto.to_string(),
    ];
    if !host_ip.is_unspecified() {
        args.push("-d".into());
        args.push(host_ip.to_string());
    }
    args.extend([
        "--dport".into(),
        req.host_addr.port().to_string(),
        "-j".into(),
        "DNAT".into(),
        "--to-destination".into(),
        req.container_addr.to_string(),
        "-m".into(),
        "comment".into(),
        "--comment".into(),
        map.rule_comment.clone(),
    ]);
    args
}

/// Builds iptables args for a docker mapping FORWARD ACCEPT rule (filter/NATMAP).
fn build_forward_accept_args(map: &DockerPortMap) -> Vec<String> {
    let req = &map.request;
    vec![
        "-t".into(),
        "filter".into(),
        "-A".into(),
        NATMAP.into(),
        "-d".into(),
        req.container_addr.ip().to_string(),
        "-p".into(),
        req.proto.to_string(),
        "--dport".into(),
        req.container_addr.port().to_string(),
        "-j".into(),
        "ACCEPT".into(),
        "-m".into(),
        "comment".into(),
        "--comment".into(),
        map.rule_comment.clone(),
    ]
}

/// Builds iptables args for a docker mapping POSTROUTING MASQUERADE rule.
fn build_masquerade_args(map: &DockerPortMap) -> Vec<String> {
    let req = &map.request;
    vec![
        "-t".into(),
        "nat".into(),
        "-A".into(),
        "POSTROUTING".into(),
        "-s".into(),
        req.container_addr.ip().to_string(),
        "-d".into(),
        req.container_addr.ip().to_string(),
        "-p".into(),
        req.proto.to_string(),
        "--dport".into(),
        req.container_addr.port().to_string(),
        "-j".into(),
        "MASQUERADE".into(),
        "-m".into(),
        "comment".into(),
        "--comment".into(),
        map.rule_comment.clone(),
    ]
}

/// Builds iptables args for a docker mapping OUTPUT DNAT rule.
fn build_output_dnat_args(map: &DockerPortMap, output_dst: &str) -> Vec<String> {
    let req = &map.request;
    vec![
        "-t".into(),
        "nat".into(),
        "-A".into(),
        "OUTPUT".into(),
        "-d".into(),
        output_dst.into(),
        "-p".into(),
        req.proto.to_string(),
        "--dport".into(),
        req.host_addr.port().to_string(),
        "-j".into(),
        "DNAT".into(),
        "--to-destination".into(),
        req.container_addr.to_string(),
        "-m".into(),
        "comment".into(),
        "--comment".into(),
        map.rule_comment.clone(),
    ]
}

/// Builds iptables args for a docker mapping loopback MASQUERADE rule, if needed.
/// Returns `None` when the rule is not required.
fn build_loopback_masq_args(map: &DockerPortMap) -> Option<Vec<String>> {
    let req = &map.request;
    let host_ip = req.host_addr.ip();
    let needs_loopback_masq = (host_ip.is_loopback() || host_ip.is_unspecified())
        && !req.container_addr.ip().is_loopback();
    if !needs_loopback_masq {
        return None;
    }
    let loopback_src = if map.request.is_ipv6() {
        "::1/128"
    } else {
        "127.0.0.0/8"
    };
    Some(vec![
        "-t".into(),
        "nat".into(),
        "-A".into(),
        "POSTROUTING".into(),
        "-s".into(),
        loopback_src.into(),
        "-d".into(),
        req.container_addr.ip().to_string(),
        "-p".into(),
        req.proto.to_string(),
        "--dport".into(),
        req.container_addr.port().to_string(),
        "-j".into(),
        "MASQUERADE".into(),
        "-m".into(),
        "comment".into(),
        "--comment".into(),
        map.rule_comment.clone(),
    ])
}

/// Builds iptables args for a static DNAT PREROUTING rule.
fn build_static_dnat_prerouting_args(config: &DnatConfig) -> Vec<String> {
    let comment = config.rule_comment();
    let mut args = vec!["-t".into(), "nat".into(), "-A".into(), "PREROUTING".into()];
    if let Some(ref iface) = config.ext_if {
        args.push("-i".into());
        args.push(iface.clone());
    }
    args.push("-d".into());
    args.push(config.ext_ip.clone());
    args.push("-p".into());
    args.push(config.proto.to_lowercase().into());
    if config.ports.contains(',') {
        args.extend([
            "-m".into(),
            "multiport".into(),
            "--dports".into(),
            config.ports.clone(),
        ]);
    } else {
        args.extend(["--dport".into(), config.ports.clone()]);
    }
    let dest = if config.ports.contains(',') {
        config.int_ip.clone()
    } else {
        format!("{}:{}", config.int_ip, config.ports)
    };
    args.extend(["-j".into(), "DNAT".into(), "--to-destination".into(), dest]);
    args.extend(["-m".into(), "comment".into(), "--comment".into(), comment]);
    args
}

/// Builds iptables args for a static DNAT FORWARD ACCEPT rule.
fn build_static_dnat_forward_args(config: &DnatConfig) -> Vec<String> {
    let comment = config.rule_comment();
    let mut args: Vec<String> = vec!["-A".into(), "FORWARD".into()];
    args.push("-p".into());
    args.push(config.proto.to_lowercase().into());
    args.push("-d".into());
    args.push(config.int_ip.clone());
    if config.ports.contains(',') {
        args.extend([
            "-m".into(),
            "multiport".into(),
            "--dports".into(),
            config.ports.clone(),
        ]);
    } else {
        args.extend(["--dport".into(), config.ports.clone()]);
    }
    args.extend(["-j".into(), "ACCEPT".into()]);
    args.extend(["-m".into(), "comment".into(), "--comment".into(), comment]);
    args
}

/// Builds iptables args for a static SNAT POSTROUTING rule.
fn build_snat_args(config: &SnatConfig) -> Vec<String> {
    let comment = config.rule_comment();
    vec![
        "-t".into(),
        "nat".into(),
        "-A".into(),
        "POSTROUTING".into(),
        "-s".into(),
        config.int_ip.clone(),
        "-o".into(),
        config.ext_if.clone(),
        "-j".into(),
        "SNAT".into(),
        "--to-source".into(),
        config.ext_ip.clone(),
        "-m".into(),
        "comment".into(),
        "--comment".into(),
        comment,
    ]
}

/// Builds iptables args for a hairpin PREROUTING DNAT rule, if needed.
/// Returns `None` when `lan_cidr` is set (skip the PREROUTING DNAT).
fn build_hairpin_prerouting_args(config: &HairpinConfig) -> Option<Vec<String>> {
    if config.lan_cidr.is_some() {
        return None;
    }
    let comment = config.rule_comment();
    let mut args: Vec<String> = vec![
        "-t".into(),
        "nat".into(),
        "-A".into(),
        "PREROUTING".into(),
        "-s".into(),
        config.int_ip.clone(),
        "-d".into(),
        config.ext_ip.clone(),
    ];
    args.push("-p".into());
    args.push(config.proto.to_lowercase().into());
    if config.ports.contains(',') {
        args.extend([
            "-m".into(),
            "multiport".into(),
            "--dports".into(),
            config.ports.clone(),
        ]);
    } else {
        args.extend(["--dport".into(), config.ports.clone()]);
    }
    args.extend([
        "-j".into(),
        "DNAT".into(),
        "--to-destination".into(),
        config.int_ip.clone(),
    ]);
    args.extend(["-m".into(), "comment".into(), "--comment".into(), comment]);
    Some(args)
}

/// Builds iptables args for a hairpin POSTROUTING MASQUERADE rule.
fn build_hairpin_postrouting_args(config: &HairpinConfig) -> Vec<String> {
    let comment = config.rule_comment();
    let src = config.lan_cidr.as_deref().unwrap_or("0.0.0.0/0");
    let mut args: Vec<String> = vec![
        "-t".into(),
        "nat".into(),
        "-A".into(),
        "POSTROUTING".into(),
        "-s".into(),
        src.into(),
        "-d".into(),
        config.int_ip.clone(),
    ];
    args.push("-p".into());
    args.push(config.proto.to_lowercase().into());
    if config.ports.contains(',') {
        args.extend([
            "-m".into(),
            "multiport".into(),
            "--dports".into(),
            config.ports.clone(),
        ]);
    } else {
        args.extend(["--dport".into(), config.ports.clone()]);
    }
    args.extend(["-j".into(), "MASQUERADE".into()]);
    args.extend(["-m".into(), "comment".into(), "--comment".into(), comment]);
    args
}

impl Default for IptablesManager {
    fn default() -> Self {
        Self::new()
    }
}

impl IptablesManager {
    /// Creates a new [`IptablesManager`].
    pub fn new() -> Self {
        Self
    }

    /// Creates the `NATMAP` chains and inserts jump rules.
    ///
    /// Operates on both `iptables` (IPv4) and `ip6tables` (IPv6).
    /// This method is idempotent.
    pub fn setup(&self) -> Result<()> {
        tracing::info!("setting up iptables chains and jumps");

        for &cmd in &["iptables", "ip6tables"] {
            // Verify DOCKER-USER exists (it should, Docker makes it). Create if missing.
            if !self.chain_exists(cmd, "filter", "DOCKER-USER") {
                // create new DOCKER-USER chain
                self.run_success(cmd, ["-t", "filter", "-N", "DOCKER-USER"])?;
                // insert a jump rule on first position of FORWARD chain to DOCKER-USER
                self.run_success(cmd, ["-t", "filter", "-I", "FORWARD", "-j", "DOCKER-USER"])?;
            }

            // Create NATMAP subchain in nat table (DNAT rules live here)
            if !self.chain_exists(cmd, "nat", NATMAP) {
                self.run_success(cmd, ["-t", "nat", "-N", NATMAP])?;
            }

            // Create NATMAP subchain in filter table (FORWARD ACCEPT rules live here)
            if !self.chain_exists(cmd, "filter", NATMAP) {
                self.run_success(cmd, ["-t", "filter", "-N", NATMAP])?;
            }

            // Jump from DOCKER-USER to NATMAP in filter table (if not exists)
            if !self.rule_exists(cmd, &["-t", "filter", "-C", "DOCKER-USER", "-j", NATMAP]) {
                self.run(cmd, ["-t", "filter", "-I", "DOCKER-USER", "-j", NATMAP])?;
            }

            // Jump from PREROUTING to NATMAP in nat table (if not exists)
            if !self.rule_exists(cmd, &["-t", "nat", "-C", "PREROUTING", "-j", NATMAP]) {
                self.run_success(cmd, ["-t", "nat", "-I", "PREROUTING", "-j", NATMAP])?;
            }
        }

        Ok(())
    }

    /// Installs DNAT, FORWARD ACCEPT, MASQUERADE, and OUTPUT DNAT rules for a Docker mapping.
    pub fn install_dockermap(&self, map: &DockerPortMap) -> Result<()> {
        tracing::debug!(mapping = ?map, "installing mapping");
        let cmd = self.cmd_for(map.request.is_ipv6());

        self.run(cmd, build_dnat_rule_args(map))?;
        self.run(cmd, build_forward_accept_args(map))?;
        self.run(cmd, build_masquerade_args(map))?;

        let output_dst = output_dnat_destination(map.request.host_addr.ip(), map.request.is_ipv6());
        self.run(cmd, build_output_dnat_args(map, &output_dst))?;

        if let Some(args) = build_loopback_masq_args(map) {
            self.run(cmd, &args)?;
        }

        Ok(())
    }

    /// Flushes and deletes the `NATMAP` chains and removes all natmap-commented
    /// rules from `POSTROUTING`, `OUTPUT`, `PREROUTING`, and `FORWARD` in both
    /// `iptables` (IPv4) and `ip6tables` (IPv6).
    ///
    /// Used during crash recovery and clean shutdown to reset all natmap-managed rules.
    pub fn flush_all_natmap(&self) -> Result<()> {
        tracing::info!("flushing all NATMAP iptables rules");

        for &cmd in &["iptables", "ip6tables"] {
            let _ = self.flush_chain(cmd, "nat", NATMAP);
            let _ = self.flush_chain(cmd, "filter", NATMAP);
            let _ = self.delete_all_natmap(cmd, "nat", "POSTROUTING");
            let _ = self.delete_all_natmap(cmd, "nat", "OUTPUT");
            let _ = self.delete_all_natmap(cmd, "nat", "PREROUTING");
            let _ = self.delete_all_natmap(cmd, "filter", "FORWARD");
        }
        Ok(())
    }

    /// Installs a static DNAT rule (PREROUTING + FORWARD ACCEPT).
    pub fn install_dnat(&self, config: &DnatConfig) -> Result<()> {
        self.run_success("iptables", build_static_dnat_prerouting_args(config))?;
        self.run_success("iptables", build_static_dnat_forward_args(config))?;
        Ok(())
    }

    /// Removes a static DNAT rule (PREROUTING + FORWARD ACCEPT).
    ///
    /// Uses the rule comment to find and delete matching rules.
    pub fn remove_dnat(&self, config: &DnatConfig) -> Result<()> {
        let comment = config.rule_comment();
        self.delete_all_matching("iptables", "nat", "PREROUTING", &comment)?;
        self.delete_all_matching("iptables", "filter", "FORWARD", &comment)?;
        Ok(())
    }

    /// Installs a static SNAT (source NAT) rule in the POSTROUTING chain.
    pub fn install_snat(&self, config: &SnatConfig) -> Result<()> {
        self.run_success("iptables", build_snat_args(config))?;
        Ok(())
    }

    /// Removes a static SNAT rule from the POSTROUTING chain.
    ///
    /// Uses the rule comment to find and delete matching rules.
    pub fn remove_snat(&self, config: &SnatConfig) -> Result<()> {
        let comment = config.rule_comment();
        self.delete_all_matching("iptables", "nat", "POSTROUTING", &comment)?;
        Ok(())
    }

    /// Installs a hairpin NAT rule.
    ///
    /// When `config.lan_cidr` is set:
    /// - Skips the PREROUTING DNAT (service node self-connections go through
    ///   the regular DNAT rule instead).
    /// - Uses `lan_cidr` as the MASQUERADE source match, limiting hairpin to
    ///   LAN clients only (preserving source IP for WAN clients).
    ///
    /// When `lan_cidr` is `None`, creates the full hairpin (PREROUTING DNAT +
    ///   POSTROUTING MASQUERADE with `-s 0.0.0.0/0`).
    pub fn install_hairpin(&self, config: &HairpinConfig) -> Result<()> {
        if let Some(args) = build_hairpin_prerouting_args(config) {
            self.run_success("iptables", &args)?;
        }
        self.run_success("iptables", build_hairpin_postrouting_args(config))?;
        Ok(())
    }

    /// Removes a hairpin NAT rule (PREROUTING DNAT + POSTROUTING MASQUERADE).
    ///
    /// Uses the rule comment to find and delete matching rules.
    pub fn remove_hairpin(&self, config: &HairpinConfig) -> Result<()> {
        let comment = config.rule_comment();
        self.delete_all_matching("iptables", "nat", "PREROUTING", &comment)?;
        self.delete_all_matching("iptables", "nat", "POSTROUTING", &comment)?;
        Ok(())
    }

    /// Deletes all rules in a chain whose comment starts with "natmap:".
    fn delete_all_natmap(&self, cmd: &str, table: &str, chain: &str) -> Result<()> {
        loop {
            let rules = self.get_rules(cmd, table, chain)?;
            let mut deleted = false;
            for (line_num, rule) in rules.iter().enumerate() {
                if rule.contains("--comment \"natmap:") || rule.contains("--comment natmap:") {
                    let num = (line_num + 1).to_string();
                    self.run(cmd, ["-t", table, "-D", chain, &num])?;
                    deleted = true;
                    break;
                }
            }
            if !deleted {
                break;
            }
        }
        Ok(())
    }

    /// Removes all iptables rules associated with a Docker mapping by its rule comment.
    pub fn remove_mapping(&self, map: &DockerPortMap) -> Result<()> {
        tracing::debug!(mapping = ?map, "removing mapping");
        self.remove_by_comment(&map.rule_comment, map.request.is_ipv6())?;
        Ok(())
    }

    /// Deletes rules matching the comment string across all relevant tables and chains.
    fn remove_by_comment(&self, comment: &str, is_ipv6: bool) -> Result<()> {
        let cmd = self.cmd_for(is_ipv6);

        // Delete from NATMAP in nat table
        self.delete_all_matching(cmd, "nat", NATMAP, comment)?;
        // Delete from NATMAP in filter table
        self.delete_all_matching(cmd, "filter", NATMAP, comment)?;
        // Delete from POSTROUTING in nat table
        self.delete_all_matching(cmd, "nat", "POSTROUTING", comment)?;
        // Delete from OUTPUT in nat table (localhost DNAT)
        self.delete_all_matching(cmd, "nat", "OUTPUT", comment)?;

        Ok(())
    }

    /// Flushes and deletes a specific chain in a given table.
    fn flush_chain(&self, cmd: &str, table: &str, chain: &str) -> Result<()> {
        // flush chain
        let _ = self.run(cmd, ["-t", table, "-F", chain]);
        // delete chain
        let _ = self.run(cmd, ["-t", table, "-X", chain]);
        Ok(())
    }

    // --- Helper functions ---

    /// Returns `"ip6tables"` or `"iptables"` based on address family.
    fn cmd_for(&self, is_ipv6: bool) -> &'static str {
        if is_ipv6 { "ip6tables" } else { "iptables" }
    }

    /// Runs a command. Fails and logs an error if the command returned a non-zero exit status.
    fn run_success(
        &self,
        program: impl AsRef<OsStr>,
        args: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> Result<std::process::Output> {
        let args: Vec<_> = args.into_iter().collect();
        let out = self.run(&program, &args)?;
        if out.status.success() {
            Ok(out)
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let args_str = args
                .iter()
                .map(|a| a.as_ref().to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            let program = program.as_ref().to_string_lossy();
            tracing::error!(program = %program, args = %args_str, error = %err, "command failed");
            bail!("{program} failed: {err}");
        }
    }

    /// Runs a command.
    fn run(
        &self,
        program: impl AsRef<OsStr>,
        args: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> Result<std::process::Output> {
        let args_vec: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
        let args_str = args_vec
            .iter()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        tracing::trace!(command = %program.as_ref().to_string_lossy(), args = %args_str, "raw iptables command");
        Ok(Command::new(program.as_ref()).args(&args_vec).output()?)
    }

    /// Checks whether a chain exists in the given table.
    fn chain_exists(&self, cmd: &str, table: &str, chain: &str) -> bool {
        self.run(cmd, ["-t", table, "-L", chain, "-n"])
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Checks whether a specific iptables rule already exists.
    fn rule_exists(&self, cmd: &str, args: &[&str]) -> bool {
        // cmd_success logs on fail
        self.run(cmd, args)
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Deletes all rules in a chain whose comment matches the given string.
    fn delete_all_matching(
        &self,
        cmd: &str,
        table: &str,
        chain: &str,
        comment: &str,
    ) -> Result<()> {
        // Rules and delete by line numbers.
        loop {
            let rules = self.get_rules(cmd, table, chain)?;
            let mut deleted = false;
            for (line_num, rule) in rules.iter().enumerate() {
                if rule.contains(&format!("--comment \"{comment}\""))
                    || rule.contains(&format!("--comment {comment}"))
                {
                    // Delete by line number from bottom up (or just one by one)
                    let num = (line_num + 1).to_string();
                    self.run_success(cmd, ["-t", table, "-D", chain, &num])?;
                    deleted = true;
                    break; // Start over since line numbers changed
                }
            }
            if !deleted {
                break;
            }
        }
        Ok(())
    }

    /// Returns the list of active rules in a chain (lines starting with `-A` or `-I`).
    fn get_rules(&self, cmd: &str, table: &str, chain: &str) -> Result<Vec<String>> {
        // -S -- short for --list-rules
        let out = self.run(cmd, ["-t", table, "-S", chain])?;

        // Get only -A (append) and -I (insert)
        // Ignore others, such as chain declarations
        let rules = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with("-A ") || l.starts_with("-I "))
            .map(|l| l.to_string())
            .collect();

        Ok(rules)
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::net::SocketAddr;
    use std::str::FromStr;

    use super::*;
    use crate::models::DockerPortMapRequest;
    use crate::models::TransportProtocol;

    fn test_dockermap(
        host_ip: &str,
        host_port: u16,
        ctn_ip: &str,
        ctn_port: u16,
        proto: TransportProtocol,
        id: u64,
    ) -> DockerPortMap {
        let req = DockerPortMapRequest {
            host_addr: SocketAddr::new(IpAddr::from_str(host_ip).unwrap(), host_port),
            container_addr: SocketAddr::new(IpAddr::from_str(ctn_ip).unwrap(), ctn_port),
            proto,
        };
        DockerPortMap::new(id, req, "c1".into(), "svc".into())
    }

    // ── build_dnat_rule_args ──

    #[test]
    fn dnat_args_unspecified_ip_omits_d_flag() {
        let m = test_dockermap("0.0.0.0", 8080, "10.0.0.2", 80, TransportProtocol::Tcp, 1);
        let args = build_dnat_rule_args(&m);
        assert!(args.contains(&"-t".into()));
        assert!(args.contains(&"DNAT".into()));
        assert!(args.contains(&"8080".into()));
        assert!(args.contains(&"10.0.0.2:80".into()));
        // Should NOT have -d for unspecified IP
        let d_idx = args.iter().position(|a| a == "-d");
        assert_eq!(d_idx, None, "-d should not appear for unspecified host IP");
    }

    #[test]
    fn dnat_args_specified_ip_includes_d() {
        let m = test_dockermap(
            "192.168.1.100",
            443,
            "10.0.0.2",
            443,
            TransportProtocol::Tcp,
            2,
        );
        let args = build_dnat_rule_args(&m);
        assert!(args.contains(&"-d".into()));
        assert!(args.contains(&"192.168.1.100".into()));
    }

    #[test]
    fn dnat_args_ipv6_host() {
        let m = test_dockermap("2001:db8::1", 53, "::1", 53, TransportProtocol::Udp, 3);
        let args = build_dnat_rule_args(&m);
        assert!(args.contains(&"-d".into()));
        assert!(args.contains(&"2001:db8::1".into()));
        assert!(args.contains(&"udp".into()));
        assert!(args.contains(&"[::1]:53".into()));
    }

    #[test]
    fn dnat_args_includes_comment() {
        let m = test_dockermap(
            "10.0.0.1",
            3000,
            "10.0.0.2",
            3000,
            TransportProtocol::Tcp,
            4,
        );
        let args = build_dnat_rule_args(&m);
        assert!(args.contains(&"--comment".into()));
        assert!(args.contains(&m.rule_comment));
    }

    // ── build_forward_accept_args ──

    #[test]
    fn forward_accept_args_includes_ctn_ip_and_port() {
        let m = test_dockermap("0.0.0.0", 80, "172.17.0.3", 8080, TransportProtocol::Tcp, 5);
        let args = build_forward_accept_args(&m);
        assert!(args.contains(&"172.17.0.3".into()));
        assert!(args.contains(&"8080".into()));
        assert!(args.contains(&"ACCEPT".into()));
        assert!(args.contains(&"NATMAP".into()));
    }

    // ── build_masquerade_args ──

    #[test]
    fn masquerade_args_matches_ctn_ip() {
        let m = test_dockermap(
            "0.0.0.0",
            80,
            "172.17.0.4",
            25565,
            TransportProtocol::Udp,
            6,
        );
        let args = build_masquerade_args(&m);
        assert!(args.contains(&"MASQUERADE".into()));
        // Both -s and -d should match container IP
        let s_idx = args.iter().position(|a| a == "-s").unwrap();
        let d_idx = args.iter().position(|a| a == "-d").unwrap();
        assert_eq!(
            args[s_idx + 1],
            args[d_idx + 1],
            "-s and -d should have same IP"
        );
    }

    // ── build_output_dnat_args ──

    #[test]
    fn output_dnat_args_uses_output_dst() {
        let m = test_dockermap("0.0.0.0", 9090, "10.0.0.5", 9090, TransportProtocol::Tcp, 7);
        let args = build_output_dnat_args(&m, "127.0.0.1");
        assert!(args.contains(&"127.0.0.1".into()));
        assert!(args.contains(&"OUTPUT".into()));
    }

    // ── build_loopback_masq_args ──

    #[test]
    fn loopback_masq_args_returned_when_host_unspecified_and_ctn_non_loopback() {
        let m = test_dockermap("0.0.0.0", 80, "10.0.0.2", 80, TransportProtocol::Tcp, 8);
        assert!(build_loopback_masq_args(&m).is_some());
    }

    #[test]
    fn loopback_masq_args_returned_when_host_loopback() {
        let m = test_dockermap("127.0.0.1", 80, "10.0.0.2", 80, TransportProtocol::Tcp, 9);
        assert!(build_loopback_masq_args(&m).is_some());
    }

    #[test]
    fn loopback_masq_args_none_when_ctn_is_loopback() {
        let m = test_dockermap("0.0.0.0", 80, "127.0.0.1", 80, TransportProtocol::Tcp, 10);
        assert!(build_loopback_masq_args(&m).is_none());
    }

    #[test]
    fn loopback_masq_args_none_when_host_specified() {
        let m = test_dockermap("10.0.0.1", 80, "10.0.0.2", 80, TransportProtocol::Tcp, 11);
        assert!(build_loopback_masq_args(&m).is_none());
    }

    #[test]
    fn loopback_masq_args_ipv6_src() {
        let m = test_dockermap("::", 80, "2001:db8::2", 80, TransportProtocol::Tcp, 12);
        let args = build_loopback_masq_args(&m).unwrap();
        assert!(args.contains(&"::1/128".into()));
    }

    // ── build_static_dnat_prerouting_args ──

    #[test]
    fn static_dnat_prerouting_single_port() {
        let cfg = DnatConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        };
        let args = build_static_dnat_prerouting_args(&cfg);
        assert!(args.contains(&"--dport".into()));
        assert!(args.contains(&"80".into()));
        assert!(args.contains(&"10.0.0.99:80".into()));
        assert!(!args.contains(&"multiport".into()));
    }

    #[test]
    fn static_dnat_prerouting_multiport() {
        let cfg = DnatConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80,443,8080".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        };
        let args = build_static_dnat_prerouting_args(&cfg);
        assert!(args.contains(&"multiport".into()));
        assert!(args.contains(&"80,443,8080".into()));
        assert!(args.contains(&"10.0.0.99".into()));
        assert!(!args.contains(&":".into())); // no port rewrite for multiport
    }

    #[test]
    fn static_dnat_prerouting_with_ext_if() {
        let cfg = DnatConfig {
            ext_ip: "198.51.100.10".into(),
            int_ip: "10.0.0.1".into(),
            ports: "53".into(),
            proto: TransportProtocol::Udp,
            ext_if: Some("eth0".into()),
            preserve_src_ip: false,
        };
        let args = build_static_dnat_prerouting_args(&cfg);
        assert!(args.contains(&"-i".into()));
        assert!(args.contains(&"eth0".into()));
    }

    #[test]
    fn static_dnat_prerouting_udp_proto() {
        let cfg = DnatConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "19132".into(),
            proto: TransportProtocol::Udp,
            ext_if: None,
            preserve_src_ip: false,
        };
        let args = build_static_dnat_prerouting_args(&cfg);
        assert!(args.contains(&"udp".into()));
    }

    // ── build_static_dnat_forward_args ──

    #[test]
    fn static_dnat_forward_single_port() {
        let cfg = DnatConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        };
        let args = build_static_dnat_forward_args(&cfg);
        assert!(args.contains(&"ACCEPT".into()));
        assert!(args.contains(&"--dport".into()));
        assert!(args.contains(&"80".into()));
        assert!(!args.contains(&"multiport".into()));
    }

    #[test]
    fn static_dnat_forward_multiport() {
        let cfg = DnatConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "3000,3001,3002".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        };
        let args = build_static_dnat_forward_args(&cfg);
        assert!(args.contains(&"multiport".into()));
        assert!(args.contains(&"3000,3001,3002".into()));
    }

    // ── build_snat_args ──

    #[test]
    fn snat_args_contains_expected_fields() {
        let cfg = SnatConfig {
            int_ip: "10.0.0.1".into(),
            ext_ip: "203.0.113.50".into(),
            ext_if: "eth0".into(),
        };
        let args = build_snat_args(&cfg);
        assert!(args.contains(&"SNAT".into()));
        assert!(args.contains(&"10.0.0.1".into()));
        assert!(args.contains(&"203.0.113.50".into()));
        assert!(args.contains(&"eth0".into()));
    }

    // ── build_hairpin_prerouting_args ──

    #[test]
    fn hairpin_prerouting_args_returned_when_no_lan_cidr() {
        let cfg = HairpinConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80".into(),
            proto: TransportProtocol::Tcp,
            lan_cidr: None,
        };
        assert!(build_hairpin_prerouting_args(&cfg).is_some());
    }

    #[test]
    fn hairpin_prerouting_args_none_when_lan_cidr_set() {
        let cfg = HairpinConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80".into(),
            proto: TransportProtocol::Tcp,
            lan_cidr: Some("10.0.0.0/24".into()),
        };
        assert!(build_hairpin_prerouting_args(&cfg).is_none());
    }

    #[test]
    fn hairpin_prerouting_args_multiport() {
        let cfg = HairpinConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80,443".into(),
            proto: TransportProtocol::Tcp,
            lan_cidr: None,
        };
        let args = build_hairpin_prerouting_args(&cfg).unwrap();
        assert!(args.contains(&"multiport".into()));
    }

    // ── build_hairpin_postrouting_args ──

    #[test]
    fn hairpin_postrouting_args_default_src_without_cidr() {
        let cfg = HairpinConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80".into(),
            proto: TransportProtocol::Tcp,
            lan_cidr: None,
        };
        let args = build_hairpin_postrouting_args(&cfg);
        assert!(args.contains(&"0.0.0.0/0".into()));
        assert!(args.contains(&"MASQUERADE".into()));
    }

    #[test]
    fn hairpin_postrouting_args_uses_lan_cidr_when_set() {
        let cfg = HairpinConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80".into(),
            proto: TransportProtocol::Udp,
            lan_cidr: Some("10.0.0.0/24".into()),
        };
        let args = build_hairpin_postrouting_args(&cfg);
        assert!(args.contains(&"10.0.0.0/24".into()));
        assert!(!args.contains(&"0.0.0.0/0".into()));
    }

    #[test]
    fn hairpin_postrouting_args_multiport() {
        let cfg = HairpinConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "3000,3001".into(),
            proto: TransportProtocol::Tcp,
            lan_cidr: None,
        };
        let args = build_hairpin_postrouting_args(&cfg);
        assert!(args.contains(&"multiport".into()));
    }

    // ── cmd_for ──
    // The manager struct just delegates; test the helper directly.

    #[test]
    fn cmd_for_ipv4_returns_iptables() {
        let mgr = IptablesManager::new();
        assert_eq!(mgr.cmd_for(false), "iptables");
    }

    #[test]
    fn cmd_for_ipv6_returns_ip6tables() {
        let mgr = IptablesManager::new();
        assert_eq!(mgr.cmd_for(true), "ip6tables");
    }
}
