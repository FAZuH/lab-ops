//! Service discovery daemon for homelab clusters.
//!
//! Watches Docker container events, manages port forwarding via `lab-ops natmap`,
//! registers services with Consul.
//!
//! # Subcommands
//!
//! - `daemon` — Long-running daemon on service nodes (watches Docker events)
//! - `sync` — One-shot sync on service nodes
//! - `check` — Validate the discovery configuration file
//! - `forwarding-sync` — One-shot proxy-side DNAT rule sync
//!
//! The CLI is exposed through [`cli::run_cli`] and integrated as the
//! `lab-ops auto-discover` subcommand.

pub mod cli;
mod config;
mod consul;
mod daemon;
mod docker;
mod forwarding;
mod model;
mod natmap;
