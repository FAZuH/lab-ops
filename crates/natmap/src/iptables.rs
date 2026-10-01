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

/// Interface for the iptables rule operations the daemon relies on.
///
/// [`IptablesManager`] is the production implementation. Daemon tests use
/// fakes so orchestration can be exercised without touching the host firewall.
pub trait Iptables: Send + Sync {
    /// Creates the `NATMAP` chains and inserts jump rules.
    fn setup(&self) -> Result<()>;

    /// Flushes and deletes the `NATMAP` chains and removes all natmap-commented rules.
    fn flush_all_natmap(&self) -> Result<()>;

    /// Installs DNAT, FORWARD ACCEPT, MASQUERADE, and OUTPUT DNAT rules for a Docker mapping.
    fn install_dockermap(&self, map: &DockerPortMap) -> Result<()>;

    /// Removes all iptables rules associated with a Docker mapping by its rule comment.
    fn remove_mapping(&self, map: &DockerPortMap) -> Result<()>;

    /// Installs a static DNAT rule (PREROUTING + FORWARD ACCEPT).
    fn install_dnat(&self, config: &DnatConfig) -> Result<()>;

    /// Removes a static DNAT rule (PREROUTING + FORWARD ACCEPT).
    fn remove_dnat(&self, config: &DnatConfig) -> Result<()>;

    /// Installs a static SNAT (source NAT) rule in the POSTROUTING chain.
    fn install_snat(&self, config: &SnatConfig) -> Result<()>;

    /// Removes a static SNAT rule from the POSTROUTING chain.
    fn remove_snat(&self, config: &SnatConfig) -> Result<()>;

    /// Installs a hairpin NAT rule.
    fn install_hairpin(&self, config: &HairpinConfig) -> Result<()>;

    /// Removes a hairpin NAT rule (PREROUTING DNAT + POSTROUTING MASQUERADE).
    fn remove_hairpin(&self, config: &HairpinConfig) -> Result<()>;

    /// Returns all natmap-commented rules across all tables.
    fn list_rules(&self) -> Result<Vec<String>>;
}

// ── Pure argument builders (testable without iptables) ──

/// Appends the port-match args for a static rule.
///
/// A comma-separated `ports` list is a multi-port set, which iptables only
/// matches through `-m multiport --dports`; a single port uses `--dport`.
fn push_port_args(args: &mut Vec<String>, ports: &str) {
    if ports.contains(',') {
        args.extend([
            "-m".into(),
            "multiport".into(),
            "--dports".into(),
            ports.into(),
        ]);
    } else {
        args.extend(["--dport".into(), ports.into()]);
    }
}

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
    args.push(config.proto.to_string());
    push_port_args(&mut args, &config.ports);
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
    args.push(config.proto.to_string());
    args.push("-d".into());
    args.push(config.int_ip.clone());
    push_port_args(&mut args, &config.ports);
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
    args.push(config.proto.to_string());
    push_port_args(&mut args, &config.ports);
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
    args.push(config.proto.to_string());
    push_port_args(&mut args, &config.ports);
    args.extend(["-j".into(), "MASQUERADE".into()]);
    args.extend(["-m".into(), "comment".into(), "--comment".into(), comment]);
    args
}

impl IptablesManager {
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
            // Docker normally creates DOCKER-USER, but the daemon can win the race.
            if !self.chain_exists(cmd, "filter", "DOCKER-USER") {
                self.run_success(cmd, ["-t", "filter", "-N", "DOCKER-USER"])?;
                // -I, not -A: the jump must precede Docker's own FORWARD rules.
                self.run_success(cmd, ["-t", "filter", "-I", "FORWARD", "-j", "DOCKER-USER"])?;
            }

            // DNAT rules land in nat/NATMAP, FORWARD ACCEPT in filter/NATMAP.
            if !self.chain_exists(cmd, "nat", NATMAP) {
                self.run_success(cmd, ["-t", "nat", "-N", NATMAP])?;
            }

            if !self.chain_exists(cmd, "filter", NATMAP) {
                self.run_success(cmd, ["-t", "filter", "-N", NATMAP])?;
            }

            if !self.rule_exists(cmd, &["-t", "filter", "-C", "DOCKER-USER", "-j", NATMAP]) {
                self.run(cmd, ["-t", "filter", "-I", "DOCKER-USER", "-j", NATMAP])?;
            }

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

