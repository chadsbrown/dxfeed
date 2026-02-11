//! Aggregator core: wires spot table, skimmer quality, and filter evaluation
//! into a single processing pipeline. Synchronous state machine — no async here.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::callsign::normalize_callsign;
use crate::domain::{ModePolicy, OriginatorKind};
use crate::filter::compiled::FilterConfig;
use crate::filter::config::CallsignNormalizationSerde;
use crate::filter::decision::FilterDecision;
use crate::filter::evaluate::evaluate;
use crate::freq::{freq_bucket, freq_to_band, resolve_mode};
use crate::model::{
    DxEvent, DxSpotEvent, GeoResolved, OriginatorId, SkimQualityTag, SourceId, SpotEventKind,
    SpotKey, SpotObservation, SpotView,
};
use crate::parser::spot::{ParsedSpot, SkimmerFields};
use crate::resolver::enrichment::EnrichmentResolver;
use crate::resolver::entity::EntityResolver;
use crate::skimmer::config::SkimmerQualityConfig;
use crate::skimmer::quality::SkimmerQualityEngine;

use super::spot_table::{CallBandModeKey, IngestInput, SpotTable, SpotTableConfig, SpotTableResult};

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
#[derive(Debug, Clone)]
pub struct DedupeConfig {
    /// Emit Update events when a spot changes meaningfully.
    pub emit_updates: bool,
    /// Minimum time between Update emissions for the same spot.
    /// When set, Update events for a spot are suppressed until this
    /// duration has elapsed since the last emission. `None` disables
    /// the cooldown (every qualifying Update is emitted immediately).
    pub min_update_interval: Option<Duration>,
    /// When true (default), detect QSY: if a station already has an emitted
    /// spot on this band/mode at a different frequency bucket, withdraw the
    /// old spot before emitting the new one.
    pub detect_qsy: bool,
}

impl Default for DedupeConfig {
    fn default() -> Self {
        Self {
            emit_updates: false,
            min_update_interval: None,
            detect_qsy: true,
        }
    }
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
    /// Hard cap on total number of spots in the table.
    pub max_spots: usize,
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
            max_spots: 50_000,
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
    pub skimmer_fields: Option<SkimmerFields>,
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
    entity_resolver: Option<Box<dyn EntityResolver>>,
    enrichment_resolver: Option<Box<dyn EnrichmentResolver>>,
    config: AggregatorConfig,
}

impl Aggregator {
    pub fn new(
        filter: FilterConfig,
        skimmer_config: Option<SkimmerQualityConfig>,
        config: AggregatorConfig,
        entity_resolver: Option<Box<dyn EntityResolver>>,
        enrichment_resolver: Option<Box<dyn EnrichmentResolver>>,
    ) -> Self {
        let spot_table = SpotTable::new(SpotTableConfig {
            ttl: config.spot_ttl,
            max_observations_per_spot: config.max_observations_per_spot,
            max_spots: config.max_spots,
        });

        let skimmer_engine = skimmer_config.map(SkimmerQualityEngine::new);

        Self {
            spot_table,
            filter,
            skimmer_engine,
            entity_resolver,
            enrichment_resolver,
            config,
        }
    }

