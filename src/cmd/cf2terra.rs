//! Converts BIND DNS zone files to Terraform Cloudflare DNS resources.
//!
//! Parses standard BIND zone file format and emits Terraform `cloudflare_record`
//! resource blocks.

use std::fs;
use std::io;
use std::path::Path;

use color_eyre::Result;
use color_eyre::eyre::Context;

use crate::cmd::dns_parser;

/// Parses a BIND zone file and prints Terraform Cloudflare DNS resources to stdout.
pub fn run(
    zone_file: impl AsRef<Path>,
    zone_name: Option<impl AsRef<str>>,
    zone_id_var: String,
) -> Result<()> {
    let content = fs::read_to_string(&zone_file)
        .with_context(|| format!("failed to read file {}", zone_file.as_ref().display()))?;

    let zone = zone_name
        .map(|s| s.as_ref().to_string())
        .unwrap_or_else(|| extract_zone(&content));

    let records = dns_parser::parse_zone(&content);
    print_terraform_resources(&records, &zone, &zone_id_var, io::stdout().lock())?;

    Ok(())
}

/// Extracts the zone name from the SOA record.
fn extract_zone(content: &str) -> String {
    for line in content.lines() {
        if dns_parser::SOA.is_match(line) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let name = parts[0].trim_end_matches('.');
            return name.to_string();
        }
    }
    "example.com".to_string()
}

/// Prints the Terraform resource blocks for all parsed DNS records to the given writer.
fn print_terraform_resources<W: io::Write>(
    records: &[dns_parser::DnsRecord],
    zone: &str,
    zone_id: &str,
    mut out: W,
) -> Result<()> {
    for (i, rec) in records.iter().enumerate() {
        let record_name = dns_parser::strip_zone(&rec.name, zone);
        let resource_name = format!(
            "record_{}_{}_{}",
            record_name.replace('.', "_").replace('@', "apex"),
            rec.rtype.to_lowercase(),
            i
        );

        writeln!(out, "resource \"cloudflare_record\" \"{resource_name}\" {{")?;
        writeln!(out, "  zone_id = {zone_id}")?;
        writeln!(out, "  name    = {record_name:?}")?;
        writeln!(out, "  type    = {:?}", rec.rtype)?;
        writeln!(out, "  ttl     = {}", rec.ttl)?;

        if dns_parser::can_proxy(&rec.rtype)
            && let Some(proxied) = rec.proxied
        {
            writeln!(out, "  proxied = {proxied}")?;
        }

        match rec.rtype.as_str() {
            "SRV" => {
                let (_, service, proto) = dns_parser::parse_srv_name(&rec.name, zone);
                let parts: Vec<&str> = rec.data.split_whitespace().collect();
                let priority: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                let weight: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
                let port: u32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                let target = parts.get(3).map(|s| s.trim_end_matches('.')).unwrap_or("");

                writeln!(out, "  data {{")?;
                writeln!(out, "    service  = {service:?}")?;
                writeln!(out, "    proto    = {proto:?}")?;
                writeln!(out, "    name     = {record_name:?}")?;
                writeln!(out, "    priority = {priority}")?;
                writeln!(out, "    weight   = {weight}")?;
                writeln!(out, "    port     = {port}")?;
                writeln!(out, "    target   = {target:?}")?;
                writeln!(out, "  }}")?;
            }
            "MX" => {
                let parts: Vec<&str> = rec.data.splitn(2, ' ').collect();
                let priority = parts.first().map(|s| s.trim()).unwrap_or("0");
                let value = parts
                    .get(1)
                    .map(|s| s.trim_end_matches('.').trim())
                    .unwrap_or("");

                writeln!(out, "  priority = {priority}")?;
                writeln!(out, "  content  = {value:?}")?;
            }
            "TXT" => {
                let txt = dns_parser::parse_txt_data(&rec.data);
                writeln!(out, "  content  = {txt:?}")?;
            }
            _ => {
                let value = rec.data.trim_end_matches('.');
                writeln!(out, "  content  = {value:?}")?;
            }
        }

        writeln!(out, "}}")?;
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
"#;

    #[test]
    fn output_produces_hcl() {
        let records = dns_parser::parse_zone(SAMPLE_ZONE);
        let zone = "example.com";
        let zone_id = "var.cloudflare_zone_id";

        let mut buf = Vec::new();
        let result = print_terraform_resources(&records, zone, zone_id, &mut buf);
        assert!(result.is_ok(), "print_terraform_resources should succeed");
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("resource \"cloudflare_record\" \"record_apex_a_1\""));
    }
}
