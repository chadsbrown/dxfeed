//! DX cluster source connector.
//!
//! Connects to a DX cluster node via TCP/telnet, handles login,
//! strips IAC sequences, and parses spot lines into observations.

use std::time::Duration;

use chrono::Utc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::aggregator::core::IncomingObservation;
use crate::callsign::is_likely_skimmer;
use crate::domain::OriginatorKind;
use crate::model::{DxAnnounce, SourceConnectionState, SourceId, SourceStatus};
use crate::parser::spot::{parse_line, parse_skimmer_comment, ParsedLine};

use super::iac::strip_iac;
use super::{SourceError, SourceMessage};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default RBN host.
pub const RBN_DEFAULT_HOST: &str = "telnet.reversebeacon.net";
/// Default RBN port.
pub const RBN_DEFAULT_PORT: u16 = 7000;

// ---------------------------------------------------------------------------
// OriginatorPolicy
// ---------------------------------------------------------------------------

/// How the source determines whether a spot is from a skimmer or a human.
#[derive(Debug, Clone, Copy, Default)]
pub enum OriginatorPolicy {
    /// Infer per-spot from spotter callsign pattern and comment fields.
    #[default]
    Auto,
    /// Force all spots from this connection to Skimmer.
    AllSkimmer,
    /// Force all spots from this connection to Human.
    AllHuman,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for a DX cluster source connection.
#[derive(Debug, Clone)]
pub struct ClusterSourceConfig {
    pub host: String,
    pub port: u16,
    pub callsign: String,
    pub password: Option<String>,
    pub source_id: SourceId,
    /// Timeout for login sequence completion.
    pub login_timeout: Duration,
    /// Inactivity timeout: if no data received for this duration, consider
    /// the connection hung and disconnect.
    pub read_timeout: Duration,
    /// How to determine the originator kind for spots from this connection.
    pub originator_policy: OriginatorPolicy,
}

impl ClusterSourceConfig {
    /// Create a config for a standard DX cluster node.
    pub fn new(host: impl Into<String>, port: u16, callsign: impl Into<String>, source_id: SourceId) -> Self {
        Self {
            host: host.into(),
            port,
            callsign: callsign.into(),
            password: None,
            source_id,
            login_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(300),
            originator_policy: OriginatorPolicy::Auto,
        }
    }

