//! Source connectors for DX cluster nodes.

pub mod cluster;
pub mod iac;
pub mod supervisor;

use crate::aggregator::core::IncomingObservation;
use crate::model::{DxAnnounce, SourceStatus};

/// Messages sent from a source connector to the aggregator.
pub enum SourceMessage {
    /// A parsed spot observation ready for aggregation.
    Observation(IncomingObservation),
    /// Connection state change.
    Status(SourceStatus),
    /// A text announcement from the cluster.
    Announce(DxAnnounce),
}

/// Errors from source connectors.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("connection failed: {0}")]
    ConnectFailed(String),
    #[error("login failed: {0}")]
    LoginFailed(String),
    #[error("read timeout (inactivity)")]
    ReadTimeout,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("channel closed")]
    ChannelClosed,
    #[error("shutdown requested")]
    Shutdown,
}
