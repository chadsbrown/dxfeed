//! Source supervision: automatic reconnect with exponential backoff and jitter.

use std::time::Duration;

use chrono::Utc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::model::{SourceConnectionState, SourceId, SourceStatus};

use super::rbn::{run_rbn_source, RbnSourceConfig};
use super::telnet::{run_telnet_source, TelnetSourceConfig};
use super::{SourceError, SourceMessage};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Which type of source to run.
#[derive(Debug, Clone)]
pub enum SourceConfig {
    Telnet(TelnetSourceConfig),
    Rbn(RbnSourceConfig),
}

impl SourceConfig {
    fn source_id(&self) -> &SourceId {
        match self {
            Self::Telnet(c) => &c.source_id,
            Self::Rbn(c) => &c.source_id,
        }
    }
}

/// Exponential backoff configuration.
#[derive(Debug, Clone)]
pub struct BackoffConfig {
    /// Initial delay before first reconnect attempt.
    pub initial: Duration,
    /// Maximum delay between reconnect attempts.
    pub max: Duration,
    /// Multiplier applied to the delay after each failure.
    pub multiplier: f64,
    /// Random jitter factor (0.0 = none, 1.0 = up to 100% additional).
    pub jitter_factor: f64,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(2),
            max: Duration::from_secs(120),
            multiplier: 2.0,
            jitter_factor: 0.25,
        }
    }
}

// ---------------------------------------------------------------------------
// BackoffState
// ---------------------------------------------------------------------------

struct BackoffState {
    config: BackoffConfig,
    current: Duration,
    attempt: u32,
}

impl BackoffState {
    fn new(config: BackoffConfig) -> Self {
        let current = config.initial;
        Self {
            config,
            current,
            attempt: 0,
        }
    }

    fn next_delay(&mut self) -> Duration {
        self.attempt += 1;
        let delay = self.current;

        // Apply jitter: random factor in [1.0, 1.0 + jitter_factor]
        let jitter = if self.config.jitter_factor > 0.0 {
            // Simple deterministic jitter based on attempt number
            // (avoids requiring a random number generator dependency)
            let pseudo_random = ((self.attempt as f64 * 7.3).sin().abs()) * self.config.jitter_factor;
            1.0 + pseudo_random
        } else {
            1.0
        };

        let jittered = Duration::from_secs_f64(delay.as_secs_f64() * jitter);

        // Advance for next call
        let next = Duration::from_secs_f64(self.current.as_secs_f64() * self.config.multiplier);
        self.current = next.min(self.config.max);

        jittered.min(self.config.max)
    }

    fn reset(&mut self) {
        self.current = self.config.initial;
        self.attempt = 0;
    }
}

// ---------------------------------------------------------------------------
// Supervised source
// ---------------------------------------------------------------------------

/// Run a source with automatic reconnection on failure.
///
/// The supervisor wraps the underlying source connector and handles:
/// - Automatic reconnection with exponential backoff + jitter
/// - SourceStatus emission on each state transition
/// - Clean shutdown via CancellationToken
///
/// Runs until shutdown is requested. Reconnection attempts continue
/// indefinitely (with bounded backoff) until shutdown.
pub async fn run_supervised_source(
    config: SourceConfig,
    backoff_config: BackoffConfig,
    tx: mpsc::Sender<SourceMessage>,
    shutdown: CancellationToken,
) {
    let mut backoff = BackoffState::new(backoff_config);
    let source_id = config.source_id().clone();

    loop {
        // Run the source
        let result = run_source(&config, tx.clone(), shutdown.clone()).await;

        if shutdown.is_cancelled() {
            let _ = tx
                .send(SourceMessage::Status(SourceStatus {
                    source_id: source_id.clone(),
                    state: SourceConnectionState::Shutdown,
                    timestamp: Utc::now(),
                }))
                .await;
            return;
        }

        // Source exited (error or EOF): schedule reconnect
        match result {
            Err(SourceError::Shutdown) => {
                let _ = tx
                    .send(SourceMessage::Status(SourceStatus {
                        source_id: source_id.clone(),
                        state: SourceConnectionState::Shutdown,
                        timestamp: Utc::now(),
                    }))
                    .await;
                return;
            }
            Err(ref e) => {
                let delay = backoff.next_delay();

                let _ = tx
                    .send(SourceMessage::Status(SourceStatus {
                        source_id: source_id.clone(),
                        state: SourceConnectionState::Reconnecting {
                            attempt: backoff.attempt,
                        },
                        timestamp: Utc::now(),
                    }))
                    .await;

                // Log-style: include error reason (consumer can see it via status)
                let _ = tx
                    .send(SourceMessage::Status(SourceStatus {
                        source_id: source_id.clone(),
                        state: SourceConnectionState::Failed {
                            reason: e.to_string(),
                        },
                        timestamp: Utc::now(),
                    }))
                    .await;

                // Wait for backoff delay or shutdown
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown.cancelled() => {
                        let _ = tx
                            .send(SourceMessage::Status(SourceStatus {
                                source_id: source_id.clone(),
                                state: SourceConnectionState::Shutdown,
                                timestamp: Utc::now(),
                            }))
                            .await;
                        return;
                    }
                }
            }
            Ok(()) => {
                // Clean EOF: reconnect immediately (reset backoff since connection was healthy)
                backoff.reset();

                let _ = tx
                    .send(SourceMessage::Status(SourceStatus {
                        source_id: source_id.clone(),
                        state: SourceConnectionState::Reconnecting { attempt: 1 },
                        timestamp: Utc::now(),
                    }))
                    .await;

                // Brief pause before reconnecting on clean EOF
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    _ = shutdown.cancelled() => {
                        let _ = tx
                            .send(SourceMessage::Status(SourceStatus {
                                source_id: source_id.clone(),
                                state: SourceConnectionState::Shutdown,
                                timestamp: Utc::now(),
                            }))
                            .await;
                        return;
                    }
                }
            }
        }
    }
}

