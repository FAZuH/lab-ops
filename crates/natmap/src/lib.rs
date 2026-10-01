//! `natmap` — iptables NAT rule management for static VMs and Docker containers.
//! Control daemon for iptables forwarding and DNAT rules via a Unix socket API.
//!
//! This crate provides a daemon that acts as the central authority for all
//! iptables NAT rules. It handles:
//!
//! - **Static DNAT/SNAT/hairpin rules** for VMs with persistent configuration
//! - **Dynamic Docker port mappings** that auto-discover published ports at
//!   container start and allow host-port remapping without restarting containers
//! - **Crash recovery** by persisting state to disk and flushing stale rules on
//!   restart
//! - **Port conflict prevention** via a TCP pre-bind allocator
//!
//! The daemon exposes an HTTP API over a Unix socket. CLI commands in the
//! parent crate communicate with it through [`cli::run_cli`].

/// HTTP handlers for the daemon's Unix-socket API.
pub mod api;
/// Argument parsing and dispatch for the `natmap` subcommand.
pub mod cli;
/// Typed client for the daemon API, used by auto-discover.
pub mod client;
/// CLI command handlers and the Docker mapping parser.
pub mod command;
/// Dynamic shell-completion sources.
pub mod completions;
/// Socket, state-file, and package-name constants.
pub mod consts;
/// The daemon itself: state, lifecycle, and Docker events.
pub mod daemon;
/// Docker discovery of published container port mappings.
pub mod docker;
/// systemd unit installation for the natmap daemon.
pub mod install;
/// Rule construction and installation.
pub mod iptables;
/// Request, response, config, and state types.
pub mod models;
/// `ip rule` / `ip route` management for source-IP preservation.
pub mod policy_route;
/// HTTP-over-Unix-socket request helper.
pub mod utils;