    /// Create a config pre-set for the Reverse Beacon Network.
    ///
    /// Uses the default RBN host/port and sets `originator_policy` to
    /// `AllSkimmer` since all RBN spots come from automated skimmers.
    pub fn rbn(callsign: impl Into<String>, source_id: SourceId) -> Self {
        Self {
            host: RBN_DEFAULT_HOST.into(),
            port: RBN_DEFAULT_PORT,
            callsign: callsign.into(),
            password: None,
            source_id,
            login_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(300),
            originator_policy: OriginatorPolicy::AllSkimmer,
        }
    }
}

// ---------------------------------------------------------------------------
// Cluster source task
// ---------------------------------------------------------------------------

/// Run a DX cluster source connection.
///
/// Connects, logs in, reads lines, parses spots, and sends observations
/// on the provided channel. Returns when the connection closes, a fatal
/// error occurs, or shutdown is requested.
pub async fn run_cluster_source(
    config: ClusterSourceConfig,
    tx: mpsc::Sender<SourceMessage>,
    shutdown: CancellationToken,
) -> Result<(), SourceError> {
    // Notify: connecting
    let _ = tx
        .send(SourceMessage::Status(SourceStatus {
            source_id: config.source_id.clone(),
            state: SourceConnectionState::Connecting,
            timestamp: Utc::now(),
        }))
        .await;

    // Connect
    let addr = format!("{}:{}", config.host, config.port);
    let stream = tokio::select! {
        result = TcpStream::connect(&addr) => {
            result.map_err(|e| SourceError::ConnectFailed(e.to_string()))?
        }
        _ = shutdown.cancelled() => {
            return Err(SourceError::Shutdown);
        }
    };

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Notify: connected
    let _ = tx
        .send(SourceMessage::Status(SourceStatus {
            source_id: config.source_id.clone(),
            state: SourceConnectionState::Connected,
            timestamp: Utc::now(),
        }))
        .await;

    // Login phase
    login(&config, &mut reader, &mut writer, &shutdown).await?;

    // Main read loop
    loop {
        let mut raw_buf = Vec::new();

        let read_result = tokio::select! {
            result = reader.read_until(b'\n', &mut raw_buf) => result,
            _ = tokio::time::sleep(config.read_timeout) => {
                return Err(SourceError::ReadTimeout);
            }
            _ = shutdown.cancelled() => {
                return Err(SourceError::Shutdown);
            }
        };

        match read_result {
            Ok(0) => {
                // EOF: server disconnected
                return Ok(());
            }
            Ok(_) => {
                // Strip IAC sequences
                let iac_result = strip_iac(&mut raw_buf);

                // Send IAC WONT responses
                for response in &iac_result.responses {
                    let _ = writer.write_all(response).await;
                }

                // Decode line (Latin-1 safe: treat each byte as its Unicode codepoint)
                let line = decode_latin1(&raw_buf);
                let line = line.trim_end_matches(['\r', '\n']);

                if line.is_empty() {
                    continue;
                }

                // Parse and dispatch
                match parse_line(line) {
                    ParsedLine::Spot(parsed) => {
                        let originator_kind = match config.originator_policy {
                            OriginatorPolicy::AllSkimmer => OriginatorKind::Skimmer,
                            OriginatorPolicy::AllHuman => OriginatorKind::Human,
                            OriginatorPolicy::Auto => {
                                if is_likely_skimmer(&parsed.spotter_call) {
                                    OriginatorKind::Skimmer
                                } else {
                                    OriginatorKind::Human
                                }
                            }
                        };

                        let skimmer_fields = parsed.comment.as_deref().map(parse_skimmer_comment);

                        let msg = SourceMessage::Observation(IncomingObservation {
                            parsed,
                            source_id: config.source_id.clone(),
                            originator_kind,
                            received_at: Utc::now(),
                            skimmer_fields,
                        });

                        if tx.send(msg).await.is_err() {
                            return Err(SourceError::ChannelClosed);
                        }
                    }
                    ParsedLine::Announce(text) => {
                        let _ = tx
                            .send(SourceMessage::Announce(DxAnnounce {
                                source: config.source_id.clone(),
                                text,
                                timestamp: Utc::now(),
                            }))
                            .await;
                    }
                    ParsedLine::Prompt(_) | ParsedLine::Propagation(_) | ParsedLine::Unknown(_) => {
                        // Silently dropped
                    }
                }
            }
            Err(e) => {
                return Err(SourceError::Io(e));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Login state machine
// ---------------------------------------------------------------------------

async fn login(
    config: &ClusterSourceConfig,
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    shutdown: &CancellationToken,
) -> Result<(), SourceError> {
    let deadline = tokio::time::Instant::now() + config.login_timeout;
    let mut sent_callsign = false;
    let mut sent_password = false;

    loop {
        let mut raw_buf = Vec::new();

        let read_result = tokio::select! {
            result = reader.read_until(b'\n', &mut raw_buf) => result,
            _ = tokio::time::sleep_until(deadline) => {
                return Err(SourceError::LoginFailed("login timeout".into()));
            }
            _ = shutdown.cancelled() => {
                return Err(SourceError::Shutdown);
            }
        };

        match read_result {
            Ok(0) => return Err(SourceError::LoginFailed("connection closed during login".into())),
            Ok(_) => {
                let iac_result = strip_iac(&mut raw_buf);
                for response in &iac_result.responses {
                    let _ = writer.write_all(response).await;
                }

                let line = decode_latin1(&raw_buf);
                let line_lower = line.to_lowercase();

                // Check for login/callsign prompt
                if !sent_callsign
                    && (line_lower.contains("login")
                        || line_lower.contains("call")
                        || line_lower.contains("enter your")
                        || line_lower.contains("your call"))
                {
                    writer
                        .write_all(format!("{}\r\n", config.callsign).as_bytes())
                        .await
                        .map_err(|e| SourceError::LoginFailed(e.to_string()))?;
                    writer.flush().await.ok();
                    sent_callsign = true;
                    continue;
                }

                // Check for password prompt
                if sent_callsign
                    && !sent_password
                    && line_lower.contains("password")
                {
                    if let Some(pw) = &config.password {
                        writer
                            .write_all(format!("{pw}\r\n").as_bytes())
                            .await
                            .map_err(|e| SourceError::LoginFailed(e.to_string()))?;
                        writer.flush().await.ok();
                    }
                    sent_password = true;
                    continue;
                }

                // If we've sent the callsign and see a non-prompt line,
                // login is likely complete
                if sent_callsign {
                    let parsed = parse_line(line.trim_end_matches(['\r', '\n']));
                    if !matches!(parsed, ParsedLine::Prompt(_)) {
                        return Ok(());
                    }
                }
            }
            Err(e) => return Err(SourceError::Io(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Latin-1 decoding (Fix #5)
// ---------------------------------------------------------------------------

/// Decode bytes as Latin-1 (ISO 8859-1) to a String.
///
/// Latin-1 bytes map directly to Unicode codepoints 0-255, so every byte
/// is valid. This handles the common case of DX cluster nodes sending
/// accented characters in station comments.
fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn decode_latin1_ascii() {
        assert_eq!(decode_latin1(b"Hello"), "Hello");
    }

    #[test]
    fn decode_latin1_high_bytes() {
        // é = 0xE9 in Latin-1, maps to U+00E9
        assert_eq!(decode_latin1(&[0xE9]), "\u{00E9}");
    }

    #[test]
    fn decode_latin1_mixed() {
        let bytes = b"Caf\xe9";
        assert_eq!(decode_latin1(bytes), "Café");
    }

    // -----------------------------------------------------------------------
    // Mock server integration tests
    // -----------------------------------------------------------------------

    async fn start_mock_server(lines: Vec<&str>) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let lines: Vec<String> = lines.into_iter().map(String::from).collect();

        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            for line in &lines {
                stream.write_all(line.as_bytes()).await.unwrap();
                stream.write_all(b"\r\n").await.unwrap();
            }
            // Brief delay before closing so client can read
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        (addr, handle)
    }

    #[tokio::test]
    async fn connect_login_and_parse_spot() {
        let lines = vec![
            "Please enter your callsign:",
            "Hello W1AW, welcome to the cluster",
            "DX de W3LPL:     14025.0  JA1ABC       CQ                         1830Z",
        ];

        let (addr, server) = start_mock_server(lines).await;

        let config = ClusterSourceConfig::new(
            addr.ip().to_string(),
            addr.port(),
            "W1AW",
            SourceId("test".into()),
        );

        let (tx, mut rx) = mpsc::channel(32);
        let shutdown = CancellationToken::new();

        let client = tokio::spawn(run_cluster_source(config, tx, shutdown.clone()));

        // Collect messages
        let mut observations = Vec::new();
        let mut statuses = Vec::new();

        while let Some(msg) = rx.recv().await {
            match msg {
                SourceMessage::Observation(obs) => {
                    observations.push(obs);
                }
                SourceMessage::Status(s) => {
                    statuses.push(s);
                }
                SourceMessage::Announce(_) => {}
            }
        }

        server.await.unwrap();
        let _ = client.await;

        // Should have connecting + connected statuses
        assert!(statuses.len() >= 2);
        assert_eq!(statuses[0].state, SourceConnectionState::Connecting);
        assert_eq!(statuses[1].state, SourceConnectionState::Connected);

        // Should have parsed the spot
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].parsed.dx_call, "JA1ABC");
        assert_eq!(observations[0].parsed.freq_hz, 14_025_000);
    }

    #[tokio::test]
    async fn shutdown_cancels_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server just accepts and holds the connection
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        });

        let config = ClusterSourceConfig::new(
            addr.ip().to_string(),
            addr.port(),
            "W1AW",
            SourceId("test".into()),
        );

        let (tx, _rx) = mpsc::channel(32);
        let shutdown = CancellationToken::new();

        let shutdown_clone = shutdown.clone();
        let client = tokio::spawn(run_cluster_source(config, tx, shutdown_clone));

        // Give it time to connect, then shutdown
        tokio::time::sleep(Duration::from_millis(100)).await;
        shutdown.cancel();

        let result = client.await.unwrap();
        assert!(matches!(
            result,
            Err(SourceError::Shutdown) | Err(SourceError::LoginFailed(_))
        ));

        server.abort();
    }

    #[tokio::test]
    async fn eof_detected() {
        // Server sends prompt, accepts login, sends a spot, then closes gracefully
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Send login prompt
            stream.write_all(b"login:\r\n").await.unwrap();
            // Read client's callsign response
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf).await;
            // Send welcome + spot
            stream.write_all(b"Welcome to the cluster\r\n").await.unwrap();
            stream.write_all(b"DX de W1AW:      14025.0  JA1ABC       CQ                         1830Z\r\n").await.unwrap();
            // Graceful shutdown
            tokio::time::sleep(Duration::from_millis(50)).await;
            stream.shutdown().await.ok();
        });

        let config = ClusterSourceConfig::new(
            addr.ip().to_string(),
            addr.port(),
            "W1AW",
            SourceId("test".into()),
        );

        let (tx, _rx) = mpsc::channel(32);
        let shutdown = CancellationToken::new();

        let result = run_cluster_source(config, tx, shutdown).await;
        assert!(result.is_ok(), "expected Ok, got: {result:?}");

        server.await.unwrap();
    }

    #[tokio::test]
    async fn all_skimmer_policy_tags_as_skimmer() {
        let lines = vec![
            "login:",
            "Welcome",
            "DX de W3LPL-2:   14025.0  JA1ABC       15 dB  22 WPM  CQ         1830Z",
        ];
        let (addr, server) = start_mock_server(lines).await;

        let mut config = ClusterSourceConfig::new(
            addr.ip().to_string(),
            addr.port(),
            "W1AW",
            SourceId("rbn".into()),
        );
        config.originator_policy = OriginatorPolicy::AllSkimmer;

        let (tx, mut rx) = mpsc::channel(32);
        let shutdown = CancellationToken::new();

        tokio::spawn(run_cluster_source(config, tx, shutdown));

        let mut observations = Vec::new();
        while let Some(msg) = rx.recv().await {
            if let SourceMessage::Observation(obs) = msg {
                observations.push(obs);
            }
        }

        server.await.unwrap();

        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].originator_kind, OriginatorKind::Skimmer);
        assert!(observations[0].skimmer_fields.is_some());
    }

    #[tokio::test]
    async fn auto_policy_detects_skimmer_by_callsign() {
        let lines = vec![
            "login:",
            "Welcome",
            "DX de W3LPL-2:   14025.0  JA1ABC       15 dB  22 WPM  CQ         1830Z",
            "DX de W1AW:      14030.0  DL1ABC       CQ TEST                    1831Z",
        ];
        let (addr, server) = start_mock_server(lines).await;

        let config = ClusterSourceConfig::new(
            addr.ip().to_string(),
            addr.port(),
            "W1AW",
            SourceId("test".into()),
        );

        let (tx, mut rx) = mpsc::channel(32);
        let shutdown = CancellationToken::new();

        tokio::spawn(run_cluster_source(config, tx, shutdown));

        let mut observations = Vec::new();
        while let Some(msg) = rx.recv().await {
            if let SourceMessage::Observation(obs) = msg {
                observations.push(obs);
            }
        }

        server.await.unwrap();

        assert_eq!(observations.len(), 2);
        // W3LPL-2 has a -N suffix → skimmer
        assert_eq!(observations[0].originator_kind, OriginatorKind::Skimmer);
        // W1AW has no -N suffix → human
        assert_eq!(observations[1].originator_kind, OriginatorKind::Human);
    }

    #[tokio::test]
    async fn rbn_constructor_tags_all_as_skimmer() {
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

        let mut config = ClusterSourceConfig::rbn("W1AW", SourceId("rbn-test".into()));
        config.host = addr.ip().to_string();
        config.port = addr.port();

        let (tx, mut rx) = mpsc::channel(32);
        let shutdown = CancellationToken::new();

        tokio::spawn(run_cluster_source(config, tx, shutdown));

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
            assert!(obs.skimmer_fields.is_some());
        }

        // Check skimmer fields parsed correctly
        let fields = observations[0].skimmer_fields.as_ref().unwrap();
        assert_eq!(fields.snr_db, Some(15));
        assert_eq!(fields.wpm, Some(22));
        assert!(fields.is_cq);
    }

    #[test]
    fn rbn_constructor_defaults() {
        let config = ClusterSourceConfig::rbn("W1AW", SourceId("rbn".into()));
        assert_eq!(config.host, RBN_DEFAULT_HOST);
        assert_eq!(config.port, RBN_DEFAULT_PORT);
        assert!(matches!(config.originator_policy, OriginatorPolicy::AllSkimmer));
        assert!(config.password.is_none());
    }
}