async fn run_source(
    config: &SourceConfig,
    tx: mpsc::Sender<SourceMessage>,
    shutdown: CancellationToken,
) -> Result<(), SourceError> {
    match config {
        SourceConfig::Telnet(c) => run_telnet_source(c.clone(), tx, shutdown).await,
        SourceConfig::Rbn(c) => run_rbn_source(c.clone(), tx, shutdown).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn test_backoff() -> BackoffConfig {
        BackoffConfig {
            initial: Duration::from_millis(50),
            max: Duration::from_millis(500),
            multiplier: 2.0,
            jitter_factor: 0.0, // no jitter for deterministic tests
        }
    }

    #[test]
    fn backoff_doubles_up_to_max() {
        let mut state = BackoffState::new(test_backoff());

        let d1 = state.next_delay();
        assert_eq!(d1, Duration::from_millis(50));

        let d2 = state.next_delay();
        assert_eq!(d2, Duration::from_millis(100));

        let d3 = state.next_delay();
        assert_eq!(d3, Duration::from_millis(200));

        let d4 = state.next_delay();
        assert_eq!(d4, Duration::from_millis(400));

        // Should cap at max (500ms)
        let d5 = state.next_delay();
        assert_eq!(d5, Duration::from_millis(500));

        let d6 = state.next_delay();
        assert_eq!(d6, Duration::from_millis(500));
    }

    #[test]
    fn backoff_reset() {
        let mut state = BackoffState::new(test_backoff());
        state.next_delay();
        state.next_delay();
        state.next_delay();

        state.reset();
        let d = state.next_delay();
        assert_eq!(d, Duration::from_millis(50));
    }

    #[test]
    fn backoff_with_jitter() {
        let mut config = test_backoff();
        config.jitter_factor = 0.5;
        let mut state = BackoffState::new(config);

        let d1 = state.next_delay();
        // Should be between 50ms and 75ms (50 * [1.0, 1.5])
        assert!(d1 >= Duration::from_millis(50));
        assert!(d1 <= Duration::from_millis(75));
    }

    #[tokio::test]
    async fn reconnect_after_server_drops() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server accepts twice: first drops immediately, second sends a spot
        let server = tokio::spawn(async move {
            // First connection: login then drop
            let (mut s1, _) = listener.accept().await.unwrap();
            s1.write_all(b"login:\r\n").await.unwrap();
            let mut buf = [0u8; 64];
            let _ = s1.read(&mut buf).await;
            s1.write_all(b"Welcome\r\n").await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
            drop(s1);

            // Second connection: login + spot
            let (mut s2, _) = listener.accept().await.unwrap();
            s2.write_all(b"login:\r\n").await.unwrap();
            let _ = s2.read(&mut buf).await;
            s2.write_all(b"Welcome\r\n").await.unwrap();
            s2.write_all(b"DX de W1AW:      14025.0  JA1ABC       CQ                         1830Z\r\n").await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            s2.shutdown().await.ok();
        });

        let config = SourceConfig::Telnet(TelnetSourceConfig::new(
            addr.ip().to_string(),
            addr.port(),
            "W1AW",
            SourceId("test".into()),
        ));

        let (tx, mut rx) = mpsc::channel(64);
        let shutdown = CancellationToken::new();

        let shutdown_clone = shutdown.clone();
        let supervisor = tokio::spawn(async move {
            run_supervised_source(config, test_backoff(), tx, shutdown_clone).await;
        });

        // Collect messages until we get a spot
        let mut got_spot = false;
        let mut reconnect_seen = false;

        let timeout = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some(SourceMessage::Observation(_)) => {
                            got_spot = true;
                            break;
                        }
                        Some(SourceMessage::Status(s)) => {
                            if matches!(s.state, SourceConnectionState::Reconnecting { .. }) {
                                reconnect_seen = true;
                            }
                        }
                        Some(SourceMessage::Announce(_)) => {}
                        None => break,
                    }
                }
                _ = &mut timeout => break,
            }
        }

        shutdown.cancel();
        let _ = supervisor.await;
        server.await.unwrap();

        assert!(reconnect_seen, "should have seen a reconnect status");
        assert!(got_spot, "should have received a spot on second connection");
    }

    #[tokio::test]
    async fn shutdown_stops_supervisor() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server holds connection indefinitely
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        });

        let config = SourceConfig::Telnet(TelnetSourceConfig::new(
            addr.ip().to_string(),
            addr.port(),
            "W1AW",
            SourceId("test".into()),
        ));

        let (tx, mut rx) = mpsc::channel(64);
        let shutdown = CancellationToken::new();

        let shutdown_clone = shutdown.clone();
        let supervisor = tokio::spawn(async move {
            run_supervised_source(config, test_backoff(), tx, shutdown_clone).await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        shutdown.cancel();

        // Supervisor should exit
        let _ = tokio::time::timeout(Duration::from_secs(2), supervisor).await;

        // Should see a Shutdown status
        let mut saw_shutdown = false;
        while let Ok(msg) = rx.try_recv() {
            if let SourceMessage::Status(s) = msg {
                if s.state == SourceConnectionState::Shutdown {
                    saw_shutdown = true;
                }
            }
        }
        assert!(saw_shutdown);

        server.abort();
    }
}