    /// Process an incoming observation through the full pipeline.
    ///
    /// Returns a `Vec<DxEvent>` — usually 0 or 1 events, but may return
    /// `[Withdraw, New]` when QSY is detected (station changed frequency
    /// bucket within the same band/mode).
    pub fn process_observation(&mut self, obs: IncomingObservation) -> Vec<DxEvent> {
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

        // Extract skimmer fields
        let skim = obs.skimmer_fields.unwrap_or_default();

        // 4. Build observation and ingest into spot table
        let observation = SpotObservation {
            source: obs.source_id.clone(),
            originator: OriginatorId(spotter_call_norm.clone()),
            originator_kind: obs.originator_kind,
            snr_db: skim.snr_db,
            wpm: skim.wpm,
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

        // 5. Resolve entity info (if resolver is available)
        if let Some(resolver) = &self.entity_resolver {
            if let Some(state) = self.spot_table.get_mut(&key) {
                if state.dx_entity.is_none() {
                    state.dx_entity = resolver.resolve(&dx_call_norm);
                }
                state.spotter_entity = resolver.resolve(&spotter_call_norm);
            }
        }

        // 5b. Resolve enrichment data (if resolver is available)
        if let Some(resolver) = &self.enrichment_resolver {
            if let Some(state) = self.spot_table.get_mut(&key) {
                if state.lotw.is_none() {
                    state.lotw = resolver.lotw_user(&dx_call_norm);
                    state.in_master_db = resolver.in_master_db(&dx_call_norm);
                    state.in_callbook = resolver.in_callbook(&dx_call_norm);
                    state.memberships = resolver.memberships(&dx_call_norm);
                }
            }
        }

        // 6. Compute skimmer quality tag (if enabled and skimmer)
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
                return vec![];
            }
        }

        // 7. Build SpotView and apply general filter
        let state = match self.spot_table.get(&key) {
            Some(s) => s,
            None => return vec![],
        };
        let view = self.build_spot_view(state, &spotter_call_norm, &obs.source_id.0, obs.originator_kind, &skim, now);

        if let FilterDecision::Drop(_) = evaluate(&view, &self.filter) {
            return vec![];
        }

        // 8. Decide emission: New vs Update vs suppress
        let base_event = self.decide_emission(&key, &table_result, now);
        // -- mutable borrow from decide_emission is released here --

        // 9. QSY detection: if a New spot was emitted and we already have an
        //    emitted spot for this (call, band, mode) at a *different* freq
        //    bucket, withdraw the old spot first.
        if self.config.dedupe.detect_qsy {
            if let Some(DxEvent::Spot(ref e)) = base_event {
                if e.kind == SpotEventKind::New {
                    let cbm = CallBandModeKey::from(&key);
                    if let Some(old_key) = self.spot_table.lookup_emitted(&cbm).cloned() {
                        if old_key != key {
                            // QSY: withdraw old, emit new
                            let mut events = Vec::with_capacity(2);
                            if let Some((old_spot, old_rev)) = self.spot_table.remove_spot(&old_key) {
                                events.push(DxEvent::Spot(DxSpotEvent {
                                    spot: old_spot,
                                    kind: SpotEventKind::Withdraw,
                                    revision: old_rev,
                                }));
                            }
                            self.spot_table.register_emission(&key);
                            events.push(base_event.unwrap());
                            return events;
                        }
                    }
                    self.spot_table.register_emission(&key);
                }
            }
        }

