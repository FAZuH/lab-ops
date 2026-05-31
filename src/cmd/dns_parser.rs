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
