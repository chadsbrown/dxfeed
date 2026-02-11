//! Serde-friendly filter configuration structs.
//!
//! These types represent the user-facing configuration that can be serialized
//! to/from JSON/TOML. They are validated and compiled into runtime types
//! (in `compiled.rs`) before use in the filter evaluator.

use std::collections::BTreeSet;

use crate::domain::{
    Band, Continent, DxMode, ModePolicy, PortablePolicy, TriState, UnknownFieldPolicy,
};

// ---------------------------------------------------------------------------
// Top-level filter config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FilterConfigSerde {
    /// Maximum age in seconds; spots older than this at ingestion time are dropped.
    pub max_age_secs: u64,
    /// Optional frequency sanity bounds (min_hz, max_hz). Spots outside are dropped.
    pub freq_sanity_hz: Option<(u64, u64)>,

    pub unknown_policy: UnknownFieldPolicy,

    pub rf: RfFiltersSerde,
    pub dx: CallsignFiltersSerde,
    pub spotter: SpotterFiltersSerde,
    pub content: ContentFiltersSerde,
    pub correlation: CorrelationFiltersSerde,
    pub skimmer: SkimmerMetricFiltersSerde,
    pub geo: GeoFiltersSerde,
    pub enrichment: EnrichmentFiltersSerde,
}

