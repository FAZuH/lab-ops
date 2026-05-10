use std::process::Command;

use color_eyre::Result;
use tracing::debug;
use tracing::error;
use tracing::info;

use crate::models::ActivePortMapping;

const DOCKER_USER_CHAIN: &str = "DOCKER-USER";
const NATMAP_CHAIN: &str = "NATMAP";

pub struct IptablesManager;

impl Default for IptablesManager {
    fn default() -> Self {
        Self::new()
    }
}

impl IptablesManager {
    pub fn new() -> Self {
        Self
    }

    /// Sets up the required chains and jumps.
    pub fn setup(&self) -> Result<()> {
        info!("Setting up iptables chains and jumps");

        for &cmd in &["iptables", "ip6tables"] {
            // Verify DOCKER-USER exists (it should, Docker makes it). Create if missing.
            if !self.chain_exists(cmd, "filter", DOCKER_USER_CHAIN) {
                self.run(cmd, &["-t", "filter", "-N", DOCKER_USER_CHAIN], true)?;
                self.run(
                    cmd,
                    &["-t", "filter", "-I", "FORWARD", "-j", DOCKER_USER_CHAIN],
                    true,
                )?;
            }

            // Create NATMAP subchain in nat table (DNAT rules live here)
            if !self.chain_exists(cmd, "nat", NATMAP_CHAIN) {
                self.run(cmd, &["-t", "nat", "-N", NATMAP_CHAIN], true)?;
            }

            // Create NATMAP subchain in filter table (FORWARD ACCEPT rules live here)
            if !self.chain_exists(cmd, "filter", NATMAP_CHAIN) {
                self.run(cmd, &["-t", "filter", "-N", NATMAP_CHAIN], true)?;
            }

            // Jump from DOCKER-USER to NATMAP in filter table (if not exists)
            if !self.rule_exists(
                cmd,
                &["-t", "filter", "-C", DOCKER_USER_CHAIN, "-j", NATMAP_CHAIN],
            ) {
                self.run(
                    cmd,
                    &["-t", "filter", "-I", DOCKER_USER_CHAIN, "-j", NATMAP_CHAIN],
                    true,
                )?;
            }

            // Jump from PREROUTING to NATMAP in nat table (if not exists)
            if !self.rule_exists(cmd, &["-t", "nat", "-C", "PREROUTING", "-j", NATMAP_CHAIN]) {
                self.run(
                    cmd,
                    &["-t", "nat", "-I", "PREROUTING", "-j", NATMAP_CHAIN],
                    true,
                )?;
            }
        }

        Ok(())
    }

