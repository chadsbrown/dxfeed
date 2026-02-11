//! End-to-end integration tests: raw bytes → DxFeed → DxEvents.

use std::time::Duration;

use dxfeed::domain::{Band, DxMode};
use dxfeed::feed::{DxFeedBuilder, DxFeed};
use dxfeed::model::{DxEvent, SourceId, SpotEventKind};
use dxfeed::source::supervisor::{BackoffConfig, SourceConfig};
use dxfeed::source::telnet::TelnetSourceConfig;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn fast_backoff() -> BackoffConfig {
    BackoffConfig {
        initial: Duration::from_millis(50),
        max: Duration::from_millis(200),
        multiplier: 2.0,
        jitter_factor: 0.0,
    }
}

/// Bind a TCP listener on a random port.
async fn bind_listener() -> (TcpListener, std::net::SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

async fn serve_lines(listener: TcpListener, lines: Vec<String>) {
    let (mut stream, _) = listener.accept().await.unwrap();
    stream.write_all(b"login:\r\n").await.unwrap();
    let mut buf = [0u8; 128];
    let _ = stream.read(&mut buf).await;
    stream.write_all(b"Welcome to DX Cluster\r\n").await.unwrap();

    for line in &lines {
        stream.write_all(line.as_bytes()).await.unwrap();
        stream.write_all(b"\r\n").await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(100)).await;
    stream.shutdown().await.ok();
}

/// Collect spot events from a feed with a timeout.
async fn collect_spots(feed: &mut DxFeed, timeout_secs: u64) -> Vec<dxfeed::model::DxSpotEvent> {
    let mut spots = Vec::new();
    let timeout = tokio::time::sleep(Duration::from_secs(timeout_secs));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            event = feed.next_event() => {
                match event {
                    Some(DxEvent::Spot(e)) => spots.push(e),
                    Some(_) => {}
                    None => break,
                }
            }
            _ = &mut timeout => break,
        }
    }

    spots
}

// ---------------------------------------------------------------------------
// Test 1: Basic pipeline — fixture file through full pipeline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn basic_pipeline_dxspider_fixture() {
    let fixture = include_str!("fixtures/dxspider_spots.txt");
    let lines: Vec<String> = fixture.lines().map(|l| l.to_string()).collect();
    let (listener, addr) = bind_listener().await;

    let server = tokio::spawn(serve_lines(listener, lines));

    let config = SourceConfig::Telnet(TelnetSourceConfig::new(
        addr.ip().to_string(),
        addr.port(),
        "TEST",
        SourceId("test-dxspider".into()),
    ));

    let mut feed = DxFeedBuilder::new()
        .add_source(config)
        .set_backoff(fast_backoff())
        .tick_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    let spots = collect_spots(&mut feed, 5).await;

    feed.shutdown();
    server.await.unwrap();

    // The fixture has 20 lines. Due to dedup (JA1ABC appears 3x on 14025,
    // DL1ABC appears 2x on 7025), we expect fewer unique spots.
    assert!(
        !spots.is_empty(),
        "should have received some spots from DXSpider fixture"
    );

    // All emitted should be New (no emit_updates by default)
    for spot in &spots {
        assert_eq!(spot.kind, SpotEventKind::New);
    }

    // Verify specific spots are present
    let dx_calls: Vec<&str> = spots.iter().map(|s| s.spot.dx_call.as_str()).collect();
    assert!(dx_calls.contains(&"JA1ABC"), "should see JA1ABC");
    assert!(dx_calls.contains(&"DL1ABC"), "should see DL1ABC");
    assert!(dx_calls.contains(&"G3WCB"), "should see G3WCB");
}

