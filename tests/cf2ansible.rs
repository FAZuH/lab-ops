use std::process::Command;

struct TestOutput {
    stdout: String,
}

impl TestOutput {
    fn new(file: &str) -> Self {
        let output = Command::new(env!("CARGO_BIN_EXE_lab-ops"))
            .arg(lab_ops::consts::CMD_CF2ANSIBLE)
            .arg(format!("tests/{file}"))
            .output()
            .expect("Failed to run binary");

        assert!(
            output.status.success(),
            "Binary failed for {}: {}",
            file,
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        TestOutput { stdout }
    }

    fn count(&self, needle: &str) -> usize {
        self.stdout.matches(needle).count()
    }

    fn contains(&self, needle: &str) -> bool {
        self.stdout.contains(needle)
    }

    fn assert_common(&self, zone: &str) {
        assert!(
            self.stdout.starts_with("---"),
            "Should start with YAML marker"
        );
        assert!(self.contains(zone), "Should contain zone name");
        assert!(!self.contains("SOA"), "Should NOT contain SOA records");
        assert!(
            self.contains("api_token: \"{{ cloudflare_api_token }}\""),
            "Every task should include api_token"
        );
        assert!(self.contains("state: present"));
        assert!(self.contains("tags: [\"dns\"]"));
        assert!(!self.contains("data:"), "Should not use data block");
    }

    fn assert_type_count(&self, rtype: &str, expected: usize) {
        let needle = format!("\n    type: {rtype}\n");
        assert_eq!(
            self.count(&needle),
            expected,
            "Expected {} {} records, got {}",
            expected,
            rtype,
            self.count(&needle)
        );
    }
}

#[test]
fn domain0() {
    let t = TestOutput::new("domain0.com.txt");
    t.assert_common("domain0.com");

    t.assert_type_count("NS", 2);
    t.assert_type_count("AAAA", 1);
    // A and AAAA must be counted separately since "type: A" would also match "type: AAAA"
    let a_count = t.count("\n    type: A\n");
    assert_eq!(a_count, 3, "Expected 3 A records, got {a_count}");
    t.assert_type_count("CNAME", 13);
    t.assert_type_count("MX", 1);
    t.assert_type_count("SRV", 12);
    t.assert_type_count("TLSA", 1);
    t.assert_type_count("TXT", 6);

    // Deep subdomain
    assert!(t.contains("domain0-sg-proxmox-1.server"));
    assert!(t.contains("record: domain0-sg-proxmox-1.server"));

    // SRV subdomain (minecraft on mc subdomain)
    assert!(t.contains("service: minecraft"));
    assert!(t.contains("record: mc"));
    assert!(t.contains("port: 25565"));
    assert!(t.contains("weight: 5"));

    // CNAME to external domain
    assert!(t.contains("domain0.github.io"));
}

#[test]
fn domain1() {
    let t = TestOutput::new("domain1.id.txt");
    t.assert_common("domain1.id");

    t.assert_type_count("NS", 2);
    t.assert_type_count("A", 2);
    t.assert_type_count("CNAME", 6);
    t.assert_type_count("MX", 1);
    t.assert_type_count("SRV", 11);
    t.assert_type_count("TLSA", 1);
    t.assert_type_count("TXT", 5);

    assert!(t.contains("record: \"@\""));
    assert!(t.contains("service: autodiscover"));
}

#[test]
fn domain2() {
    let t = TestOutput::new("domain2.com.txt");
    t.assert_common("domain2.com");

    t.assert_type_count("NS", 2);
    t.assert_type_count("A", 2);
    t.assert_type_count("CNAME", 5);
    t.assert_type_count("MX", 1);
    t.assert_type_count("SRV", 1);
    t.assert_type_count("TLSA", 1);
    t.assert_type_count("TXT", 4);

    assert!(t.contains("proxied: true"));
    assert!(t.contains("ttl: 3600"));
}

#[test]
fn domain3() {
    let t = TestOutput::new("domain3.com.txt");
    t.assert_common("domain3.com");

    t.assert_type_count("NS", 2);
    t.assert_type_count("A", 2);
    t.assert_type_count("CNAME", 5);
    t.assert_type_count("MX", 1);
    t.assert_type_count("SRV", 11);
    t.assert_type_count("TLSA", 1);
    t.assert_type_count("TXT", 6);

    assert!(t.contains("proxied: true"));
    assert!(t.contains("proxied: false"));
    assert!(t.contains("priority: 5"));
    assert!(t.contains("ttl: 86400"));
    assert!(t.contains("ttl: 3600"));
    assert!(t.contains("service: autodiscover"));
    assert!(t.contains("proto: tcp"));
    assert!(t.contains("cert_usage: 3"));
    assert!(t.contains("selector: 1"));
    assert!(t.contains("hash_type: 1"));
    assert!(t.contains("record: \"@\""));
}

#[test]
fn all_files_no_data_block() {
    for file in &[
        "domain0.com.txt",
        "domain1.id.txt",
        "domain2.com.txt",
        "domain3.com.txt",
    ] {
        let t = TestOutput::new(file);
        assert!(!t.contains("data:"), "{file} should not contain data block");
    }
}

#[test]
fn all_files_api_token_in_every_task() {
    for file in &[
        "domain0.com.txt",
        "domain1.id.txt",
        "domain2.com.txt",
        "domain3.com.txt",
    ] {
        let t = TestOutput::new(file);
        let task_count = t.count("- name:");
        let token_count = t.count("api_token:");
        assert_eq!(
            token_count, task_count,
            "{file} should have api_token in every task ({task_count} tasks, {token_count} tokens)"
        );
    }
}
