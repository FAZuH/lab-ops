use std::fs;
use std::io::Write;
use std::io::{self};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

type MyResult<T> = Result<T, Box<dyn std::error::Error>>;

static SOA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\S+\s+\d+\s+IN\s+SOA\s+").unwrap());
static ZONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+)\s+(\d+)\s+IN\s+(\S+)\s+(.*)$").unwrap());
static PROXIED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*;\s*cf_tags=cf-proxied:(true|false)\s*$").unwrap());
static TXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]*)""#).unwrap());

#[derive(Debug, Clone)]
struct DnsRecord {
    name: String,
    ttl: u32,
    rtype: String,
    data: String,
    proxied: Option<bool>,
}

pub fn run(zone_file: impl AsRef<Path>, zone_name: Option<impl AsRef<str>>) -> MyResult<()> {
    let content = fs::read_to_string(&zone_file)
        .map_err(|e| format!("failed to read {}: {e}", zone_file.as_ref().display()))?;

    let zone = zone_name
        .map(|s| s.as_ref().to_string())
        .unwrap_or_else(|| extract_zone(&content));

    let records = parse_zone(&content);
    print_ansible_tasks(&records, &zone)?;

    Ok(())
}

fn extract_zone(content: &str) -> String {
    for line in content.lines() {
        if SOA.is_match(line) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let name = parts[0].trim_end_matches('.');
            return name.to_string();
        }
    }
    "example.com".to_string()
}

fn parse_zone(content: &str) -> Vec<DnsRecord> {
    let mut records = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }

        let caps = match ZONE.captures(line) {
            Some(c) => c,
            None => continue,
        };

        let name = caps[1].to_string();
        let ttl: u32 = caps[2].parse().unwrap_or(1);
        let rtype = caps[3].to_uppercase();
        if rtype == "SOA" {
            continue;
        }

        let raw_data = caps[4].to_string();

        let (data, proxied) = split_data_and_proxied(&raw_data);

        records.push(DnsRecord {
            name,
            ttl,
            rtype,
            data,
            proxied,
        });
    }

    records
}

fn split_data_and_proxied(raw: &str) -> (String, Option<bool>) {
    let proxied = PROXIED.captures(raw).map(|c| c[1].to_lowercase() == "true");
    let data = PROXIED.replace(raw, "").trim().to_string();

    (data, proxied)
}

fn strip_zone(fqdn: &str, zone: &str) -> String {
    let fqdn = fqdn.trim_end_matches('.');
    let zone = zone.trim_end_matches('.');

    if fqdn == zone {
        return "@".to_string();
    }

    let suffix = format!(".{}", zone);
    if fqdn.to_lowercase().ends_with(&suffix.to_lowercase()) {
        let end = fqdn.len() - suffix.len();
        return fqdn[..end].to_string();
    }

    fqdn.to_string()
}

fn parse_srv_name(fqdn: &str, zone: &str) -> (String, String, String) {
    let record_part = strip_zone(fqdn, zone);

    if record_part == "@" {
        return ("@".to_string(), "_unknown".to_string(), "_tcp".to_string());
    }

    let parts: Vec<&str> = record_part.split('.').collect();

    let service = if !parts.is_empty() && parts[0].starts_with('_') {
        parts[0][1..].to_string()
    } else {
        "_unknown".to_string()
    };

    let proto = if parts.len() >= 2 && parts[1].starts_with('_') {
        parts[1][1..].to_string()
    } else {
        "_tcp".to_string()
    };

    let remaining = if parts.len() > 2 {
        parts[2..].join(".")
    } else {
        "@".to_string()
    };

    (remaining, service, proto)
}

fn parse_tlsa_name(fqdn: &str, zone: &str) -> (String, u32, String) {
    let record_part = strip_zone(fqdn, zone);

    if record_part == "@" {
        return ("@".to_string(), 0, "tcp".to_string());
    }

    let parts: Vec<&str> = record_part.split('.').collect();

    let port: u32 = if !parts.is_empty() && parts[0].starts_with('_') {
        parts[0][1..].parse().unwrap_or(0)
    } else {
        0
    };

    let proto = if parts.len() >= 2 && parts[1].starts_with('_') {
        parts[1][1..].to_string()
    } else {
        "tcp".to_string()
    };

    let remaining = if parts.len() > 2 {
        parts[2..].join(".")
    } else {
        "@".to_string()
    };

    (remaining, port, proto)
}

