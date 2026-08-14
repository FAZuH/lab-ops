//! Transport protocol enum (TCP / UDP).
//!
//! Canonical representation shared across the workspace. Previously defined
//! in natmap's `models.rs`; extracted here so auto-discover can use it without
//! depending on natmap.
//!
//! ```
//! use lab_ops_lab_lib::TransportProtocol;
//!
//! assert_eq!(TransportProtocol::Tcp.to_string(), "tcp");
//! assert_eq!(TransportProtocol::Udp.to_string(), "udp");
//! assert_eq!("tcp".parse::<TransportProtocol>().unwrap(), TransportProtocol::Tcp);
//! assert!("xyz".parse::<TransportProtocol>().is_err());
//! ```

use std::fmt::Display;

use serde::Deserialize;
use serde::Serialize;

/// Transport protocol (TCP or UDP).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum TransportProtocol {
    #[default]
    Tcp,
    Udp,
}

impl std::str::FromStr for TransportProtocol {
    type Err = color_eyre::eyre::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tcp" => Ok(Self::Tcp),
            "udp" => Ok(Self::Udp),
            _ => Err(color_eyre::eyre::eyre!("Invalid transport protocol {s}")),
        }
    }
}

impl TransportProtocol {
    /// Returns the lowercase protocol name.
    pub fn to_lowercase(&self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

impl Display for TransportProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_lowercase())
    }
}
