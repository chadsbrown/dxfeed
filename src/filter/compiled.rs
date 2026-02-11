//! Validated, compiled runtime filter configuration.
//!
//! These types are produced by `FilterConfigSerde::validate_and_compile()`
//! and used by the filter evaluator on the hot path.

use std::collections::BTreeSet;
use std::ops::RangeInclusive;
use std::time::Duration;

use crate::domain::{Band, Continent, DxMode, ModePolicy, TriState, UnknownFieldPolicy};
use crate::filter::config::{CallsignNormalizationSerde, FilterConfigSerde, MembershipRuleSerde};
use crate::filter::error::FilterConfigError;
use crate::filter::pattern::CompiledPatternSet;

// ---------------------------------------------------------------------------
// Top-level compiled config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FilterConfig {
    pub max_age: Duration,
    pub freq_sanity: Option<RangeInclusive<u64>>,
    pub unknown_policy: UnknownFieldPolicy,

    pub rf: RfFilters,
    pub dx: CallsignFilters,
    pub spotter: SpotterFilters,
    pub content: ContentFilters,
    pub correlation: CorrelationFilters,
    pub skimmer: SkimmerMetricFilters,
    pub geo: GeoFilters,
    pub enrichment: EnrichmentFilters,
}

// ---------------------------------------------------------------------------
// Sub-configs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RfFilters {
    pub band_allow: BTreeSet<Band>,
    pub band_deny: BTreeSet<Band>,
    pub mode_allow: BTreeSet<DxMode>,
    pub mode_deny: BTreeSet<DxMode>,
    pub freq_allow_hz: Vec<RangeInclusive<u64>>,
    pub freq_deny_hz: Vec<RangeInclusive<u64>>,
    pub profiles: Vec<RfProfile>,
    pub mode_policy: ModePolicy,
}

#[derive(Debug, Clone)]
pub struct RfProfile {
    pub name: String,
    pub ranges_hz: Vec<RangeInclusive<u64>>,
}

#[derive(Debug, Clone)]
pub struct CallsignFilters {
    pub allow: CompiledPatternSet,
    pub deny: CompiledPatternSet,
    pub normalization: CallsignNormalizationSerde,
}

#[derive(Debug, Clone)]
pub struct SpotterFilters {
    pub callsign: CallsignFilters,
    pub source_allow: BTreeSet<String>,
    pub source_deny: BTreeSet<String>,
    pub allow_human: bool,
    pub allow_skimmer: bool,
    pub allow_unknown_kind: bool,
}

#[derive(Debug, Clone)]
pub struct ContentFilters {
    pub comment_allow: CompiledPatternSet,
    pub comment_deny: CompiledPatternSet,
    pub cq_only: bool,
    pub suppress_split_qsy: bool,
    pub require_comment_when_allowlist_present: bool,
    /// Pre-compiled regex for detecting split/QSY keywords.
    pub split_qsy_regex: Option<regex::Regex>,
    /// Pre-compiled regex for detecting CQ in comments.
    pub cq_regex: Option<regex::Regex>,
}

#[derive(Debug, Clone)]
pub struct CorrelationFilters {
    pub min_unique_originators: u8,
    pub originators_human_only: bool,
    pub min_unique_sources: u8,
    pub min_observations: u8,
    pub window: Duration,
    pub min_repeats_same_key: u8,
}

#[derive(Debug, Clone)]
pub struct SkimmerMetricFilters {
    pub snr_db: Option<RangeInclusive<i8>>,
    pub wpm: Option<RangeInclusive<u16>>,
    pub drop_dupes: bool,
    pub require_cq: bool,
}

#[derive(Debug, Clone)]
pub struct GeoFilters {
    pub dx: EntityFilters,
    pub spotter: EntityFilters,
    pub require_resolvers: bool,
}

#[derive(Debug, Clone)]
pub struct EntityFilters {
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

    pub grid_allow: CompiledPatternSet,
    pub grid_deny: CompiledPatternSet,
}

#[derive(Debug, Clone)]
pub struct EnrichmentFilters {
    pub lotw: TriState,
    pub in_master_db: TriState,
    pub in_callbook: TriState,
    pub membership: Vec<MembershipRuleSerde>,
}

