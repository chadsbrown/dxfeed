//! Reverse Beacon Network (RBN) source connector.
//!
//! RBN uses the same telnet protocol as DX clusters but all spots are
//! from automated skimmers. This module provides a convenience constructor
//! and the `run_rbn_source` entry point that delegates to the telnet source
//! with `skimmer_source = true`.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::model::SourceId;

use super::telnet::{run_telnet_source, TelnetSourceConfig};
use super::{SourceError, SourceMessage};

/// Default RBN host.
pub const RBN_DEFAULT_HOST: &str = "telnet.reversebeacon.net";
/// Default RBN port.
pub const RBN_DEFAULT_PORT: u16 = 7000;

/// Configuration for an RBN source connection.
#[derive(Debug, Clone)]
pub struct RbnSourceConfig {
    pub host: String,
    pub port: u16,
    pub callsign: String,
    pub source_id: SourceId,
    pub login_timeout: Duration,
    pub read_timeout: Duration,
}

impl RbnSourceConfig {
    /// Create a config with default RBN host/port.
    pub fn new(callsign: impl Into<String>, source_id: SourceId) -> Self {
        Self {
            host: RBN_DEFAULT_HOST.into(),
            port: RBN_DEFAULT_PORT,
            callsign: callsign.into(),
            source_id,
            login_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(300),
        }
    }

    /// Convert to a TelnetSourceConfig with skimmer_source = true.
    fn to_telnet_config(&self) -> TelnetSourceConfig {
        TelnetSourceConfig {
            host: self.host.clone(),
            port: self.port,
            callsign: self.callsign.clone(),
            password: None,
            source_id: self.source_id.clone(),
            login_timeout: self.login_timeout,
            read_timeout: self.read_timeout,
            skimmer_source: true,
        }
    }
}

/// Run an RBN source connection.
///
/// All spots are tagged with `OriginatorKind::Skimmer` and RBN comment
/// fields (SNR, WPM, CQ) are automatically parsed.
pub async fn run_rbn_source(
    config: RbnSourceConfig,
    tx: mpsc::Sender<SourceMessage>,
    shutdown: CancellationToken,
) -> Result<(), SourceError> {
    run_telnet_source(config.to_telnet_config(), tx, shutdown).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::OriginatorKind;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn rbn_source_tags_all_as_skimmer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"Please enter your call:\r\n").await.unwrap();
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf).await;
            stream.write_all(b"Hello from RBN\r\n").await.unwrap();
            stream.write_all(b"DX de W3LPL-2:   14025.0  JA1ABC       15 dB  22 WPM  CQ         1830Z\r\n").await.unwrap();
            stream.write_all(b"DX de DK8JP-1:   14025.1  JA1ABC       12 dB  22 WPM  CQ         1830Z\r\n").await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            stream.shutdown().await.ok();
        });

        let mut config = RbnSourceConfig::new("W1AW", SourceId("rbn-test".into()));
        config.host = addr.ip().to_string();
        config.port = addr.port();

        let (tx, mut rx) = mpsc::channel(32);
        let shutdown = CancellationToken::new();

        tokio::spawn(run_rbn_source(config, tx, shutdown));

        let mut observations = Vec::new();
        while let Some(msg) = rx.recv().await {
            if let SourceMessage::Observation(obs) = msg {
                observations.push(obs);
            }
        }

        server.await.unwrap();

        assert_eq!(observations.len(), 2);
        for obs in &observations {
            assert_eq!(obs.originator_kind, OriginatorKind::Skimmer);
            assert!(obs.rbn_fields.is_some());
        }

        // Check RBN fields parsed correctly
        let rbn = observations[0].rbn_fields.as_ref().unwrap();
        assert_eq!(rbn.snr_db, Some(15));
        assert_eq!(rbn.wpm, Some(22));
        assert!(rbn.is_cq);
    }

    #[test]
    fn rbn_config_defaults() {
        let config = RbnSourceConfig::new("W1AW", SourceId("rbn".into()));
        assert_eq!(config.host, RBN_DEFAULT_HOST);
        assert_eq!(config.port, RBN_DEFAULT_PORT);
    }

    #[test]
    fn rbn_to_telnet_config_sets_skimmer() {
        let config = RbnSourceConfig::new("W1AW", SourceId("rbn".into()));
        let telnet = config.to_telnet_config();
        assert!(telnet.skimmer_source);
        assert!(telnet.password.is_none());
    }
}
