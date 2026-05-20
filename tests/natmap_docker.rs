#[cfg(feature = "docker-tests")]
mod docker_tests {
    use std::process::Command;
    use std::sync::Once;

    static INIT: Once = Once::new();

    fn setup_docker_image() -> &'static str {
        let image_name = "lab-ops-natmap-test:latest";
        INIT.call_once(|| {
            let dockerfile =
                "FROM ubuntu:24.04\nRUN apt-get update && apt-get install -y iptables\n";
            let mut cmd = Command::new("docker");
            cmd.args(["build", "-t", image_name, "-"]);

            use std::io::Write;
            let mut child = cmd
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("Failed to spawn docker build");
            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            stdin
                .write_all(dockerfile.as_bytes())
                .expect("Failed to write to stdin");
            drop(stdin);

            let status = child.wait().expect("Failed to wait for docker build");
            assert!(status.success(), "Failed to build docker image");
        });
        image_name
    }

    fn run_in_docker(args: &[&str]) -> String {
        let image = setup_docker_image();
        let binary_path = env!("CARGO_BIN_EXE_lab-ops");
        let mut cmd = Command::new("docker");
        cmd.args([
            "run",
            "--rm",
            "--privileged",
            "-v",
            &format!("{binary_path}:/usr/local/bin/lab-ops"),
            image,
            "sh",
            "-c",
        ]);

        let shell_cmd = args.join(" ");
        cmd.arg(&shell_cmd);

        let output = cmd.output().expect("Failed to execute docker run");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            panic!("Docker command failed: {shell_cmd}\nstdout:\n{stdout}\nstderr:\n{stderr}");
        }

        String::from_utf8_lossy(&output.stdout).to_string()
    }

    #[test]
    fn natmap_forward() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "iptables-save",
            "|",
            "grep DNAT",
        ]);
        assert!(
            out.contains("--to-destination 10.0.0.1"),
            "DNAT rule missing:\n{out}"
        );
    }

    #[test]
    fn natmap_snat() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns snat --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ext-if eth0",
            "&&",
            "iptables-save",
            "|",
            "grep SNAT",
        ]);
        assert!(
            out.contains("SNAT --to-source 1.2.3.4"),
            "SNAT rule missing:\n{out}"
        );
    }

    #[test]
    fn natmap_hairpin() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns hairpin --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "iptables-save",
            "|",
            "grep DNAT",
        ]);
        assert!(
            out.contains("--to-destination 10.0.0.1"),
            "Hairpin rule missing:\n{out}"
        );
    }

    // --- Tests for flush_all_natmap (crash recovery / shutdown cleanup) ---

    /// Daemon startup must flush natmap-commented rules from POSTROUTING.
    #[test]
    fn flush_postrouting_natmap_rules_on_startup() {
        run_in_docker(&[
            // Add a natmap-commented MASQUERADE rule (simulating stale Docker mapping hairpin)
            "iptables -t nat -A POSTROUTING -s 10.0.0.1 -d 10.0.0.1 -p tcp --dport 8080 -j MASQUERADE -m comment --comment 'natmap:deadbeef:32771'",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q 'natmap:deadbeef' || (echo 'FAIL: rule not added' >&2 && exit 1)",
            "&&",
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q 'natmap:deadbeef' && (echo 'FAIL: natmap rule not flushed from POSTROUTING' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// Daemon startup must flush natmap-commented rules from OUTPUT.
    #[test]
    fn flush_output_natmap_rules_on_startup() {
        run_in_docker(&[
            "iptables -t nat -A OUTPUT -d 127.0.0.1 -p tcp --dport 8080 -j DNAT --to-destination 10.0.0.1:80 -m comment --comment 'natmap:cafebabe:32771'",
            "&&",
            "iptables -t nat -S OUTPUT | grep -q 'natmap:cafebabe' || (echo 'FAIL: rule not added' >&2 && exit 1)",
            "&&",
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "iptables -t nat -S OUTPUT | grep -q 'natmap:cafebabe' && (echo 'FAIL: natmap rule not flushed from OUTPUT' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// Non-natmap rules in POSTROUTING must survive the flush.
    #[test]
    fn flush_preserves_non_natmap_postrouting_rules() {
        run_in_docker(&[
            "iptables -t nat -A POSTROUTING -s 10.0.0.0/24 -o eth0 -j MASQUERADE",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q '10.0.0.0/24' || (echo 'FAIL: non-natmap rule not added' >&2 && exit 1)",
            "&&",
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q '10.0.0.0/24' || (echo 'FAIL: non-natmap rule was incorrectly flushed' >&2 && exit 1)",
            "&&",
            "echo PASS",
        ]);
    }

    /// Non-natmap rules in OUTPUT must survive the flush.
    #[test]
    fn flush_preserves_non_natmap_output_rules() {
        run_in_docker(&[
            "iptables -t nat -A OUTPUT -p tcp -d 192.168.1.0/24 -j REDIRECT --to-port 3128",
            "&&",
            "iptables -t nat -S OUTPUT | grep -q 'REDIRECT' || (echo 'FAIL: non-natmap rule not added' >&2 && exit 1)",
            "&&",
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "iptables -t nat -S OUTPUT | grep -q 'REDIRECT' || (echo 'FAIL: non-natmap rule was incorrectly flushed' >&2 && exit 1)",
            "&&",
            "echo PASS",
        ]);
    }

    /// Daemon startup must flush natmap-commented rules via ip6tables too.
    #[test]
    fn flush_ip6tables_postrouting_natmap_rules() {
        run_in_docker(&[
            // ip6tables may not be available in all environments; skip if missing
            "which ip6tables || (echo 'SKIP: ip6tables not available' && exit 0)",
            "&&",
            "ip6tables -t nat -A POSTROUTING -s fc00::1 -d fc00::1 -p tcp --dport 8080 -j MASQUERADE -m comment --comment 'natmap:ipv6dead:32771'",
            "&&",
            "ip6tables -t nat -S POSTROUTING | grep -q 'natmap:ipv6dead' || (echo 'FAIL: ip6tables rule not added' >&2 && exit 1)",
            "&&",
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "ip6tables -t nat -S POSTROUTING | grep -q 'natmap:ipv6dead' && (echo 'FAIL: ip6tables natmap rule not flushed' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// Multiple natmap rules in the same chain must all be cleaned.
    #[test]
    fn flush_multiple_natmap_rules_in_postrouting() {
        run_in_docker(&[
            "iptables -t nat -A POSTROUTING -s 10.0.0.1 -d 10.0.0.1 -p tcp --dport 8080 -j MASQUERADE -m comment --comment 'natmap:aaa:11111'",
            "&&",
            "iptables -t nat -A POSTROUTING -s 10.0.0.2 -d 10.0.0.2 -p tcp --dport 9090 -j MASQUERADE -m comment --comment 'natmap:bbb:22222'",
            "&&",
            "iptables -t nat -A POSTROUTING -s 10.0.0.3 -d 10.0.0.3 -p tcp --dport 3000 -j MASQUERADE -m comment --comment 'natmap:ccc:33333'",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -c 'natmap:' | grep -q '3' || (echo 'FAIL: expected 3 natmap rules' >&2 && exit 1)",
            "&&",
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q 'natmap:' && (echo 'FAIL: natmap rules not flushed' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// Daemon startup must also flush natmap rules from the NATMAP chain in the filter table.
    #[test]
    fn flush_natmap_chain_in_filter_table() {
        run_in_docker(&[
            // The daemon setup() creates NATMAP chain, then flush_all_natmap() flushes it.
            // We start a daemon, stop it, manually add a stale rule to NATMAP chain,
            // restart, and verify it's gone.
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "kill %1 2>/dev/null; sleep 1",
            "&&",
            // Now add a stale rule to filter/NATMAP manually
            "iptables -t filter -A NATMAP -d 10.0.0.1 -p tcp --dport 80 -j ACCEPT -m comment --comment 'natmap:stale:32771'",
            "&&",
            "iptables -t filter -S NATMAP | grep -q 'natmap:stale' || (echo 'FAIL: stale rule not added' >&2 && exit 1)",
            "&&",
            // Restart daemon which should flush the stale rule via flush_all_natmap
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            // NATMAP chain is flushed AND deleted (-X), so listing it should fail or be empty
            "iptables -t filter -S NATMAP 2>/dev/null | grep -q 'natmap:stale' && (echo 'FAIL: stale rule not flushed from filter/NATMAP' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// NATMAP chain in nat table must also be flushed on startup.
    #[test]
    fn flush_natmap_chain_in_nat_table() {
        run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "kill %1 2>/dev/null; sleep 1",
            "&&",
            "iptables -t nat -A NATMAP -p tcp --dport 9999 -j DNAT --to-destination 10.0.0.1:80 -m comment --comment 'natmap:stale:9999'",
            "&&",
            "iptables -t nat -S NATMAP | grep -q 'natmap:stale' || (echo 'FAIL: stale rule not added' >&2 && exit 1)",
            "&&",
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "iptables -t nat -S NATMAP 2>/dev/null | grep -q 'natmap:stale' && (echo 'FAIL: stale rule not flushed from nat/NATMAP' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// Graceful shutdown (SIGINT) must flush natmap rules from POSTROUTING.
    #[test]
    fn graceful_shutdown_flushes_postrouting() {
        run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "DAEMON_PID=$!",
            "&&",
            "sleep 2",
            "&&",
            // Add a natmap rule that should be cleaned on shutdown
            "iptables -t nat -A POSTROUTING -s 10.0.0.5 -d 10.0.0.5 -p tcp --dport 9090 -j MASQUERADE -m comment --comment 'natmap:shutdown:32772'",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q 'natmap:shutdown' || (echo 'FAIL: rule not added' >&2 && exit 1)",
            "&&",
            // Send SIGINT to trigger graceful shutdown
            "kill -INT $DAEMON_PID",
            "&&",
            "sleep 2",
            "&&",
            // Verify rule was flushed during shutdown
            "iptables -t nat -S POSTROUTING | grep -q 'natmap:shutdown' && (echo 'FAIL: rule not flushed on shutdown' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// Graceful shutdown must flush natmap rules from OUTPUT.
    #[test]
    fn graceful_shutdown_flushes_output() {
        run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "DAEMON_PID=$!",
            "&&",
            "sleep 2",
            "&&",
            "iptables -t nat -A OUTPUT -d 127.0.0.1 -p tcp --dport 3000 -j DNAT --to-destination 10.0.0.10:3000 -m comment --comment 'natmap:shutdown:32773'",
            "&&",
            "iptables -t nat -S OUTPUT | grep -q 'natmap:shutdown' || (echo 'FAIL: rule not added' >&2 && exit 1)",
            "&&",
            "kill -INT $DAEMON_PID",
            "&&",
            "sleep 2",
            "&&",
            "iptables -t nat -S OUTPUT | grep -q 'natmap:shutdown' && (echo 'FAIL: rule not flushed on shutdown' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// Flushing an empty chain (no natmap rules) must not error.
    #[test]
    fn flush_when_no_natmap_rules_present() {
        let out = run_in_docker(&[
            // Ensure POSTROUTING has no natmap rules, then start daemon.
            // The daemon should start successfully even with nothing to flush.
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q 'natmap:' && (echo 'UNEXPECTED: pre-existing natmap rule' >&2 && exit 1) || echo PASS",
        ]);
        assert!(
            out.contains("PASS"),
            "Daemon should start cleanly with no natmap rules to flush:\n{out}"
        );
    }

    /// Both POSTROUTING and OUTPUT natmap rules are flushed in a single daemon start.
    #[test]
    fn flush_both_postrouting_and_output_simultaneously() {
        run_in_docker(&[
            "iptables -t nat -A POSTROUTING -s 10.0.0.1 -d 10.0.0.1 -p tcp --dport 8080 -j MASQUERADE -m comment --comment 'natmap:both:32771'",
            "&&",
            "iptables -t nat -A OUTPUT -d 127.0.0.1 -p tcp --dport 8080 -j DNAT --to-destination 10.0.0.1:80 -m comment --comment 'natmap:both:32771'",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q 'natmap:both' || (echo 'FAIL: POSTROUTING rule missing' >&2 && exit 1)",
            "&&",
            "iptables -t nat -S OUTPUT | grep -q 'natmap:both' || (echo 'FAIL: OUTPUT rule missing' >&2 && exit 1)",
            "&&",
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q 'natmap:both' && (echo 'FAIL: POSTROUTING rule not flushed' >&2 && exit 1) || echo 'POSTROUTING OK'",
            "&&",
            "iptables -t nat -S OUTPUT | grep -q 'natmap:both' && (echo 'FAIL: OUTPUT rule not flushed' >&2 && exit 1) || echo 'OUTPUT OK'",
        ]);
    }

    /// NATMAP chain jump rules (PREROUTING -> NATMAP, DOCKER-USER -> NATMAP) must survive flush.
    #[test]
    fn flush_preserves_natmap_jump_rules() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            // Verify jump from PREROUTING to NATMAP still exists after flush_all_natmap
            "iptables -t nat -S PREROUTING | grep -q -- '-j NATMAP' || (echo 'FAIL: PREROUTING -> NATMAP jump missing' >&2 && exit 1)",
            "&&",
            // Verify jump from DOCKER-USER to NATMAP still exists
            "iptables -t filter -S DOCKER-USER | grep -q -- '-j NATMAP' || (echo 'FAIL: DOCKER-USER -> NATMAP jump missing' >&2 && exit 1)",
            "&&",
            "echo PASS",
        ]);
        assert!(
            out.contains("PASS"),
            "NATMAP jump rules should survive flush:\n{out}"
        );
    }

    /// Rules with comments containing 'natmap:' as a substring (not prefix) must NOT be flushed.
    #[test]
    fn flush_does_not_match_natmap_substring_in_comment() {
        run_in_docker(&[
            // A comment that contains "natmap:" somewhere in the middle, not as a prefix
            "iptables -t nat -A POSTROUTING -s 10.0.0.1 -d 10.0.0.1 -p tcp --dport 8080 -j MASQUERADE -m comment --comment 'my-natmap:custom-rule'",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q 'my-natmap:custom-rule' || (echo 'FAIL: rule not added' >&2 && exit 1)",
            "&&",
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            // This rule should survive because it doesn't start with "natmap:"
            "iptables -t nat -S POSTROUTING | grep -q 'my-natmap:custom-rule' || (echo 'FAIL: non-prefixed rule incorrectly flushed' >&2 && exit 1)",
            "&&",
            "echo PASS",
        ]);
    }

    // --- Tests for clear command ---

    /// `clear` must remove a deployed DNAT rule.
    #[test]
    fn clear_removes_dnat() {
        run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "iptables -t nat -S NATMAP | grep -q 'DNAT' || (echo 'FAIL: DNAT rule not installed' >&2 && exit 1)",
            "&&",
            "lab-ops natmap --socket /tmp/ns clear",
            "&&",
            "iptables -t nat -S NATMAP | grep -q 'DNAT' && (echo 'FAIL: DNAT rule not cleared' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// `clear` must remove a deployed SNAT rule.
    #[test]
    fn clear_removes_snat() {
        run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns snat --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ext-if eth0",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q 'SNAT' || (echo 'FAIL: SNAT rule not installed' >&2 && exit 1)",
            "&&",
            "lab-ops natmap --socket /tmp/ns clear",
            "&&",
            "iptables -t nat -S POSTROUTING | grep -q 'natmap:' && (echo 'FAIL: SNAT rule not cleared' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// `clear` must remove a deployed hairpin rule.
    #[test]
    fn clear_removes_hairpin() {
        run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns hairpin --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "iptables -t nat -S NATMAP | grep -q 'DNAT' || (echo 'FAIL: Hairpin rule not installed' >&2 && exit 1)",
            "&&",
            "lab-ops natmap --socket /tmp/ns clear",
            "&&",
            "iptables -t nat -S NATMAP | grep -q 'DNAT' && (echo 'FAIL: Hairpin rule not cleared' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// `clear` must remove all types of rules simultaneously.
    #[test]
    fn clear_removes_all_rules() {
        run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "lab-ops natmap --socket /tmp/ns snat --ext-ip 5.6.7.8 --int-ip 10.0.0.2 --ext-if eth0",
            "&&",
            "lab-ops natmap --socket /tmp/ns hairpin --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ports 9090",
            "&&",
            "lab-ops natmap --socket /tmp/ns clear",
            "&&",
            "iptables -t nat -S | grep -q 'natmap:' && (echo 'FAIL: natmap rules remain after clear' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    /// After `clear`, restarting the daemon must not re-create rules (state was reset).
    #[test]
    fn clear_resets_state() {
        run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "iptables -t nat -S NATMAP | grep -q '1.2.3.4' || (echo 'FAIL: rule not installed' >&2 && exit 1)",
            "&&",
            "lab-ops natmap --socket /tmp/ns clear",
            "&&",
            // Kill daemon
            "kill %1 2>/dev/null; sleep 1",
            "&&",
            // Restart daemon — it loads state from disk; cleared state should be empty
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            // Verify no natmap rules were re-created from stale state
            "iptables -t nat -S | grep -q 'natmap:' && (echo 'FAIL: rules re-created from stale state after clear' >&2 && exit 1) || echo 'PASS'",
        ]);
    }

    // --- Tests for new Port Allocator (IP_FREEBIND) behaviors ---

    /// IP_FREEBIND must allow reserving a port on an IP that is not local to the machine.
    #[test]
    fn natmap_dnat_non_local_ip_freebind() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 198.51.100.99 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "iptables -t nat -S PREROUTING | grep -q '198.51.100.99' && echo 'PASS' || (echo 'FAIL: rule not created for non-local IP' >&2 && exit 1)",
        ]);
        assert!(
            out.contains("PASS"),
            "DNAT rule for non-local IP missing:\n{out}"
        );
    }

    /// IP_FREEBIND must allow reserving the exact same port on two DIFFERENT external IPs.
    #[test]
    fn natmap_dnat_multiple_ips_same_port() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 198.51.100.1 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 198.51.100.2 --int-ip 10.0.0.2 --ports 8080",
            "&&",
            "iptables -t nat -S PREROUTING | grep -q '198.51.100.1' || (echo 'FAIL 1' >&2 && exit 1)",
            "&&",
            "iptables -t nat -S PREROUTING | grep -q '198.51.100.2' || (echo 'FAIL 2' >&2 && exit 1)",
            "&&",
            "echo 'PASS'",
        ]);
        assert!(
            out.contains("PASS"),
            "Failed to reserve same port on different IPs:\n{out}"
        );
    }

    /// Trying to reserve the exact same port on the EXACT same external IP must return a Conflict.
    #[test]
    fn natmap_dnat_conflict_same_ip_same_port() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 198.51.100.1 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "if lab-ops natmap --socket /tmp/ns dnat --ext-ip 198.51.100.1 --int-ip 10.0.0.2 --ports 8080 2>&1 | grep -qi 'conflict'; then echo 'PASS'; else echo 'FAIL: missing conflict error' >&2 && exit 1; fi",
        ]);
        assert!(
            out.contains("PASS"),
            "Conflict error was not returned:\n{out}"
        );
    }

    /// Deleting a rule must correctly deallocate the port, allowing it to be immediately re-reserved.
    #[test]
    fn natmap_dnat_release_port_on_delete() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 198.51.100.1 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --delete --ext-ip 198.51.100.1 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 198.51.100.1 --int-ip 10.0.0.2 --ports 8080",
            "&&",
            "iptables -t nat -S PREROUTING | grep -q '10.0.0.2' && echo 'PASS' || (echo 'FAIL: port not released' >&2 && exit 1)",
        ]);
        assert!(
            out.contains("PASS"),
            "Failed to re-reserve port after deletion:\n{out}"
        );
    }

    /// The daemon must correctly allocate multiple ports passed as a comma-separated list.
    #[test]
    fn natmap_dnat_multiple_ports() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 198.51.100.1 --int-ip 10.0.0.1 --ports 8080,8081",
            "&&",
            "iptables -t nat -S PREROUTING | grep -q '8080' || (echo 'FAIL 8080' >&2 && exit 1)",
            "&&",
            "iptables -t nat -S PREROUTING | grep -q '8081' || (echo 'FAIL 8081' >&2 && exit 1)",
            "&&",
            "echo 'PASS'",
        ]);
        assert!(
            out.contains("PASS"),
            "Multiple ports reservation failed:\n{out}"
        );
    }

    /// UDP protocol must be correctly specified and matched in the resulting iptables rule.
    #[test]
    fn natmap_dnat_udp() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 198.51.100.1 --int-ip 10.0.0.1 --ports 53 --proto udp",
            "&&",
            "iptables -t nat -S PREROUTING | grep -i -q 'udp' && echo 'PASS' || (echo 'FAIL' >&2 && exit 1)",
        ]);
        assert!(out.contains("PASS"), "UDP protocol rule failed:\n{out}");
    }
}
