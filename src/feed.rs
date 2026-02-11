//! DxFeed builder and top-level API.
//!
//! The primary entry point for consuming applications. Use [`DxFeedBuilder`]
//! to configure sources, filters, and options, then call [`build()`](DxFeedBuilder::build)
//! to start the feed. The returned [`DxFeed`] handle provides access to
//! filtered DX events and controls shutdown.

use std::time::Duration;

use chrono::Utc;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::aggregator::core::{Aggregator, AggregatorConfig};
use crate::filter::compiled::FilterConfig;
use crate::filter::config::FilterConfigSerde;
use crate::filter::error::FilterConfigError;
use crate::model::DxEvent;
use crate::resolver::entity::EntityResolver;
use crate::skimmer::config::SkimmerQualityConfig;
use crate::source::SourceMessage;
use crate::source::supervisor::{run_supervised_source, BackoffConfig, SourceConfig};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from building or configuring a DxFeed.
#[derive(Debug, thiserror::Error)]
pub enum DxFeedError {
    #[error("no sources configured")]
    NoSources,
    #[error("filter config error: {0}")]
    FilterConfig(#[from] FilterConfigError),
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for constructing a [`DxFeed`] instance.
///
/// At minimum, one source must be added via [`add_source()`](DxFeedBuilder::add_source).
/// All other options have sensible defaults.
pub struct DxFeedBuilder {
    sources: Vec<SourceConfig>,
    filter: FilterConfigSerde,
    aggregator_config: AggregatorConfig,
    skimmer_quality: Option<SkimmerQualityConfig>,
    entity_resolver: Option<Box<dyn EntityResolver>>,
    backoff: BackoffConfig,
    source_channel_capacity: usize,
    event_channel_capacity: usize,
    tick_interval: Duration,
}

impl Default for DxFeedBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DxFeedBuilder {
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            filter: FilterConfigSerde::default(),
            aggregator_config: AggregatorConfig::default(),
            skimmer_quality: None,
            entity_resolver: None,
            backoff: BackoffConfig::default(),
            source_channel_capacity: 256,
            event_channel_capacity: 256,
            tick_interval: Duration::from_secs(10),
        }
    }

    /// Add a source (telnet cluster or RBN) to the feed.
    pub fn add_source(mut self, source: SourceConfig) -> Self {
        self.sources.push(source);
        self
    }

    /// Set the filter configuration. Compiled and validated at build time.
    pub fn set_filter(mut self, filter: FilterConfigSerde) -> Self {
        self.filter = filter;
        self
    }

    /// Set the aggregator configuration (freq bucketing, callsign normalization, etc.).
    pub fn set_aggregator_config(mut self, config: AggregatorConfig) -> Self {
        self.aggregator_config = config;
        self
    }

    /// Enable skimmer quality gating with the given configuration.
    pub fn set_skimmer_quality(mut self, config: SkimmerQualityConfig) -> Self {
        self.skimmer_quality = Some(config);
        self
    }

    /// Set an entity (DXCC) resolver for geographic data.
    pub fn entity_resolver(mut self, resolver: Box<dyn EntityResolver>) -> Self {
        self.entity_resolver = Some(resolver);
        self
    }

    /// Set the reconnection backoff configuration for all sources.
    pub fn set_backoff(mut self, config: BackoffConfig) -> Self {
        self.backoff = config;
        self
    }

    /// Set the capacity of the source-to-aggregator channel.
    pub fn source_channel_capacity(mut self, cap: usize) -> Self {
        self.source_channel_capacity = cap;
        self
    }

    /// Set the capacity of the aggregator-to-consumer channel.
    pub fn event_channel_capacity(mut self, cap: usize) -> Self {
        self.event_channel_capacity = cap;
        self
    }

    /// Set the interval for TTL eviction ticks.
    pub fn tick_interval(mut self, interval: Duration) -> Self {
        self.tick_interval = interval;
        self
    }

