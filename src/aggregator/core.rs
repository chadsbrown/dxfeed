//! Aggregator core: wires spot table, skimmer quality, and filter evaluation
//! into a single processing pipeline. Synchronous state machine — no async here.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::callsign::normalize_callsign;
use crate::domain::{Band, DxMode, ModePolicy, OriginatorKind};
use crate::filter::compiled::FilterConfig;
use crate::filter::config::CallsignNormalizationSerde;
use crate::filter::decision::FilterDecision;
use crate::filter::evaluate::evaluate;
use crate::freq::{freq_bucket, freq_to_band, resolve_mode};
use crate::model::{
    DxEvent, DxSpot, DxSpotEvent, GeoResolved, OriginatorId, SkimQualityTag, SourceId,
    SpotConfidence, SpotEventKind, SpotKey, SpotObservation, SpotView,
};
use crate::parser::spot::{ParsedSpot, RbnFields};
use crate::skimmer::config::SkimmerQualityConfig;
use crate::skimmer::quality::SkimmerQualityEngine;

use super::spot_table::{IngestInput, SpotTable, SpotTableConfig, SpotTableResult};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Frequency bucketing configuration.
#[derive(Debug, Clone)]
pub struct FreqBucketConfig {
    pub cw_bucket_hz: u64,
    pub ssb_bucket_hz: u64,
    pub dig_bucket_hz: u64,
}

impl Default for FreqBucketConfig {
    fn default() -> Self {
        Self {
            cw_bucket_hz: 10,
            ssb_bucket_hz: 1000,
            dig_bucket_hz: 100,
        }
    }
}

/// Deduplication / emission control.
#[derive(Debug, Clone, Default)]
pub struct DedupeConfig {
    /// Emit Update events when a spot changes meaningfully.
    pub emit_updates: bool,
}

/// Full aggregator configuration.
#[derive(Debug, Clone)]
pub struct AggregatorConfig {
    pub freq_bucket: FreqBucketConfig,
    pub callsign_norm: CallsignNormalizationSerde,
    pub dedupe: DedupeConfig,
    pub spot_ttl: Duration,
    pub max_observations_per_spot: usize,
    pub mode_policy: ModePolicy,
}

