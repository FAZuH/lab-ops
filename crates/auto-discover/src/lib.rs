//! Service discovery daemon for homelab clusters.
//!
//! Watches Docker container events, manages port forwarding via `lab-ops natmap`,
//! registers services with Consul, and generates nginx configs stored in Consul KV.
//!
//! # Subcommands
//!
//! - `daemon` — Long-running daemon on service nodes (watches Docker events)
//! - `sync` — One-shot sync on service nodes
//! - `check` — Validate the discovery configuration file
//! - `forwarding-daemon` — Proxy-side daemon for kernel-level DNAT rules (30s polling)
//! - `forwarding-sync` — One-shot proxy-side DNAT rule sync
//! - `nginx-daemon` — Proxy-side daemon for nginx configs (Consul blocking queries)
//! - `nginx-sync` — One-shot proxy-side nginx config sync
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
mod nginx_daemon;
mod port;
