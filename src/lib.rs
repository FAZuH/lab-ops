//! Homelab operations toolkit.
//!
//! Provides CLI utilities for managing DNS zone conversion, Docker networking,
//! iptables NAT rules, and service discovery. The binary is split into
//! subcommands routed through [`cli::Cli`].

pub mod cli;
pub mod cmd;