// ---------------------------------------------------------------------------
// validate_and_compile
// ---------------------------------------------------------------------------

impl FilterConfigSerde {
    /// Validate all configuration values and compile patterns into a runtime
    /// `FilterConfig`. Returns an error if any value is invalid.
    pub fn validate_and_compile(self) -> Result<FilterConfig, FilterConfigError> {
        if self.max_age_secs == 0 {
            return Err(FilterConfigError::MaxAgeZero);
        }
        let max_age = Duration::from_secs(self.max_age_secs);

        // Validate frequency sanity range
        let freq_sanity = match self.freq_sanity_hz {
            Some((a, b)) => {
                if a > b {
                    return Err(FilterConfigError::InvalidFreqRange(a, b));
                }
                Some(a..=b)
            }
            None => None,
        };

        // RF filters
        let rf = {
            let mut allow = Vec::new();
            for (a, b) in self.rf.freq_allow_hz {
                if a > b {
                    return Err(FilterConfigError::InvalidFreqRange(a, b));
                }
                allow.push(a..=b);
            }
            let mut deny = Vec::new();
            for (a, b) in self.rf.freq_deny_hz {
                if a > b {
                    return Err(FilterConfigError::InvalidFreqRange(a, b));
                }
                deny.push(a..=b);
            }

            let profiles = self
                .rf
                .profiles
                .into_iter()
                .map(|p| {
                    let mut ranges = Vec::new();
                    for (a, b) in p.ranges_hz {
                        if a > b {
                            return Err(FilterConfigError::InvalidFreqRange(a, b));
                        }
                        ranges.push(a..=b);
                    }
                    Ok(RfProfile {
                        name: p.name,
                        ranges_hz: ranges,
                    })
                })
                .collect::<Result<Vec<_>, FilterConfigError>>()?;

            RfFilters {
                band_allow: self.rf.band_allow,
                band_deny: self.rf.band_deny,
                mode_allow: self.rf.mode_allow,
                mode_deny: self.rf.mode_deny,
                freq_allow_hz: allow,
                freq_deny_hz: deny,
                profiles,
                mode_policy: self.rf.mode_policy,
            }
        };

        // DX callsign filters
        let dx = CallsignFilters {
            allow: CompiledPatternSet::compile(self.dx.allow)?,
            deny: CompiledPatternSet::compile(self.dx.deny)?,
            normalization: self.dx.normalization,
        };

        // Spotter filters
        let spotter = SpotterFilters {
            callsign: CallsignFilters {
                allow: CompiledPatternSet::compile(self.spotter.callsign.allow)?,
                deny: CompiledPatternSet::compile(self.spotter.callsign.deny)?,
                normalization: self.spotter.callsign.normalization,
            },
            source_allow: self.spotter.source_allow,
            source_deny: self.spotter.source_deny,
            allow_human: self.spotter.allow_human,
            allow_skimmer: self.spotter.allow_skimmer,
            allow_unknown_kind: self.spotter.allow_unknown_kind,
        };

        // Content filters (pre-compile helper regexes)
        let content = {
            let split_qsy_regex = if self.content.suppress_split_qsy {
                Some(regex::Regex::new(r"(?i)\b(UP|QSX|SPLIT|QSY)\b").unwrap())
            } else {
                None
            };
            let cq_regex = if self.content.cq_only {
                Some(regex::Regex::new(r"(?i)\bCQ\b").unwrap())
            } else {
                None
            };

            ContentFilters {
                comment_allow: CompiledPatternSet::compile(self.content.comment_allow)?,
                comment_deny: CompiledPatternSet::compile(self.content.comment_deny)?,
                cq_only: self.content.cq_only,
                suppress_split_qsy: self.content.suppress_split_qsy,
                require_comment_when_allowlist_present: self
                    .content
                    .require_comment_when_allowlist_present,
                split_qsy_regex,
                cq_regex,
            }
        };

        // Correlation filters (clamp minimums to 1)
        let correlation = CorrelationFilters {
            min_unique_originators: self.correlation.min_unique_originators.max(1),
            originators_human_only: self.correlation.originators_human_only,
            min_unique_sources: self.correlation.min_unique_sources.max(1),
            min_observations: self.correlation.min_observations.max(1),
            window: Duration::from_secs(self.correlation.window_secs.max(1)),
            min_repeats_same_key: self.correlation.min_repeats_same_key.max(1),
        };

        // Skimmer metric filters (auto-correct reversed ranges)
        let skimmer = {
            let snr_db = self
                .skimmer
                .snr_db
                .map(|(a, b)| if a <= b { a..=b } else { b..=a });
            let wpm = self
                .skimmer
                .wpm
                .map(|(a, b)| if a <= b { a..=b } else { b..=a });
            SkimmerMetricFilters {
                snr_db,
                wpm,
                drop_dupes: self.skimmer.drop_dupes,
                require_cq: self.skimmer.require_cq,
            }
        };

        // Geo filters
        let compile_entity = |e: crate::filter::config::EntityFiltersSerde| -> Result<EntityFilters, FilterConfigError> {
            Ok(EntityFilters {
                continent_allow: e.continent_allow,
                continent_deny: e.continent_deny,
                cq_zone_allow: e.cq_zone_allow,
                cq_zone_deny: e.cq_zone_deny,
                itu_zone_allow: e.itu_zone_allow,
                itu_zone_deny: e.itu_zone_deny,
                entity_allow: e.entity_allow,
                entity_deny: e.entity_deny,
                country_allow: e.country_allow,
                country_deny: e.country_deny,
                state_allow: e.state_allow,
                state_deny: e.state_deny,
                grid_allow: CompiledPatternSet::compile(e.grid_allow)?,
                grid_deny: CompiledPatternSet::compile(e.grid_deny)?,
            })
        };

        let geo = GeoFilters {
            dx: compile_entity(self.geo.dx)?,
            spotter: compile_entity(self.geo.spotter)?,
            require_resolvers: self.geo.require_resolvers,
        };

        // Enrichment filters
        let enrichment = EnrichmentFilters {
            lotw: self.enrichment.lotw,
            in_master_db: self.enrichment.in_master_db,
            in_callbook: self.enrichment.in_callbook,
            membership: self.enrichment.membership,
        };

        Ok(FilterConfig {
            max_age,
            freq_sanity,
            unknown_policy: self.unknown_policy,
            rf,
            dx,
            spotter,
            content,
            correlation,
            skimmer,
            geo,
            enrichment,
        })
    }
}

