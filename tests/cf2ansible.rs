//! End-to-end test for the `cf2ansible` zone-to-Ansible conversion.

use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize)]
struct Task {
    #[serde(rename = "community.general.cloudflare_dns")]
    dns: DnsArgs,
    tags: Vec<String>,
    data: Option<serde_yaml::Value>,
}

#[derive(Deserialize)]
struct DnsArgs {
    zone: String,
    record: String,
    #[serde(rename = "type")]
    rtype: String,
    value: String,
    api_token: String,
    state: String,
    ttl: Option<u32>,
    proxied: Option<bool>,
    service: Option<String>,
    proto: Option<String>,
    port: Option<u32>,
    priority: Option<u32>,
    weight: Option<u32>,
    cert_usage: Option<u32>,
    selector: Option<u32>,
    hash_type: Option<u32>,
}

const API_TOKEN: &str = "{{ cloudflare_api_token }}";

const ZONE_FILES: [&str; 4] = [
    "domain0.com.txt",
    "domain1.id.txt",
    "domain2.com.txt",
    "domain3.com.txt",
];

/// The Ansible tasks the binary emitted for one zone file, parsed into the
/// structure the playbook carries. Every assertion reads a field, so a
/// reformat of the emitter — indentation, quoting, key order — cannot change
/// the outcome.
struct Tasks {
    zone: String,
    tasks: Vec<Task>,
}

impl Tasks {
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
        let tasks: Vec<Task> =
            serde_yaml::from_str(&stdout).unwrap_or_else(|e| panic!("{file} is not YAML: {e}"));
        assert!(!tasks.is_empty(), "{file} produced no tasks");

        Tasks {
            zone: file.trim_end_matches(".txt").to_string(),
            tasks,
        }
    }

    fn count(&self, rtype: &str) -> usize {
        self.tasks.iter().filter(|t| t.dns.rtype == rtype).count()
    }

    /// Every task of `rtype` whose record name is `record`, ordered as emitted.
    fn matching(&self, rtype: &str, record: &str) -> Vec<&DnsArgs> {
        self.tasks
            .iter()
            .filter(|t| t.dns.rtype == rtype && t.dns.record == record)
            .map(|t| &t.dns)
            .collect()
    }

    /// The single task of `rtype` for `record`, or a panic naming what is there.
    fn task(&self, rtype: &str, record: &str) -> &DnsArgs {
        let mut found = self.matching(rtype, record).into_iter();
        let task = found
            .next()
            .unwrap_or_else(|| panic!("no {rtype} task for record {record} in {}", self.zone));
        assert!(
            found.next().is_none(),
            "more than one {rtype} task for record {record} in {}",
            self.zone
        );
        task
    }

    /// The `field` of every matching task, sorted so the assertion does not
    /// depend on the order the zone file listed them in.
    fn field<T: Ord + Copy>(
        &self,
        rtype: &str,
        record: &str,
        field: impl Fn(&DnsArgs) -> T,
    ) -> Vec<T> {
        let mut values: Vec<T> = self
            .matching(rtype, record)
            .iter()
            .map(|t| field(t))
            .collect();
        values.sort();
        values
    }
}

#[test]
fn cf2ansible_skips_soa_records() {
    for file in ZONE_FILES {
        let t = Tasks::new(file);
        assert_eq!(t.count("SOA"), 0, "{file} emitted the SOA record");
    }
}

#[test]
fn cf2ansible_every_task_carries_shared_module_args() {
    for file in ZONE_FILES {
        let t = Tasks::new(file);
        for task in &t.tasks {
            let what = format!("{} {}/{}", t.zone, task.dns.rtype, task.dns.record);
            assert_eq!(task.dns.zone, t.zone, "wrong zone on {what}");
            assert_eq!(task.dns.api_token, API_TOKEN, "wrong api_token on {what}");
            assert_eq!(task.dns.state, "present", "wrong state on {what}");
            assert_eq!(task.tags, vec!["dns".to_string()], "wrong tags on {what}");
            assert!(task.data.is_none(), "{what} used a data block");
        }
    }
}

/// One task per non-SOA record, with the per-type tally the zone file implies.
/// `A` and `AAAA` are counted from the parsed `type` field, so neither can
/// mask the other.
#[test]
fn cf2ansible_record_type_counts() {
    let expected: &[(&str, &[(&str, usize)])] = &[
        (
            "domain0.com.txt",
            &[
                ("NS", 2),
                ("A", 3),
                ("AAAA", 1),
                ("CNAME", 13),
                ("MX", 1),
                ("SRV", 12),
                ("TLSA", 1),
                ("TXT", 6),
            ],
        ),
        (
            "domain1.id.txt",
            &[
                ("NS", 2),
                ("A", 2),
                ("AAAA", 0),
                ("CNAME", 6),
                ("MX", 1),
                ("SRV", 11),
                ("TLSA", 1),
                ("TXT", 5),
            ],
        ),
        (
            "domain2.com.txt",
            &[
                ("NS", 2),
                ("A", 2),
                ("AAAA", 0),
                ("CNAME", 5),
                ("MX", 1),
                ("SRV", 1),
                ("TLSA", 1),
                ("TXT", 4),
            ],
        ),
        (
            "domain3.com.txt",
            &[
                ("NS", 2),
                ("A", 2),
                ("AAAA", 0),
                ("CNAME", 5),
                ("MX", 1),
                ("SRV", 11),
                ("TLSA", 1),
                ("TXT", 6),
            ],
        ),
    ];

    for (file, counts) in expected {
        let t = Tasks::new(file);
        for (rtype, want) in *counts {
            assert_eq!(t.count(rtype), *want, "wrong {rtype} count in {file}");
        }
    }
}

