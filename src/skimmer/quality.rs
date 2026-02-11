//! Skimmer quality engine: SkimValid/Busted/Qsy/Unknown classification.
//!
//! Implements CT1BOH/AR-Cluster-style skimmer quality tagging. Tracks recent
//! skimmer observations in a time-windowed index keyed by DX callsign (Fix #4),
//! and maintains a verified-call map for QSY and busted-call detection.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::domain::OriginatorKind;
use crate::model::{SkimQualityDetail, SkimQualityTag};

use super::config::SkimmerQualityConfig;

// ---------------------------------------------------------------------------
// Public engine
// ---------------------------------------------------------------------------

/// The skimmer quality engine.
pub struct SkimmerQualityEngine {
    config: SkimmerQualityConfig,
    /// Recent observations keyed by normalized DX callsign.
    observations: HashMap<String, Vec<SkimObs>>,
    /// Total observation count across all keys (for hard cap enforcement).
    total_observations: usize,
    /// Verified calls with their last known frequency and verification time.
    verified_calls: HashMap<String, VerifiedCallState>,
}

/// A single skimmer observation recorded in the index.
#[derive(Debug, Clone)]
struct SkimObs {
    freq_hz: u64,
    skimmer_call: String,
    timestamp: DateTime<Utc>,
}

/// State for a verified (corroborated) callsign.
#[derive(Debug, Clone)]
struct VerifiedCallState {
    last_freq_hz: u64,
    last_verified: DateTime<Utc>,
}

impl SkimmerQualityEngine {
    pub fn new(config: SkimmerQualityConfig) -> Self {
        Self {
            config,
            observations: HashMap::new(),
            total_observations: 0,
            verified_calls: HashMap::new(),
        }
    }

    /// Record a skimmer observation for a DX callsign.
    pub fn record_observation(
        &mut self,
        dx_call: &str,
        freq_hz: u64,
        skimmer_call: &str,
        now: DateTime<Utc>,
    ) {
        // Enforce hard cap: if at limit, evict oldest entries first
        if self.total_observations >= self.config.max_tracked_observations {
            self.evict_oldest_entries();
        }

        let obs = SkimObs {
            freq_hz,
            skimmer_call: skimmer_call.to_string(),
            timestamp: now,
        };

        self.observations
            .entry(dx_call.to_string())
            .or_default()
            .push(obs);
        self.total_observations += 1;
    }

    /// Compute the quality tag for a DX callsign at a given frequency.
    ///
    /// Call this after `record_observation()` for the current spot.
    pub fn compute_tag(&mut self, dx_call: &str, freq_hz: u64, now: DateTime<Utc>) -> SkimQualityTag {
        // Count distinct skimmers corroborating this call within freq+time window
        let distinct_skimmers = if self.config.compute_valid {
            self.count_distinct_skimmers(dx_call, freq_hz, now)
        } else {
            0
        };

        // Check SkimValid
        if self.config.compute_valid
            && distinct_skimmers >= self.config.valid_required_distinct_skimmers as usize
        {
            // Check if this is a QSY from a previously verified frequency
            if self.config.compute_qsy {
                if let Some(prev) = self.verified_calls.get(dx_call) {
                    let freq_diff = (freq_hz as i64 - prev.last_freq_hz as i64).abs();
                    if freq_diff > self.config.qsy_freq_window_hz {
                        // Update verified state to new frequency
                        self.verified_calls.insert(
                            dx_call.to_string(),
                            VerifiedCallState {
                                last_freq_hz: freq_hz,
                                last_verified: now,
                            },
                        );
                        return SkimQualityTag::Qsy;
                    }
                }
            }

            // Mark as verified (new or same frequency)
            self.verified_calls.insert(
                dx_call.to_string(),
                VerifiedCallState {
                    last_freq_hz: freq_hz,
                    last_verified: now,
                },
            );
            return SkimQualityTag::Valid;
        }

        // Check SkimBusted: similar to a verified call at nearby frequency
        if self.config.compute_busted
            && self.find_busted_match(dx_call, freq_hz).is_some()
        {
            return SkimQualityTag::Busted;
        }

        SkimQualityTag::Unknown
    }

