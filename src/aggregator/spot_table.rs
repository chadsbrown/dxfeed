//! In-memory spot state table for deduplication and correlation.
//!
//! The `SpotTable` is a `HashMap<SpotKey, SpotState>` with TTL eviction,
//! bounded observation history, and incremental originator/source tracking.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::domain::{Band, DxMode, OriginatorKind};
use crate::model::{DxSpot, SpotConfidence, SpotKey, SpotObservation};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the spot table.
pub struct SpotTableConfig {
    /// How long a spot lives before eviction (default: 900s / 15 min).
    pub ttl: Duration,
    /// Maximum observations to keep per spot (default: 20).
    pub max_observations_per_spot: usize,
}

impl Default for SpotTableConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(900),
            max_observations_per_spot: 20,
        }
    }
}

// ---------------------------------------------------------------------------
// SpotState — internal state for one aggregated spot
// ---------------------------------------------------------------------------

/// Internal state for one aggregated spot in the table.
pub struct SpotState {
    pub spot: DxSpot,
    /// Whether this spot has ever been emitted as a New event.
    pub first_emitted: bool,
    /// The revision number at the last emission.
    pub last_emitted_revision: u32,
    /// Current revision (incremented on each new observation).
    pub revision: u32,
    /// Set of unique originator callsigns (all kinds).
    pub originator_set: HashSet<String>,
    /// Set of unique human originator callsigns.
    pub human_originator_set: HashSet<String>,
    /// Set of unique source IDs.
    pub source_set: HashSet<String>,
}

// ---------------------------------------------------------------------------
// SpotTableResult
// ---------------------------------------------------------------------------

/// Result of ingesting an observation into the spot table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpotTableResult {
    /// A brand new spot was created.
    NewSpot(SpotKey),
    /// An existing spot was updated with a new observation.
    UpdatedSpot(SpotKey),
}

// ---------------------------------------------------------------------------
// IngestInput
// ---------------------------------------------------------------------------

/// Bundle of data needed to ingest a new observation.
pub struct IngestInput {
    /// Pre-computed spot key.
    pub key: SpotKey,
    /// Original DX callsign (for display, may differ from key's normalized form).
    pub dx_call: String,
    /// Exact frequency in Hz.
    pub freq_hz: u64,
    pub band: Band,
    pub mode: DxMode,
    pub comment: Option<String>,
    pub observation: SpotObservation,
}

// ---------------------------------------------------------------------------
// SpotTable
// ---------------------------------------------------------------------------

/// In-memory table of aggregated spots, keyed by `SpotKey`.
pub struct SpotTable {
    entries: HashMap<SpotKey, SpotState>,
    config: SpotTableConfig,
}

impl SpotTable {
    pub fn new(config: SpotTableConfig) -> Self {
        Self {
            entries: HashMap::new(),
            config,
        }
    }

    /// Ingest a new observation, creating or updating a spot.
    pub fn ingest(&mut self, input: IngestInput) -> SpotTableResult {
        let obs_time = input.observation.obs_time;

        if let Some(state) = self.entries.get_mut(&input.key) {
            state.revision += 1;
            state.spot.last_seen = obs_time;
            state.spot.freq_hz = input.freq_hz;

            if input.comment.is_some() {
                state.spot.comment = input.comment;
            }

            // Track unique originators and sources
            let orig_call = input.observation.originator.0.clone();
            let orig_kind = input.observation.originator_kind;
            let source_id = input.observation.source.0.clone();

            state.originator_set.insert(orig_call.clone());
            if orig_kind == OriginatorKind::Human {
                state.human_originator_set.insert(orig_call);
            }
            state.source_set.insert(source_id);

            // Update counts on the spot
            state.spot.unique_originators = state.originator_set.len().min(255) as u8;
            state.spot.unique_sources = state.source_set.len().min(255) as u8;

            // Add observation, enforce cap by dropping oldest
            state.spot.observations.push(input.observation);
            if state.spot.observations.len() > self.config.max_observations_per_spot {
                state.spot.observations.remove(0);
            }

            SpotTableResult::UpdatedSpot(input.key)
        } else {
            // New spot
            let orig_call = input.observation.originator.0.clone();
            let orig_kind = input.observation.originator_kind;
            let source_id = input.observation.source.0.clone();

            let mut originator_set = HashSet::new();
            let mut human_originator_set = HashSet::new();
            let mut source_set = HashSet::new();

            originator_set.insert(orig_call.clone());
            if orig_kind == OriginatorKind::Human {
                human_originator_set.insert(orig_call);
            }
            source_set.insert(source_id);

            let observations = vec![input.observation];

            let spot = DxSpot {
                spot_key: input.key.clone(),
                dx_call: input.dx_call,
                freq_hz: input.freq_hz,
                band: input.band,
                mode: input.mode,
                first_seen: obs_time,
                last_seen: obs_time,
                spot_time: None,
                comment: input.comment,
                observations,
                unique_originators: 1,
                unique_sources: 1,
                skim_quality: None,
                quality_detail: None,
                confidence: SpotConfidence::default(),
            };

            let state = SpotState {
                spot,
                first_emitted: false,
                last_emitted_revision: 0,
                revision: 1,
                originator_set,
                human_originator_set,
                source_set,
            };

            let key = input.key.clone();
            self.entries.insert(input.key, state);
            SpotTableResult::NewSpot(key)
        }
    }

