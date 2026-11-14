//! Runtime metrics for the DxFeed pipeline.
//!
//! [`MetricsHandle`] is a cheaply cloneable set of counters shared by the
//! source tasks and the aggregator. [`DxFeedMetrics`] is a point-in-time
//! snapshot of them, obtained from [`DxFeed::metrics()`](crate::feed::DxFeed::metrics).
//!
//! Every observation the aggregator receives ends in exactly one of four
//! outcomes, so at any quiescent moment:
//!
//! ```text
//! spots_received = spots_gated + spots_filtered + spots_deduped
//!                + emitted_new + emitted_update
//! ```
//!
//! Withdraws are counted separately; they are caused by expiry, QSY and
//! eviction rather than one-for-one by observations.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::filter::decision::FilterDropReason;
use crate::model::{DxEvent, SkimQualityTag, SourceId, SpotEventKind, WithdrawReason};

// ---------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------

/// Shared metrics handle, cheaply cloneable.
///
/// All counters use relaxed atomic operations — exact consistency between
/// counters is not required for metrics.
#[derive(Debug, Clone)]
pub struct MetricsHandle {
    inner: Arc<MetricsInner>,
}

#[derive(Debug, Default)]
struct MetricsInner {
    spots_received: AtomicU64,
    spots_deduped: AtomicU64,
    skimmer_tagged: [AtomicU64; SKIM_TAGS],
    skimmer_gated: [AtomicU64; SKIM_TAGS],
    filtered: [AtomicU64; FilterDropReason::ALL.len()],
    emitted_new: AtomicU64,
    emitted_update: AtomicU64,
    withdrawn: [AtomicU64; WITHDRAW_REASONS],
    spot_table_size: AtomicU64,
    sources: Mutex<Vec<(SourceId, SourceMetricsHandle)>>,
}

const SKIM_TAGS: usize = 4;
const WITHDRAW_REASONS: usize = 3;

fn skim_tag_index(tag: SkimQualityTag) -> usize {
    match tag {
        SkimQualityTag::Valid => 0,
        SkimQualityTag::Busted => 1,
        SkimQualityTag::Qsy => 2,
        SkimQualityTag::Unknown => 3,
    }
}

fn withdraw_reason_index(reason: WithdrawReason) -> usize {
    match reason {
        WithdrawReason::Expired => 0,
        WithdrawReason::Qsy => 1,
        WithdrawReason::Evicted => 2,
    }
}

/// Per-source counters, written by one source task.
#[derive(Debug, Clone, Default)]
pub struct SourceMetricsHandle {
    inner: Arc<SourceCounters>,
}

#[derive(Debug, Default)]
struct SourceCounters {
    spots_received: AtomicU64,
    unparsed_lines: AtomicU64,
    reconnects: AtomicU64,
}

impl SourceMetricsHandle {
    /// A spot line was parsed and handed to the aggregator.
    pub fn inc_spots_received(&self) {
        self.inner.spots_received.fetch_add(1, Ordering::Relaxed);
    }

    /// A non-empty line arrived after login that was not a spot, an
    /// announcement, propagation data or a prompt.
    pub fn inc_unparsed_lines(&self) {
        self.inner.unparsed_lines.fetch_add(1, Ordering::Relaxed);
    }

    /// The connection ended and a reconnect was scheduled.
    pub fn inc_reconnects(&self) {
        self.inner.reconnects.fetch_add(1, Ordering::Relaxed);
    }
}

