//! Runtime metrics for the DxFeed pipeline.
//!
//! [`DxFeedMetrics`] provides a snapshot of counters and gauges that track
//! pipeline health: spots received, filtered, emitted, parse errors,
//! reconnects, and current spot table size.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Shared metrics handle, cheaply cloneable.
///
/// All counters use relaxed atomic operations — exact consistency is not
/// required for metrics.
#[derive(Debug, Clone)]
pub struct MetricsHandle {
    inner: Arc<MetricsInner>,
}

#[derive(Debug)]
struct MetricsInner {
    spots_received: AtomicU64,
    spots_filtered: AtomicU64,
    spots_emitted: AtomicU64,
    parse_errors: AtomicU64,
    reconnects: AtomicU64,
    spot_table_size: AtomicU64,
}

/// A point-in-time snapshot of pipeline metrics.
#[derive(Debug, Clone)]
pub struct DxFeedMetrics {
    /// Total spot observations received from all sources.
    pub spots_received: u64,
    /// Total spots dropped by the filter.
    pub spots_filtered: u64,
    /// Total spot events emitted to the consumer.
    pub spots_emitted: u64,
    /// Total unparseable lines encountered.
    pub parse_errors: u64,
    /// Total source reconnection attempts.
    pub reconnects: u64,
    /// Current number of active spots in the spot table.
    pub spot_table_size: u64,
}

impl MetricsHandle {
    /// Create a new metrics handle.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MetricsInner {
                spots_received: AtomicU64::new(0),
                spots_filtered: AtomicU64::new(0),
                spots_emitted: AtomicU64::new(0),
                parse_errors: AtomicU64::new(0),
                reconnects: AtomicU64::new(0),
                spot_table_size: AtomicU64::new(0),
            }),
        }
    }

    /// Increment spots received counter.
    pub fn inc_spots_received(&self) {
        self.inner.spots_received.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment spots filtered counter.
    pub fn inc_spots_filtered(&self) {
        self.inner.spots_filtered.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment spots emitted counter.
    pub fn inc_spots_emitted(&self) {
        self.inner.spots_emitted.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment parse errors counter.
    pub fn inc_parse_errors(&self) {
        self.inner.parse_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment reconnects counter.
    pub fn inc_reconnects(&self) {
        self.inner.reconnects.fetch_add(1, Ordering::Relaxed);
    }

    /// Set the current spot table size gauge.
    pub fn set_spot_table_size(&self, size: u64) {
        self.inner.spot_table_size.store(size, Ordering::Relaxed);
    }

    /// Take a point-in-time snapshot of all metrics.
    pub fn snapshot(&self) -> DxFeedMetrics {
        DxFeedMetrics {
            spots_received: self.inner.spots_received.load(Ordering::Relaxed),
            spots_filtered: self.inner.spots_filtered.load(Ordering::Relaxed),
            spots_emitted: self.inner.spots_emitted.load(Ordering::Relaxed),
            parse_errors: self.inner.parse_errors.load(Ordering::Relaxed),
            reconnects: self.inner.reconnects.load(Ordering::Relaxed),
            spot_table_size: self.inner.spot_table_size.load(Ordering::Relaxed),
        }
    }
}

impl Default for MetricsHandle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_default_zeros() {
        let handle = MetricsHandle::new();
        let snap = handle.snapshot();
        assert_eq!(snap.spots_received, 0);
        assert_eq!(snap.spots_filtered, 0);
        assert_eq!(snap.spots_emitted, 0);
        assert_eq!(snap.parse_errors, 0);
        assert_eq!(snap.reconnects, 0);
        assert_eq!(snap.spot_table_size, 0);
    }

    #[test]
    fn metrics_increment_and_snapshot() {
        let handle = MetricsHandle::new();
        handle.inc_spots_received();
        handle.inc_spots_received();
        handle.inc_spots_emitted();
        handle.inc_spots_filtered();
        handle.inc_parse_errors();
        handle.inc_reconnects();
        handle.set_spot_table_size(42);

        let snap = handle.snapshot();
        assert_eq!(snap.spots_received, 2);
        assert_eq!(snap.spots_emitted, 1);
        assert_eq!(snap.spots_filtered, 1);
        assert_eq!(snap.parse_errors, 1);
        assert_eq!(snap.reconnects, 1);
        assert_eq!(snap.spot_table_size, 42);
    }

    #[test]
    fn metrics_clone_shares_state() {
        let handle1 = MetricsHandle::new();
        let handle2 = handle1.clone();

        handle1.inc_spots_received();
        handle2.inc_spots_received();

        assert_eq!(handle1.snapshot().spots_received, 2);
        assert_eq!(handle2.snapshot().spots_received, 2);
    }

    #[test]
    fn metrics_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MetricsHandle>();
    }
}