impl Default for FilterConfigSerde {
    fn default() -> Self {
        Self {
            max_age_secs: 15 * 60,
            freq_sanity_hz: Some((1_800_000, 54_000_000)),
            unknown_policy: UnknownFieldPolicy::Neutral,
            rf: RfFiltersSerde::default(),
            dx: CallsignFiltersSerde::default(),
            spotter: SpotterFiltersSerde::default(),
            content: ContentFiltersSerde::default(),
            correlation: CorrelationFiltersSerde::default(),
            skimmer: SkimmerMetricFiltersSerde::default(),
            geo: GeoFiltersSerde::default(),
            enrichment: EnrichmentFiltersSerde::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// RF filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RfFiltersSerde {
    pub band_allow: BTreeSet<Band>,
    pub band_deny: BTreeSet<Band>,

    pub mode_allow: BTreeSet<DxMode>,
    pub mode_deny: BTreeSet<DxMode>,

    /// Explicit frequency allow ranges (Hz). Empty = allow all (subject to deny).
    pub freq_allow_hz: Vec<(u64, u64)>,
    pub freq_deny_hz: Vec<(u64, u64)>,

    /// CC-style profiles: if non-empty, spot must match at least one profile.
    pub profiles: Vec<RfProfileSerde>,

    pub mode_policy: ModePolicy,
}

impl Default for RfFiltersSerde {
    fn default() -> Self {
        Self {
            band_allow: BTreeSet::new(),
            band_deny: BTreeSet::new(),
            mode_allow: BTreeSet::new(),
            mode_deny: BTreeSet::new(),
            freq_allow_hz: vec![],
            freq_deny_hz: vec![],
            profiles: vec![],
            mode_policy: ModePolicy::PreferUpstreamFallbackInfer,
        }
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RfProfileSerde {
    pub name: String,
    pub ranges_hz: Vec<(u64, u64)>,
}

// ---------------------------------------------------------------------------
// Callsign filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CallsignFiltersSerde {
    pub allow: PatternSetSerde,
    pub deny: PatternSetSerde,
    pub normalization: CallsignNormalizationSerde,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CallsignNormalizationSerde {
    pub uppercase: bool,
    pub portable_policy: PortablePolicy,
    pub strip_non_alnum: bool,
}

impl Default for CallsignNormalizationSerde {
    fn default() -> Self {
        Self {
            uppercase: true,
            portable_policy: PortablePolicy::KeepAsIs,
            strip_non_alnum: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Spotter filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpotterFiltersSerde {
    pub callsign: CallsignFiltersSerde,
    pub source_allow: BTreeSet<String>,
    pub source_deny: BTreeSet<String>,
    pub allow_human: bool,
    pub allow_skimmer: bool,
    pub allow_unknown_kind: bool,
}

impl Default for SpotterFiltersSerde {
    fn default() -> Self {
        Self {
            callsign: CallsignFiltersSerde::default(),
            source_allow: BTreeSet::new(),
            source_deny: BTreeSet::new(),
            allow_human: true,
            allow_skimmer: true,
            allow_unknown_kind: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Content / comment filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ContentFiltersSerde {
    pub comment_allow: PatternSetSerde,
    pub comment_deny: PatternSetSerde,
    pub cq_only: bool,
    pub suppress_split_qsy: bool,
    pub require_comment_when_allowlist_present: bool,
}

impl Default for ContentFiltersSerde {
    fn default() -> Self {
        Self {
            comment_allow: PatternSetSerde::default(),
            comment_deny: PatternSetSerde::default(),
            cq_only: false,
            suppress_split_qsy: false,
            require_comment_when_allowlist_present: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Correlation filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CorrelationFiltersSerde {
    pub min_unique_originators: u8,
    pub originators_human_only: bool,
    pub min_unique_sources: u8,
    pub min_observations: u8,
    pub window_secs: u64,
    pub min_repeats_same_key: u8,
}

impl Default for CorrelationFiltersSerde {
    fn default() -> Self {
        Self {
            min_unique_originators: 1,
            originators_human_only: false,
            min_unique_sources: 1,
            min_observations: 1,
            window_secs: 180,
            min_repeats_same_key: 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Skimmer metric filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SkimmerMetricFiltersSerde {
    /// Optional SNR range (min_db, max_db).
    pub snr_db: Option<(i8, i8)>,
    /// Optional WPM range (min, max).
    pub wpm: Option<(u16, u16)>,
    pub drop_dupes: bool,
    pub require_cq: bool,
}

// ---------------------------------------------------------------------------
// Geo / entity filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GeoFiltersSerde {
    pub dx: EntityFiltersSerde,
    pub spotter: EntityFiltersSerde,
    pub require_resolvers: bool,
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EntityFiltersSerde {
    pub continent_allow: BTreeSet<Continent>,
    pub continent_deny: BTreeSet<Continent>,
    pub cq_zone_allow: BTreeSet<u8>,
    pub cq_zone_deny: BTreeSet<u8>,
    pub itu_zone_allow: BTreeSet<u8>,
    pub itu_zone_deny: BTreeSet<u8>,

    pub entity_allow: BTreeSet<String>,
    pub entity_deny: BTreeSet<String>,

    pub country_allow: BTreeSet<String>,
    pub country_deny: BTreeSet<String>,

    pub state_allow: BTreeSet<String>,
    pub state_deny: BTreeSet<String>,

    pub grid_allow: PatternSetSerde,
    pub grid_deny: PatternSetSerde,
}

// ---------------------------------------------------------------------------
// Enrichment filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EnrichmentFiltersSerde {
    pub lotw: TriState,
    pub in_master_db: TriState,
    pub in_callbook: TriState,
    pub membership: Vec<MembershipRuleSerde>,
}

impl Default for EnrichmentFiltersSerde {
    fn default() -> Self {
        Self {
            lotw: TriState::Any,
            in_master_db: TriState::Any,
            in_callbook: TriState::Any,
            membership: Vec::new(),
        }
    }
}

/// A membership requirement or exclusion rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MembershipRuleSerde {
    Require(String),
    Deny(String),
}

// ---------------------------------------------------------------------------
// Pattern set (globs + regexes)
// ---------------------------------------------------------------------------

/// A set of glob and/or regex patterns for matching callsigns, comments, etc.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PatternSetSerde {
    #[cfg_attr(feature = "serde", serde(default))]
    pub globs: Vec<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub regexes: Vec<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub case_insensitive: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let cfg = FilterConfigSerde::default();
        assert_eq!(cfg.max_age_secs, 15 * 60);
        assert_eq!(cfg.freq_sanity_hz, Some((1_800_000, 54_000_000)));
        assert_eq!(cfg.unknown_policy, UnknownFieldPolicy::Neutral);
        assert!(cfg.rf.band_allow.is_empty());
        assert!(cfg.rf.band_deny.is_empty());
        assert!(cfg.dx.allow.globs.is_empty());
        assert!(cfg.spotter.allow_human);
        assert!(cfg.spotter.allow_skimmer);
        assert!(!cfg.content.cq_only);
        assert_eq!(cfg.correlation.min_unique_originators, 1);
        assert_eq!(cfg.correlation.window_secs, 180);
        assert!(cfg.skimmer.snr_db.is_none());
        assert!(!cfg.geo.require_resolvers);
        assert_eq!(cfg.enrichment.lotw, TriState::Any);
    }

    #[test]
    fn membership_rule_ord() {
        // Verify Ord works (needed if we ever put these in a BTreeSet)
        let r1 = MembershipRuleSerde::Require("FOC".into());
        let r2 = MembershipRuleSerde::Deny("FOC".into());
        // Require < Deny by declaration order (discriminant)
        assert!(r1 < r2);
    }

    #[test]
    fn pattern_set_default_is_empty() {
        let ps = PatternSetSerde::default();
        assert!(ps.globs.is_empty());
        assert!(ps.regexes.is_empty());
        assert!(!ps.case_insensitive);
    }

    #[cfg(feature = "serde")]
    mod serde_tests {
        use super::*;

        #[test]
        fn default_config_json_round_trip() {
            let cfg = FilterConfigSerde::default();
            let json = serde_json::to_string_pretty(&cfg).unwrap();
            let back: FilterConfigSerde = serde_json::from_str(&json).unwrap();
            assert_eq!(back.max_age_secs, cfg.max_age_secs);
            assert_eq!(back.freq_sanity_hz, cfg.freq_sanity_hz);
            assert_eq!(back.unknown_policy, cfg.unknown_policy);
            assert_eq!(back.rf.mode_policy, cfg.rf.mode_policy);
            assert_eq!(back.correlation.window_secs, cfg.correlation.window_secs);
            assert_eq!(back.enrichment.lotw, cfg.enrichment.lotw);
        }

        #[test]
        fn config_with_bands_round_trip() {
            let mut cfg = FilterConfigSerde::default();
            cfg.rf.band_allow.insert(Band::B20);
            cfg.rf.band_allow.insert(Band::B40);
            cfg.rf.mode_allow.insert(DxMode::CW);

            let json = serde_json::to_string(&cfg).unwrap();
            let back: FilterConfigSerde = serde_json::from_str(&json).unwrap();
            assert!(back.rf.band_allow.contains(&Band::B20));
            assert!(back.rf.band_allow.contains(&Band::B40));
            assert!(back.rf.mode_allow.contains(&DxMode::CW));
            assert_eq!(back.rf.band_allow.len(), 2);
        }

        #[test]
        fn config_with_patterns_round_trip() {
            let mut cfg = FilterConfigSerde::default();
            cfg.dx.allow.globs.push("W1*".into());
            cfg.dx.deny.regexes.push("^(VE|VA)".into());
            cfg.dx.allow.case_insensitive = true;

            let json = serde_json::to_string(&cfg).unwrap();
            let back: FilterConfigSerde = serde_json::from_str(&json).unwrap();
            assert_eq!(back.dx.allow.globs, vec!["W1*"]);
            assert_eq!(back.dx.deny.regexes, vec!["^(VE|VA)"]);
            assert!(back.dx.allow.case_insensitive);
        }

        #[test]
        fn config_with_geo_round_trip() {
            let mut cfg = FilterConfigSerde::default();
            cfg.geo.dx.continent_allow.insert(Continent::EU);
            cfg.geo.dx.cq_zone_allow.insert(14);
            cfg.geo.dx.cq_zone_allow.insert(15);
            cfg.geo.require_resolvers = true;

            let json = serde_json::to_string(&cfg).unwrap();
            let back: FilterConfigSerde = serde_json::from_str(&json).unwrap();
            assert!(back.geo.dx.continent_allow.contains(&Continent::EU));
            assert!(back.geo.dx.cq_zone_allow.contains(&14));
            assert!(back.geo.dx.cq_zone_allow.contains(&15));
            assert!(back.geo.require_resolvers);
        }

        #[test]
        fn config_with_enrichment_round_trip() {
            let mut cfg = FilterConfigSerde::default();
            cfg.enrichment.lotw = TriState::RequireTrue;
            cfg.enrichment.membership.push(MembershipRuleSerde::Require("FOC".into()));
            cfg.enrichment.membership.push(MembershipRuleSerde::Deny("SPAM".into()));

            let json = serde_json::to_string(&cfg).unwrap();
            let back: FilterConfigSerde = serde_json::from_str(&json).unwrap();
            assert_eq!(back.enrichment.lotw, TriState::RequireTrue);
            assert_eq!(back.enrichment.membership.len(), 2);
        }

        #[test]
        fn config_with_skimmer_metrics_round_trip() {
            let mut cfg = FilterConfigSerde::default();
            cfg.skimmer.snr_db = Some((5, 40));
            cfg.skimmer.wpm = Some((15, 45));
            cfg.skimmer.drop_dupes = true;
            cfg.skimmer.require_cq = true;

            let json = serde_json::to_string(&cfg).unwrap();
            let back: FilterConfigSerde = serde_json::from_str(&json).unwrap();
            assert_eq!(back.skimmer.snr_db, Some((5, 40)));
            assert_eq!(back.skimmer.wpm, Some((15, 45)));
            assert!(back.skimmer.drop_dupes);
            assert!(back.skimmer.require_cq);
        }
    }
}
