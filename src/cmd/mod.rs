//! Subcommand implementations for lab-ops.

/// Cloudflare DNS zone to Ansible `cloudflare_dns` tasks.
pub mod cf2ansible;
/// Cloudflare DNS zone to Terraform `cloudflare_record` resources.
pub mod cf2terra;
/// Shared BIND zone-file parsing for both zone converters.
pub mod dns_parser;
/// The `dockernet` network inspection subcommand.
pub mod dockernet;
