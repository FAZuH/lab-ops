//! Homelab operations toolkit.
//!
//! Provides CLI utilities for managing DNS zone conversion, Docker networking,
//! iptables NAT rules, and service discovery. The binary is split into
//! subcommands routed through [`cli::Cli`].

/// Top-level CLI definition.
pub mod cli;
/// Inline subcommand implementations.
pub mod cmd;
/// Command-name constants.
pub mod consts;
