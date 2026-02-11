use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use crate::domain::{Band, Continent, DxMode, OriginatorKind};

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Identifies an upstream connection (e.g., a specific telnet cluster node).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SourceId(pub String);

/// Identifies a spotter or skimmer callsign.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OriginatorId(pub String);

// ---------------------------------------------------------------------------
// SpotKey — HashMap key for aggregated spots
// ---------------------------------------------------------------------------

/// Composite key for deduplication and aggregation.
/// Two observations with the same SpotKey are considered the same spot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpotKey {
    /// Normalized DX callsign (uppercase, portable policy applied).
    pub dx_call_norm: String,
    pub band: Band,
    pub mode: DxMode,
    /// Frequency truncated to the bucket size for this mode (e.g., 10 Hz for CW).
    pub freq_bucket_hz: u64,
}

// ---------------------------------------------------------------------------
// Events — the library's output
// ---------------------------------------------------------------------------

/// Top-level event emitted to the consuming application.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DxEvent {
    Spot(DxSpotEvent),
    Announce(DxAnnounce),
    SourceStatus(SourceStatus),
    Error(DxError),
}

/// A spot event with lifecycle tracking.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DxSpotEvent {
    pub spot: DxSpot,
    pub kind: SpotEventKind,
    /// Monotonically increasing per SpotKey.
    pub revision: u32,
}

/// Lifecycle state of a spot event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SpotEventKind {
    /// First time this spot passes filters and is emitted.
    New,
    /// Meaningful change to an already-emitted spot.
    Update,
    /// Spot removed (TTL expiry or explicit withdrawal).
    Withdraw,
}

// ---------------------------------------------------------------------------
// DxSpot — the aggregated spot
// ---------------------------------------------------------------------------

/// An aggregated DX spot, built from one or more observations.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DxSpot {
    pub spot_key: SpotKey,
    pub dx_call: String,
    pub freq_hz: u64,
    pub band: Band,
    pub mode: DxMode,

    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// Parsed from the spot line; not trusted for TTL decisions.
    pub spot_time: Option<DateTime<Utc>>,

    pub comment: Option<String>,

    /// Bounded list of recent observations contributing to this spot.
    pub observations: Vec<SpotObservation>,
    pub unique_originators: u8,
    pub unique_sources: u8,

    pub skim_quality: Option<SkimQualityTag>,
    pub quality_detail: Option<SkimQualityDetail>,
    pub confidence: SpotConfidence,
}

// ---------------------------------------------------------------------------
// SpotObservation — a single sighting from one source
// ---------------------------------------------------------------------------

/// A single observation of a DX spot from one source/originator.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpotObservation {
    pub source: SourceId,
    pub originator: OriginatorId,
    pub originator_kind: OriginatorKind,
    pub snr_db: Option<i8>,
    pub wpm: Option<u16>,
    pub obs_time: DateTime<Utc>,
    /// Raw spot line text, only populated when the `raw-lines` feature is enabled.
    #[cfg(feature = "raw-lines")]
    pub raw: Option<String>,
}

// ---------------------------------------------------------------------------
// Skimmer quality
// ---------------------------------------------------------------------------

/// CT1BOH/AR-Cluster-style skimmer quality classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SkimQualityTag {
    /// Corroborated by multiple distinct skimmers.
    Valid,
    /// Likely a decoding error (similar to a verified call at nearby freq).
    Busted,
    /// Previously verified call now on a significantly different frequency.
    Qsy,
    /// Insufficient data to classify.
    Unknown,
}

/// Detailed skimmer quality information for debugging/display.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SkimQualityDetail {
    /// Number of distinct skimmers that reported this call within the frequency window.
    pub corroborating_skimmers: u8,
    /// If Busted: the verified call this was confused with.
    pub similar_call: Option<String>,
    /// If Busted: the edit distance to the similar call.
    pub edit_distance: Option<u8>,
    /// If Qsy: the previous verified frequency.
    pub previous_freq_hz: Option<u64>,
}

/// General confidence level for a spot (computed from multiple factors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SpotConfidence {
    High,
    Medium,
    Low,
    Unverified,
}

