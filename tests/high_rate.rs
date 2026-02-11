//! High-rate stress test: feeds many spots through the pipeline
//! to verify bounded memory and no panics.

use std::time::Duration;

use dxfeed::feed::DxFeedBuilder;
use dxfeed::model::{DxEvent, SourceId, SpotEventKind};
use dxfeed::source::supervisor::{BackoffConfig, SourceConfig};
use dxfeed::source::cluster::ClusterSourceConfig;

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

/// Generate a large number of distinct spot lines.
fn generate_spots(count: usize) -> Vec<String> {
    let mut lines = Vec::with_capacity(count);
    for i in 0..count {
        // Vary the DX call and frequency to create distinct spots
        let call_num = i % 10000;
        let band_freq = match i % 5 {
            0 => 14025,
            1 => 7025,
            2 => 21025,
            3 => 3525,
            _ => 28025,
        };
        let freq = band_freq * 1000 + (i % 100) * 10;
        let freq_khz = freq as f64 / 1000.0;
        let spotter_num = i % 100;

        lines.push(format!(
            "DX de TEST{spotter_num:<4}:  {freq_khz:>9.1}  DX{call_num:<5}       CQ                         1830Z"
        ));
    }
    lines
}

#[tokio::test]
async fn high_rate_no_panic() {
    let spot_count = 2000;
    let lines = generate_spots(spot_count);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.write_all(b"login:\r\n").await.unwrap();
        let mut buf = [0u8; 128];
        let _ = stream.read(&mut buf).await;
        stream.write_all(b"Welcome\r\n").await.unwrap();

        for line in &lines {
            stream.write_all(line.as_bytes()).await.unwrap();
            stream.write_all(b"\r\n").await.unwrap();
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
        stream.shutdown().await.ok();
    });

    let config = SourceConfig::Cluster(ClusterSourceConfig::new(
        addr.ip().to_string(),
        addr.port(),
        "TEST",
        SourceId("stress".into()),
    ));

    let mut feed = DxFeedBuilder::new()
        .add_source(config)
        .set_backoff(fast_backoff())
        .tick_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    let mut spot_count_received = 0u64;
    let timeout = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            event = feed.next_event() => {
                match event {
                    Some(DxEvent::Spot(e)) => {
                        spot_count_received += 1;
                        assert_eq!(e.kind, SpotEventKind::New);
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            _ = &mut timeout => break,
        }
    }

    feed.shutdown();
    server.await.unwrap();

    // Should have received a significant number of spots
    assert!(
        spot_count_received > 100,
        "should have received many spots, got {spot_count_received}"
    );
}

#[tokio::test]
async fn high_rate_multi_source() {
    let spot_count = 500;

    // Two sources sending overlapping spots
    let lines1 = generate_spots(spot_count);
    let lines2 = generate_spots(spot_count);

    let listener1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr1 = listener1.local_addr().unwrap();
    let listener2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr2 = listener2.local_addr().unwrap();

    let server1 = tokio::spawn(async move {
        let (mut stream, _) = listener1.accept().await.unwrap();
        stream.write_all(b"login:\r\n").await.unwrap();
        let mut buf = [0u8; 128];
        let _ = stream.read(&mut buf).await;
        stream.write_all(b"Welcome\r\n").await.unwrap();
        for line in &lines1 {
            stream.write_all(line.as_bytes()).await.unwrap();
            stream.write_all(b"\r\n").await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        stream.shutdown().await.ok();
    });

    let server2 = tokio::spawn(async move {
        let (mut stream, _) = listener2.accept().await.unwrap();
        stream.write_all(b"login:\r\n").await.unwrap();
        let mut buf = [0u8; 128];
        let _ = stream.read(&mut buf).await;
        stream.write_all(b"Welcome\r\n").await.unwrap();
        for line in &lines2 {
            stream.write_all(line.as_bytes()).await.unwrap();
            stream.write_all(b"\r\n").await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        stream.shutdown().await.ok();
    });

    let config1 = SourceConfig::Cluster(ClusterSourceConfig::new(
        addr1.ip().to_string(),
        addr1.port(),
        "TEST",
        SourceId("stress1".into()),
    ));
    let config2 = SourceConfig::Cluster(ClusterSourceConfig::new(
        addr2.ip().to_string(),
        addr2.port(),
        "TEST",
        SourceId("stress2".into()),
    ));

    let mut feed = DxFeedBuilder::new()
        .add_source(config1)
        .add_source(config2)
        .set_backoff(fast_backoff())
        .tick_interval(Duration::from_secs(60))
        .build()
        .unwrap();

    let mut spot_count_received = 0u64;
    let timeout = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            event = feed.next_event() => {
                match event {
                    Some(DxEvent::Spot(_)) => {
                        spot_count_received += 1;
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

    // Should have received spots (overlapping spots get deduped)
    assert!(
        spot_count_received > 50,
        "should have received many spots from two sources, got {spot_count_received}"
    );
}