    /// Compute the quality tag with detailed information.
    pub fn compute_tag_with_detail(
        &mut self,
        dx_call: &str,
        freq_hz: u64,
        now: DateTime<Utc>,
    ) -> (SkimQualityTag, SkimQualityDetail) {
        let distinct_skimmers = if self.config.compute_valid {
            self.count_distinct_skimmers(dx_call, freq_hz, now)
        } else {
            0
        };

        // SkimValid check
        if self.config.compute_valid
            && distinct_skimmers >= self.config.valid_required_distinct_skimmers as usize
        {
            // QSY check
            if self.config.compute_qsy {
                if let Some(prev) = self.verified_calls.get(dx_call) {
                    let freq_diff = (freq_hz as i64 - prev.last_freq_hz as i64).abs();
                    if freq_diff > self.config.qsy_freq_window_hz {
                        let previous_freq = prev.last_freq_hz;
                        self.verified_calls.insert(
                            dx_call.to_string(),
                            VerifiedCallState {
                                last_freq_hz: freq_hz,
                                last_verified: now,
                            },
                        );
                        return (
                            SkimQualityTag::Qsy,
                            SkimQualityDetail {
                                corroborating_skimmers: distinct_skimmers.min(255) as u8,
                                similar_call: None,
                                edit_distance: None,
                                previous_freq_hz: Some(previous_freq),
                            },
                        );
                    }
                }
            }

            self.verified_calls.insert(
                dx_call.to_string(),
                VerifiedCallState {
                    last_freq_hz: freq_hz,
                    last_verified: now,
                },
            );
            return (
                SkimQualityTag::Valid,
                SkimQualityDetail {
                    corroborating_skimmers: distinct_skimmers.min(255) as u8,
                    similar_call: None,
                    edit_distance: None,
                    previous_freq_hz: None,
                },
            );
        }

        // SkimBusted check
        if self.config.compute_busted {
            if let Some((similar, dist)) = self.find_busted_match(dx_call, freq_hz) {
                return (
                    SkimQualityTag::Busted,
                    SkimQualityDetail {
                        corroborating_skimmers: distinct_skimmers.min(255) as u8,
                        similar_call: Some(similar),
                        edit_distance: Some(dist as u8),
                        previous_freq_hz: None,
                    },
                );
            }
        }

        (
            SkimQualityTag::Unknown,
            SkimQualityDetail {
                corroborating_skimmers: distinct_skimmers.min(255) as u8,
                similar_call: None,
                edit_distance: None,
                previous_freq_hz: None,
            },
        )
    }

    /// Check whether a spot with the given tag and originator kind should be emitted.
    pub fn should_emit(&self, tag: SkimQualityTag, originator_kind: OriginatorKind) -> bool {
        // Human-originated spots are never blocked by skimmer gating
        if self.config.apply_only_to_skimmer && originator_kind != OriginatorKind::Skimmer {
            return true;
        }

        if !self.config.enabled || !self.config.gate_skimmer_output {
            return true;
        }

        match tag {
            SkimQualityTag::Valid => self.config.allow_valid,
            SkimQualityTag::Qsy => self.config.allow_qsy,
            SkimQualityTag::Unknown => self.config.allow_unknown,
            SkimQualityTag::Busted => self.config.allow_busted,
        }
    }