// ---------------------------------------------------------------------------
// Helper: check if EntityFilters has any active filters
// ---------------------------------------------------------------------------

impl EntityFilters {
    /// Returns true if any filter in this entity filter set is active.
    pub fn has_any_filters(&self) -> bool {
        !self.continent_allow.is_empty()
            || !self.continent_deny.is_empty()
            || !self.cq_zone_allow.is_empty()
            || !self.cq_zone_deny.is_empty()
            || !self.itu_zone_allow.is_empty()
            || !self.itu_zone_deny.is_empty()
            || !self.entity_allow.is_empty()
            || !self.entity_deny.is_empty()
            || !self.country_allow.is_empty()
            || !self.country_deny.is_empty()
            || !self.state_allow.is_empty()
            || !self.state_deny.is_empty()
            || !self.grid_allow.is_empty()
            || !self.grid_deny.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::*;
    use crate::filter::config::*;

    #[test]
    fn default_config_compiles() {
        let cfg = FilterConfigSerde::default();
        let compiled = cfg.validate_and_compile().unwrap();
        assert_eq!(compiled.max_age, std::time::Duration::from_secs(15 * 60));
        assert!(compiled.freq_sanity.is_some());
        assert!(compiled.rf.band_allow.is_empty());
        assert!(compiled.dx.allow.is_empty());
    }

    #[test]
    fn max_age_zero_errors() {
        let mut cfg = FilterConfigSerde::default();
        cfg.max_age_secs = 0;
        let result = cfg.validate_and_compile();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("max_age"));
    }