// ---------------------------------------------------------------------------
// Test 2: Multi-source deduplication
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multi_source_dedup() {
    // Both sources send the same spot
    let line = "DX de W3LPL:     14025.0  JA1ABC       CQ                         1830Z";

    let (listener1, addr1) = bind_listener().await;
    let (listener2, addr2) = bind_listener().await;

    let server1 = tokio::spawn(serve_lines(listener1, vec![line.to_string()]));
    let server2 = tokio::spawn(serve_lines(listener2, vec![line.to_string()]));

    let config1 = SourceConfig::Telnet(TelnetSourceConfig::new(
        addr1.ip().to_string(),
        addr1.port(),
        "TEST",
        SourceId("src1".into()),
    ));
    let config2 = SourceConfig::Telnet(TelnetSourceConfig::new(
        addr2.ip().to_string(),
        addr2.port(),
        "TEST",
        SourceId("src2".into()),
    ));

    let mut feed = DxFeedBuilder::new()
        .add_source(config1)
        .add_source(config2)
        .set_backoff(fast_backoff())
        .tick_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    let spots = collect_spots(&mut feed, 5).await;

    feed.shutdown();
    server1.await.unwrap();
    server2.await.unwrap();

    // Should get exactly one New event for JA1ABC (deduped)
    let new_spots: Vec<_> = spots
        .iter()
        .filter(|s| s.kind == SpotEventKind::New)
        .collect();
    assert_eq!(
        new_spots.len(),
        1,
        "same spot from two sources should dedup to one New event"
    );
    assert_eq!(new_spots[0].spot.dx_call, "JA1ABC");
}

// ---------------------------------------------------------------------------
// Test 3: Skimmer gating end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn skimmer_gating_end_to_end() {
    use dxfeed::skimmer::config::SkimmerQualityConfig;
    use dxfeed::source::rbn::RbnSourceConfig;

    // Three different skimmers report the same call
    let lines = vec![
        "DX de W3LPL-2:   14025.0  JA1ABC       CW    18 dB  25 WPM  CQ     1830Z".to_string(),
        "DX de DK8JP-1:   14025.1  JA1ABC       CW    15 dB  25 WPM  CQ     1830Z".to_string(),
        "DX de VE3NEA-3:  14025.0  JA1ABC       CW    22 dB  25 WPM  CQ     1830Z".to_string(),
    ];

    let (listener, addr) = bind_listener().await;
    let server = tokio::spawn(serve_lines(listener, lines));

    let mut rbn_config = RbnSourceConfig::new("TEST", SourceId("rbn-test".into()));
    rbn_config.host = addr.ip().to_string();
    rbn_config.port = addr.port();

    let mut feed = DxFeedBuilder::new()
        .add_source(SourceConfig::Rbn(rbn_config))
        .set_skimmer_quality(SkimmerQualityConfig::default())
        .set_backoff(fast_backoff())
        .tick_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    let spots = collect_spots(&mut feed, 5).await;

    feed.shutdown();
    server.await.unwrap();

    // With default gating (3 distinct skimmers required), the spot should
    // eventually be emitted after the 3rd skimmer reports.
    let new_spots: Vec<_> = spots
        .iter()
        .filter(|s| s.kind == SpotEventKind::New)
        .collect();
    assert_eq!(
        new_spots.len(),
        1,
        "should emit one New event after skimmer gating passes"
    );
    assert_eq!(new_spots[0].spot.dx_call, "JA1ABC");
}

// ---------------------------------------------------------------------------
// Test 4: Filter scenarios
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filter_blocks_denied_band() {
    use dxfeed::filter::config::FilterConfigSerde;

    let lines = vec![
        "DX de W3LPL:     14025.0  JA1ABC       CQ                         1830Z".to_string(),
        "DX de VE3NEA:     7025.5  DL1ABC       CQ TEST                    1831Z".to_string(),
    ];

    let (listener, addr) = bind_listener().await;
    let server = tokio::spawn(serve_lines(listener, lines));

    // Deny 20m band
    let mut filter = FilterConfigSerde::default();
    filter.rf.band_deny.insert(Band::B20);

    let config = SourceConfig::Telnet(TelnetSourceConfig::new(
        addr.ip().to_string(),
        addr.port(),
        "TEST",
        SourceId("test".into()),
    ));

    let mut feed = DxFeedBuilder::new()
        .add_source(config)
        .set_filter(filter)
        .set_backoff(fast_backoff())
        .tick_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    let spots = collect_spots(&mut feed, 5).await;

    feed.shutdown();
    server.await.unwrap();

    // JA1ABC is on 20m (denied), DL1ABC is on 40m (allowed)
    let dx_calls: Vec<&str> = spots.iter().map(|s| s.spot.dx_call.as_str()).collect();
    assert!(!dx_calls.contains(&"JA1ABC"), "20m spot should be blocked");
    assert!(dx_calls.contains(&"DL1ABC"), "40m spot should pass");
}