impl Default for SpotConfidence {
    fn default() -> Self {
        Self::Unverified
    }
}

// ---------------------------------------------------------------------------
// Announce, SourceStatus, DxError
// ---------------------------------------------------------------------------

/// A text announcement from a cluster node.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DxAnnounce {
    pub source: SourceId,
    pub text: String,
    pub timestamp: DateTime<Utc>,
}

/// Connection state of an upstream source.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SourceStatus {
    pub source_id: SourceId,
    pub state: SourceConnectionState,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SourceConnectionState {
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
    Failed { reason: String },
    Shutdown,
}

/// A non-fatal error from a source or the aggregator.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DxError {
    pub source_id: Option<SourceId>,
    pub message: String,
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// SpotView — borrow type for filter evaluation (hot path)
// ---------------------------------------------------------------------------

/// A lightweight borrow view of aggregated spot state, used by the filter
/// evaluator. Constructed cheaply from SpotState by the aggregator.
///
/// Uses `DateTime<Utc>` consistently (not SystemTime) per project decision.
#[derive(Debug)]
pub struct SpotView<'a> {
    pub now: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,

    pub freq_hz: u64,
    pub band: Band,
    pub mode: DxMode,

    /// Normalized DX callsign.
    pub dx_call_norm: &'a str,

    pub comment: Option<&'a str>,

    // Correlation stats
    pub unique_originators: u8,
    pub unique_human_originators: u8,
    pub unique_sources: u8,
    pub total_observations_in_window: u8,
    pub repeats_same_key_in_window: u8,

    // Representative spotter info
    pub spotter_call_norm: Option<&'a str>,
    pub spotter_source_id: Option<&'a str>,
    pub originator_kind: OriginatorKind,

    // Skimmer metrics
    pub snr_db: Option<i8>,
    pub wpm: Option<u16>,
    pub is_skimmer_dupe: bool,
    pub is_skimmer_cq: bool,

    // Resolved geo data (optional)
    pub dx_geo: GeoResolved<'a>,
    pub spotter_geo: GeoResolved<'a>,

    // Enrichment flags (optional)
    pub lotw: Option<bool>,
    pub in_master_db: Option<bool>,
    pub in_callbook: Option<bool>,
    pub memberships: Option<&'a BTreeSet<String>>,
}

/// Resolved geographic/entity data for a callsign.
#[derive(Debug, Default)]
pub struct GeoResolved<'a> {
    pub continent: Option<Continent>,
    pub cq_zone: Option<u8>,
    pub itu_zone: Option<u8>,
    pub entity: Option<&'a str>,
    pub country: Option<&'a str>,
    pub state: Option<&'a str>,
    pub grid: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// SpotView builder helper for tests
// ---------------------------------------------------------------------------