    #[test]
    fn reversed_freq_sanity_errors() {
        let mut cfg = FilterConfigSerde::default();
        cfg.freq_sanity_hz = Some((54_000_000, 1_800_000)); // reversed
        let result = cfg.validate_and_compile();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid frequency range"));
    }

    #[test]
    fn reversed_freq_allow_range_errors() {
        let mut cfg = FilterConfigSerde::default();
        cfg.rf.freq_allow_hz.push((14_100_000, 14_000_000)); // reversed
        let result = cfg.validate_and_compile();
        assert!(result.is_err());
    }

    #[test]
    fn reversed_freq_deny_range_errors() {
        let mut cfg = FilterConfigSerde::default();
        cfg.rf.freq_deny_hz.push((7_100_000, 7_000_000));
        let result = cfg.validate_and_compile();
        assert!(result.is_err());
    }

    #[test]
    fn reversed_profile_range_errors() {
        let mut cfg = FilterConfigSerde::default();
        cfg.rf.profiles.push(RfProfileSerde {
            name: "bad".into(),
            ranges_hz: vec![(14_100_000, 14_000_000)],
        });
        let result = cfg.validate_and_compile();
        assert!(result.is_err());
    }

    #[test]
    fn invalid_glob_in_dx_allow_errors() {
        let mut cfg = FilterConfigSerde::default();
        cfg.dx.allow.globs.push("[invalid".into());
        let result = cfg.validate_and_compile();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid glob"));
    }

    #[test]
    fn invalid_regex_in_dx_deny_errors() {
        let mut cfg = FilterConfigSerde::default();
        cfg.dx.deny.regexes.push("(unclosed".into());
        let result = cfg.validate_and_compile();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid regex"));
    }

    #[test]
    fn patterns_compile_correctly() {
        let mut cfg = FilterConfigSerde::default();
        cfg.dx.allow.globs.push("W1*".into());
        cfg.dx.deny.regexes.push("^(VE|VA)".into());

        let compiled = cfg.validate_and_compile().unwrap();
        assert!(compiled.dx.allow.is_match("W1AW"));
        assert!(!compiled.dx.allow.is_match("K1TTT"));
        assert!(compiled.dx.deny.is_match("VE3NEA"));
        assert!(!compiled.dx.deny.is_match("W1AW"));
    }

    #[test]
    fn content_cq_regex_compiled_when_enabled() {
        let mut cfg = FilterConfigSerde::default();
        cfg.content.cq_only = true;
        let compiled = cfg.validate_and_compile().unwrap();
        assert!(compiled.content.cq_regex.is_some());
        assert!(compiled.content.cq_regex.as_ref().unwrap().is_match("CQ"));
    }

    #[test]
    fn content_split_regex_compiled_when_enabled() {
        let mut cfg = FilterConfigSerde::default();
        cfg.content.suppress_split_qsy = true;
        let compiled = cfg.validate_and_compile().unwrap();
        assert!(compiled.content.split_qsy_regex.is_some());
        let rx = compiled.content.split_qsy_regex.as_ref().unwrap();
        assert!(rx.is_match("UP 5"));
        assert!(rx.is_match("QSX 14030"));
        assert!(rx.is_match("split"));
        assert!(rx.is_match("QSY"));
    }

    #[test]
    fn content_regexes_not_compiled_when_disabled() {
        let cfg = FilterConfigSerde::default();
        let compiled = cfg.validate_and_compile().unwrap();
        assert!(compiled.content.cq_regex.is_none());
        assert!(compiled.content.split_qsy_regex.is_none());
    }

    #[test]
    fn correlation_clamps_to_minimum_1() {
        let mut cfg = FilterConfigSerde::default();
        cfg.correlation.min_unique_originators = 0;
        cfg.correlation.min_unique_sources = 0;
        cfg.correlation.min_observations = 0;
        cfg.correlation.min_repeats_same_key = 0;
        cfg.correlation.window_secs = 0;

        let compiled = cfg.validate_and_compile().unwrap();
        assert_eq!(compiled.correlation.min_unique_originators, 1);
        assert_eq!(compiled.correlation.min_unique_sources, 1);
        assert_eq!(compiled.correlation.min_observations, 1);
        assert_eq!(compiled.correlation.min_repeats_same_key, 1);
        assert_eq!(compiled.correlation.window, std::time::Duration::from_secs(1));
    }