    /// Evict all spots whose `last_seen` is older than TTL relative to `now`.
    ///
    /// Returns the keys of evicted spots (for Withdraw event emission).
    pub fn evict_expired(&mut self, now: DateTime<Utc>) -> Vec<SpotKey> {
        let ttl_secs = self.config.ttl.as_secs() as i64;

        let expired: Vec<SpotKey> = self
            .entries
            .iter()
            .filter(|(_, state)| {
                now.signed_duration_since(state.spot.last_seen).num_seconds() > ttl_secs
            })
            .map(|(key, _)| key.clone())
            .collect();

        for key in &expired {
            self.entries.remove(key);
        }

        expired
    }

    pub fn get(&self, key: &SpotKey) -> Option<&SpotState> {
        self.entries.get(key)
    }

    pub fn get_mut(&mut self, key: &SpotKey) -> Option<&mut SpotState> {
        self.entries.get_mut(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DxMode, OriginatorKind};
    use crate::freq::freq_bucket;
    use crate::model::{OriginatorId, SourceId, SpotKey, SpotObservation};

    fn make_key(dx_call: &str, freq_hz: u64, mode: DxMode) -> SpotKey {
        SpotKey {
            dx_call_norm: dx_call.to_uppercase(),
            band: crate::freq::freq_to_band(freq_hz),
            mode,
            freq_bucket_hz: freq_bucket(freq_hz, mode, 10, 1000, 100),
        }
    }

    fn make_obs(
        originator: &str,
        source: &str,
        kind: OriginatorKind,
        time: DateTime<Utc>,
    ) -> SpotObservation {
        SpotObservation {
            source: SourceId(source.into()),
            originator: OriginatorId(originator.into()),
            originator_kind: kind,
            snr_db: None,
            wpm: None,
            obs_time: time,
            #[cfg(feature = "raw-lines")]
            raw: None,
        }
    }

    fn make_input(
        dx_call: &str,
        freq_hz: u64,
        mode: DxMode,
        originator: &str,
        source: &str,
        kind: OriginatorKind,
        time: DateTime<Utc>,
    ) -> IngestInput {
        IngestInput {
            key: make_key(dx_call, freq_hz, mode),
            dx_call: dx_call.into(),
            freq_hz,
            band: crate::freq::freq_to_band(freq_hz),
            mode,
            comment: None,
            observation: make_obs(originator, source, kind, time),
        }
    }

    fn default_table() -> SpotTable {
        SpotTable::new(SpotTableConfig::default())
    }

    // -----------------------------------------------------------------------
    // Basic ingest
    // -----------------------------------------------------------------------

    #[test]
    fn ingest_first_observation_is_new_spot() {
        let mut table = default_table();
        let now = Utc::now();
        let input = make_input("JA1ABC", 14_025_000, DxMode::CW, "W1AW", "src1", OriginatorKind::Human, now);
        let result = table.ingest(input);

        assert!(matches!(result, SpotTableResult::NewSpot(_)));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn ingest_same_key_is_updated_spot() {
        let mut table = default_table();
        let now = Utc::now();

        let input1 = make_input("JA1ABC", 14_025_000, DxMode::CW, "W1AW", "src1", OriginatorKind::Human, now);
        let input2 = make_input("JA1ABC", 14_025_005, DxMode::CW, "VE3NEA", "src1", OriginatorKind::Human, now);

        table.ingest(input1);
        let result = table.ingest(input2);

        assert!(matches!(result, SpotTableResult::UpdatedSpot(_)));
        assert_eq!(table.len(), 1);

        let key = make_key("JA1ABC", 14_025_000, DxMode::CW);
        let state = table.get(&key).unwrap();
        assert_eq!(state.spot.observations.len(), 2);
    }

    #[test]
    fn ingest_different_key_is_second_new_spot() {
        let mut table = default_table();
        let now = Utc::now();

        let input1 = make_input("JA1ABC", 14_025_000, DxMode::CW, "W1AW", "src1", OriginatorKind::Human, now);
        let input2 = make_input("DL1ABC", 21_025_000, DxMode::CW, "W1AW", "src1", OriginatorKind::Human, now);

        let r1 = table.ingest(input1);
        let r2 = table.ingest(input2);

        assert!(matches!(r1, SpotTableResult::NewSpot(_)));
        assert!(matches!(r2, SpotTableResult::NewSpot(_)));
        assert_eq!(table.len(), 2);
    }

    // -----------------------------------------------------------------------
    // TTL eviction
    // -----------------------------------------------------------------------

    #[test]
    fn evict_expired_removes_old_spots() {
        let mut table = SpotTable::new(SpotTableConfig {
            ttl: Duration::from_secs(60),
            ..Default::default()
        });

        let now = Utc::now();
        let old_time = now - chrono::Duration::seconds(120);

        let input = make_input("JA1ABC", 14_025_000, DxMode::CW, "W1AW", "src1", OriginatorKind::Human, old_time);
        table.ingest(input);
        assert_eq!(table.len(), 1);

        let expired = table.evict_expired(now);
        assert_eq!(expired.len(), 1);
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn evict_expired_keeps_fresh_spots() {
        let mut table = SpotTable::new(SpotTableConfig {
            ttl: Duration::from_secs(60),
            ..Default::default()
        });

        let now = Utc::now();

        let input = make_input("JA1ABC", 14_025_000, DxMode::CW, "W1AW", "src1", OriginatorKind::Human, now);
        table.ingest(input);

        let expired = table.evict_expired(now);
        assert!(expired.is_empty());
        assert_eq!(table.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Observation cap
    // -----------------------------------------------------------------------

    #[test]
    fn observation_cap_enforced() {
        let mut table = SpotTable::new(SpotTableConfig {
            max_observations_per_spot: 5,
            ..Default::default()
        });

        let now = Utc::now();

        for i in 0..10 {
            let input = make_input(
                "JA1ABC",
                14_025_000,
                DxMode::CW,
                &format!("SKIM-{i}"),
                "src1",
                OriginatorKind::Skimmer,
                now,
            );
            table.ingest(input);
        }

        let key = make_key("JA1ABC", 14_025_000, DxMode::CW);
        let state = table.get(&key).unwrap();
        assert_eq!(state.spot.observations.len(), 5);

        // The oldest observations should have been dropped; most recent kept
        assert_eq!(state.spot.observations[0].originator.0, "SKIM-5");
        assert_eq!(state.spot.observations[4].originator.0, "SKIM-9");
    }

    // -----------------------------------------------------------------------
    // Unique originator / source tracking
    // -----------------------------------------------------------------------

    #[test]
    fn unique_originators_counted() {
        let mut table = default_table();
        let now = Utc::now();

        for call in ["W1AW", "VE3NEA", "DL1ABC"] {
            let input = make_input("JA1ABC", 14_025_000, DxMode::CW, call, "src1", OriginatorKind::Human, now);
            table.ingest(input);
        }

        let key = make_key("JA1ABC", 14_025_000, DxMode::CW);
        let state = table.get(&key).unwrap();
        assert_eq!(state.spot.unique_originators, 3);
        assert_eq!(state.originator_set.len(), 3);
    }

    #[test]
    fn unique_human_originators_tracked() {
        let mut table = default_table();
        let now = Utc::now();

        let human = make_input("JA1ABC", 14_025_000, DxMode::CW, "W1AW", "src1", OriginatorKind::Human, now);
        let skim = make_input("JA1ABC", 14_025_000, DxMode::CW, "DK8JP-1", "src1", OriginatorKind::Skimmer, now);

        table.ingest(human);
        table.ingest(skim);

        let key = make_key("JA1ABC", 14_025_000, DxMode::CW);
        let state = table.get(&key).unwrap();
        assert_eq!(state.originator_set.len(), 2);
        assert_eq!(state.human_originator_set.len(), 1);
    }

    #[test]
    fn unique_sources_counted() {
        let mut table = default_table();
        let now = Utc::now();

        let input1 = make_input("JA1ABC", 14_025_000, DxMode::CW, "W1AW", "rbn-east", OriginatorKind::Skimmer, now);
        let input2 = make_input("JA1ABC", 14_025_005, DxMode::CW, "VE3NEA", "rbn-west", OriginatorKind::Skimmer, now);

        table.ingest(input1);
        table.ingest(input2);

        let key = make_key("JA1ABC", 14_025_000, DxMode::CW);
        let state = table.get(&key).unwrap();
        assert_eq!(state.spot.unique_sources, 2);
        assert_eq!(state.source_set.len(), 2);
    }

    #[test]
    fn duplicate_originator_not_double_counted() {
        let mut table = default_table();
        let now = Utc::now();

        // Same spotter reports twice
        for _ in 0..3 {
            let input = make_input("JA1ABC", 14_025_000, DxMode::CW, "W1AW", "src1", OriginatorKind::Human, now);
            table.ingest(input);
        }

        let key = make_key("JA1ABC", 14_025_000, DxMode::CW);
        let state = table.get(&key).unwrap();
        assert_eq!(state.spot.unique_originators, 1);
        assert_eq!(state.spot.observations.len(), 3);
    }

    // -----------------------------------------------------------------------
    // Frequency bucketing integration
    // -----------------------------------------------------------------------

    #[test]
    fn cw_spots_5hz_apart_same_bucket() {
        // CW bucket = 10 Hz, so 14025100 and 14025105 → same bucket 14025100
        let key1 = make_key("JA1ABC", 14_025_100, DxMode::CW);
        let key2 = make_key("JA1ABC", 14_025_105, DxMode::CW);
        assert_eq!(key1, key2);
    }

    #[test]
    fn cw_spots_15hz_apart_different_buckets() {
        // CW bucket = 10 Hz, so 14025100 and 14025115 → buckets 14025100 vs 14025110
        let key1 = make_key("JA1ABC", 14_025_100, DxMode::CW);
        let key2 = make_key("JA1ABC", 14_025_115, DxMode::CW);
        assert_ne!(key1, key2);
    }

    #[test]
    fn ssb_spots_500hz_apart_same_bucket() {
        // SSB bucket = 1000 Hz, so 14200500 and 14200999 → same bucket 14200000
        let key1 = make_key("W1AW", 14_200_500, DxMode::SSB);
        let key2 = make_key("W1AW", 14_200_999, DxMode::SSB);
        assert_eq!(key1, key2);
    }

    #[test]
    fn dig_spots_50hz_apart_same_bucket() {
        // DIG bucket = 100 Hz, so 14074050 and 14074099 → same bucket 14074000
        let key1 = make_key("W1AW", 14_074_050, DxMode::DIG);
        let key2 = make_key("W1AW", 14_074_099, DxMode::DIG);
        assert_eq!(key1, key2);
    }

    // -----------------------------------------------------------------------
    // Revision tracking
    // -----------------------------------------------------------------------

    #[test]
    fn revision_increments_on_each_observation() {
        let mut table = default_table();
        let now = Utc::now();

        for i in 0..5 {
            let input = make_input(
                "JA1ABC",
                14_025_000,
                DxMode::CW,
                &format!("SPOTTER-{i}"),
                "src1",
                OriginatorKind::Human,
                now,
            );
            table.ingest(input);
        }

        let key = make_key("JA1ABC", 14_025_000, DxMode::CW);
        let state = table.get(&key).unwrap();
        assert_eq!(state.revision, 5);
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn empty_table() {
        let table = default_table();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn evict_on_empty_table() {
        let mut table = default_table();
        let expired = table.evict_expired(Utc::now());
        assert!(expired.is_empty());
    }

    #[test]
    fn comment_updated_on_subsequent_observation() {
        let mut table = default_table();
        let now = Utc::now();

        let mut input1 = make_input("JA1ABC", 14_025_000, DxMode::CW, "W1AW", "src1", OriginatorKind::Human, now);
        input1.comment = Some("CQ".into());
        table.ingest(input1);

        let mut input2 = make_input("JA1ABC", 14_025_005, DxMode::CW, "VE3NEA", "src1", OriginatorKind::Human, now);
        input2.comment = Some("CQ TEST".into());
        table.ingest(input2);

        let key = make_key("JA1ABC", 14_025_000, DxMode::CW);
        let state = table.get(&key).unwrap();
        assert_eq!(state.spot.comment.as_deref(), Some("CQ TEST"));
    }

    #[test]
    fn none_comment_does_not_overwrite() {
        let mut table = default_table();
        let now = Utc::now();

        let mut input1 = make_input("JA1ABC", 14_025_000, DxMode::CW, "W1AW", "src1", OriginatorKind::Human, now);
        input1.comment = Some("CQ".into());
        table.ingest(input1);

        // Second observation has no comment
        let input2 = make_input("JA1ABC", 14_025_005, DxMode::CW, "VE3NEA", "src1", OriginatorKind::Human, now);
        table.ingest(input2);

        let key = make_key("JA1ABC", 14_025_000, DxMode::CW);
        let state = table.get(&key).unwrap();
        assert_eq!(state.spot.comment.as_deref(), Some("CQ"));
    }
}
