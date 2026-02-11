//! Configuration for the skimmer quality engine.

use std::time::Duration;

/// Configuration for skimmer quality classification and gating.
///
/// Based on CT1BOH/AR-Cluster-style skimmer quality logic.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SkimmerQualityConfig {
    /// Master enable/disable for the quality engine.
    pub enabled: bool,

    // Individually controllable computations
    pub compute_valid: bool,
    pub compute_busted: bool,
    pub compute_qsy: bool,

    // Gating policy (what tags are allowed to be emitted)
    pub gate_skimmer_output: bool,
    pub allow_valid: bool,
    pub allow_qsy: bool,
    pub allow_unknown: bool,
    pub allow_busted: bool,

    // SkimValid parameters
    /// Number of distinct skimmers required to corroborate a spot.
    pub valid_required_distinct_skimmers: u8,
    /// Frequency window (Hz) within which skimmer reports are considered corroborating.
    pub valid_freq_window_hz: i64,
    /// Time window for counting corroborations.
    #[cfg_attr(feature = "serde", serde(with = "crate::skimmer::config::duration_secs"))]
    pub lookback_window: Duration,

    // SkimBusted parameters
    /// Frequency window (Hz) for busted-call detection.
    pub busted_freq_window_hz: i64,
    /// Maximum Levenshtein edit distance for busted-call similarity.
    pub similar_call_max_edit_distance: u8,

    // SkimQsy parameters
    /// Frequency difference (Hz) beyond which a verified call is considered QSY.
    pub qsy_freq_window_hz: i64,

    /// Only apply skimmer quality gating to skimmer-originated spots.
    pub apply_only_to_skimmer: bool,

    /// Hard cap on total tracked observations to prevent unbounded memory.
    pub max_tracked_observations: usize,
}

impl Default for SkimmerQualityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            compute_valid: true,
            compute_busted: true,
            compute_qsy: true,
            gate_skimmer_output: true,
            allow_valid: true,
            allow_qsy: false,
            allow_unknown: false,
            allow_busted: false,
            valid_required_distinct_skimmers: 3,
            valid_freq_window_hz: 300,
            lookback_window: Duration::from_secs(180),
            busted_freq_window_hz: 100,
            similar_call_max_edit_distance: 1,
            qsy_freq_window_hz: 400,
            apply_only_to_skimmer: true,
            max_tracked_observations: 100_000,
        }
    }
}

/// Serde helper for Duration as seconds.
#[cfg(feature = "serde")]
mod duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(Duration::from_secs(secs))
    }
}