        self.delete_all_matching(cmd, "nat", NATMAP, comment)?;
        self.delete_all_matching(cmd, "filter", NATMAP, comment)?;
        self.delete_all_matching(cmd, "nat", "POSTROUTING", comment)?;
        // OUTPUT carries the localhost DNAT rule.
        self.delete_all_matching(cmd, "nat", "OUTPUT", comment)?;

        Ok(())
    }

    /// Flushes and deletes a specific chain in a given table.
    ///
    /// Best-effort: both calls ignore their result so a missing chain or a
    /// chain still referenced elsewhere does not abort the surrounding flush.
    fn flush_chain(&self, cmd: &str, table: &str, chain: &str) -> Result<()> {
        let _ = self.run(cmd, ["-t", table, "-F", chain]);
        let _ = self.run(cmd, ["-t", table, "-X", chain]);
        Ok(())
    }

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
            tracing::error!(program = %program, args = %args_str, error = %format!("{err:#}"), "command failed");
            bail!("{program} failed: {err}");
        }
    }

    /// Runs a command, logging the invocation at trace and returning its output
    /// whatever the exit status.
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
    ///
    /// Uses `run`, not `run_success`: `-C` exits non-zero when the rule is
    /// absent, which is the answer, not a failure.
    fn rule_exists(&self, cmd: &str, args: &[&str]) -> bool {
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
        loop {
            let rules = self.get_rules(cmd, table, chain)?;
            let mut deleted = false;
            for (line_num, rule) in rules.iter().enumerate() {
                if rule.contains(&format!("--comment \"{comment}\""))
                    || rule.contains(&format!("--comment {comment}"))
                {
                    let num = (line_num + 1).to_string();
                    self.run_success(cmd, ["-t", table, "-D", chain, &num])?;
                    deleted = true;
                    break; // Restart: deleting shifts every later line number.
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
        // -S also prints chain declarations, which have no line number to delete.
        let out = self.run(cmd, ["-t", table, "-S", chain])?;

        let rules = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with("-A ") || l.starts_with("-I "))
            .map(|l| l.to_string())
            .collect();

        Ok(rules)
    }

    /// Returns all natmap-commented rules across all tables.
    ///
    /// Runs `iptables-save` (all tables) and filters for lines containing the
    /// `natmap:` comment prefix. Fails when `iptables-save` exits non-zero.
    pub fn list_rules(&self) -> Result<Vec<String>> {
        let out = self.run_success("iptables-save", [] as [&str; 0])?;
        let rules = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains("natmap:"))
            .map(|l| l.to_string())
            .collect();
        Ok(rules)
    }
}

impl Iptables for IptablesManager {
    fn setup(&self) -> Result<()> {
        IptablesManager::setup(self)
    }

    fn flush_all_natmap(&self) -> Result<()> {
        IptablesManager::flush_all_natmap(self)
    }

    fn install_dockermap(&self, map: &DockerPortMap) -> Result<()> {
        IptablesManager::install_dockermap(self, map)
    }

    fn remove_mapping(&self, map: &DockerPortMap) -> Result<()> {
        IptablesManager::remove_mapping(self, map)
    }

    fn install_dnat(&self, config: &DnatConfig) -> Result<()> {
        IptablesManager::install_dnat(self, config)
    }

    fn remove_dnat(&self, config: &DnatConfig) -> Result<()> {
        IptablesManager::remove_dnat(self, config)
    }

    fn install_snat(&self, config: &SnatConfig) -> Result<()> {
        IptablesManager::install_snat(self, config)
    }

    fn remove_snat(&self, config: &SnatConfig) -> Result<()> {
        IptablesManager::remove_snat(self, config)
    }

    fn install_hairpin(&self, config: &HairpinConfig) -> Result<()> {
        IptablesManager::install_hairpin(self, config)
    }

    fn remove_hairpin(&self, config: &HairpinConfig) -> Result<()> {
        IptablesManager::remove_hairpin(self, config)
    }