impl<'a> SpotView<'a> {
    /// Create a SpotView with sensible defaults for testing.
    /// All optional/geo/enrichment fields are empty/default.
    pub fn test_default(
        now: DateTime<Utc>,
        dx_call: &'a str,
        freq_hz: u64,
        band: Band,
        mode: DxMode,
    ) -> Self {
        Self {
            now,
            last_seen: now,
            freq_hz,
            band,
            mode,
            dx_call_norm: dx_call,
            comment: None,
            unique_originators: 1,
            unique_human_originators: 1,
            unique_sources: 1,
            total_observations_in_window: 1,
            repeats_same_key_in_window: 1,
            spotter_call_norm: None,
            spotter_source_id: None,
            originator_kind: OriginatorKind::Human,
            snr_db: None,
            wpm: None,
            is_skimmer_dupe: false,
            is_skimmer_cq: false,
            dx_geo: GeoResolved::default(),
            spotter_geo: GeoResolved::default(),
            lotw: None,
            in_master_db: None,
            in_callbook: None,
            memberships: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn spot_key_equality_and_hashing() {
        let k1 = SpotKey {
            dx_call_norm: "W1AW".into(),
            band: Band::B20,
            mode: DxMode::CW,
            freq_bucket_hz: 14_025_000,
        };
        let k2 = SpotKey {
            dx_call_norm: "W1AW".into(),
            band: Band::B20,
            mode: DxMode::CW,
            freq_bucket_hz: 14_025_000,
        };
        let k3 = SpotKey {
            dx_call_norm: "W1AW".into(),
            band: Band::B20,
            mode: DxMode::CW,
            freq_bucket_hz: 14_025_010,
        };

        assert_eq!(k1, k2);
        assert_ne!(k1, k3);

        let mut map = HashMap::new();
        map.insert(k1.clone(), 1);
        assert_eq!(map.get(&k2), Some(&1));
        assert_eq!(map.get(&k3), None);
    }

    #[test]
    fn spot_key_different_band_is_different() {
        let k1 = SpotKey {
            dx_call_norm: "W1AW".into(),
            band: Band::B20,
            mode: DxMode::CW,
            freq_bucket_hz: 14_025_000,
        };
        let k2 = SpotKey {
            dx_call_norm: "W1AW".into(),
            band: Band::B40,
            mode: DxMode::CW,
            freq_bucket_hz: 7_025_000,
        };
        assert_ne!(k1, k2);
    }

    #[test]
    fn dx_event_variants() {
        let now = Utc::now();

        let spot_event = DxEvent::Spot(DxSpotEvent {
            spot: DxSpot {
                spot_key: SpotKey {
                    dx_call_norm: "JA1ABC".into(),
                    band: Band::B20,
                    mode: DxMode::CW,
                    freq_bucket_hz: 14_025_100,
                },
                dx_call: "JA1ABC".into(),
                freq_hz: 14_025_100,
                band: Band::B20,
                mode: DxMode::CW,
                first_seen: now,
                last_seen: now,
                spot_time: None,
                comment: Some("CQ".into()),
                observations: vec![],
                unique_originators: 1,
                unique_sources: 1,
                skim_quality: None,
                quality_detail: None,
                confidence: SpotConfidence::default(),
            },
            kind: SpotEventKind::New,
            revision: 1,
        });

        // Just verify it constructs without panic
        match &spot_event {
            DxEvent::Spot(e) => {
                assert_eq!(e.kind, SpotEventKind::New);
                assert_eq!(e.revision, 1);
                assert_eq!(e.spot.dx_call, "JA1ABC");
            }
            _ => panic!("expected Spot variant"),
        }
    }

    #[test]
    fn spot_view_test_default() {
        let now = Utc::now();
        let view = SpotView::test_default(now, "W1AW", 14_025_000, Band::B20, DxMode::CW);

        assert_eq!(view.dx_call_norm, "W1AW");
        assert_eq!(view.freq_hz, 14_025_000);
        assert_eq!(view.band, Band::B20);
        assert_eq!(view.mode, DxMode::CW);
        assert_eq!(view.originator_kind, OriginatorKind::Human);
        assert_eq!(view.unique_originators, 1);
        assert!(view.comment.is_none());
        assert!(view.dx_geo.continent.is_none());
        assert!(view.lotw.is_none());
    }

    #[test]
    fn spot_confidence_default() {
        assert_eq!(SpotConfidence::default(), SpotConfidence::Unverified);
    }

    #[test]
    fn source_connection_state_equality() {
        assert_eq!(SourceConnectionState::Connecting, SourceConnectionState::Connecting);
        assert_eq!(
            SourceConnectionState::Reconnecting { attempt: 3 },
            SourceConnectionState::Reconnecting { attempt: 3 }
        );
        assert_ne!(
            SourceConnectionState::Reconnecting { attempt: 1 },
            SourceConnectionState::Reconnecting { attempt: 2 }
        );
    }

    #[test]
    fn skim_quality_tag_variants() {
        assert_ne!(SkimQualityTag::Valid, SkimQualityTag::Busted);
        assert_ne!(SkimQualityTag::Qsy, SkimQualityTag::Unknown);
        assert_eq!(SkimQualityTag::Valid, SkimQualityTag::Valid);
    }
}