    /// Build and start the feed.
    ///
    /// Validates and compiles the filter config, spawns source supervisor tasks
    /// and the aggregator task, and returns a [`DxFeed`] handle.
    pub fn build(self) -> Result<DxFeed, DxFeedError> {
        if self.sources.is_empty() {
            return Err(DxFeedError::NoSources);
        }

        let compiled_filter = self.filter.validate_and_compile()?;
        let shutdown = CancellationToken::new();

        // Source → aggregator channel
        let (source_tx, source_rx) = mpsc::channel(self.source_channel_capacity);

        // Aggregator → consumer channel
        let (event_tx, event_rx) = mpsc::channel(self.event_channel_capacity);

        // Filter hot-reload channel
        let (filter_tx, filter_rx) = watch::channel(compiled_filter);

        // Spawn source supervisor tasks
        let mut source_handles = Vec::new();
        for source in self.sources {
            let tx = source_tx.clone();
            let token = shutdown.clone();
            let backoff = self.backoff.clone();
            let handle = tokio::spawn(async move {
                run_supervised_source(source, backoff, tx, token).await;
            });
            source_handles.push(handle);
        }

        // Drop builder's copy so channel closes when all sources stop
        drop(source_tx);

        // Spawn aggregator task
        let agg_shutdown = shutdown.clone();
        let aggregator_handle = tokio::spawn(run_aggregator_task(
            source_rx,
            event_tx,
            filter_rx,
            self.aggregator_config,
            self.skimmer_quality,
            self.entity_resolver,
            agg_shutdown,
            self.tick_interval,
        ));

        Ok(DxFeed {
            event_rx,
            shutdown,
            filter_tx,
            _source_handles: source_handles,
            _aggregator_handle: aggregator_handle,
        })
    }
}

// ---------------------------------------------------------------------------
// DxFeed handle
// ---------------------------------------------------------------------------

/// Handle to a running DX feed.
///
/// Provides access to filtered DX events via [`next_event()`](DxFeed::next_event)
/// and controls shutdown via [`shutdown()`](DxFeed::shutdown).
pub struct DxFeed {
    event_rx: mpsc::Receiver<DxEvent>,
    shutdown: CancellationToken,
    filter_tx: watch::Sender<FilterConfig>,
    _source_handles: Vec<JoinHandle<()>>,
    _aggregator_handle: JoinHandle<()>,
}

impl DxFeed {
    /// Receive the next DX event. Returns `None` when the feed is shut down
    /// and all pending events have been consumed.
    pub async fn next_event(&mut self) -> Option<DxEvent> {
        self.event_rx.recv().await
    }

    /// Request clean shutdown of all sources and the aggregator.
    ///
    /// Existing events in the channel can still be drained via `next_event()`.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Hot-reload the filter configuration without restarting connections.
    ///
    /// The new filter is validated and compiled before being applied.
    /// Returns an error if the configuration is invalid (the old filter
    /// remains active).
    pub fn update_filter(&self, filter: FilterConfigSerde) -> Result<(), FilterConfigError> {
        let compiled = filter.validate_and_compile()?;
        let _ = self.filter_tx.send(compiled);
        Ok(())
    }

    /// Returns `true` if shutdown has been requested.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.is_cancelled()
    }
}

