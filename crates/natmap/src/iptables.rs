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

/// Manages the lifecycle of iptables rules used by natmap.
///
/// Creates the `NATMAP` chain in both the `nat` and `filter` tables,
/// inserts jumps from `PREROUTING` and `DOCKER-USER`, and provides
/// methods to install/remove individual rules.
pub struct IptablesManager;

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
        let req = &map.request;

        let host_ip = req.host_addr.ip();
        let host_port = req.host_addr.port().to_string();
        let container_addr = req.container_addr.to_string();
        let container_ip = req.container_addr.ip().to_string();
        let container_port = req.container_addr.port().to_string();
        let proto = req.proto.to_string();
        let comment = &map.rule_comment;

        // 1. DNAT rule via nat NATMAP
        self.run(
            cmd,
            [
                "-t",
                "nat",
                "-A",
                NATMAP,
                "-p",
                &proto,
                "--dport",
                &host_port,
                "-j",
                "DNAT",
                "--to-destination",
                &container_addr,
                "-m",
                "comment",
                "--comment",
                comment,
            ],
        )?;

        // 2. FORWARD ACCEPT rule in filter NATMAP
        self.run(
            cmd,
            [
                "-t",
                "filter",
                "-A",
                NATMAP,
                "-d",
                &container_ip,
                "-p",
                &proto,
                "--dport",
                &container_port,
                "-j",
                "ACCEPT",
                "-m",
                "comment",
                "--comment",
                comment,
            ],
        )?;

        // 3. Masquerade (hairpin NAT)
        self.run(
            cmd,
            [
                "-t",
                "nat",
                "-A",
                "POSTROUTING",
                "-s",
                &container_ip,
                "-d",
                &container_ip,
                "-p",
                &proto,
                "--dport",
                &container_port,
                "-j",
                "MASQUERADE",
                "-m",
                "comment",
                "--comment",
                comment,
            ],
        )?;

        // 4. OUTPUT DNAT rule — always needed for locally-generated traffic.
        //    PREROUTING only catches forwarded/ingress traffic; locally-generated
        //    packets (curl localhost, curl <host-ip>) go through OUTPUT.
        let output_dst = if host_ip.is_unspecified() {
            if map.request.is_ipv6() {
                "::1"
            } else {
                "127.0.0.1"
            }
        } else {
            &map.request.host_addr.ip().to_string()
        };
        self.run(
            cmd,
            [
                "-t",
                "nat",
                "-A",
                "OUTPUT",
                "-d",
                output_dst,
                "-p",
                &proto,
                "--dport",
                &host_port,
                "-j",
                "DNAT",
                "--to-destination",
                &container_addr,
                "-m",
                "comment",
                "--comment",
                comment,
            ],
        )?;

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
        let comment = config.rule_comment();
        let multiport = config.ports.contains(',');
        let port_args = if multiport {
            vec!["-m", "multiport", "--dports", &config.ports]
        } else {
            vec!["--dport", &config.ports]
        };

        let mut pre_args = vec!["-t", "nat", "-A", "PREROUTING"];
        if let Some(ref iface) = config.ext_if {
            pre_args.extend(vec!["-i", iface]);
        }
        let proto = config.proto.to_lowercase();
        pre_args.extend(vec!["-d", &config.ext_ip, "-p", proto]);
        pre_args.extend(port_args.clone());
        let dest = if multiport {
            config.int_ip.clone()
        } else {
            format!("{}:{}", config.int_ip, config.ports)
        };
        pre_args.extend(vec!["-j", "DNAT", "--to-destination", &dest]);
        pre_args.extend(vec!["-m", "comment", "--comment", &comment]);
        self.run_success("iptables", &pre_args)?;

        let mut fwd_args = vec!["-A", "FORWARD", "-p", proto, "-d", &config.int_ip];
        fwd_args.extend(port_args);
        fwd_args.extend(vec!["-j", "ACCEPT"]);
        fwd_args.extend(vec!["-m", "comment", "--comment", &comment]);
        self.run_success("iptables", &fwd_args)?;
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
        let comment = config.rule_comment();
        let args = vec![
            "-t",
            "nat",
            "-A",
            "POSTROUTING",
            "-s",
            &config.int_ip,
            "-o",
            &config.ext_if,
            "-j",
            "SNAT",
            "--to-source",
            &config.ext_ip,
            "-m",
            "comment",
            "--comment",
            &comment,
        ];
        self.run_success("iptables", &args)?;
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

    /// Installs a hairpin NAT rule (PREROUTING DNAT + POSTROUTING MASQUERADE).
    pub fn install_hairpin(&self, config: &HairpinConfig) -> Result<()> {
        let comment = config.rule_comment();
        let multiport = config.ports.contains(',');
        let port_args: Vec<&str> = if multiport {
            vec!["-m", "multiport", "--dports", &config.ports]
        } else {
            vec!["--dport", &config.ports]
        };
        let proto = config.proto.to_lowercase();

        let mut pre_args = vec![
            "-t",
            "nat",
            "-A",
            "PREROUTING",
            "-s",
            &config.int_ip,
            "-d",
            &config.ext_ip,
            "-p",
            proto,
        ];
        pre_args.extend(port_args.clone());
        pre_args.extend(vec!["-j", "DNAT", "--to-destination", &config.int_ip]);
        pre_args.extend(vec!["-m", "comment", "--comment", &comment]);
        self.run_success("iptables", &pre_args)?;

        let mut post_args = vec![
            "-t",
            "nat",
            "-A",
            "POSTROUTING",
            "-s",
            "0.0.0.0/0",
            "-d",
            &config.int_ip,
            "-p",
            proto,
        ];
        post_args.extend(port_args);
        post_args.extend(vec!["-j", "MASQUERADE"]);
        post_args.extend(vec!["-m", "comment", "--comment", &comment]);
        self.run_success("iptables", &post_args)?;
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