        base_event.into_iter().collect()
    }

    /// Periodic maintenance: evict expired spots and return Withdraw events.
    pub fn tick(&mut self, now: DateTime<Utc>) -> Vec<DxEvent> {
        let mut events = Vec::new();

        // Evict expired spots and emit Withdraw events with full spot data
        let expired = self.spot_table.evict_expired(now);
        for (spot, revision) in expired {
            events.push(DxEvent::Spot(DxSpotEvent {
                spot,
                kind: SpotEventKind::Withdraw,
                revision,
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

    /// Replace the active filter with a new compiled filter config.
    pub fn update_filter(&mut self, filter: FilterConfig) {
        self.filter = filter;
    }

    /// Replace the aggregator configuration at runtime.
    ///
    /// Cascades relevant settings to the spot table (TTL, max_spots,
    /// max_observations_per_spot).
    ///
    /// **Warning:** Changing `freq_bucket`, `callsign_norm`, or `mode_policy`
    /// at runtime will cause new observations to hash to different `SpotKey`s
    /// than existing spots. The consumer is responsible for understanding this
    /// effect.
    pub fn update_aggregator_config(&mut self, config: AggregatorConfig) {
        let table_config = SpotTableConfig {
            ttl: config.spot_ttl,
            max_observations_per_spot: config.max_observations_per_spot,
            max_spots: config.max_spots,
        };
        self.spot_table.update_config(table_config);
        self.config = config;
    }

    /// Replace or toggle the skimmer quality engine at runtime.
    ///
    /// - `Some(cfg)` with an existing engine: updates the engine's config.
    /// - `Some(cfg)` with no engine and `cfg.enabled`: creates a new engine.
    /// - `None`: disables the engine entirely.
    pub fn update_skimmer_config(&mut self, config: Option<SkimmerQualityConfig>) {
        match (&mut self.skimmer_engine, config) {
            (Some(engine), Some(cfg)) => engine.update_config(cfg),
            (None, Some(cfg)) if cfg.enabled => {
                self.skimmer_engine = Some(SkimmerQualityEngine::new(cfg));
            }
            (Some(_), None) => self.skimmer_engine = None,
            _ => {}
        }
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
        skim: &SkimmerFields,
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
            snr_db: skim.snr_db,
            wpm: skim.wpm,
            is_skimmer_dupe: false,
            is_skimmer_cq: skim.is_cq,
            dx_geo: match &state.dx_entity {
                Some(info) => GeoResolved {
                    continent: Some(info.continent),
                    cq_zone: Some(info.cq_zone),
                    itu_zone: Some(info.itu_zone),
                    entity: Some(&info.entity_name),
                    country: None,
                    state: None,
                    grid: None,
                },
                None => GeoResolved::default(),
            },
            spotter_geo: match &state.spotter_entity {
                Some(info) => GeoResolved {
                    continent: Some(info.continent),
                    cq_zone: Some(info.cq_zone),
                    itu_zone: Some(info.itu_zone),
                    entity: Some(&info.entity_name),
                    country: None,
                    state: None,
                    grid: None,
                },
                None => GeoResolved::default(),
            },
            lotw: state.lotw,
            in_master_db: state.in_master_db,
            in_callbook: state.in_callbook,
            memberships: state.memberships.as_ref(),
        }
    }

    fn decide_emission(
        &mut self,
        key: &SpotKey,
        table_result: &SpotTableResult,
        now: DateTime<Utc>,
    ) -> Option<DxEvent> {
        let emit_updates = self.config.dedupe.emit_updates;
        let min_update_interval = self.config.dedupe.min_update_interval;
        let state = self.spot_table.get_mut(key)?;

        match table_result {
            SpotTableResult::NewSpot(_) => {
                state.first_emitted = true;
                state.last_emitted_revision = state.revision;
                state.last_emitted_at = Some(now);

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
                    state.last_emitted_at = Some(now);

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
                    // Enforce per-spot update cooldown
                    if let Some(interval) = min_update_interval {
                        if let Some(last_at) = state.last_emitted_at {
                            let elapsed = now.signed_duration_since(last_at);
                            if elapsed < chrono::Duration::from_std(interval).unwrap_or(chrono::Duration::MAX) {
                                return None;
                            }
                        }
                    }

                    state.last_emitted_revision = state.revision;
                    state.last_emitted_at = Some(now);

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
            skimmer_fields: None,
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
            skimmer_fields: None,
        }
    }

    fn default_aggregator() -> Aggregator {
        Aggregator::new(default_filter(), None, AggregatorConfig::default(), None, None)
    }

    // -----------------------------------------------------------------------
    // Basic emission
    // -----------------------------------------------------------------------

    #[test]
    fn single_observation_emits_new() {
        let mut agg = default_aggregator();
        let obs = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);

        let events = agg.process_observation(obs);
        assert_eq!(events.len(), 1);

        if let DxEvent::Spot(e) = &events[0] {
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
        assert_eq!(e1.len(), 1);

        let e2 = agg.process_observation(obs2);
        assert!(e2.is_empty()); // suppressed
    }

    #[test]
    fn duplicate_with_emit_updates_emits_update() {
        let mut config = AggregatorConfig::default();
        config.dedupe.emit_updates = true;

        let mut agg = Aggregator::new(default_filter(), None, config, None, None);

        let obs1 = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);
        let obs2 = make_obs("JA1ABC", "VE3NEA", 14_025_005, "src1", OriginatorKind::Human);

        agg.process_observation(obs1);
        let e2 = agg.process_observation(obs2);
        assert_eq!(e2.len(), 1);

        if let DxEvent::Spot(e) = &e2[0] {
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

        let mut agg = Aggregator::new(filter, None, AggregatorConfig::default(), None, None);

        let obs = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);
        let events = agg.process_observation(obs);
        assert!(events.is_empty());
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
            None,
            None,
        );

        // Single skimmer report → SkimUnknown → blocked
        let obs = make_obs("JA1ABC", "W3LPL-2", 14_025_000, "rbn", OriginatorKind::Skimmer);
        let events = agg.process_observation(obs);
        assert!(events.is_empty());
    }

    #[test]
    fn skimmer_becomes_valid_emits_new() {
        let skim_cfg = SkimmerQualityConfig::default();

        let mut agg = Aggregator::new(
            default_filter(),
            Some(skim_cfg),
            AggregatorConfig::default(),
            None,
            None,
        );

        let now = Utc::now();

        // Three distinct skimmers → becomes SkimValid → should emit
        let obs1 = make_obs_at("JA1ABC", "W3LPL-2", 14_025_000, "rbn", OriginatorKind::Skimmer, now);
        let obs2 = make_obs_at("JA1ABC", "DK8JP-1", 14_025_050, "rbn", OriginatorKind::Skimmer, now);
        let obs3 = make_obs_at("JA1ABC", "VE3NEA-3", 14_025_100, "rbn", OriginatorKind::Skimmer, now);

        assert!(agg.process_observation(obs1).is_empty()); // Unknown → blocked
        assert!(agg.process_observation(obs2).is_empty()); // still Unknown

        let e3 = agg.process_observation(obs3);
        assert_eq!(e3.len(), 1); // Now Valid → emits

        if let DxEvent::Spot(e) = &e3[0] {
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
            None,
            None,
        );

        let obs = make_obs("JA1ABC", "W1AW", 14_025_000, "cluster1", OriginatorKind::Human);
        let events = agg.process_observation(obs);
        assert_eq!(events.len(), 1); // Humans always pass gating
    }

    // -----------------------------------------------------------------------
    // tick() and TTL eviction
    // -----------------------------------------------------------------------

    #[test]
    fn tick_evicts_expired_spots() {
        let mut config = AggregatorConfig::default();
        config.spot_ttl = Duration::from_secs(60);
        let mut agg = Aggregator::new(default_filter(), None, config, None, None);

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
        let events = agg.process_observation(obs);
        assert_eq!(events.len(), 1);

        if let DxEvent::Spot(e) = &events[0] {
            assert_eq!(e.spot.spot_key.dx_call_norm, "JA1ABC");
        } else {
            panic!("expected Spot event");
        }
    }

    #[test]
    fn band_and_mode_inferred_from_frequency() {
        let mut agg = default_aggregator();

        let obs = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);
        let events = agg.process_observation(obs);
        assert_eq!(events.len(), 1);

        if let DxEvent::Spot(e) = &events[0] {
            assert_eq!(e.spot.band, Band::B20);
            assert_eq!(e.spot.mode, DxMode::CW);
        } else {
            panic!("expected Spot event");
        }
    }

    // -----------------------------------------------------------------------
    // Revision tracking
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // update_aggregator_config / update_skimmer_config
    // -----------------------------------------------------------------------

    #[test]
    fn update_aggregator_config_cascades_to_table() {
        let mut agg = default_aggregator();
        let now = Utc::now();

        // Insert a spot with default TTL (900s)
        let old = now - chrono::Duration::seconds(120);
        let obs = make_obs_at("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human, old);
        agg.process_observation(obs);
        assert_eq!(agg.spot_table().len(), 1);

        // Reduce TTL to 60s — the spot is 120s old, so tick should evict it
        let mut config = AggregatorConfig::default();
        config.spot_ttl = Duration::from_secs(60);
        agg.update_aggregator_config(config);

        let events = agg.tick(now);
        assert_eq!(agg.spot_table().len(), 0);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn update_skimmer_config_changes_gating() {
        let skim_cfg = SkimmerQualityConfig::default();
        let mut agg = Aggregator::new(
            default_filter(),
            Some(skim_cfg),
            AggregatorConfig::default(),
            None,
            None,
        );

        // Single skimmer report → Unknown → blocked
        let obs = make_obs("JA1ABC", "W3LPL-2", 14_025_000, "rbn", OriginatorKind::Skimmer);
        assert!(agg.process_observation(obs).is_empty());

        // Update config to allow_unknown = true
        let mut new_cfg = SkimmerQualityConfig::default();
        new_cfg.allow_unknown = true;
        agg.update_skimmer_config(Some(new_cfg));

        // Now a single skimmer report should pass gating
        let obs2 = make_obs("DL1ABC", "DK8JP-1", 21_025_000, "rbn", OriginatorKind::Skimmer);
        assert!(!agg.process_observation(obs2).is_empty());
    }

    #[test]
    fn update_skimmer_config_creates_engine() {
        // Start with no skimmer engine
        let mut agg = default_aggregator();

        // Enabling skimmer config should create the engine
        let skim_cfg = SkimmerQualityConfig::default();
        agg.update_skimmer_config(Some(skim_cfg));

        // Now skimmer gating should block unknown spots
        let obs = make_obs("JA1ABC", "W3LPL-2", 14_025_000, "rbn", OriginatorKind::Skimmer);
        assert!(agg.process_observation(obs).is_empty());
    }

    #[test]
    fn update_skimmer_config_disables_engine() {
        let skim_cfg = SkimmerQualityConfig::default();
        let mut agg = Aggregator::new(
            default_filter(),
            Some(skim_cfg),
            AggregatorConfig::default(),
            None,
            None,
        );

        // Disable the engine
        agg.update_skimmer_config(None);

        // Now skimmer spots should pass through (no gating)
        let obs = make_obs("JA1ABC", "W3LPL-2", 14_025_000, "rbn", OriginatorKind::Skimmer);
        assert!(!agg.process_observation(obs).is_empty());
    }

    // -----------------------------------------------------------------------
    // Revision tracking
    // -----------------------------------------------------------------------

    #[test]
    fn revision_increments_across_observations() {
        let mut config = AggregatorConfig::default();
        config.dedupe.emit_updates = true;
        let mut agg = Aggregator::new(default_filter(), None, config, None, None);

        let obs1 = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);
        let obs2 = make_obs("JA1ABC", "VE3NEA", 14_025_005, "src1", OriginatorKind::Human);
        let obs3 = make_obs("JA1ABC", "DL1ABC", 14_025_003, "src1", OriginatorKind::Human);

        let e1 = agg.process_observation(obs1).into_iter().next().unwrap();
        let e2 = agg.process_observation(obs2).into_iter().next().unwrap();
        let e3 = agg.process_observation(obs3).into_iter().next().unwrap();

        if let (DxEvent::Spot(s1), DxEvent::Spot(s2), DxEvent::Spot(s3)) = (e1, e2, e3) {
            assert_eq!(s1.revision, 1);
            assert_eq!(s2.revision, 2);
            assert_eq!(s3.revision, 3);
        } else {
            panic!("expected Spot events");
        }
    }

    // -----------------------------------------------------------------------
    // Update cooldown
    // -----------------------------------------------------------------------

    #[test]
    fn update_cooldown_suppresses_rapid_updates() {
        let mut config = AggregatorConfig::default();
        config.dedupe.emit_updates = true;
        config.dedupe.min_update_interval = Some(Duration::from_secs(30));
        let mut agg = Aggregator::new(default_filter(), None, config, None, None);

        let t0 = Utc::now();
        let t10 = t0 + chrono::Duration::seconds(10);
        let t35 = t0 + chrono::Duration::seconds(35);

        // First observation → New (always emitted)
        let obs1 = make_obs_at("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human, t0);
        let e1 = agg.process_observation(obs1);
        assert_eq!(e1.len(), 1);
        assert!(matches!(&e1[0], DxEvent::Spot(e) if e.kind == SpotEventKind::New));

        // Second observation 10s later → within cooldown → suppressed
        let obs2 = make_obs_at("JA1ABC", "VE3NEA", 14_025_005, "src1", OriginatorKind::Human, t10);
        let e2 = agg.process_observation(obs2);
        assert!(e2.is_empty(), "update within cooldown should be suppressed");

        // Third observation 35s after first → cooldown expired → Update emitted
        let obs3 = make_obs_at("JA1ABC", "DL1ABC", 14_025_003, "src1", OriginatorKind::Human, t35);
        let e3 = agg.process_observation(obs3);
        assert_eq!(e3.len(), 1);
        assert!(matches!(&e3[0], DxEvent::Spot(e) if e.kind == SpotEventKind::Update));
    }

    #[test]
    fn update_cooldown_none_disables_throttle() {
        let mut config = AggregatorConfig::default();
        config.dedupe.emit_updates = true;
        config.dedupe.min_update_interval = None;
        let mut agg = Aggregator::new(default_filter(), None, config, None, None);

        let obs1 = make_obs("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human);
        let obs2 = make_obs("JA1ABC", "VE3NEA", 14_025_005, "src1", OriginatorKind::Human);

        agg.process_observation(obs1);
        let e2 = agg.process_observation(obs2);
        assert_eq!(e2.len(), 1);
        assert!(matches!(&e2[0], DxEvent::Spot(e) if e.kind == SpotEventKind::Update));
    }

    #[test]
    fn cooldown_resets_after_emitted_update() {
        let mut config = AggregatorConfig::default();
        config.dedupe.emit_updates = true;
        config.dedupe.min_update_interval = Some(Duration::from_secs(10));
        let mut agg = Aggregator::new(default_filter(), None, config, None, None);

        let t0 = Utc::now();

        // New at t0
        let obs1 = make_obs_at("JA1ABC", "W1AW", 14_025_000, "src1", OriginatorKind::Human, t0);
        agg.process_observation(obs1);

        // Update at t0+15s → emitted (cooldown from t0 expired)
        let t15 = t0 + chrono::Duration::seconds(15);
        let obs2 = make_obs_at("JA1ABC", "VE3NEA", 14_025_005, "src1", OriginatorKind::Human, t15);
        let e2 = agg.process_observation(obs2);
        assert_eq!(e2.len(), 1);
        assert!(matches!(&e2[0], DxEvent::Spot(e) if e.kind == SpotEventKind::Update));

        // Update at t0+20s → suppressed (only 5s since last emission at t15)
        let t20 = t0 + chrono::Duration::seconds(20);
        let obs3 = make_obs_at("JA1ABC", "DL1ABC", 14_025_003, "src1", OriginatorKind::Human, t20);
        let e3 = agg.process_observation(obs3);
        assert!(e3.is_empty(), "should be within cooldown from last Update");

        // Update at t0+26s → emitted (cooldown from t15 expired)
        let t26 = t0 + chrono::Duration::seconds(26);
        let obs4 = make_obs_at("JA1ABC", "K1ABC", 14_025_003, "src1", OriginatorKind::Human, t26);
        let e4 = agg.process_observation(obs4);
        assert_eq!(e4.len(), 1);
        assert!(matches!(&e4[0], DxEvent::Spot(e) if e.kind == SpotEventKind::Update));
    }

    // -----------------------------------------------------------------------
    // QSY detection
    // -----------------------------------------------------------------------

    #[test]
    fn qsy_within_band_emits_withdraw_then_new() {
        let mut agg = default_aggregator();

        // First spot: N9UNX on 7.043 MHz (40m CW)
        let obs1 = make_obs("N9UNX", "W1AW", 7_043_000, "src1", OriginatorKind::Human);
        let e1 = agg.process_observation(obs1);
        assert_eq!(e1.len(), 1);
        assert!(matches!(&e1[0], DxEvent::Spot(e) if e.kind == SpotEventKind::New));

        // QSY: same call, same band/mode, different freq bucket (7.044 MHz)
        let obs2 = make_obs("N9UNX", "VE3NEA", 7_044_000, "src1", OriginatorKind::Human);
        let e2 = agg.process_observation(obs2);
        assert_eq!(e2.len(), 2, "should emit [Withdraw, New]");

        // First event: Withdraw of old spot
        if let DxEvent::Spot(w) = &e2[0] {
            assert_eq!(w.kind, SpotEventKind::Withdraw);
            assert_eq!(w.spot.spot_key.dx_call_norm, "N9UNX");
            assert_eq!(w.spot.freq_hz, 7_043_000);
        } else {
            panic!("expected Withdraw event");
        }

        // Second event: New spot at new frequency
        if let DxEvent::Spot(n) = &e2[1] {
            assert_eq!(n.kind, SpotEventKind::New);
            assert_eq!(n.spot.spot_key.dx_call_norm, "N9UNX");
            assert_eq!(n.spot.freq_hz, 7_044_000);
        } else {
            panic!("expected New event");
        }

        // Old spot should be removed from the table
        assert_eq!(agg.spot_table().len(), 1);
    }

    #[test]
    fn qsy_different_band_no_withdraw() {
        let mut agg = default_aggregator();

        // Spot on 40m CW
        let obs1 = make_obs("N9UNX", "W1AW", 7_043_000, "src1", OriginatorKind::Human);
        let e1 = agg.process_observation(obs1);
        assert_eq!(e1.len(), 1);

        // Different band (20m CW) — NOT a QSY, independent spot
        let obs2 = make_obs("N9UNX", "VE3NEA", 14_025_000, "src1", OriginatorKind::Human);
        let e2 = agg.process_observation(obs2);
        assert_eq!(e2.len(), 1, "different band should just be a New");
        assert!(matches!(&e2[0], DxEvent::Spot(e) if e.kind == SpotEventKind::New));

        // Both spots should remain in the table
        assert_eq!(agg.spot_table().len(), 2);
    }

    #[test]
    fn qsy_disabled_no_withdraw() {
        let mut config = AggregatorConfig::default();
        config.dedupe.detect_qsy = false;
        let mut agg = Aggregator::new(default_filter(), None, config, None, None);

        // First spot
        let obs1 = make_obs("N9UNX", "W1AW", 7_043_000, "src1", OriginatorKind::Human);
        agg.process_observation(obs1);

        // Different freq bucket on same band — should be independent New (no withdraw)
        let obs2 = make_obs("N9UNX", "VE3NEA", 7_044_000, "src1", OriginatorKind::Human);
        let e2 = agg.process_observation(obs2);
        assert_eq!(e2.len(), 1);
        assert!(matches!(&e2[0], DxEvent::Spot(e) if e.kind == SpotEventKind::New));

        // Both spots coexist
        assert_eq!(agg.spot_table().len(), 2);
    }

    #[test]
    fn qsy_only_triggers_after_gating() {
        // With skimmer gating, a single unverified skimmer should NOT trigger QSY
        let skim_cfg = SkimmerQualityConfig::default();
        let mut agg = Aggregator::new(
            default_filter(),
            Some(skim_cfg),
            AggregatorConfig::default(),
            None,
            None,
        );

        let now = Utc::now();

        // First: establish a valid spot via 3 skimmers on 7.043
        let obs1 = make_obs_at("N9UNX", "SKIM1", 7_043_000, "rbn", OriginatorKind::Skimmer, now);
        let obs2 = make_obs_at("N9UNX", "SKIM2", 7_043_050, "rbn", OriginatorKind::Skimmer, now);
        let obs3 = make_obs_at("N9UNX", "SKIM3", 7_043_080, "rbn", OriginatorKind::Skimmer, now);

        assert!(agg.process_observation(obs1).is_empty());
        assert!(agg.process_observation(obs2).is_empty());
        let e3 = agg.process_observation(obs3);
        assert_eq!(e3.len(), 1); // Valid → emits New

        // Now a single skimmer at a different frequency — blocked by gating, no QSY
        let obs4 = make_obs_at("N9UNX", "SKIM4", 7_044_000, "rbn", OriginatorKind::Skimmer, now);
        let e4 = agg.process_observation(obs4);
        assert!(e4.is_empty(), "unverified spot should not trigger QSY");

        // Original spot still in table
        assert_eq!(agg.spot_table().len(), 2); // both freq buckets ingested
    }

    #[test]
    fn qsy_rapid_back_and_forth() {
        let mut agg = default_aggregator();

        // A on freq A
        let obs1 = make_obs("N9UNX", "W1AW", 7_043_000, "src1", OriginatorKind::Human);
        let e1 = agg.process_observation(obs1);
        assert_eq!(e1.len(), 1);

        // A → freq B (QSY)
        let obs2 = make_obs("N9UNX", "VE3NEA", 7_044_000, "src1", OriginatorKind::Human);
        let e2 = agg.process_observation(obs2);
        assert_eq!(e2.len(), 2);
        assert!(matches!(&e2[0], DxEvent::Spot(e) if e.kind == SpotEventKind::Withdraw));
        assert!(matches!(&e2[1], DxEvent::Spot(e) if e.kind == SpotEventKind::New));

        // B → freq A (QSY back)
        let obs3 = make_obs("N9UNX", "DL1ABC", 7_043_000, "src1", OriginatorKind::Human);
        let e3 = agg.process_observation(obs3);
        assert_eq!(e3.len(), 2);
        assert!(matches!(&e3[0], DxEvent::Spot(e) if e.kind == SpotEventKind::Withdraw));
        assert!(matches!(&e3[1], DxEvent::Spot(e) if e.kind == SpotEventKind::New));

        // Only one spot should exist in the table
        assert_eq!(agg.spot_table().len(), 1);
    }

    #[test]
    fn qsy_eviction_cleans_index() {
        let mut config = AggregatorConfig::default();
        config.spot_ttl = Duration::from_secs(60);
        let mut agg = Aggregator::new(default_filter(), None, config, None, None);

        let now = Utc::now();
        let old = now - chrono::Duration::seconds(120);

        // Create an emitted spot (old timestamp)
        let obs = make_obs_at("N9UNX", "W1AW", 7_043_000, "src1", OriginatorKind::Human, old);
        agg.process_observation(obs);

        // Evict it via tick
        let events = agg.tick(now);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], DxEvent::Spot(e) if e.kind == SpotEventKind::Withdraw));

        // Now a new spot at a different freq should NOT produce a QSY withdraw
        // (old spot was already evicted and index cleaned)
        let obs2 = make_obs_at("N9UNX", "VE3NEA", 7_044_000, "src1", OriginatorKind::Human, now);
        let e2 = agg.process_observation(obs2);
        assert_eq!(e2.len(), 1, "should just be New, no QSY withdraw");
        assert!(matches!(&e2[0], DxEvent::Spot(e) if e.kind == SpotEventKind::New));
    }
}