#[tokio::test]
async fn filter_allows_only_listed_modes() {
    use dxfeed::filter::config::FilterConfigSerde;

    let lines = vec![
        // CW spot (14025)
        "DX de W3LPL:     14025.0  JA1ABC       CQ                         1830Z".to_string(),
        // SSB spot (14195)
        "DX de AA1K:     14195.0  EA3FHP       59 in MA                   1835Z".to_string(),
    ];

    let (listener, addr) = bind_listener().await;
    let server = tokio::spawn(serve_lines(listener, lines));

    // Only allow CW
    let mut filter = FilterConfigSerde::default();
    filter.rf.mode_allow.insert(DxMode::CW);

    let config = SourceConfig::Telnet(TelnetSourceConfig::new(
        addr.ip().to_string(),
        addr.port(),
        "TEST",
        SourceId("test".into()),
    ));

    let mut feed = DxFeedBuilder::new()
        .add_source(config)
        .set_filter(filter)
        .set_backoff(fast_backoff())
        .tick_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    let spots = collect_spots(&mut feed, 5).await;

    feed.shutdown();
    server.await.unwrap();

    let dx_calls: Vec<&str> = spots.iter().map(|s| s.spot.dx_call.as_str()).collect();
    assert!(dx_calls.contains(&"JA1ABC"), "CW spot should pass");
    assert!(!dx_calls.contains(&"EA3FHP"), "SSB spot should be blocked");
}