    #[test]
    fn skimmer_reversed_ranges_auto_corrected() {
        let mut cfg = FilterConfigSerde::default();
        cfg.skimmer.snr_db = Some((40, 5)); // reversed
        cfg.skimmer.wpm = Some((45, 15)); // reversed

        let compiled = cfg.validate_and_compile().unwrap();
        assert_eq!(compiled.skimmer.snr_db, Some(5..=40));
        assert_eq!(compiled.skimmer.wpm, Some(15..=45));
    }

    #[test]
    fn geo_filters_compile() {
        let mut cfg = FilterConfigSerde::default();
        cfg.geo.dx.continent_allow.insert(Continent::EU);
        cfg.geo.dx.cq_zone_allow.insert(14);
        cfg.geo.dx.grid_allow.globs.push("FN*".into());
        cfg.geo.require_resolvers = true;

        let compiled = cfg.validate_and_compile().unwrap();
        assert!(compiled.geo.dx.continent_allow.contains(&Continent::EU));
        assert!(compiled.geo.dx.cq_zone_allow.contains(&14));
        assert!(compiled.geo.dx.grid_allow.is_match("FN31"));
        assert!(compiled.geo.require_resolvers);
        assert!(compiled.geo.dx.has_any_filters());
    }

    #[test]
    fn entity_filters_has_any_filters() {
        let mut cfg = FilterConfigSerde::default();
        // Empty by default
        let compiled = cfg.validate_and_compile().unwrap();
        assert!(!compiled.geo.dx.has_any_filters());
        assert!(!compiled.geo.spotter.has_any_filters());

        // Add one filter
        cfg = FilterConfigSerde::default();
        cfg.geo.dx.cq_zone_deny.insert(3);
        let compiled = cfg.validate_and_compile().unwrap();
        assert!(compiled.geo.dx.has_any_filters());
    }

    #[test]
    fn none_freq_sanity_compiles() {
        let mut cfg = FilterConfigSerde::default();
        cfg.freq_sanity_hz = None;
        let compiled = cfg.validate_and_compile().unwrap();
        assert!(compiled.freq_sanity.is_none());
    }

    #[test]
    fn enrichment_compiles() {
        let mut cfg = FilterConfigSerde::default();
        cfg.enrichment.lotw = TriState::RequireTrue;
        cfg.enrichment
            .membership
            .push(MembershipRuleSerde::Require("FOC".into()));

        let compiled = cfg.validate_and_compile().unwrap();
        assert_eq!(compiled.enrichment.lotw, TriState::RequireTrue);
        assert_eq!(compiled.enrichment.membership.len(), 1);
    }

    #[test]
    fn spotter_patterns_compile() {
        let mut cfg = FilterConfigSerde::default();
        cfg.spotter.callsign.allow.globs.push("W3*".into());
        cfg.spotter.source_deny.insert("bad_node".into());
        cfg.spotter.allow_skimmer = false;

        let compiled = cfg.validate_and_compile().unwrap();
        assert!(compiled.spotter.callsign.allow.is_match("W3LPL"));
        assert!(compiled.spotter.source_deny.contains("bad_node"));
        assert!(!compiled.spotter.allow_skimmer);
    }

    #[test]
    fn profile_with_valid_ranges_compiles() {
        let mut cfg = FilterConfigSerde::default();
        cfg.rf.profiles.push(RfProfileSerde {
            name: "CW contest".into(),
            ranges_hz: vec![
                (14_000_000, 14_070_000),
                (7_000_000, 7_040_000),
            ],
        });

        let compiled = cfg.validate_and_compile().unwrap();
        assert_eq!(compiled.rf.profiles.len(), 1);
        assert_eq!(compiled.rf.profiles[0].name, "CW contest");
        assert_eq!(compiled.rf.profiles[0].ranges_hz.len(), 2);
    }
}
