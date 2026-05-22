//! `natmap` — iptables NAT rule management for static VMs and Docker containers.
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

pub mod api;
pub mod cli;
pub mod command;
pub mod consts;
pub mod daemon;
pub mod docker;
pub mod install;
pub mod iptables;
pub mod models;
pub mod utils;