/// The apex keeps the zone name as its record name; only the SRV and TLSA
/// labels collapse to `@`.
#[test]
fn cf2ansible_apex_record_name() {
    let t = Tasks::new("domain1.id.txt");
    assert_eq!(t.task("A", "domain1.id").value, "203.0.113.2");
    assert_eq!(t.task("MX", "domain1.id").value, "mail.domain1.id");
    assert_eq!(t.task("TXT", "domain1.id").value, "TRUNCATED");

    let t = Tasks::new("domain0.com.txt");
    assert_eq!(t.task("A", "domain0.com").value, "203.0.113.3");
    assert_eq!(t.task("AAAA", "domain0.com").value, "2402:1f00:8001:82b::1");
}

#[test]
fn cf2ansible_preserves_deep_subdomain_record() {
    let t = Tasks::new("domain0.com.txt");
    let deep = t.task("A", "domain0-sg-proxmox-1.server");
    assert_eq!(deep.value, "203.0.113.3");
    assert_eq!(deep.zone, "domain0.com");
}

#[test]
fn cf2ansible_srv_apex_service_split_out_of_record_name() {
    for (file, target) in [
        ("domain1.id.txt", "mail.domain1.id"),
        ("domain3.com.txt", "mail.domain2.com"),
    ] {
        let t = Tasks::new(file);
        let autodiscover = t
            .matching("SRV", "@")
            .into_iter()
            .find(|s| s.service.as_deref() == Some("autodiscover"))
            .expect("no autodiscover SRV task");
        assert_eq!(autodiscover.proto.as_deref(), Some("tcp"));
        assert_eq!(autodiscover.port, Some(443));
        assert_eq!(autodiscover.value, target);
    }
}

#[test]
fn cf2ansible_srv_subdomain_keeps_remaining_label() {
    let t = Tasks::new("domain0.com.txt");
    let srv = t.task("SRV", "mc");
    assert_eq!(srv.service.as_deref(), Some("minecraft"));
    assert_eq!(srv.proto.as_deref(), Some("tcp"));
    assert_eq!(srv.port, Some(25565));
    assert_eq!(srv.weight, Some(5));
    assert_eq!(srv.value, "mc.domain0.com");
}

#[test]
fn cf2ansible_cname_target_outside_zone_kept_whole() {
    let t = Tasks::new("domain0.com.txt");
    assert_eq!(t.task("CNAME", "notes").value, "domain0.github.io");
}

#[test]
fn cf2ansible_proxied_flag_follows_annotation() {
    let t = Tasks::new("domain3.com.txt");
    assert_eq!(t.task("A", "domain3.com").proxied, Some(true));
    assert_eq!(t.task("A", "mail").proxied, Some(false));

    let t = Tasks::new("domain2.com.txt");
    assert_eq!(t.task("CNAME", "www").proxied, Some(true));

    let t = Tasks::new("domain0.com.txt");
    assert_eq!(t.task("A", "domain0.com").proxied, Some(false));
}

#[test]
fn cf2ansible_ttl_emitted_only_when_not_one() {
    let t = Tasks::new("domain3.com.txt");
    assert_eq!(t.field("NS", "domain3.com", |d| d.ttl), [Some(86400); 2]);
    assert_eq!(t.field("TXT", "domain3.com", |d| d.ttl), [None, Some(3600)]);
    assert_eq!(t.field("A", "domain3.com", |d| d.ttl), [None]);

    let t = Tasks::new("domain2.com.txt");
    assert_eq!(t.field("TXT", "domain2.com", |d| d.ttl), [None, Some(3600)]);
}

#[test]
fn cf2ansible_mx_priority_from_zone_data() {
    let t = Tasks::new("domain3.com.txt");
    let mx = t.task("MX", "domain3.com");
    assert_eq!(mx.priority, Some(5));
    assert_eq!(mx.value, "mail.domain2.com");
}

#[test]
fn cf2ansible_tlsa_fields_split_out_of_name_and_data() {
    let t = Tasks::new("domain3.com.txt");
    let tlsa = t.task("TLSA", "mail");
    assert_eq!(tlsa.port, Some(25));
    assert_eq!(tlsa.proto.as_deref(), Some("tcp"));
    assert_eq!(tlsa.cert_usage, Some(3));
    assert_eq!(tlsa.selector, Some(1));
    assert_eq!(tlsa.hash_type, Some(1));
    assert_eq!(tlsa.value, "TRUNCATED");
}