impl MetricsHandle {
    /// Create a new metrics handle with all counters at zero.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MetricsInner::default()),
        }
    }

    /// Get the counters for a source, registering it on first use.
    pub fn source(&self, source_id: &SourceId) -> SourceMetricsHandle {
        let mut sources = self.inner.sources.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, handle)) = sources.iter().find(|(id, _)| id == source_id) {
            return handle.clone();
        }
        let handle = SourceMetricsHandle::default();
        sources.push((source_id.clone(), handle.clone()));
        handle
    }

    /// An observation reached the aggregator.
    pub fn inc_spots_received(&self) {
        self.inner.spots_received.fetch_add(1, Ordering::Relaxed);
    }

    /// A skimmer observation was classified by the quality engine.
    pub fn inc_skimmer_tagged(&self, tag: SkimQualityTag) {
        self.inner.skimmer_tagged[skim_tag_index(tag)].fetch_add(1, Ordering::Relaxed);
    }

    /// An observation was blocked by skimmer quality gating.
    pub fn inc_skimmer_gated(&self, tag: SkimQualityTag) {
        self.inner.skimmer_gated[skim_tag_index(tag)].fetch_add(1, Ordering::Relaxed);
    }

    /// An observation was dropped by the filter.
    pub fn inc_filtered(&self, reason: FilterDropReason) {
        self.inner.filtered[reason as usize].fetch_add(1, Ordering::Relaxed);
    }

    /// An observation passed gating and the filter but produced no event,
    /// because its spot was already emitted and no update was due.
    pub fn inc_spots_deduped(&self) {
        self.inner.spots_deduped.fetch_add(1, Ordering::Relaxed);
    }

    /// Count the spot events the aggregator is about to hand to the consumer.
    pub fn record_events(&self, events: &[DxEvent]) {
        for event in events {
            let DxEvent::Spot(e) = event else { continue };
            match e.kind {
                SpotEventKind::New => {
                    self.inner.emitted_new.fetch_add(1, Ordering::Relaxed);
                }
                SpotEventKind::Update => {
                    self.inner.emitted_update.fetch_add(1, Ordering::Relaxed);
                }
                SpotEventKind::Withdraw => {
                    // Every Withdraw carries a reason; fall back to Expired
                    // rather than lose the count if one ever does not.
                    let reason = e.withdraw_reason.unwrap_or(WithdrawReason::Expired);
                    self.inner.withdrawn[withdraw_reason_index(reason)]
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Set the current spot table size gauge.
    pub fn set_spot_table_size(&self, size: u64) {
        self.inner.spot_table_size.store(size, Ordering::Relaxed);
    }

    /// Take a point-in-time snapshot of all metrics.
    pub fn snapshot(&self) -> DxFeedMetrics {
        let load = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let tags = |a: &[AtomicU64; SKIM_TAGS]| SkimTagCounts {
            valid: load(&a[0]),
            busted: load(&a[1]),
            qsy: load(&a[2]),
            unknown: load(&a[3]),
        };
        let inner = &self.inner;

        let filtered_by_reason: Vec<(FilterDropReason, u64)> = FilterDropReason::ALL
            .iter()
            .map(|&reason| (reason, load(&inner.filtered[reason as usize])))
            .filter(|(_, count)| *count > 0)
            .collect();

        let sources: Vec<SourceMetrics> = inner
            .sources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, handle)| SourceMetrics {
                source_id: id.clone(),
                spots_received: load(&handle.inner.spots_received),
                unparsed_lines: load(&handle.inner.unparsed_lines),
                reconnects: load(&handle.inner.reconnects),
            })
            .collect();

        let skimmer_gated = tags(&inner.skimmer_gated);
        let withdrawn = WithdrawCounts {
            expired: load(&inner.withdrawn[0]),
            qsy: load(&inner.withdrawn[1]),
            evicted: load(&inner.withdrawn[2]),
        };
        let emitted_new = load(&inner.emitted_new);
        let emitted_update = load(&inner.emitted_update);
        let emitted_withdraw = withdrawn.total();

        DxFeedMetrics {
            spots_received: load(&inner.spots_received),
            spots_gated: skimmer_gated.total(),
            spots_filtered: filtered_by_reason.iter().map(|(_, n)| n).sum(),
            spots_deduped: load(&inner.spots_deduped),
            spots_emitted: emitted_new + emitted_update + emitted_withdraw,
            emitted_new,
            emitted_update,
            emitted_withdraw,
            withdrawn,
            skimmer_tagged: tags(&inner.skimmer_tagged),
            skimmer_gated,
            filtered_by_reason,
            parse_errors: sources.iter().map(|s| s.unparsed_lines).sum(),
            reconnects: sources.iter().map(|s| s.reconnects).sum(),
            spot_table_size: load(&inner.spot_table_size),
            sources,
        }
    }
}

impl Default for MetricsHandle {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Snapshot types
// ---------------------------------------------------------------------------

/// A point-in-time snapshot of pipeline metrics. All counts are totals
/// since the feed was built, except `spot_table_size`, which is a gauge.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DxFeedMetrics {
    /// Spot observations that reached the aggregator, from all sources.
    pub spots_received: u64,
    /// Observations blocked by skimmer quality gating.
    pub spots_gated: u64,
    /// Observations dropped by the filter.
    pub spots_filtered: u64,
    /// Observations that passed gating and the filter but produced no
    /// event, because their spot was already emitted.
    pub spots_deduped: u64,
    /// Spot events produced for the consumer (`New` + `Update` + `Withdraw`).
    pub spots_emitted: u64,
    pub emitted_new: u64,
    pub emitted_update: u64,
    pub emitted_withdraw: u64,
    /// `Withdraw` events by reason.
    pub withdrawn: WithdrawCounts,
    /// Skimmer observations by the quality tag they were given.
    pub skimmer_tagged: SkimTagCounts,
    /// Gated observations by quality tag (a subset of `skimmer_tagged`).
    pub skimmer_gated: SkimTagCounts,
    /// Filter drops by reason; reasons with no drops are omitted.
    pub filtered_by_reason: Vec<(FilterDropReason, u64)>,
    /// Lines received after login that could not be classified, from all
    /// sources. A high count against a node suggests an unsupported format.
    pub parse_errors: u64,
    /// Reconnects scheduled, from all sources.
    pub reconnects: u64,
    /// Current number of spots in the spot table (emitted or not).
    pub spot_table_size: u64,
    /// Per-source counters, in the order sources were added.
    pub sources: Vec<SourceMetrics>,
}

/// Counters for one upstream source.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SourceMetrics {
    pub source_id: SourceId,
    /// Spot lines parsed from this source.
    pub spots_received: u64,
    /// Lines received after login that could not be classified.
    pub unparsed_lines: u64,
    /// Reconnects scheduled for this source.
    pub reconnects: u64,
}