fn parse_txt_data(raw: &str) -> String {
    let mut result = String::new();

    for cap in TXT.captures_iter(raw) {
        result.push_str(&cap[1]);
    }

    result
}

fn can_proxy(rtype: &str) -> bool {
    matches!(rtype, "A" | "AAAA" | "CNAME")
}

fn yaml_escape(s: &str) -> String {
    const SPECIAL_CHARS: &[char] = &[
        ':', '#', '"', '\'', '{', '}', '[', ']', ',', '&', '*', '?', '|', '<', '>', '=', '!', '%',
        '@', '`', '\n', '\t', '\r',
    ];

    let needs_quoting = s.is_empty()
        || s.starts_with(' ')
        || s.ends_with(' ')
        || matches!(s.chars().next(), Some('-' | '~' | ':'))
        || s.contains(SPECIAL_CHARS)
        || s.chars().next().is_some_and(|c| c.is_ascii_digit())
        || matches!(s, "true" | "false" | "null" | "yes" | "no" | "on" | "off");

    if needs_quoting {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    } else {
        s.to_string()
    }
}

fn print_ansible_tasks(records: &[DnsRecord], zone: &str) -> MyResult<()> {
    print_ansible_tasks_to(records, zone, io::stdout().lock())
}

fn print_ansible_tasks_to<W: Write>(records: &[DnsRecord], zone: &str, mut out: W) -> MyResult<()> {
    writeln!(out, "---")?;
    writeln!(out, "# DNS records for zone: {}", zone)?;
    writeln!(out, "# Generated by {}", crate::cli::CMD_CF2ANSIBLE)?;
    writeln!(out)?;

    for rec in records {
        let record_name = strip_zone(&rec.name, zone);

        let task_name = match rec.rtype.as_str() {
            "SRV" => {
                let (_, service, proto) = parse_srv_name(&rec.name, zone);
                let full = strip_zone(&rec.name, zone);
                format!(
                    "Create {} {} SRV record",
                    zone,
                    if full == "@" {
                        format!("_{}._{}", service, proto)
                    } else {
                        full
                    }
                )
            }
            "TLSA" => {
                let full = strip_zone(&rec.name, zone);
                format!("Create {} {} TLSA record", zone, full)
            }
            _ => format!(
                "Create {} {} {} record",
                zone,
                if record_name == "@" {
                    "@ (root)"
                } else {
                    record_name.as_str()
                },
                rec.rtype
            ),
        };

        writeln!(out, "- name: {}", yaml_escape(&task_name))?;
        writeln!(out, "  community.general.cloudflare_dns:")?;
        writeln!(out, "    zone: {}", yaml_escape(zone))?;

        match rec.rtype.as_str() {
            "SRV" => {
                let (record, service, proto) = parse_srv_name(&rec.name, zone);
                let parts: Vec<&str> = rec.data.split_whitespace().collect();
                let priority: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                let weight: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
                let port: u32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                let target = parts.get(3).map(|s| s.trim_end_matches('.')).unwrap_or("");

                writeln!(out, "    record: {}", yaml_escape(&record))?;
                writeln!(out, "    type: {}", yaml_escape(&rec.rtype))?;
                writeln!(out, "    service: {}", yaml_escape(&service))?;
                writeln!(out, "    proto: {}", yaml_escape(&proto))?;
                if port != 0 {
                    writeln!(out, "    port: {}", port)?;
                }
                if priority != 0 {
                    writeln!(out, "    priority: {}", priority)?;
                }
                if weight != 1 {
                    writeln!(out, "    weight: {}", weight)?;
                }
                writeln!(out, "    value: {}", yaml_escape(target))?;
            }
            "TLSA" => {
                let (record, port, proto) = parse_tlsa_name(&rec.name, zone);
                let parts: Vec<&str> = rec.data.split_whitespace().collect();
                let cert_usage: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                let selector: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                let hash_type: u32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                let value = parts.get(3..).map(|p| p.join(" ")).unwrap_or_default();

                writeln!(out, "    record: {}", yaml_escape(&record))?;
                writeln!(out, "    type: {}", yaml_escape(&rec.rtype))?;
                writeln!(out, "    port: {}", port)?;
                writeln!(out, "    proto: {}", yaml_escape(&proto))?;
                writeln!(out, "    cert_usage: {}", cert_usage)?;
                writeln!(out, "    selector: {}", selector)?;
                writeln!(out, "    hash_type: {}", hash_type)?;
                writeln!(out, "    value: {}", yaml_escape(&value))?;
            }
            "MX" => {
                let parts: Vec<&str> = rec.data.splitn(2, ' ').collect();
                let priority = parts.first().map(|s| s.trim()).unwrap_or("0");
                let value = parts
                    .get(1)
                    .map(|s| s.trim_end_matches('.').trim())
                    .unwrap_or("");

                writeln!(out, "    record: {}", yaml_escape(&record_name))?;
                writeln!(out, "    type: {}", yaml_escape(&rec.rtype))?;
                writeln!(out, "    value: {}", yaml_escape(value))?;
                writeln!(out, "    priority: {}", priority)?;
            }
            "TXT" => {
                let txt = parse_txt_data(&rec.data);
                writeln!(out, "    record: {}", yaml_escape(&record_name))?;
                writeln!(out, "    type: {}", yaml_escape(&rec.rtype))?;
                writeln!(out, "    value: {}", yaml_escape(&txt))?;
            }
            _ => {
                let value = rec.data.trim_end_matches('.');
                writeln!(out, "    record: {}", yaml_escape(&record_name))?;
                writeln!(out, "    type: {}", yaml_escape(&rec.rtype))?;
                writeln!(out, "    value: {}", yaml_escape(value))?;
            }
        }

        if rec.ttl != 1 {
            writeln!(out, "    ttl: {}", rec.ttl)?;
        }

        if can_proxy(&rec.rtype)
            && let Some(proxied) = rec.proxied
        {
            writeln!(
                out,
                "    proxied: {}",
                if proxied { "true" } else { "false" }
            )?;
        }

        writeln!(out, "    api_token: \"{{{{ cloudflare_api_token }}}}\"")?;
        writeln!(out, "    state: present")?;
        writeln!(out, "  tags: [\"dns\"]")?;
        writeln!(out)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_ZONE: &str = r#";;
;; Domain:     example.com.
;;
;; NS Records
example.com.	86400	IN	NS	ns1.example.com.

;; A Records
example.com.	1	IN	A	203.0.113.1 ; cf_tags=cf-proxied:true
mail.example.com.	1	IN	A	192.0.2.1 ; cf_tags=cf-proxied:false
api.example.com.	3600	IN	A	10.0.0.1

;; AAAA Records
example.com.	1	IN	AAAA	2001:db8::1

;; CNAME Records
www.example.com.	1	IN	CNAME	example.com.

;; MX Records
example.com.	1	IN	MX	5 mail.example.com.

;; TXT Records
example.com.	1	IN	TXT	"v=spf1 include:_spf.example.com ~all"
dkim._domainkey.example.com.	1	IN	TXT	"p=one" "two"

;; SRV Records (apex)
_autodiscover._tcp.example.com.	1	IN	SRV	0 1 443 mail.example.com.

;; SRV Records (subdomain)
_minecraft._tcp.mc.example.com.	1	IN	SRV	0 5 25565 mc.example.com.

;; TLSA Records
_25._tcp.mail.example.com.	1	IN	TLSA	3 1 1 CERTDATA
"#;

    #[test]
    fn test_parse_zone_count() {
        let records = parse_zone(SAMPLE_ZONE);
        assert!(
            records.len() >= 12,
            "Expected at least 12 records, got {}",
            records.len()
        );
    }

    #[test]
    fn test_strip_zone_apex() {
        assert_eq!(strip_zone("example.com.", "example.com"), "@");
    }

    #[test]
    fn test_strip_zone_subdomain() {
        assert_eq!(strip_zone("mail.example.com.", "example.com"), "mail");
    }

    #[test]
    fn test_strip_zone_deep() {
        assert_eq!(
            strip_zone("_autodiscover._tcp.example.com.", "example.com"),
            "_autodiscover._tcp"
        );
    }

    #[test]
    fn test_strip_zone_deep_with_sub() {
        assert_eq!(
            strip_zone("_25._tcp.mail.example.com.", "example.com"),
            "_25._tcp.mail"
        );
    }

    #[test]
    fn test_strip_zone_hyphenated_subdomain() {
        assert_eq!(
            strip_zone("domain0-sg-proxmox-1.server.example.com.", "example.com"),
            "domain0-sg-proxmox-1.server"
        );
    }

    #[test]
    fn test_parse_txt_single() {
        assert_eq!(parse_txt_data(r#""hello world""#), "hello world");
    }

    #[test]
    fn test_parse_txt_multi() {
        assert_eq!(parse_txt_data(r#""hello " "world""#), "hello world");
    }

    #[test]
    fn test_split_data_proxied_true() {
        let (data, proxied) = split_data_and_proxied("203.0.113.1 ; cf_tags=cf-proxied:true");
        assert_eq!(data, "203.0.113.1");
        assert_eq!(proxied, Some(true));
    }

    #[test]
    fn test_split_data_proxied_false() {
        let (data, proxied) = split_data_and_proxied("192.0.2.1 ; cf_tags=cf-proxied:false");
        assert_eq!(data, "192.0.2.1");
        assert_eq!(proxied, Some(false));
    }

    #[test]
    fn test_split_data_no_proxied() {
        let (data, proxied) = split_data_and_proxied("192.0.2.1");
        assert_eq!(data, "192.0.2.1");
        assert_eq!(proxied, None);
    }

    #[test]
    fn test_extract_zone() {
        let content = "example.com.\t3600\tIN\tSOA\tns1.example.com. admin.example.com. 2026050601 10000 2400 604800 3600";
        assert_eq!(extract_zone(content), "example.com");
    }

    #[test]
    fn test_extract_zone_no_dot() {
        let content = "example.com\t3600\tIN\tSOA\tns1.example.com. admin.example.com. 2026050601 10000 2400 604800 3600";
        assert_eq!(extract_zone(content), "example.com");
    }

    #[test]
    fn test_parse_srv_apex() {
        let (record, service, proto) =
            parse_srv_name("_autodiscover._tcp.example.com.", "example.com");
        assert_eq!(record, "@");
        assert_eq!(service, "autodiscover");
        assert_eq!(proto, "tcp");
    }

    #[test]
    fn test_parse_srv_subdomain() {
        let (record, service, proto) =
            parse_srv_name("_minecraft._tcp.mc.example.com.", "example.com");
        assert_eq!(record, "mc");
        assert_eq!(service, "minecraft");
        assert_eq!(proto, "tcp");
    }

    #[test]
    fn test_parse_tlsa_name() {
        let (record, port, proto) = parse_tlsa_name("_25._tcp.mail.example.com.", "example.com");
        assert_eq!(record, "mail");
        assert_eq!(port, 25);
        assert_eq!(proto, "tcp");
    }

    #[test]
    fn test_parse_tlsa_name_apex() {
        let (record, port, proto) = parse_tlsa_name("_443._tcp.example.com.", "example.com");
        assert_eq!(record, "@");
        assert_eq!(port, 443);
        assert_eq!(proto, "tcp");
    }

    #[test]
    fn test_output_produces_yaml() {
        let records = parse_zone(SAMPLE_ZONE);
        let types: Vec<&str> = records.iter().map(|r| r.rtype.as_str()).collect();
        assert!(types.contains(&"A"), "Missing A");
        assert!(types.contains(&"AAAA"), "Missing AAAA");
        assert!(types.contains(&"NS"), "Missing NS");
        assert!(types.contains(&"CNAME"), "Missing CNAME");
        assert!(types.contains(&"MX"), "Missing MX");
        assert!(types.contains(&"TXT"), "Missing TXT");
        assert!(types.contains(&"SRV"), "Missing SRV");
        assert!(types.contains(&"TLSA"), "Missing TLSA");
    }

    #[test]
    fn test_a_record_proxied() {
        let records = parse_zone(SAMPLE_ZONE);
        let apex_a = records
            .iter()
            .find(|r| r.name == "example.com." && r.rtype == "A")
            .unwrap();
        assert_eq!(apex_a.proxied, Some(true));

        let mail_a = records
            .iter()
            .find(|r| r.name == "mail.example.com." && r.rtype == "A")
            .unwrap();
        assert_eq!(mail_a.proxied, Some(false));
    }

    #[test]
    fn test_ns_records_not_proxied() {
        let records = parse_zone(SAMPLE_ZONE);
        for rec in records.iter().filter(|r| r.rtype == "NS") {
            assert_eq!(rec.proxied, None, "NS records should not have proxied flag");
        }
    }

    #[test]
    fn test_output_with_api_token() {
        let records = parse_zone(SAMPLE_ZONE);
        let zone = "example.com";
        let mut buf = Vec::new();
        let result = print_ansible_tasks_to(&records, zone, &mut buf);
        assert!(result.is_ok(), "print_ansible_tasks should succeed");
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("api_token: \"{{ cloudflare_api_token }}\""));
    }
}