// ---------------------------------------------------------------------------
// Test 5: Hot filter update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hot_filter_update_takes_effect() {
    use dxfeed::filter::config::FilterConfigSerde;

    let (listener, addr) = bind_listener().await;

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.write_all(b"login:\r\n").await.unwrap();
        let mut buf = [0u8; 128];
        let _ = stream.read(&mut buf).await;
        stream.write_all(b"Welcome\r\n").await.unwrap();

        // Send first spot
        stream
            .write_all(b"DX de W3LPL:     14025.0  JA1ABC       CQ                         1830Z\r\n")
            .await
            .unwrap();

        // Wait for filter update to take effect
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Send second spot (same band, different call)
        stream
            .write_all(b"DX de VE3NEA:    14030.0  DL1ABC       CQ                         1831Z\r\n")
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;
        stream.shutdown().await.ok();
    });

    let config = SourceConfig::Telnet(TelnetSourceConfig::new(
        addr.ip().to_string(),
        addr.port(),
        "TEST",
        SourceId("test".into()),
    ));

    let mut feed = DxFeedBuilder::new()
        .add_source(config)
        .set_backoff(fast_backoff())
        .tick_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    // Wait for first spot
    let mut got_first = false;
    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            event = feed.next_event() => {
                match event {
                    Some(DxEvent::Spot(e)) if e.kind == SpotEventKind::New => {
                        got_first = true;
                        break;
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            _ = &mut timeout => break,
        }
    }
    assert!(got_first, "should receive first spot");

    // Now deny 20m
    let mut new_filter = FilterConfigSerde::default();
    new_filter.rf.band_deny.insert(Band::B20);
    feed.update_filter(new_filter).unwrap();

    // Collect remaining — should NOT see DL1ABC (20m is denied now)
    let mut got_second = false;
    let timeout2 = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout2);

    loop {
        tokio::select! {
            event = feed.next_event() => {
                match event {
                    Some(DxEvent::Spot(e)) if e.kind == SpotEventKind::New => {
                        got_second = true;
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
    assert!(!got_second, "second 20m spot should be blocked after filter update");
}

// ---------------------------------------------------------------------------
// Test 6: Dialect coverage — RBN fixture
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rbn_fixture_produces_spots() {
    use dxfeed::source::rbn::RbnSourceConfig;

    let fixture = include_str!("fixtures/rbn_spots.txt");
    let lines: Vec<String> = fixture.lines().map(|l| l.to_string()).collect();

    let (listener, addr) = bind_listener().await;
    let server = tokio::spawn(serve_lines(listener, lines));

    let mut rbn_config = RbnSourceConfig::new("TEST", SourceId("rbn-test".into()));
    rbn_config.host = addr.ip().to_string();
    rbn_config.port = addr.port();

    let mut feed = DxFeedBuilder::new()
        .add_source(SourceConfig::Rbn(rbn_config))
        .set_backoff(fast_backoff())
        .tick_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    let spots = collect_spots(&mut feed, 5).await;

    feed.shutdown();
    server.await.unwrap();

    // RBN spots should produce events (no skimmer gating by default)
    assert!(!spots.is_empty(), "should have received spots from RBN fixture");

    // Verify RBN-specific: all should be tagged as skimmer origin
    // (This is verified internally — here we just check the spots arrived)
    let dx_calls: Vec<&str> = spots.iter().map(|s| s.spot.dx_call.as_str()).collect();
    assert!(dx_calls.contains(&"JA1ABC"));
    assert!(dx_calls.contains(&"DL1ABC"));
}

// ---------------------------------------------------------------------------
// Test 7: Reconnect scenario
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reconnect_after_server_drop() {
    let (listener, addr) = bind_listener().await;

    let server = tokio::spawn(async move {
        // First connection: send one spot, then drop
        {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"login:\r\n").await.unwrap();
            let mut buf = [0u8; 128];
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
        }

        // Second connection: send different spot
        {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"login:\r\n").await.unwrap();
            let mut buf = [0u8; 128];
            let _ = stream.read(&mut buf).await;
            stream.write_all(b"Welcome\r\n").await.unwrap();
            stream
                .write_all(
                    b"DX de VE3NEA:     7025.5  DL1ABC       CQ TEST                    1831Z\r\n",
                )
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            stream.shutdown().await.ok();
        }
    });

    let config = SourceConfig::Telnet(TelnetSourceConfig::new(
        addr.ip().to_string(),
        addr.port(),
        "TEST",
        SourceId("test".into()),
    ));

    let mut feed = DxFeedBuilder::new()
        .add_source(config)
        .set_backoff(fast_backoff())
        .tick_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    let spots = collect_spots(&mut feed, 5).await;

    feed.shutdown();
    server.await.unwrap();

    let dx_calls: Vec<&str> = spots.iter().map(|s| s.spot.dx_call.as_str()).collect();
    assert!(
        dx_calls.contains(&"JA1ABC"),
        "should receive spot from first connection"
    );
    assert!(
        dx_calls.contains(&"DL1ABC"),
        "should receive spot from second connection (after reconnect)"
    );
}

// ---------------------------------------------------------------------------
// Test 8: AR-Cluster dialect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn arcluster_fixture_produces_spots() {
    let fixture = include_str!("fixtures/arcluster_spots.txt");
    let lines: Vec<String> = fixture.lines().map(|l| l.to_string()).collect();

    let (listener, addr) = bind_listener().await;
    let server = tokio::spawn(serve_lines(listener, lines));

    let config = SourceConfig::Telnet(TelnetSourceConfig::new(
        addr.ip().to_string(),
        addr.port(),
        "TEST",
        SourceId("test-arcluster".into()),
    ));

    let mut feed = DxFeedBuilder::new()
        .add_source(config)
        .set_backoff(fast_backoff())
        .tick_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    let spots = collect_spots(&mut feed, 5).await;

    feed.shutdown();
    server.await.unwrap();

    assert!(!spots.is_empty(), "should receive spots from AR-Cluster fixture");

    let dx_calls: Vec<&str> = spots.iter().map(|s| s.spot.dx_call.as_str()).collect();
    assert!(dx_calls.contains(&"JA1ABC"));
    assert!(dx_calls.contains(&"DL1ABC"));
}