    fn list_rules(&self) -> Result<Vec<String>> {
        IptablesManager::list_rules(self)
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

    fn make_dockermap(
        host_ip: &str,
        host_port: u16,
        ctn_ip: &str,
        ctn_port: u16,
        proto: TransportProtocol,
    ) -> DockerPortMap {
        let req = DockerPortMapRequest {
            host_addr: SocketAddr::new(IpAddr::from_str(host_ip).unwrap(), host_port),
            container_addr: SocketAddr::new(IpAddr::from_str(ctn_ip).unwrap(), ctn_port),
            proto,
        };
        DockerPortMap::new(1, req, "c1".into(), "svc".into())
    }

    fn make_dnat(
        ext_ip: &str,
        int_ip: &str,
        ports: &str,
        proto: TransportProtocol,
        ext_if: Option<&str>,
    ) -> DnatConfig {
        DnatConfig {
            ext_ip: ext_ip.into(),
            int_ip: int_ip.into(),
            ports: ports.into(),
            proto,
            ext_if: ext_if.map(Into::into),
            preserve_src_ip: false,
        }
    }

    fn make_hairpin(
        ext_ip: &str,
        int_ip: &str,
        ports: &str,
        proto: TransportProtocol,
        lan_cidr: Option<&str>,
    ) -> HairpinConfig {
        HairpinConfig {
            ext_ip: ext_ip.into(),
            int_ip: int_ip.into(),
            ports: ports.into(),
            proto,
            lan_cidr: lan_cidr.map(Into::into),
        }
    }

    // The loop lives here, not in a test body, so the cases stay a plain table.
    // Each case gives the argv as one literal command line, split on whitespace;
    // no argv token in this module contains a space, so the split is exact.
    fn assert_argv<T>(build: fn(&T) -> Vec<String>, cases: &[(&str, T, &str)]) {
        for (name, input, expected) in cases {
            let expected: Vec<String> = expected.split_whitespace().map(Into::into).collect();
            assert_eq!(build(input), expected, "case: {name}");
        }
    }

    // Same, for builders that return `None` when the rule is not needed.
    fn assert_argv_opt<T>(build: fn(&T) -> Option<Vec<String>>, cases: &[(&str, T, Option<&str>)]) {
        for (name, input, expected) in cases {
            let expected = expected.map(|argv| argv.split_whitespace().map(Into::into).collect());
            assert_eq!(build(input), expected, "case: {name}");
        }
    }

    #[test]
    fn build_dnat_rule_args_exact_argv() {
        assert_argv(
            build_dnat_rule_args,
            &[
                (
                    "unspecified host ip omits -d",
                    make_dockermap("0.0.0.0", 8080, "10.0.0.2", 80, TransportProtocol::Tcp),
                    "-t nat -A NATMAP -p tcp --dport 8080 -j DNAT --to-destination 10.0.0.2:80 -m comment --comment natmap:c1:8080",
                ),
                (
                    "specified host ip matches -d",
                    make_dockermap(
                        "192.168.1.100",
                        443,
                        "10.0.0.2",
                        443,
                        TransportProtocol::Tcp,
                    ),
                    "-t nat -A NATMAP -p tcp -d 192.168.1.100 --dport 443 -j DNAT --to-destination 10.0.0.2:443 -m comment --comment natmap:c1:443",
                ),
                (
                    "ipv6 host and container",
                    make_dockermap("2001:db8::1", 53, "::1", 53, TransportProtocol::Udp),
                    "-t nat -A NATMAP -p udp -d 2001:db8::1 --dport 53 -j DNAT --to-destination [::1]:53 -m comment --comment natmap:c1:53",
                ),
                (
                    "comment carries the host port",
                    make_dockermap("10.0.0.1", 3000, "10.0.0.2", 3000, TransportProtocol::Tcp),
                    "-t nat -A NATMAP -p tcp -d 10.0.0.1 --dport 3000 -j DNAT --to-destination 10.0.0.2:3000 -m comment --comment natmap:c1:3000",
                ),
            ],
        );
    }

    #[test]
    fn build_forward_accept_args_exact_argv() {
        assert_argv(
            build_forward_accept_args,
            &[(
                "matches the container address, not the host one",
                make_dockermap("0.0.0.0", 80, "172.17.0.3", 8080, TransportProtocol::Tcp),
                "-t filter -A NATMAP -d 172.17.0.3 -p tcp --dport 8080 -j ACCEPT -m comment --comment natmap:c1:80",
            )],
        );
    }

    #[test]
    fn build_masquerade_args_exact_argv() {
        assert_argv(
            build_masquerade_args,
            &[(
                "source and destination both match the container",
                make_dockermap("0.0.0.0", 80, "172.17.0.4", 25565, TransportProtocol::Udp),
                "-t nat -A POSTROUTING -s 172.17.0.4 -d 172.17.0.4 -p udp --dport 25565 -j MASQUERADE -m comment --comment natmap:c1:80",
            )],
        );
    }

    #[test]
    fn build_output_dnat_args_exact_argv() {
        assert_argv(
            |map| build_output_dnat_args(map, "127.0.0.1"),
            &[(
                "matches the output destination and the host port",
                make_dockermap("0.0.0.0", 9090, "10.0.0.5", 9443, TransportProtocol::Tcp),
                "-t nat -A OUTPUT -d 127.0.0.1 -p tcp --dport 9090 -j DNAT --to-destination 10.0.0.5:9443 -m comment --comment natmap:c1:9090",
            )],
        );
    }

    #[test]
    fn build_loopback_masq_args_exact_argv() {
        assert_argv_opt(
            build_loopback_masq_args,
            &[
                (
                    "unspecified host ip and non-loopback container",
                    make_dockermap("0.0.0.0", 80, "10.0.0.2", 80, TransportProtocol::Tcp),
                    Some(
                        "-t nat -A POSTROUTING -s 127.0.0.0/8 -d 10.0.0.2 -p tcp --dport 80 -j MASQUERADE -m comment --comment natmap:c1:80",
                    ),
                ),
                (
                    "loopback host ip",
                    make_dockermap("127.0.0.1", 80, "10.0.0.2", 80, TransportProtocol::Tcp),
                    Some(
                        "-t nat -A POSTROUTING -s 127.0.0.0/8 -d 10.0.0.2 -p tcp --dport 80 -j MASQUERADE -m comment --comment natmap:c1:80",
                    ),
                ),
                (
                    "container is loopback",
                    make_dockermap("0.0.0.0", 80, "127.0.0.1", 80, TransportProtocol::Tcp),
                    None,
                ),
                (
                    "host ip is specified",
                    make_dockermap("10.0.0.1", 80, "10.0.0.2", 80, TransportProtocol::Tcp),
                    None,
                ),
                (
                    "ipv6 uses a /128 loopback source",
                    make_dockermap("::", 80, "2001:db8::2", 80, TransportProtocol::Tcp),
                    Some(
                        "-t nat -A POSTROUTING -s ::1/128 -d 2001:db8::2 -p tcp --dport 80 -j MASQUERADE -m comment --comment natmap:c1:80",
                    ),
                ),
            ],
        );
    }

    #[test]
    fn build_static_dnat_prerouting_args_exact_argv() {
        assert_argv(
            build_static_dnat_prerouting_args,
            &[
                (
                    "single port appends the port to the destination",
                    make_dnat(
                        "203.0.113.50",
                        "10.0.0.99",
                        "80",
                        TransportProtocol::Tcp,
                        None,
                    ),
                    "-t nat -A PREROUTING -d 203.0.113.50 -p tcp --dport 80 -j DNAT --to-destination 10.0.0.99:80 -m comment --comment natmap:dnat:203.0.113.50:80",
                ),
                (
                    "multiport uses the multiport match and no port rewrite",
                    make_dnat(
                        "203.0.113.50",
                        "10.0.0.99",
                        "80,443,8080",
                        TransportProtocol::Tcp,
                        None,
                    ),
                    "-t nat -A PREROUTING -d 203.0.113.50 -p tcp -m multiport --dports 80,443,8080 -j DNAT --to-destination 10.0.0.99 -m comment --comment natmap:dnat:203.0.113.50:80,443,8080",
                ),
                (
                    "external interface precedes the address match",
                    make_dnat(
                        "198.51.100.10",
                        "10.0.0.1",
                        "53",
                        TransportProtocol::Udp,
                        Some("eth0"),
                    ),
                    "-t nat -A PREROUTING -i eth0 -d 198.51.100.10 -p udp --dport 53 -j DNAT --to-destination 10.0.0.1:53 -m comment --comment natmap:dnat:198.51.100.10:53",
                ),
                (
                    "udp on a high port",
                    make_dnat(
                        "203.0.113.50",
                        "10.0.0.99",
                        "19132",
                        TransportProtocol::Udp,
                        None,
                    ),
                    "-t nat -A PREROUTING -d 203.0.113.50 -p udp --dport 19132 -j DNAT --to-destination 10.0.0.99:19132 -m comment --comment natmap:dnat:203.0.113.50:19132",
                ),
            ],
        );
    }

    #[test]
    fn build_static_dnat_forward_args_exact_argv() {
        assert_argv(
            build_static_dnat_forward_args,
            &[
                (
                    "single port",
                    make_dnat(
                        "203.0.113.50",
                        "10.0.0.99",
                        "80",
                        TransportProtocol::Tcp,
                        None,
                    ),
                    "-A FORWARD -p tcp -d 10.0.0.99 --dport 80 -j ACCEPT -m comment --comment natmap:dnat:203.0.113.50:80",
                ),
                (
                    "multiport",
                    make_dnat(
                        "203.0.113.50",
                        "10.0.0.99",
                        "3000,3001,3002",
                        TransportProtocol::Tcp,
                        None,
                    ),
                    "-A FORWARD -p tcp -d 10.0.0.99 -m multiport --dports 3000,3001,3002 -j ACCEPT -m comment --comment natmap:dnat:203.0.113.50:3000,3001,3002",
                ),
            ],
        );
    }

    #[test]
    fn build_snat_args_exact_argv() {
        assert_argv(
            build_snat_args,
            &[(
                "matches the internal source and masquerades to the external ip",
                SnatConfig {
                    int_ip: "10.0.0.1".into(),
                    ext_ip: "203.0.113.50".into(),
                    ext_if: "eth0".into(),
                },
                "-t nat -A POSTROUTING -s 10.0.0.1 -o eth0 -j SNAT --to-source 203.0.113.50 -m comment --comment natmap:snat:10.0.0.1:203.0.113.50",
            )],
        );
    }

    #[test]
    fn build_hairpin_prerouting_args_exact_argv() {
        assert_argv_opt(
            build_hairpin_prerouting_args,
            &[
                (
                    "no lan cidr installs the full hairpin",
                    make_hairpin(
                        "203.0.113.50",
                        "10.0.0.99",
                        "80",
                        TransportProtocol::Tcp,
                        None,
                    ),
                    Some(
                        "-t nat -A PREROUTING -s 10.0.0.99 -d 203.0.113.50 -p tcp --dport 80 -j DNAT --to-destination 10.0.0.99 -m comment --comment natmap:hairpin:203.0.113.50:10.0.0.99:80",
                    ),
                ),
                (
                    "lan cidr skips the prerouting rule",
                    make_hairpin(
                        "203.0.113.50",
                        "10.0.0.99",
                        "80",
                        TransportProtocol::Tcp,
                        Some("10.0.0.0/24"),
                    ),
                    None,
                ),
                (
                    "multiport",
                    make_hairpin(
                        "203.0.113.50",
                        "10.0.0.99",
                        "80,443",
                        TransportProtocol::Tcp,
                        None,
                    ),
                    Some(
                        "-t nat -A PREROUTING -s 10.0.0.99 -d 203.0.113.50 -p tcp -m multiport --dports 80,443 -j DNAT --to-destination 10.0.0.99 -m comment --comment natmap:hairpin:203.0.113.50:10.0.0.99:80,443",
                    ),
                ),
            ],
        );
    }

    #[test]
    fn build_hairpin_postrouting_args_exact_argv() {
        assert_argv(
            build_hairpin_postrouting_args,
            &[
                (
                    "no lan cidr matches every source",
                    make_hairpin(
                        "203.0.113.50",
                        "10.0.0.99",
                        "80",
                        TransportProtocol::Tcp,
                        None,
                    ),
                    "-t nat -A POSTROUTING -s 0.0.0.0/0 -d 10.0.0.99 -p tcp --dport 80 -j MASQUERADE -m comment --comment natmap:hairpin:203.0.113.50:10.0.0.99:80",
                ),
                (
                    "lan cidr narrows the masquerade to the lan",
                    make_hairpin(
                        "203.0.113.50",
                        "10.0.0.99",
                        "80",
                        TransportProtocol::Udp,
                        Some("10.0.0.0/24"),
                    ),
                    "-t nat -A POSTROUTING -s 10.0.0.0/24 -d 10.0.0.99 -p udp --dport 80 -j MASQUERADE -m comment --comment natmap:hairpin:203.0.113.50:10.0.0.99:80",
                ),
                (
                    "multiport",
                    make_hairpin(
                        "203.0.113.50",
                        "10.0.0.99",
                        "3000,3001",
                        TransportProtocol::Tcp,
                        None,
                    ),
                    "-t nat -A POSTROUTING -s 0.0.0.0/0 -d 10.0.0.99 -p tcp -m multiport --dports 3000,3001 -j MASQUERADE -m comment --comment natmap:hairpin:203.0.113.50:10.0.0.99:3000,3001",
                ),
            ],
        );
    }

    // The manager struct just delegates; test the helper directly.

    #[test]
    fn cmd_for_selects_binary_by_address_family() {
        let mgr = IptablesManager::new();

        assert_eq!(mgr.cmd_for(false), "iptables");
        assert_eq!(mgr.cmd_for(true), "ip6tables");
    }
}