    /// Installs mapping rules for a given ActivePortMapping
    pub fn install_mapping(&self, mapping: &ActivePortMapping) -> Result<()> {
        debug!("Installing mapping: {:?}", mapping);
        let cmd = self.cmd_for(mapping.request.is_ipv6());

        let host_ip = mapping.request.host_addr.ip();
        let host_port = mapping.request.host_addr.port().to_string();
        let container_addr = mapping.request.container_addr.to_string();
        let container_ip = mapping.request.container_addr.ip().to_string();
        let container_port = mapping.request.container_addr.port().to_string();
        let proto = mapping.request.proto.to_string();
        let comment = &mapping.rule_comment;

        // 1. DNAT rule (nat/PREROUTING via NATMAP)
        self.run(
            cmd,
            &[
                "-t",
                "nat",
                "-A",
                NATMAP_CHAIN,
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
            true,
        )?;

        // 2. FORWARD ACCEPT rule in filter/NATMAP
        self.run(
            cmd,
            &[
                "-t",
                "filter",
                "-A",
                NATMAP_CHAIN,
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
            true,
        )?;

        // 3. Masquerade (hairpin NAT)
        self.run(
            cmd,
            &[
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
            true,
        )?;

        // 4. OUTPUT DNAT rule — always needed for locally-generated traffic.
        //    PREROUTING only catches forwarded/ingress traffic; locally-generated
        //    packets (curl localhost, curl <host-ip>) go through OUTPUT.
        let output_dst = if host_ip.is_unspecified() {
            if mapping.request.is_ipv6() {
                "::1"
            } else {
                "127.0.0.1"
            }
        } else {
            &mapping.request.host_addr.ip().to_string()
        };
        self.run(
            cmd,
            &[
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
            true,
        )?;

        Ok(())
    }

    /// Removes all rules associated with a mapping comment
    pub fn remove_mapping(&self, mapping: &ActivePortMapping) -> Result<()> {
        debug!("Removing mapping: {:?}", mapping);
        self.remove_by_comment(&mapping.rule_comment, mapping.request.is_ipv6())?;
        Ok(())
    }

    /// Delete rules matching exactly the comment (across all tables/chains where we insert)
    fn remove_by_comment(&self, comment: &str, is_ipv6: bool) -> Result<()> {
        let cmd = self.cmd_for(is_ipv6);

        // Delete from NATMAP in nat table
        self.delete_all_matching(cmd, "nat", NATMAP_CHAIN, comment)?;
        // Delete from NATMAP in filter table
        self.delete_all_matching(cmd, "filter", NATMAP_CHAIN, comment)?;
        // Delete from POSTROUTING in nat table
        self.delete_all_matching(cmd, "nat", "POSTROUTING", comment)?;
        // Delete from OUTPUT in nat table (localhost DNAT)
        self.delete_all_matching(cmd, "nat", "OUTPUT", comment)?;

        Ok(())
    }

    /// Flush all rules for a specific container by iterating its comments
    pub fn flush_container_rules(&self, container_id: &str) -> Result<()> {
        debug!("Flushing all rules for container: {}", container_id);
        let prefix = format!("dockernatmap:{}", container_id);

        for &cmd in &["iptables", "ip6tables"] {
            // Find and delete rules matching prefix in relevant chains
            let _ = self.delete_all_matching_prefix(cmd, "nat", NATMAP_CHAIN, &prefix);
            let _ = self.delete_all_matching_prefix(cmd, "filter", NATMAP_CHAIN, &prefix);
            let _ = self.delete_all_matching_prefix(cmd, "nat", "POSTROUTING", &prefix);
            let _ = self.delete_all_matching_prefix(cmd, "nat", "OUTPUT", &prefix);
        }

        Ok(())
    }

    // Helper functions

    fn cmd_for(&self, is_ipv6: bool) -> &'static str {
        if is_ipv6 { "ip6tables" } else { "iptables" }
    }

    fn run(&self, cmd: &str, args: &[&str], fail_on_error: bool) -> Result<bool> {
        let out = Command::new(cmd).args(args).output()?;
        if out.status.success() {
            Ok(true)
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            if fail_on_error {
                error!("{} {} failed: {}", cmd, args.join(" "), err);
                color_eyre::eyre::bail!("{} failed: {}", cmd, err);
            }
            Ok(false)
        }
    }

    fn chain_exists(&self, cmd: &str, table: &str, chain: &str) -> bool {
        self.run(cmd, &["-t", table, "-L", chain, "-n"], false)
            .unwrap_or(false)
    }

    fn rule_exists(&self, cmd: &str, args: &[&str]) -> bool {
        self.run(cmd, args, false).unwrap_or(false)
    }

    fn delete_all_matching(
        &self,
        cmd: &str,
        table: &str,
        chain: &str,
        comment: &str,
    ) -> Result<()> {
        // This is a simplistic approach: loop finding and deleting using `iptables-save` logic or repeated `-D`
        // We will read rules and find line numbers.
        loop {
            let rules = self.get_rules(cmd, table, chain)?;
            let mut deleted = false;
            for (line_num, rule) in rules.iter().enumerate() {
                if rule.contains(&format!("--comment \"{}\"", comment))
                    || rule.contains(&format!("--comment {}", comment))
                {
                    // Delete by line number from bottom up (or just one by one)
                    let num = (line_num + 1).to_string();
                    self.run(cmd, &["-t", table, "-D", chain, &num], false)?;
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

    fn delete_all_matching_prefix(
        &self,
        cmd: &str,
        table: &str,
        chain: &str,
        prefix: &str,
    ) -> Result<()> {
        loop {
            let rules = self.get_rules(cmd, table, chain)?;
            let mut deleted = false;
            for (line_num, rule) in rules.iter().enumerate() {
                if rule.contains(&format!("--comment \"{}:", prefix))
                    || rule.contains(&format!("--comment {}:", prefix))
                    || rule.contains(&format!("--comment {}", prefix))
                {
                    let num = (line_num + 1).to_string();
                    self.run(cmd, &["-t", table, "-D", chain, &num], false)?;
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

    fn get_rules(&self, cmd: &str, table: &str, chain: &str) -> Result<Vec<String>> {
        let out = Command::new(cmd)
            .args(["-t", table, "-S", chain])
            .output()?;

        let stdout = String::from_utf8_lossy(&out.stdout);
        // filter out the chain declaration itself (-N or -P)
        let rules: Vec<String> = stdout
            .lines()
            .filter(|l| l.starts_with("-A ") || l.starts_with("-I "))
            .map(|l| l.to_string())
            .collect();

        Ok(rules)
    }
}