    /// Evict observations and verified calls older than the lookback window.
    pub fn evict_expired(&mut self, now: DateTime<Utc>) {
        let lookback_secs = self.config.lookback_window.as_secs() as i64;

        // Evict old observations
        self.observations.retain(|_, obs_list| {
            let before = obs_list.len();
            obs_list.retain(|o| {
                now.signed_duration_since(o.timestamp).num_seconds() <= lookback_secs
            });
            let after = obs_list.len();
            self.total_observations -= before - after;
            !obs_list.is_empty()
        });

        // Evict old verified calls (use a longer window: 2x lookback)
        let verified_cutoff_secs = lookback_secs * 2;
        self.verified_calls.retain(|_, state| {
            now.signed_duration_since(state.last_verified).num_seconds() <= verified_cutoff_secs
        });
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Count distinct skimmer callsigns reporting `dx_call` within the
    /// frequency and time windows.
    fn count_distinct_skimmers(
        &self,
        dx_call: &str,
        freq_hz: u64,
        now: DateTime<Utc>,
    ) -> usize {
        let lookback_secs = self.config.lookback_window.as_secs() as i64;
        let freq_window = self.config.valid_freq_window_hz;

        let Some(obs_list) = self.observations.get(dx_call) else {
            return 0;
        };

        let mut seen_skimmers = std::collections::HashSet::new();

        for obs in obs_list {
            let age = now.signed_duration_since(obs.timestamp).num_seconds();
            if age > lookback_secs {
                continue;
            }
            let freq_diff = (freq_hz as i64 - obs.freq_hz as i64).abs();
            if freq_diff <= freq_window {
                seen_skimmers.insert(&obs.skimmer_call);
            }
        }

        seen_skimmers.len()
    }

    /// Find a verified call that is similar to `dx_call` at a nearby frequency.
    /// Returns (similar_call, edit_distance) if found.
    fn find_busted_match(&self, dx_call: &str, freq_hz: u64) -> Option<(String, usize)> {
        let max_dist = self.config.similar_call_max_edit_distance as usize;
        let freq_window = self.config.busted_freq_window_hz;

        for (verified_call, state) in &self.verified_calls {
            if verified_call == dx_call {
                continue;
            }
            let freq_diff = (freq_hz as i64 - state.last_freq_hz as i64).abs();
            if freq_diff > freq_window {
                continue;
            }
            let dist = levenshtein(dx_call, verified_call);
            if dist <= max_dist {
                return Some((verified_call.clone(), dist));
            }
        }

        None
    }

    /// Evict the oldest observation entries when at the hard cap.
    fn evict_oldest_entries(&mut self) {
        // Find and remove the key with the oldest average timestamp
        let oldest_key = self
            .observations
            .iter()
            .filter(|(_, obs)| !obs.is_empty())
            .min_by_key(|(_, obs)| obs.first().map(|o| o.timestamp))
            .map(|(k, _)| k.clone());

        if let Some(key) = oldest_key {
            if let Some(removed) = self.observations.remove(&key) {
                self.total_observations -= removed.len();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Levenshtein distance (inline implementation)
// ---------------------------------------------------------------------------

/// Compute the Levenshtein edit distance between two strings.
///
/// Uses the standard dynamic programming approach with O(min(m,n)) space.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    // Ensure b is the shorter string for space efficiency
    let (a_bytes, b_bytes) = if a_bytes.len() < b_bytes.len() {
        (b_bytes, a_bytes)
    } else {
        (a_bytes, b_bytes)
    };

    let m = a_bytes.len();
    let n = b_bytes.len();

    if n == 0 {
        return m;
    }

    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_bytes[i - 1] == b_bytes[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skimmer::config::SkimmerQualityConfig;
    use chrono::Duration;

    fn default_engine() -> SkimmerQualityEngine {
        SkimmerQualityEngine::new(SkimmerQualityConfig::default())
    }

    fn now_plus_secs(base: DateTime<Utc>, secs: i64) -> DateTime<Utc> {
        base + Duration::seconds(secs)
    }

    // -----------------------------------------------------------------------
    // Levenshtein distance
    // -----------------------------------------------------------------------

    #[test]
    fn levenshtein_identical() {
        assert_eq!(levenshtein("W1AW", "W1AW"), 0);
    }

    #[test]
    fn levenshtein_one_char_diff() {
        assert_eq!(levenshtein("W1AW", "W1AX"), 1);
        assert_eq!(levenshtein("W1AW", "K1AW"), 1);
    }

    #[test]
    fn levenshtein_two_char_diff() {
        assert_eq!(levenshtein("W1AW", "K1AX"), 2);
    }

    #[test]
    fn levenshtein_different_lengths() {
        assert_eq!(levenshtein("W1AW", "W1AWX"), 1);
        assert_eq!(levenshtein("W1AW", "W1A"), 1);
    }

    #[test]
    fn levenshtein_empty() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("W1AW", ""), 4);
        assert_eq!(levenshtein("", "W1AW"), 4);
    }

    // -----------------------------------------------------------------------
    // SkimUnknown (below threshold)
    // -----------------------------------------------------------------------

    #[test]
    fn single_report_is_unknown() {
        let mut engine = default_engine();
        let now = Utc::now();

        engine.record_observation("JA1ABC", 14_025_000, "W3LPL-2", now);
        let tag = engine.compute_tag("JA1ABC", 14_025_000, now);

        assert_eq!(tag, SkimQualityTag::Unknown);
    }

    #[test]
    fn two_reports_still_unknown() {
        let mut engine = default_engine();
        let now = Utc::now();

        engine.record_observation("JA1ABC", 14_025_000, "W3LPL-2", now);
        engine.record_observation("JA1ABC", 14_025_100, "DK8JP-1", now);
        let tag = engine.compute_tag("JA1ABC", 14_025_000, now);

        assert_eq!(tag, SkimQualityTag::Unknown);
    }

    // -----------------------------------------------------------------------
    // SkimValid (corroborated)
    // -----------------------------------------------------------------------

    #[test]
    fn three_distinct_skimmers_is_valid() {
        let mut engine = default_engine();
        let now = Utc::now();

        engine.record_observation("JA1ABC", 14_025_000, "W3LPL-2", now);
        engine.record_observation("JA1ABC", 14_025_050, "DK8JP-1", now);
        engine.record_observation("JA1ABC", 14_025_100, "VE3NEA-3", now);
        let tag = engine.compute_tag("JA1ABC", 14_025_000, now);

        assert_eq!(tag, SkimQualityTag::Valid);
    }

    #[test]
    fn same_skimmer_twice_does_not_count_as_two() {
        let mut engine = default_engine();
        let now = Utc::now();

        engine.record_observation("JA1ABC", 14_025_000, "W3LPL-2", now);
        engine.record_observation("JA1ABC", 14_025_050, "W3LPL-2", now); // same skimmer
        engine.record_observation("JA1ABC", 14_025_100, "DK8JP-1", now);
        let tag = engine.compute_tag("JA1ABC", 14_025_000, now);

        assert_eq!(tag, SkimQualityTag::Unknown); // only 2 distinct
    }

    #[test]
    fn reports_outside_freq_window_not_counted() {
        let mut engine = default_engine();
        let now = Utc::now();

        // Default freq window is 300 Hz
        engine.record_observation("JA1ABC", 14_025_000, "W3LPL-2", now);
        engine.record_observation("JA1ABC", 14_025_050, "DK8JP-1", now);
        engine.record_observation("JA1ABC", 14_026_000, "VE3NEA-3", now); // 1000 Hz away
        let tag = engine.compute_tag("JA1ABC", 14_025_000, now);

        assert_eq!(tag, SkimQualityTag::Unknown); // only 2 within window
    }

    // -----------------------------------------------------------------------
    // SkimQsy
    // -----------------------------------------------------------------------

    #[test]
    fn valid_then_different_freq_is_qsy() {
        let mut engine = default_engine();
        let now = Utc::now();

        // First: become valid on 20m
        engine.record_observation("JA1ABC", 14_025_000, "W3LPL-2", now);
        engine.record_observation("JA1ABC", 14_025_050, "DK8JP-1", now);
        engine.record_observation("JA1ABC", 14_025_100, "VE3NEA-3", now);
        let tag1 = engine.compute_tag("JA1ABC", 14_025_000, now);
        assert_eq!(tag1, SkimQualityTag::Valid);

        // Then: new reports on 15m (far from verified freq)
        let later = now_plus_secs(now, 60);
        engine.record_observation("JA1ABC", 21_025_000, "W3LPL-2", later);
        engine.record_observation("JA1ABC", 21_025_050, "DK8JP-1", later);
        engine.record_observation("JA1ABC", 21_025_100, "VE3NEA-3", later);
        let tag2 = engine.compute_tag("JA1ABC", 21_025_000, later);
        assert_eq!(tag2, SkimQualityTag::Qsy);
    }

    // -----------------------------------------------------------------------
    // SkimBusted
    // -----------------------------------------------------------------------

    #[test]
    fn similar_call_near_verified_freq_is_busted() {
        let mut engine = default_engine();
        let now = Utc::now();

        // Verify "W1AW" at 14025000
        engine.record_observation("W1AW", 14_025_000, "W3LPL-2", now);
        engine.record_observation("W1AW", 14_025_050, "DK8JP-1", now);
        engine.record_observation("W1AW", 14_025_100, "VE3NEA-3", now);
        engine.compute_tag("W1AW", 14_025_000, now);

        // New call "W1AX" at nearby freq (edit distance 1)
        engine.record_observation("W1AX", 14_025_020, "K1TTT-2", now);
        let tag = engine.compute_tag("W1AX", 14_025_020, now);
        assert_eq!(tag, SkimQualityTag::Busted);
    }

    #[test]
    fn dissimilar_call_not_busted() {
        let mut engine = default_engine();
        let now = Utc::now();

        // Verify "W1AW"
        engine.record_observation("W1AW", 14_025_000, "W3LPL-2", now);
        engine.record_observation("W1AW", 14_025_050, "DK8JP-1", now);
        engine.record_observation("W1AW", 14_025_100, "VE3NEA-3", now);
        engine.compute_tag("W1AW", 14_025_000, now);

        // Very different call at nearby freq
        engine.record_observation("JA1ABC", 14_025_020, "K1TTT-2", now);
        let tag = engine.compute_tag("JA1ABC", 14_025_020, now);
        assert_eq!(tag, SkimQualityTag::Unknown); // edit distance too large
    }

    // -----------------------------------------------------------------------
    // Gating (should_emit)
    // -----------------------------------------------------------------------

    #[test]
    fn gate_blocks_unknown_skimmer() {
        let engine = default_engine(); // gate enabled, allow_unknown = false
        assert!(!engine.should_emit(SkimQualityTag::Unknown, OriginatorKind::Skimmer));
    }

    #[test]
    fn gate_allows_valid_skimmer() {
        let engine = default_engine(); // allow_valid = true
        assert!(engine.should_emit(SkimQualityTag::Valid, OriginatorKind::Skimmer));
    }

    #[test]
    fn gate_blocks_busted_skimmer() {
        let engine = default_engine(); // allow_busted = false
        assert!(!engine.should_emit(SkimQualityTag::Busted, OriginatorKind::Skimmer));
    }

    #[test]
    fn gate_blocks_qsy_by_default() {
        let engine = default_engine(); // allow_qsy = false
        assert!(!engine.should_emit(SkimQualityTag::Qsy, OriginatorKind::Skimmer));
    }

    #[test]
    fn gate_allows_qsy_when_configured() {
        let mut config = SkimmerQualityConfig::default();
        config.allow_qsy = true;
        let engine = SkimmerQualityEngine::new(config);
        assert!(engine.should_emit(SkimQualityTag::Qsy, OriginatorKind::Skimmer));
    }

    #[test]
    fn human_always_passes_gating() {
        let engine = default_engine();
        assert!(engine.should_emit(SkimQualityTag::Unknown, OriginatorKind::Human));
        assert!(engine.should_emit(SkimQualityTag::Busted, OriginatorKind::Human));
    }

    #[test]
    fn gate_disabled_allows_everything() {
        let mut config = SkimmerQualityConfig::default();
        config.gate_skimmer_output = false;
        let engine = SkimmerQualityEngine::new(config);
        assert!(engine.should_emit(SkimQualityTag::Unknown, OriginatorKind::Skimmer));
        assert!(engine.should_emit(SkimQualityTag::Busted, OriginatorKind::Skimmer));
    }

    #[test]
    fn engine_disabled_allows_everything() {
        let mut config = SkimmerQualityConfig::default();
        config.enabled = false;
        let engine = SkimmerQualityEngine::new(config);
        assert!(engine.should_emit(SkimQualityTag::Unknown, OriginatorKind::Skimmer));
    }

    // -----------------------------------------------------------------------
    // Lookback window expiry
    // -----------------------------------------------------------------------

    #[test]
    fn old_observations_not_counted() {
        let mut engine = default_engine(); // lookback = 180s
        let now = Utc::now();
        let old = now - Duration::seconds(300); // 5 min ago

        // Old observations
        engine.record_observation("JA1ABC", 14_025_000, "W3LPL-2", old);
        engine.record_observation("JA1ABC", 14_025_050, "DK8JP-1", old);
        engine.record_observation("JA1ABC", 14_025_100, "VE3NEA-3", old);

        // These are outside the lookback window
        let tag = engine.compute_tag("JA1ABC", 14_025_000, now);
        assert_eq!(tag, SkimQualityTag::Unknown);
    }

    #[test]
    fn evict_expired_removes_old() {
        let mut engine = default_engine();
        let now = Utc::now();
        let old = now - Duration::seconds(300);

        engine.record_observation("JA1ABC", 14_025_000, "W3LPL-2", old);
        assert_eq!(engine.total_observations, 1);

        engine.evict_expired(now);
        assert_eq!(engine.total_observations, 0);
        assert!(engine.observations.is_empty());
    }

    // -----------------------------------------------------------------------
    // Detail reporting
    // -----------------------------------------------------------------------

    #[test]
    fn valid_detail_has_corroborating_count() {
        let mut engine = default_engine();
        let now = Utc::now();

        engine.record_observation("JA1ABC", 14_025_000, "W3LPL-2", now);
        engine.record_observation("JA1ABC", 14_025_050, "DK8JP-1", now);
        engine.record_observation("JA1ABC", 14_025_100, "VE3NEA-3", now);

        let (tag, detail) = engine.compute_tag_with_detail("JA1ABC", 14_025_000, now);
        assert_eq!(tag, SkimQualityTag::Valid);
        assert_eq!(detail.corroborating_skimmers, 3);
        assert!(detail.similar_call.is_none());
        assert!(detail.previous_freq_hz.is_none());
    }

    #[test]
    fn qsy_detail_has_previous_freq() {
        let mut engine = default_engine();
        let now = Utc::now();

        // Verify on 20m
        engine.record_observation("JA1ABC", 14_025_000, "W3LPL-2", now);
        engine.record_observation("JA1ABC", 14_025_050, "DK8JP-1", now);
        engine.record_observation("JA1ABC", 14_025_100, "VE3NEA-3", now);
        engine.compute_tag("JA1ABC", 14_025_000, now);

        // QSY to 15m
        let later = now_plus_secs(now, 60);
        engine.record_observation("JA1ABC", 21_025_000, "W3LPL-2", later);
        engine.record_observation("JA1ABC", 21_025_050, "DK8JP-1", later);
        engine.record_observation("JA1ABC", 21_025_100, "VE3NEA-3", later);

        let (tag, detail) = engine.compute_tag_with_detail("JA1ABC", 21_025_000, later);
        assert_eq!(tag, SkimQualityTag::Qsy);
        assert_eq!(detail.previous_freq_hz, Some(14_025_000));
    }

    #[test]
    fn busted_detail_has_similar_call() {
        let mut engine = default_engine();
        let now = Utc::now();

        // Verify "W1AW"
        engine.record_observation("W1AW", 14_025_000, "W3LPL-2", now);
        engine.record_observation("W1AW", 14_025_050, "DK8JP-1", now);
        engine.record_observation("W1AW", 14_025_100, "VE3NEA-3", now);
        engine.compute_tag("W1AW", 14_025_000, now);

        // Busted call
        engine.record_observation("W1AX", 14_025_020, "K1TTT-2", now);
        let (tag, detail) = engine.compute_tag_with_detail("W1AX", 14_025_020, now);
        assert_eq!(tag, SkimQualityTag::Busted);
        assert_eq!(detail.similar_call.as_deref(), Some("W1AW"));
        assert_eq!(detail.edit_distance, Some(1));
    }
}