/// Counts broken down by skimmer quality tag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SkimTagCounts {
    pub valid: u64,
    pub busted: u64,
    pub qsy: u64,
    pub unknown: u64,
}

impl SkimTagCounts {
    pub fn total(&self) -> u64 {
        self.valid + self.busted + self.qsy + self.unknown
    }
}

/// `Withdraw` counts broken down by reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WithdrawCounts {
    pub expired: u64,
    pub qsy: u64,
    pub evicted: u64,
}

impl WithdrawCounts {
    pub fn total(&self) -> u64 {
        self.expired + self.qsy + self.evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_default_zeros() {
        let snap = MetricsHandle::new().snapshot();
        assert_eq!(snap.spots_received, 0);
        assert_eq!(snap.spots_gated, 0);
        assert_eq!(snap.spots_filtered, 0);
        assert_eq!(snap.spots_deduped, 0);
        assert_eq!(snap.spots_emitted, 0);
        assert_eq!(snap.parse_errors, 0);
        assert_eq!(snap.reconnects, 0);
        assert_eq!(snap.spot_table_size, 0);
        assert!(snap.filtered_by_reason.is_empty());
        assert!(snap.sources.is_empty());
    }

    #[test]
    fn metrics_increment_and_snapshot() {
        let handle = MetricsHandle::new();
        handle.inc_spots_received();
        handle.inc_spots_received();
        handle.inc_spots_received();
        handle.inc_spots_deduped();
        handle.inc_filtered(FilterDropReason::BandDenied);
        handle.inc_filtered(FilterDropReason::BandDenied);
        handle.inc_filtered(FilterDropReason::CorrelationNotMet);
        handle.inc_skimmer_tagged(SkimQualityTag::Valid);
        handle.inc_skimmer_tagged(SkimQualityTag::Busted);
        handle.inc_skimmer_gated(SkimQualityTag::Busted);
        handle.set_spot_table_size(42);

        let snap = handle.snapshot();
        assert_eq!(snap.spots_received, 3);
        assert_eq!(snap.spots_deduped, 1);
        assert_eq!(snap.spots_filtered, 3);
        assert_eq!(
            snap.filtered_by_reason,
            vec![
                (FilterDropReason::BandDenied, 2),
                (FilterDropReason::CorrelationNotMet, 1),
            ]
        );
        assert_eq!(snap.skimmer_tagged.valid, 1);
        assert_eq!(snap.skimmer_tagged.busted, 1);
        assert_eq!(snap.skimmer_gated.busted, 1);
        assert_eq!(snap.spots_gated, 1);
        assert_eq!(snap.spot_table_size, 42);
    }

    #[test]
    fn filter_drop_reason_all_matches_discriminants() {
        // `inc_filtered` indexes by discriminant; ALL must list every
        // variant in declaration order.
        for (i, reason) in FilterDropReason::ALL.iter().enumerate() {
            assert_eq!(*reason as usize, i);
        }
    }

    #[test]
    fn source_counters_are_per_source_and_summed() {
        let handle = MetricsHandle::new();
        let a = handle.source(&SourceId("a".into()));
        let b = handle.source(&SourceId("b".into()));
        // Asking again returns the same counters, not a new registration.
        let a_again = handle.source(&SourceId("a".into()));

        a.inc_spots_received();
        a_again.inc_spots_received();
        a.inc_unparsed_lines();
        b.inc_reconnects();
        b.inc_unparsed_lines();

        let snap = handle.snapshot();
        assert_eq!(snap.sources.len(), 2);
        assert_eq!(snap.sources[0].source_id, SourceId("a".into()));
        assert_eq!(snap.sources[0].spots_received, 2);
        assert_eq!(snap.sources[0].unparsed_lines, 1);
        assert_eq!(snap.sources[1].reconnects, 1);
        assert_eq!(snap.parse_errors, 2);
        assert_eq!(snap.reconnects, 1);
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
        assert_send_sync::<SourceMetricsHandle>();
    }
}