// ---------------------------------------------------------------------------
// Aggregator task
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_aggregator_task(
    mut source_rx: mpsc::Receiver<SourceMessage>,
    event_tx: mpsc::Sender<DxEvent>,
    mut filter_rx: watch::Receiver<FilterConfig>,
    config: AggregatorConfig,
    skimmer_config: Option<SkimmerQualityConfig>,
    entity_resolver: Option<Box<dyn EntityResolver>>,
    shutdown: CancellationToken,
    tick_interval: Duration,
) {
    let initial_filter = filter_rx.borrow_and_update().clone();
    let mut aggregator = Aggregator::new(initial_filter, skimmer_config, config, entity_resolver);
    let mut tick = tokio::time::interval(tick_interval);

    loop {
        tokio::select! {
            msg = source_rx.recv() => {
                match msg {
                    Some(SourceMessage::Observation(obs)) => {
                        if let Some(event) = aggregator.process_observation(obs) {
                            if event_tx.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                    Some(SourceMessage::Status(status)) => {
                        let _ = event_tx.send(DxEvent::SourceStatus(status)).await;
                    }
                    Some(SourceMessage::Announce(announce)) => {
                        let _ = event_tx.send(DxEvent::Announce(announce)).await;
                    }
                    None => return,
                }
            }
            _ = tick.tick() => {
                let events = aggregator.tick(Utc::now());
                for event in events {
                    if event_tx.send(event).await.is_err() {
                        return;
                    }
                }
            }
            result = filter_rx.changed() => {
                if result.is_ok() {
                    let new_filter = filter_rx.borrow_and_update().clone();
                    aggregator.update_filter(new_filter);
                }
            }
            _ = shutdown.cancelled() => {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::domain::Band;
    use crate::filter::config::FilterConfigSerde;
    use crate::model::{DxEvent, SourceId, SpotEventKind};
    use crate::source::supervisor::{BackoffConfig, SourceConfig};
    use crate::source::telnet::TelnetSourceConfig;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    fn fast_backoff() -> BackoffConfig {
        BackoffConfig {
            initial: Duration::from_millis(50),
            max: Duration::from_millis(200),
            multiplier: 2.0,
            jitter_factor: 0.0,
        }
    }

    #[test]
    fn no_sources_error() {
        let result = DxFeedBuilder::new().build();
        assert!(matches!(result, Err(DxFeedError::NoSources)));
    }

    #[test]
    fn invalid_filter_error() {
        let mut filter = FilterConfigSerde::default();
        filter.freq_sanity_hz = Some((100_000_000, 1_000)); // reversed range

        let result = DxFeedBuilder::new()
            .add_source(SourceConfig::Telnet(TelnetSourceConfig::new(
                "127.0.0.1",
                7300,
                "W1AW",
                SourceId("test".into()),
            )))
            .set_filter(filter)
            .build();

        assert!(matches!(result, Err(DxFeedError::FilterConfig(_))));
    }

    #[test]
    fn builder_defaults() {
        let builder = DxFeedBuilder::new();
        assert_eq!(builder.sources.len(), 0);
        assert_eq!(builder.source_channel_capacity, 256);
        assert_eq!(builder.event_channel_capacity, 256);
        assert_eq!(builder.tick_interval, Duration::from_secs(10));
    }

    #[tokio::test]
    async fn single_source_receives_spot() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"login:\r\n").await.unwrap();
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf).await;
            stream.write_all(b"Welcome\r\n").await.unwrap();
            stream
                .write_all(
                    b"DX de W3LPL:     14025.0  JA1ABC       CQ                         1830Z\r\n",
                )
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            stream.shutdown().await.ok();
        });

        let config = SourceConfig::Telnet(TelnetSourceConfig::new(
            addr.ip().to_string(),
            addr.port(),
            "W1AW",
            SourceId("test".into()),
        ));

        let mut feed = DxFeedBuilder::new()
            .add_source(config)
            .set_backoff(fast_backoff())
            .tick_interval(Duration::from_secs(60))
            .build()
            .unwrap();

        let mut got_spot = false;
        let timeout = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                event = feed.next_event() => {
                    match event {
                        Some(DxEvent::Spot(e)) => {
                            assert_eq!(e.kind, SpotEventKind::New);
                            assert_eq!(e.spot.dx_call, "JA1ABC");
                            got_spot = true;
                            break;
                        }
                        Some(_) => {} // skip status events
                        None => break,
                    }
                }
                _ = &mut timeout => break,
            }
        }

        feed.shutdown();
        server.await.unwrap();
        assert!(got_spot, "should have received a spot");
    }

    #[tokio::test]
    async fn shutdown_stops_feed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"login:\r\n").await.unwrap();
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf).await;
            stream.write_all(b"Welcome\r\n").await.unwrap();
            // Hold connection open
            tokio::time::sleep(Duration::from_secs(60)).await;
        });

        let config = SourceConfig::Telnet(TelnetSourceConfig::new(
            addr.ip().to_string(),
            addr.port(),
            "W1AW",
            SourceId("test".into()),
        ));

        let mut feed = DxFeedBuilder::new()
            .add_source(config)
            .set_backoff(fast_backoff())
            .tick_interval(Duration::from_secs(60))
            .build()
            .unwrap();

        // Wait for connection to establish
        tokio::time::sleep(Duration::from_millis(200)).await;

        feed.shutdown();
        assert!(feed.is_shutdown());

        // Drain remaining events; should reach None
        let deadline = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                event = feed.next_event() => {
                    if event.is_none() {
                        break;
                    }
                }
                _ = &mut deadline => {
                    panic!("feed did not shut down within 2 seconds");
                }
            }
        }

        server.abort();
    }

    #[tokio::test]
    async fn filter_update_blocks_band() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"login:\r\n").await.unwrap();
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf).await;
            stream.write_all(b"Welcome\r\n").await.unwrap();

            // First spot (20m)
            stream
                .write_all(
                    b"DX de W3LPL:     14025.0  JA1ABC       CQ                         1830Z\r\n",
                )
                .await
                .unwrap();

            // Pause to allow filter update
            tokio::time::sleep(Duration::from_millis(300)).await;

            // Second spot (20m, different dx_call — should be blocked after filter update)
            stream
                .write_all(
                    b"DX de VE3NEA:    14030.0  DL1ABC       CQ                         1831Z\r\n",
                )
                .await
                .unwrap();

            tokio::time::sleep(Duration::from_millis(100)).await;
            stream.shutdown().await.ok();
        });

        let config = SourceConfig::Telnet(TelnetSourceConfig::new(
            addr.ip().to_string(),
            addr.port(),
            "W1AW",
            SourceId("test".into()),
        ));

        let mut feed = DxFeedBuilder::new()
            .add_source(config)
            .set_backoff(fast_backoff())
            .tick_interval(Duration::from_secs(60))
            .build()
            .unwrap();

        // Wait for first spot
        let mut first_spot = false;
        let timeout = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                event = feed.next_event() => {
                    match event {
                        Some(DxEvent::Spot(e)) if e.kind == SpotEventKind::New => {
                            first_spot = true;
                            break;
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
                _ = &mut timeout => break,
            }
        }

        assert!(first_spot, "should receive first spot");

        // Update filter to deny 20m
        let mut new_filter = FilterConfigSerde::default();
        new_filter.rf.band_deny.insert(Band::B20);
        feed.update_filter(new_filter).unwrap();

        // Collect remaining events — should NOT see another 20m spot
        let mut second_spot = false;
        let timeout2 = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(timeout2);

        loop {
            tokio::select! {
                event = feed.next_event() => {
                    match event {
                        Some(DxEvent::Spot(e)) if e.kind == SpotEventKind::New => {
                            second_spot = true;
                            break;
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
                _ = &mut timeout2 => break,
            }
        }

        feed.shutdown();
        server.await.unwrap();
        assert!(!second_spot, "second spot should be blocked by updated filter");
    }

    #[tokio::test]
    async fn multi_source_receives_from_both() {
        let listener1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr1 = listener1.local_addr().unwrap();
        let listener2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr2 = listener2.local_addr().unwrap();

        let server1 = tokio::spawn(async move {
            let (mut stream, _) = listener1.accept().await.unwrap();
            stream.write_all(b"login:\r\n").await.unwrap();
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf).await;
            stream.write_all(b"Welcome\r\n").await.unwrap();
            stream
                .write_all(
                    b"DX de W3LPL:     14025.0  JA1ABC       CQ                         1830Z\r\n",
                )
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            stream.shutdown().await.ok();
        });

        let server2 = tokio::spawn(async move {
            let (mut stream, _) = listener2.accept().await.unwrap();
            stream.write_all(b"login:\r\n").await.unwrap();
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf).await;
            stream.write_all(b"Welcome\r\n").await.unwrap();
            // Different spot on a different band
            stream
                .write_all(
                    b"DX de VE3NEA:     7025.0  DL1ABC       CQ                         1830Z\r\n",
                )
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            stream.shutdown().await.ok();
        });

        let config1 = SourceConfig::Telnet(TelnetSourceConfig::new(
            addr1.ip().to_string(),
            addr1.port(),
            "W1AW",
            SourceId("src1".into()),
        ));
        let config2 = SourceConfig::Telnet(TelnetSourceConfig::new(
            addr2.ip().to_string(),
            addr2.port(),
            "W1AW",
            SourceId("src2".into()),
        ));

        let mut feed = DxFeedBuilder::new()
            .add_source(config1)
            .add_source(config2)
            .set_backoff(fast_backoff())
            .tick_interval(Duration::from_secs(60))
            .build()
            .unwrap();

        let mut spots = Vec::new();
        let timeout = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                event = feed.next_event() => {
                    match event {
                        Some(DxEvent::Spot(e)) if e.kind == SpotEventKind::New => {
                            spots.push(e.spot.dx_call.clone());
                            if spots.len() >= 2 {
                                break;
                            }
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
                _ = &mut timeout => break,
            }
        }

        feed.shutdown();
        server1.await.unwrap();
        server2.await.unwrap();

        assert_eq!(spots.len(), 2, "should have received spots from both sources");
        spots.sort();
        assert_eq!(spots, vec!["DL1ABC", "JA1ABC"]);
    }
}