impl Default for AggregatorConfig {
    fn default() -> Self {
        Self {
            freq_bucket: FreqBucketConfig::default(),
            callsign_norm: CallsignNormalizationSerde::default(),
            dedupe: DedupeConfig::default(),
            spot_ttl: Duration::from_secs(900),
            max_observations_per_spot: 20,
            mode_policy: ModePolicy::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// IncomingObservation — input to the aggregator
// ---------------------------------------------------------------------------

/// A parsed spot observation ready for aggregation.
pub struct IncomingObservation {
    pub parsed: ParsedSpot,
    pub source_id: SourceId,
    pub originator_kind: OriginatorKind,
    pub received_at: DateTime<Utc>,
    pub rbn_fields: Option<RbnFields>,
}

// ---------------------------------------------------------------------------
// Aggregator
// ---------------------------------------------------------------------------

/// The core aggregator that processes observations and emits events.
///
/// Single-threaded — designed to run in one tokio task. All inputs arrive
/// via method calls; no internal locking needed.
pub struct Aggregator {
    spot_table: SpotTable,
    filter: FilterConfig,
    skimmer_engine: Option<SkimmerQualityEngine>,
    config: AggregatorConfig,
}

impl Aggregator {
    pub fn new(
        filter: FilterConfig,
        skimmer_config: Option<SkimmerQualityConfig>,
        config: AggregatorConfig,
    ) -> Self {
        let spot_table = SpotTable::new(SpotTableConfig {
            ttl: config.spot_ttl,
            max_observations_per_spot: config.max_observations_per_spot,
        });

        let skimmer_engine = skimmer_config.map(SkimmerQualityEngine::new);

        Self {
            spot_table,
            filter,
            skimmer_engine,
            config,
        }
    }

    /// Process an incoming observation through the full pipeline.
    ///
    /// Returns a `DxEvent` if the spot should be emitted, or `None` if
    /// suppressed by filtering, gating, or dedup.
    pub fn process_observation(&mut self, obs: IncomingObservation) -> Option<DxEvent> {
        let now = obs.received_at;

        // 1. Normalize callsigns
        let dx_call_norm = normalize_callsign(&obs.parsed.dx_call, &self.config.callsign_norm);
        let spotter_call_norm =
            normalize_callsign(&obs.parsed.spotter_call, &self.config.callsign_norm);

        // 2. Compute band, mode, freq bucket
        let freq_hz = obs.parsed.freq_hz;
        let band = freq_to_band(freq_hz);
        let mode = resolve_mode(freq_hz, None, self.config.mode_policy);
        let bucket = freq_bucket(
            freq_hz,
            mode,
            self.config.freq_bucket.cw_bucket_hz,
            self.config.freq_bucket.ssb_bucket_hz,
            self.config.freq_bucket.dig_bucket_hz,
        );

        // 3. Build SpotKey
        let key = SpotKey {
            dx_call_norm: dx_call_norm.clone(),
            band,
            mode,
            freq_bucket_hz: bucket,
        };

        // Extract RBN fields
        let rbn = obs.rbn_fields.unwrap_or_default();

        // 4. Build observation and ingest into spot table
        let observation = SpotObservation {
            source: obs.source_id.clone(),
            originator: OriginatorId(spotter_call_norm.clone()),
            originator_kind: obs.originator_kind,
            snr_db: rbn.snr_db,
            wpm: rbn.wpm,
            obs_time: now,
            #[cfg(feature = "raw-lines")]
            raw: None,
        };

        let input = IngestInput {
            key: key.clone(),
            dx_call: obs.parsed.dx_call.clone(),
            freq_hz,
            band,
            mode,
            comment: obs.parsed.comment.clone(),
            observation,
        };

        let table_result = self.spot_table.ingest(input);

        // 5. Compute skimmer quality tag (if enabled and skimmer)
        let skim_tag = if let Some(engine) = &mut self.skimmer_engine {
            if obs.originator_kind == OriginatorKind::Skimmer {
                engine.record_observation(&dx_call_norm, freq_hz, &spotter_call_norm, now);
                let tag = engine.compute_tag(&dx_call_norm, freq_hz, now);

                // Update spot state with quality tag
                if let Some(state) = self.spot_table.get_mut(&key) {
                    state.spot.skim_quality = Some(tag);
                }

                Some(tag)
            } else {
                None
            }
        } else {
            None
        };

        // 6. Apply skimmer gating
        if let Some(engine) = &self.skimmer_engine {
            let tag = skim_tag.unwrap_or(SkimQualityTag::Unknown);
            if !engine.should_emit(tag, obs.originator_kind) {
                return None;
            }
        }

        // 7. Build SpotView and apply general filter
        let state = self.spot_table.get(&key)?;
        let view = self.build_spot_view(state, &spotter_call_norm, &obs.source_id.0, obs.originator_kind, &rbn, now);

        if let FilterDecision::Drop(_) = evaluate(&view, &self.filter) {
            return None;
        }

        // 8. Decide emission: New vs Update vs suppress
        self.decide_emission(&key, &table_result)
    }

    /// Periodic maintenance: evict expired spots and return Withdraw events.
    pub fn tick(&mut self, now: DateTime<Utc>) -> Vec<DxEvent> {
        let mut events = Vec::new();

        // Evict expired spots (Fix #7: emit Withdraw events)
        let expired_keys = self.spot_table.evict_expired(now);
        for key in expired_keys {
            // We can't access the spot data anymore (it was evicted), so build
            // a minimal Withdraw event from the key.
            let spot = DxSpot {
                spot_key: key,
                dx_call: String::new(),
                freq_hz: 0,
                band: Band::Unknown,
                mode: DxMode::Unknown,
                first_seen: now,
                last_seen: now,
                spot_time: None,
                comment: None,
                observations: vec![],
                unique_originators: 0,
                unique_sources: 0,
                skim_quality: None,
                quality_detail: None,
                confidence: SpotConfidence::default(),
            };

            events.push(DxEvent::Spot(DxSpotEvent {
                spot,
                kind: SpotEventKind::Withdraw,
                revision: 0,
            }));
        }

        // Evict expired skimmer observations
        if let Some(engine) = &mut self.skimmer_engine {
            engine.evict_expired(now);
        }

        events
    }

    /// Get a reference to the spot table (for inspection/testing).
    pub fn spot_table(&self) -> &SpotTable {
        &self.spot_table
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn build_spot_view<'a>(
        &self,
        state: &'a super::spot_table::SpotState,
        spotter_call_norm: &'a str,
        source_id: &'a str,
        originator_kind: OriginatorKind,
        rbn: &RbnFields,
        now: DateTime<Utc>,
    ) -> SpotView<'a> {
        SpotView {
            now,
            last_seen: state.spot.last_seen,
            freq_hz: state.spot.freq_hz,
            band: state.spot.band,
            mode: state.spot.mode,
            dx_call_norm: &state.spot.spot_key.dx_call_norm,
            comment: state.spot.comment.as_deref(),
            unique_originators: state.originator_set.len().min(255) as u8,
            unique_human_originators: state.human_originator_set.len().min(255) as u8,
            unique_sources: state.source_set.len().min(255) as u8,
            total_observations_in_window: state.spot.observations.len().min(255) as u8,
            repeats_same_key_in_window: state.spot.observations.len().min(255) as u8,
            spotter_call_norm: Some(spotter_call_norm),
            spotter_source_id: Some(source_id),
            originator_kind,
            snr_db: rbn.snr_db,
            wpm: rbn.wpm,
            is_skimmer_dupe: false,
            is_skimmer_cq: rbn.is_cq,
            dx_geo: GeoResolved::default(),
            spotter_geo: GeoResolved::default(),
            lotw: None,
            in_master_db: None,
            in_callbook: None,
            memberships: None,
        }
    }

    fn decide_emission(
        &mut self,
        key: &SpotKey,
        table_result: &SpotTableResult,
    ) -> Option<DxEvent> {
        let emit_updates = self.config.dedupe.emit_updates;
        let state = self.spot_table.get_mut(key)?;

        match table_result {
            SpotTableResult::NewSpot(_) => {
                state.first_emitted = true;
                state.last_emitted_revision = state.revision;

                Some(DxEvent::Spot(DxSpotEvent {
                    spot: state.spot.clone(),
                    kind: SpotEventKind::New,
                    revision: state.revision,
                }))
            }
            SpotTableResult::UpdatedSpot(_) => {
                if !state.first_emitted {
                    // Was filtered on all previous attempts; now passes — emit New
                    state.first_emitted = true;
                    state.last_emitted_revision = state.revision;

                    return Some(DxEvent::Spot(DxSpotEvent {
                        spot: state.spot.clone(),
                        kind: SpotEventKind::New,
                        revision: state.revision,
                    }));
                }

                if !emit_updates {
                    return None;
                }

                // Meaningful change: any new observation since last emission
                if state.revision > state.last_emitted_revision {
                    state.last_emitted_revision = state.revision;

                    Some(DxEvent::Spot(DxSpotEvent {
                        spot: state.spot.clone(),
                        kind: SpotEventKind::Update,
                        revision: state.revision,
                    }))
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Band, DxMode, OriginatorKind};
    use crate::filter::config::FilterConfigSerde;
    use crate::model::{SourceId, SpotEventKind};
    use crate::parser::spot::ParsedSpot;

    fn default_filter() -> FilterConfig {
        FilterConfigSerde::default().validate_and_compile().unwrap()
    }

    fn make_obs(
        dx_call: &str,
        spotter: &str,
        freq_hz: u64,
        source: &str,
        kind: OriginatorKind,
    ) -> IncomingObservation {
        IncomingObservation {
            parsed: ParsedSpot {
                spotter_call: spotter.into(),
                dx_call: dx_call.into(),
                freq_hz,
                comment: None,
                utc_time: None,
            },
            source_id: SourceId(source.into()),
            originator_kind: kind,
            received_at: Utc::now(),
            rbn_fields: None,
        }
    }

    fn make_obs_at(
        dx_call: &str,
        spotter: &str,
        freq_hz: u64,
        source: &str,
        kind: OriginatorKind,
        time: DateTime<Utc>,
    ) -> IncomingObservation {
        IncomingObservation {
            parsed: ParsedSpot {
                spotter_call: spotter.into(),
                dx_call: dx_call.into(),
                freq_hz,
                comment: None,
                utc_time: None,
            },
            source_id: SourceId(source.into()),
            originator_kind: kind,
            received_at: time,
            rbn_fields: None,
        }
    }

    fn default_aggregator() -> Aggregator {
        Aggregator::new(default_filter(), None, AggregatorConfig::default())
    }

    // -----------------------------------------------------------------------
    // Basic emission
    // -----------------------------------------------------------------------

    #[test]
    fn single_observation_emits_new() {
        let mut agg = default_aggregator();
        let obs = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);

        let event = agg.process_observation(obs);
        assert!(event.is_some());

        if let Some(DxEvent::Spot(e)) = event {
            assert_eq!(e.kind, SpotEventKind::New);
            assert_eq!(e.spot.dx_call, "JA1ABC");
            assert_eq!(e.revision, 1);
        } else {
            panic!("expected Spot event");
        }
    }

    #[test]
    fn duplicate_without_emit_updates_suppressed() {
        let mut agg = default_aggregator();
        // emit_updates is false by default

        let obs1 = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);
        let obs2 = make_obs("JA1ABC", "VE3NEA", 14_025_005, "src1", OriginatorKind::Human);

        let e1 = agg.process_observation(obs1);
        assert!(e1.is_some());

        let e2 = agg.process_observation(obs2);
        assert!(e2.is_none()); // suppressed
    }

    #[test]
    fn duplicate_with_emit_updates_emits_update() {
        let mut config = AggregatorConfig::default();
        config.dedupe.emit_updates = true;

        let mut agg = Aggregator::new(default_filter(), None, config);

        let obs1 = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);
        let obs2 = make_obs("JA1ABC", "VE3NEA", 14_025_005, "src1", OriginatorKind::Human);

        agg.process_observation(obs1);
        let e2 = agg.process_observation(obs2);

        if let Some(DxEvent::Spot(e)) = e2 {
            assert_eq!(e.kind, SpotEventKind::Update);
            assert_eq!(e.revision, 2);
        } else {
            panic!("expected Update event");
        }
    }

    // -----------------------------------------------------------------------
    // Filter integration
    // -----------------------------------------------------------------------

    #[test]
    fn observation_filtered_by_band_deny() {
        let mut filter_cfg = FilterConfigSerde::default();
        filter_cfg.rf.band_deny.insert(Band::B20);
        let filter = filter_cfg.validate_and_compile().unwrap();

        let mut agg = Aggregator::new(filter, None, AggregatorConfig::default());

        let obs = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);
        let event = agg.process_observation(obs);
        assert!(event.is_none());
    }

    // -----------------------------------------------------------------------
    // Skimmer gating
    // -----------------------------------------------------------------------

    #[test]
    fn skimmer_gating_blocks_unknown() {
        let skim_cfg = SkimmerQualityConfig::default(); // gate enabled, allow_unknown = false

        let mut agg = Aggregator::new(
            default_filter(),
            Some(skim_cfg),
            AggregatorConfig::default(),
        );

        // Single skimmer report → SkimUnknown → blocked
        let obs = make_obs("JA1ABC", "W3LPL-2", 14_025_000, "rbn", OriginatorKind::Skimmer);
        let event = agg.process_observation(obs);
        assert!(event.is_none());
    }

    #[test]
    fn skimmer_becomes_valid_emits_new() {
        let skim_cfg = SkimmerQualityConfig::default();

        let mut agg = Aggregator::new(
            default_filter(),
            Some(skim_cfg),
            AggregatorConfig::default(),
        );

        let now = Utc::now();

        // Three distinct skimmers → becomes SkimValid → should emit
        let obs1 = make_obs_at("JA1ABC", "W3LPL-2", 14_025_000, "rbn", OriginatorKind::Skimmer, now);
        let obs2 = make_obs_at("JA1ABC", "DK8JP-1", 14_025_050, "rbn", OriginatorKind::Skimmer, now);
        let obs3 = make_obs_at("JA1ABC", "VE3NEA-3", 14_025_100, "rbn", OriginatorKind::Skimmer, now);

        assert!(agg.process_observation(obs1).is_none()); // Unknown → blocked
        assert!(agg.process_observation(obs2).is_none()); // still Unknown

        let e3 = agg.process_observation(obs3);
        assert!(e3.is_some()); // Now Valid → emits

        if let Some(DxEvent::Spot(e)) = e3 {
            assert_eq!(e.kind, SpotEventKind::New);
        } else {
            panic!("expected Spot New event");
        }
    }

    #[test]
    fn human_not_blocked_by_skimmer_gating() {
        let skim_cfg = SkimmerQualityConfig::default();

        let mut agg = Aggregator::new(
            default_filter(),
            Some(skim_cfg),
            AggregatorConfig::default(),
        );

        let obs = make_obs("JA1ABC", "W1AW", 14_025_000, "cluster1", OriginatorKind::Human);
        let event = agg.process_observation(obs);
        assert!(event.is_some()); // Humans always pass gating
    }

    // -----------------------------------------------------------------------
    // tick() and TTL eviction
    // -----------------------------------------------------------------------

    #[test]
    fn tick_evicts_expired_spots() {
        let mut config = AggregatorConfig::default();
        config.spot_ttl = Duration::from_secs(60);
        let mut agg = Aggregator::new(default_filter(), None, config);

        let now = Utc::now();
        let old = now - chrono::Duration::seconds(120);

        let obs = make_obs_at("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human, old);
        agg.process_observation(obs);
        assert_eq!(agg.spot_table().len(), 1);

        let events = agg.tick(now);
        assert_eq!(agg.spot_table().len(), 0);
        assert_eq!(events.len(), 1);

        if let DxEvent::Spot(e) = &events[0] {
            assert_eq!(e.kind, SpotEventKind::Withdraw);
        } else {
            panic!("expected Withdraw event");
        }
    }

    // -----------------------------------------------------------------------
    // Normalization
    // -----------------------------------------------------------------------

    #[test]
    fn callsign_normalized_in_spot_key() {
        let mut agg = default_aggregator();

        let obs = make_obs("ja1abc", "w1aw", 14_025_000, "src1", OriginatorKind::Human);
        let event = agg.process_observation(obs);

        if let Some(DxEvent::Spot(e)) = event {
            assert_eq!(e.spot.spot_key.dx_call_norm, "JA1ABC");
        } else {
            panic!("expected Spot event");
        }
    }

    #[test]
    fn band_and_mode_inferred_from_frequency() {
        let mut agg = default_aggregator();

        let obs = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);
        let event = agg.process_observation(obs);

        if let Some(DxEvent::Spot(e)) = event {
            assert_eq!(e.spot.band, Band::B20);
            assert_eq!(e.spot.mode, DxMode::CW);
        } else {
            panic!("expected Spot event");
        }
    }

    // -----------------------------------------------------------------------
    // Revision tracking
    // -----------------------------------------------------------------------

    #[test]
    fn revision_increments_across_observations() {
        let mut config = AggregatorConfig::default();
        config.dedupe.emit_updates = true;
        let mut agg = Aggregator::new(default_filter(), None, config);

        let obs1 = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);
        let obs2 = make_obs("JA1ABC", "VE3NEA", 14_025_005, "src1", OriginatorKind::Human);
        let obs3 = make_obs("JA1ABC", "DL1ABC", 14_025_003, "src1", OriginatorKind::Human);

        let e1 = agg.process_observation(obs1).unwrap();
        let e2 = agg.process_observation(obs2).unwrap();
        let e3 = agg.process_observation(obs3).unwrap();

        if let (DxEvent::Spot(s1), DxEvent::Spot(s2), DxEvent::Spot(s3)) = (e1, e2, e3) {
            assert_eq!(s1.revision, 1);
            assert_eq!(s2.revision, 2);
            assert_eq!(s3.revision, 3);
        } else {
            panic!("expected Spot events");
        }
    }
}
