//! Shared DNS parsing logic for zone files.

use std::sync::LazyLock;

use regex::Regex;

/// Matches the SOA record line to extract the zone name.
pub static SOA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\S+\s+\d+\s+IN\s+SOA\s+").unwrap());
/// Matches a standard DNS resource record line.
pub static ZONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+)\s+(\d+)\s+IN\s+(\S+)\s+(.*)$").unwrap());
/// Matches an inline `cf-proxied` comment annotation.
pub static PROXIED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*;\s*cf_tags=cf-proxied:(true|false)\s*$").unwrap());
/// Extracts quoted strings from TXT record data.
pub static TXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]*)""#).unwrap());

/// A parsed DNS resource record.
#[derive(Debug, Clone)]
pub struct DnsRecord {
    /// Fully-qualified domain name (trailing dot).
    pub name: String,
    /// Time-to-live in seconds.
    pub ttl: u32,
    /// Record type (A, AAAA, CNAME, MX, TXT, SRV, TLSA, NS).
    pub rtype: String,
    /// Record data (the RDATA portion).
    pub data: String,
    /// Whether Cloudflare proxying is enabled, if annotated.
    pub proxied: Option<bool>,
}

/// Parses BIND zone file content into a vector of [`DnsRecord`].
///
/// Skips empty lines, comments, and SOA records.
pub fn parse_zone(content: &str) -> Vec<DnsRecord> {
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

/// Splits raw record data into its value and an optional Cloudflare proxied flag.
pub fn split_data_and_proxied(raw: &str) -> (String, Option<bool>) {
    let proxied = PROXIED.captures(raw).map(|c| c[1].to_lowercase() == "true");
    let data = PROXIED.replace(raw, "").trim().to_string();

    (data, proxied)
}

/// Strips the zone suffix from a fully-qualified domain name.
///
/// Returns the zone name when the FQDN matches the zone (apex).
pub fn strip_zone(fqdn: &str, zone: &str) -> String {
    let fqdn = fqdn.trim_end_matches('.');
    let zone = zone.trim_end_matches('.');

    if fqdn == zone {
        return zone.to_string();
    }

    let suffix = format!(".{zone}");
    if fqdn.to_lowercase().ends_with(&suffix.to_lowercase()) {
        let end = fqdn.len() - suffix.len();
        return fqdn[..end].to_string();
    }

    fqdn.to_string()
}

/// Parses an SRV record name into (remaining record name, service, protocol).
pub fn parse_srv_name(fqdn: &str, zone: &str) -> (String, String, String) {
    let record_part = strip_zone(fqdn, zone);
    let zone = zone.trim_end_matches('.');

    if record_part == zone {
        return (zone.to_string(), "_unknown".to_string(), "_tcp".to_string());
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

/// Parses a TLSA record name into (remaining record name, port, protocol).
pub fn parse_tlsa_name(fqdn: &str, zone: &str) -> (String, u32, String) {
    let record_part = strip_zone(fqdn, zone);
    let zone = zone.trim_end_matches('.');

    if record_part == zone {
        return (zone.to_string(), 0, "tcp".to_string());
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

/// Concatenates all quoted strings in TXT record data into a single string.
pub fn parse_txt_data(raw: &str) -> String {
    let mut result = String::new();

    for cap in TXT.captures_iter(raw) {
        result.push_str(&cap[1]);
    }

    result
}

/// Returns whether a record type supports Cloudflare proxying.
pub fn can_proxy(rtype: &str) -> bool {
    matches!(rtype, "A" | "AAAA" | "CNAME")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_zone ──

    #[test]
    fn parse_zone_a_record() {
        let zone = "example.com. 300 IN A 192.0.2.1";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "example.com.");
        assert_eq!(records[0].ttl, 300);
        assert_eq!(records[0].rtype, "A");
        assert_eq!(records[0].data, "192.0.2.1");
        assert_eq!(records[0].proxied, None);
    }

    #[test]
    fn parse_zone_aaaa_record() {
        let zone = "example.com. 300 IN AAAA 2001:db8::1";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rtype, "AAAA");
        assert_eq!(records[0].data, "2001:db8::1");
    }

    #[test]
    fn parse_zone_cname_record() {
        let zone = "www.example.com. 300 IN CNAME example.com.";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rtype, "CNAME");
        assert_eq!(records[0].data, "example.com.");
    }

    #[test]
    fn parse_zone_mx_record() {
        let zone = "example.com. 300 IN MX 10 mail.example.com.";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rtype, "MX");
        assert_eq!(records[0].data, "10 mail.example.com.");
    }

    #[test]
    fn parse_zone_txt_record() {
        let zone = r#"example.com. 300 IN TXT "v=spf1 include:_spf.example.com ~all""#;
        let records = parse_zone(zone);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rtype, "TXT");
        assert_eq!(records[0].data, r#""v=spf1 include:_spf.example.com ~all""#);
    }

    #[test]
    fn parse_zone_srv_record() {
        let zone = "_sip._tcp.example.com. 300 IN SRV 10 60 5060 sip.example.com.";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rtype, "SRV");
        assert_eq!(records[0].data, "10 60 5060 sip.example.com.");
    }

    #[test]
    fn parse_zone_tlsa_record() {
        let zone = "_443._tcp.example.com. 300 IN TLSA 3 1 1 0d74adc4dfb5e6b9";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rtype, "TLSA");
    }

    #[test]
    fn parse_zone_ns_record() {
        let zone = "example.com. 86400 IN NS ns1.example.com.";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rtype, "NS");
        assert_eq!(records[0].data, "ns1.example.com.");
    }

    #[test]
    fn parse_zone_skips_soa() {
        let zone = "example.com. 86400 IN SOA ns1.example.com. admin.example.com. 2024010100 3600 900 604800 86400";
        let records = parse_zone(zone);
        assert!(records.is_empty());
    }

    #[test]
    fn parse_zone_skips_comments() {
        let zone = "; this is a comment
example.com. 300 IN A 192.0.2.1";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn parse_zone_skips_empty_lines() {
        let zone = "
example.com. 300 IN A 192.0.2.1

other.com. 300 IN A 203.0.113.5";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn parse_zone_handles_proxied_annotation() {
        let zone = "example.com. 300 IN A 192.0.2.1 ; cf_tags=cf-proxied:true";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].data, "192.0.2.1");
        assert_eq!(records[0].proxied, Some(true));
    }

    #[test]
    fn parse_zone_unproxied_annotation() {
        let zone = "example.com. 300 IN A 192.0.2.1 ; cf_tags=cf-proxied:false";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].proxied, Some(false));
    }

    #[test]
    fn parse_zone_empty_input() {
        assert!(parse_zone("").is_empty());
    }

    #[test]
    fn parse_zone_malformed_line_skipped() {
        let zone = "not-a-dns-record-line";
        assert!(parse_zone(zone).is_empty());
    }

    #[test]
    fn parse_zone_coalesces_multiple_records() {
        let zone = "example.com. 300 IN A 192.0.2.1
example.com. 300 IN AAAA 2001:db8::1
www.example.com. 300 IN CNAME example.com.";
        let records = parse_zone(zone);
        assert_eq!(records.len(), 3);
    }

    #[test]
    fn parse_zone_uppercases_rtype() {
        let zone = "example.com. 300 IN a 192.0.2.1";
        let records = parse_zone(zone);
        assert_eq!(records[0].rtype, "A");
    }

    #[test]
    fn parse_zone_lowercase_in_skipped() {
        let zone = "example.com. 300 in A 192.0.2.1";
        assert!(parse_zone(zone).is_empty());
    }

    // ── split_data_and_proxied ──

    #[test]
    fn split_data_and_proxied_no_tag() {
        let (data, proxied) = split_data_and_proxied("192.0.2.1");
        assert_eq!(data, "192.0.2.1");
        assert_eq!(proxied, None);
    }

    #[test]
    fn split_data_and_proxied_with_true() {
        let (data, proxied) = split_data_and_proxied("192.0.2.1 ; cf_tags=cf-proxied:true");
        assert_eq!(data, "192.0.2.1");
        assert_eq!(proxied, Some(true));
    }

    #[test]
    fn split_data_and_proxied_with_false() {
        let (data, proxied) = split_data_and_proxied("192.0.2.1 ; cf_tags=cf-proxied:false");
        assert_eq!(data, "192.0.2.1");
        assert_eq!(proxied, Some(false));
    }

    #[test]
    fn split_data_and_proxied_empty_string() {
        let (data, proxied) = split_data_and_proxied("");
        assert_eq!(data, "");
        assert_eq!(proxied, None);
    }

    #[test]
    fn split_data_and_proxied_unknown_value_unchanged() {
        let (data, proxied) = split_data_and_proxied("192.0.2.1 ; cf_tags=cf-proxied:maybe");
        assert_eq!(data, "192.0.2.1 ; cf_tags=cf-proxied:maybe");
        assert_eq!(proxied, None);
    }

    // ── strip_zone ──

    #[test]
    fn strip_zone_apex_match() {
        assert_eq!(strip_zone("example.com.", "example.com."), "example.com");
    }

    #[test]
    fn strip_zone_apex_without_trailing_dot() {
        assert_eq!(strip_zone("example.com", "example.com"), "example.com");
    }

    #[test]
    fn strip_zone_subdomain() {
        assert_eq!(strip_zone("www.example.com.", "example.com."), "www");
    }

    #[test]
    fn strip_zone_no_match() {
        assert_eq!(strip_zone("other.com.", "example.com."), "other.com");
    }

    #[test]
    fn strip_zone_deep_subdomain() {
        assert_eq!(strip_zone("a.b.c.example.com.", "example.com."), "a.b.c");
    }

    #[test]
    fn strip_zone_case_insensitive() {
        assert_eq!(strip_zone("WWW.EXAMPLE.COM.", "example.com."), "WWW");
    }

    #[test]
    fn strip_zone_different_zone_returns_fqdn() {
        assert_eq!(
            strip_zone("www.other.com.", "example.com."),
            "www.other.com"
        );
    }

    #[test]
    fn strip_zone_empty_fqdn() {
        assert_eq!(strip_zone("", "example.com."), "");
    }

    // ── parse_srv_name ──

    #[test]
    fn parse_srv_name_standard() {
        let (name, service, proto) = parse_srv_name("_sip._tcp.example.com.", "example.com.");
        assert_eq!(name, "@");
        assert_eq!(service, "sip");
        assert_eq!(proto, "tcp");
    }

    #[test]
    fn parse_srv_name_with_additional_labels() {
        let (name, service, proto) =
            parse_srv_name("_sip._tcp.region.example.com.", "example.com.");
        assert_eq!(name, "region");
        assert_eq!(service, "sip");
        assert_eq!(proto, "tcp");
    }

    #[test]
    fn parse_srv_name_apex() {
        let (name, service, proto) = parse_srv_name("example.com.", "example.com.");
        assert_eq!(name, "example.com");
        assert_eq!(service, "_unknown");
        assert_eq!(proto, "_tcp");
    }

    #[test]
    fn parse_srv_name_no_service_underscore() {
        let (_name, service, proto) = parse_srv_name("sip._tcp.example.com.", "example.com.");
        assert_eq!(service, "_unknown");
        assert_eq!(proto, "tcp");
    }

    // ── parse_tlsa_name ──

    #[test]
    fn parse_tlsa_name_standard() {
        let (name, port, proto) = parse_tlsa_name("_443._tcp.example.com.", "example.com.");
        assert_eq!(name, "@");
        assert_eq!(port, 443);
        assert_eq!(proto, "tcp");
    }

    #[test]
    fn parse_tlsa_name_apex() {
        let (name, port, proto) = parse_tlsa_name("example.com.", "example.com.");
        assert_eq!(name, "example.com");
        assert_eq!(port, 0);
        assert_eq!(proto, "tcp");
    }

    #[test]
    fn parse_tlsa_name_no_underscore_port() {
        let (_name, port, proto) = parse_tlsa_name("443._tcp.example.com.", "example.com.");
        assert_eq!(port, 0);
        assert_eq!(proto, "tcp");
    }

    #[test]
    fn parse_tlsa_name_no_proto_underscore() {
        let (_name, port, proto) = parse_tlsa_name("_443.tcp.example.com.", "example.com.");
        assert_eq!(port, 443);
        assert_eq!(proto, "tcp");
    }

    // ── parse_txt_data ──

    #[test]
    fn parse_txt_data_single_quote() {
        assert_eq!(parse_txt_data(r#""hello""#), "hello");
    }

    #[test]
    fn parse_txt_data_multiple_quotes() {
        assert_eq!(
            parse_txt_data(r#""v=spf1" "include:_spf.example.com" "~all""#),
            "v=spf1include:_spf.example.com~all"
        );
    }

    #[test]
    fn parse_txt_data_no_quotes() {
        assert_eq!(parse_txt_data("noquotes"), "");
    }

    #[test]
    fn parse_txt_data_empty() {
        assert_eq!(parse_txt_data(""), "");
    }

    #[test]
    fn parse_txt_data_unicode() {
        assert_eq!(parse_txt_data(r#""안녕하세요""#), "안녕하세요");
    }

    // ── can_proxy ──

    #[test]
    fn can_proxy_a_record() {
        assert!(can_proxy("A"));
    }

    #[test]
    fn can_proxy_aaaa_record() {
        assert!(can_proxy("AAAA"));
    }

    #[test]
    fn can_proxy_cname_record() {
        assert!(can_proxy("CNAME"));
    }

    #[test]
    fn can_proxy_mx_returns_false() {
        assert!(!can_proxy("MX"));
    }

    #[test]
    fn can_proxy_txt_returns_false() {
        assert!(!can_proxy("TXT"));
    }

    #[test]
    fn can_proxy_ns_returns_false() {
        assert!(!can_proxy("NS"));
    }

    #[test]
    fn can_proxy_srv_returns_false() {
        assert!(!can_proxy("SRV"));
    }

    #[test]
    fn can_proxy_empty_string_returns_false() {
        assert!(!can_proxy(""));
    }
}
