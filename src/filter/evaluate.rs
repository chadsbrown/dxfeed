//! Deterministic filter evaluation.
//!
//! The `evaluate()` function is the hot path — called for every spot state
//! change in the aggregator. It is allocation-free by design.
//!
//! **Important:** Skimmer gating (SkimValid/Busted/Qsy/Unknown) is handled
//! by the aggregator *before* calling this function. This function only
//! applies the general filter pipeline.
//!
//! Evaluation order (must be preserved):
//! 1. Age
//! 2. Frequency sanity + RF (band/mode/freq ranges/profiles)
//! 3. DX callsign allow/deny
//! 4. Spotter/originator kind + call + source
//! 5. Content/comment
//! 6. Correlation thresholds
//! 7. Skimmer metrics
//! 8. Geo/entity
//! 9. Enrichment

use std::collections::HashSet;

use crate::domain::{OriginatorKind, TriState, UnknownFieldPolicy};
use crate::filter::compiled::{EntityFilters, FilterConfig};
use crate::filter::config::MembershipRuleSerde;
use crate::filter::decision::{FilterDecision, FilterDropReason};
use crate::model::{GeoResolved, SpotView};

/// Evaluate a spot view against a compiled filter configuration.
///
/// Returns `FilterDecision::Allow` if the spot passes all filters,
/// or `FilterDecision::Drop(reason)` with the first failing reason.
pub fn evaluate(view: &SpotView<'_>, cfg: &FilterConfig) -> FilterDecision {
    // 1) Age
    if let Ok(age) = (view.now - view.last_seen).to_std() {
        if age > cfg.max_age {
            return FilterDecision::Drop(FilterDropReason::TooOld);
        }
    }
    // If last_seen is in the future relative to now, the age is negative — allow.

    // 2) Frequency sanity
    if let Some(sanity) = &cfg.freq_sanity {
        if !sanity.contains(&view.freq_hz) {
            return FilterDecision::Drop(FilterDropReason::FreqOutOfRange);
        }
    }

    // Band deny/allow
    if cfg.rf.band_deny.contains(&view.band) {
        return FilterDecision::Drop(FilterDropReason::BandDenied);
    }
    if !cfg.rf.band_allow.is_empty() && !cfg.rf.band_allow.contains(&view.band) {
        return FilterDecision::Drop(FilterDropReason::BandDenied);
    }

    // Mode deny/allow
    if cfg.rf.mode_deny.contains(&view.mode) {
        return FilterDecision::Drop(FilterDropReason::ModeDenied);
    }
    if !cfg.rf.mode_allow.is_empty() && !cfg.rf.mode_allow.contains(&view.mode) {
        return FilterDecision::Drop(FilterDropReason::ModeDenied);
    }

    // Frequency deny ranges
    if cfg
        .rf
        .freq_deny_hz
        .iter()
        .any(|r| r.contains(&view.freq_hz))
    {
        return FilterDecision::Drop(FilterDropReason::FreqDenied);
    }
    // Frequency allow ranges (if any configured)
    if !cfg.rf.freq_allow_hz.is_empty()
        && !cfg
            .rf
            .freq_allow_hz
            .iter()
            .any(|r| r.contains(&view.freq_hz))
    {
        return FilterDecision::Drop(FilterDropReason::FreqNotAllowed);
    }
    // Profiles: if present, must match at least one
    if !cfg.rf.profiles.is_empty() {
        let ok = cfg
            .rf
            .profiles
            .iter()
            .any(|p| p.ranges_hz.iter().any(|r| r.contains(&view.freq_hz)));
        if !ok {
            return FilterDecision::Drop(FilterDropReason::ProfileNoMatch);
        }
    }

    // 3) DX callsign allow/deny
    if cfg.dx.deny.is_match(view.dx_call_norm) {
        return FilterDecision::Drop(FilterDropReason::DxCallDenied);
    }
    if !cfg.dx.allow.is_empty() && !cfg.dx.allow.is_match(view.dx_call_norm) {
        return FilterDecision::Drop(FilterDropReason::DxCallNotAllowed);
    }

    // 4) Spotter/originator
    match view.originator_kind {
        OriginatorKind::Human if !cfg.spotter.allow_human => {
            return FilterDecision::Drop(FilterDropReason::OriginatorKindDenied);
        }
        OriginatorKind::Skimmer if !cfg.spotter.allow_skimmer => {
            return FilterDecision::Drop(FilterDropReason::OriginatorKindDenied);
        }
        OriginatorKind::Unknown if !cfg.spotter.allow_unknown_kind => {
            return FilterDecision::Drop(FilterDropReason::OriginatorKindDenied);
        }
        _ => {}
    }

    // Spotter source allow/deny
    if let Some(source) = view.spotter_source_id {
        if cfg.spotter.source_deny.contains(source) {
            return FilterDecision::Drop(FilterDropReason::SpotterDenied);
        }
        if !cfg.spotter.source_allow.is_empty() && !cfg.spotter.source_allow.contains(source) {
            return FilterDecision::Drop(FilterDropReason::SpotterNotAllowed);
        }
    } else if matches!(cfg.unknown_policy, UnknownFieldPolicy::FailClosed)
        && (!cfg.spotter.source_allow.is_empty() || !cfg.spotter.source_deny.is_empty())
    {
        return FilterDecision::Drop(FilterDropReason::UnknownFieldFailed);
    }

    // Spotter callsign allow/deny
    if let Some(spotter_call) = view.spotter_call_norm {
        if cfg.spotter.callsign.deny.is_match(spotter_call) {
            return FilterDecision::Drop(FilterDropReason::SpotterDenied);
        }
        if !cfg.spotter.callsign.allow.is_empty()
            && !cfg.spotter.callsign.allow.is_match(spotter_call)
        {
            return FilterDecision::Drop(FilterDropReason::SpotterNotAllowed);
        }
    } else if matches!(cfg.unknown_policy, UnknownFieldPolicy::FailClosed)
        && (!cfg.spotter.callsign.allow.is_empty() || !cfg.spotter.callsign.deny.is_empty())
    {
        return FilterDecision::Drop(FilterDropReason::UnknownFieldFailed);
    }

    // 5) Content/comment
    let comment = view.comment.unwrap_or("");

    if cfg.content.suppress_split_qsy {
        if let Some(rx) = &cfg.content.split_qsy_regex {
            if rx.is_match(comment) {
                return FilterDecision::Drop(FilterDropReason::SplitQsySuppressed);
            }
        }
    }

    if cfg.content.cq_only {
        let cq_ok = view.is_skimmer_cq
            || cfg
                .content
                .cq_regex
                .as_ref()
                .is_some_and(|rx| rx.is_match(comment));
        if !cq_ok {
            return FilterDecision::Drop(FilterDropReason::CqRequired);
        }
    }

    if cfg.content.comment_deny.is_match(comment) {
        return FilterDecision::Drop(FilterDropReason::CommentDenied);
    }
    if !cfg.content.comment_allow.is_empty() {
        if comment.is_empty() && cfg.content.require_comment_when_allowlist_present {
            return FilterDecision::Drop(FilterDropReason::CommentNotAllowed);
        }
        if !cfg.content.comment_allow.is_match(comment) {
            return FilterDecision::Drop(FilterDropReason::CommentNotAllowed);
        }
    }

    // 6) Correlation thresholds
    let uo = if cfg.correlation.originators_human_only {
        view.unique_human_originators
    } else {
        view.unique_originators
    };
    if uo < cfg.correlation.min_unique_originators
        || view.unique_sources < cfg.correlation.min_unique_sources
        || view.total_observations_in_window < cfg.correlation.min_observations
        || view.repeats_same_key_in_window < cfg.correlation.min_repeats_same_key
    {
        return FilterDecision::Drop(FilterDropReason::CorrelationNotMet);
    }

    // 7) Skimmer metric filters (only for skimmer-originated spots)
    if view.originator_kind == OriginatorKind::Skimmer {
        if cfg.skimmer.drop_dupes && view.is_skimmer_dupe {
            return FilterDecision::Drop(FilterDropReason::SkimmerMetricNotMet);
        }
        if cfg.skimmer.require_cq && !view.is_skimmer_cq {
            return FilterDecision::Drop(FilterDropReason::SkimmerMetricNotMet);
        }
        if let Some(r) = &cfg.skimmer.snr_db {
            if let Some(snr) = view.snr_db {
                if !r.contains(&snr) {
                    return FilterDecision::Drop(FilterDropReason::SkimmerMetricNotMet);
                }
            } else if matches!(cfg.unknown_policy, UnknownFieldPolicy::FailClosed) {
                return FilterDecision::Drop(FilterDropReason::UnknownFieldFailed);
            }
        }
        if let Some(r) = &cfg.skimmer.wpm {
            if let Some(wpm) = view.wpm {
                if !r.contains(&wpm) {
                    return FilterDecision::Drop(FilterDropReason::SkimmerMetricNotMet);
                }
            } else if matches!(cfg.unknown_policy, UnknownFieldPolicy::FailClosed) {
                return FilterDecision::Drop(FilterDropReason::UnknownFieldFailed);
            }
        }
    }

    // 8) Geo/entity
    if let FilterDecision::Drop(r) = eval_geo(view, cfg) {
        return FilterDecision::Drop(r);
    }

    // 9) Enrichment
    if let FilterDecision::Drop(r) = eval_enrichment(view, cfg) {
        return FilterDecision::Drop(r);
    }

    FilterDecision::Allow
}

// ---------------------------------------------------------------------------
// Geo evaluation (with fixed CQ/ITU zone checks)
// ---------------------------------------------------------------------------

fn eval_geo(view: &SpotView<'_>, cfg: &FilterConfig) -> FilterDecision {
    let geo_needed = cfg.geo.require_resolvers
        || cfg.geo.dx.has_any_filters()
        || cfg.geo.spotter.has_any_filters();

    if !geo_needed {
        return FilterDecision::Allow;
    }

    if cfg.geo.require_resolvers
        && view.dx_geo.entity.is_none()
        && view.dx_geo.country.is_none()
    {
        return unknown_to_decision(cfg.unknown_policy, true);
    }

    if let FilterDecision::Drop(r) = eval_entity(&view.dx_geo, &cfg.geo.dx, cfg.unknown_policy) {
        return FilterDecision::Drop(r);
    }
    if let FilterDecision::Drop(r) =
        eval_entity(&view.spotter_geo, &cfg.geo.spotter, cfg.unknown_policy)
    {
        return FilterDecision::Drop(r);
    }

    FilterDecision::Allow
}

fn eval_entity(
    res: &GeoResolved<'_>,
    f: &EntityFilters,
    policy: UnknownFieldPolicy,
) -> FilterDecision {
    // Continent
    if !f.continent_allow.is_empty() || !f.continent_deny.is_empty() {
        if let Some(c) = res.continent {
            if f.continent_deny.contains(&c) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
            if !f.continent_allow.is_empty() && !f.continent_allow.contains(&c) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
        } else {
            let decision = if !f.continent_allow.is_empty() {
                // Allowlist configured but field unknown
                unknown_to_decision(policy, true)
            } else {
                // Only denylist configured, field unknown
                unknown_to_decision(policy, false)
            };
            if let FilterDecision::Drop(r) = decision {
                return FilterDecision::Drop(r);
            }
        }
    }

    // CQ Zone (fixed: was missing in reference code)
    if !f.cq_zone_allow.is_empty() || !f.cq_zone_deny.is_empty() {
        if let Some(z) = res.cq_zone {
            if f.cq_zone_deny.contains(&z) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
            if !f.cq_zone_allow.is_empty() && !f.cq_zone_allow.contains(&z) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
        } else {
            let decision = if !f.cq_zone_allow.is_empty() {
                unknown_to_decision(policy, true)
            } else {
                unknown_to_decision(policy, false)
            };
            if let FilterDecision::Drop(r) = decision {
                return FilterDecision::Drop(r);
            }
        }
    }

    // ITU Zone (fixed: was missing in reference code)
    if !f.itu_zone_allow.is_empty() || !f.itu_zone_deny.is_empty() {
        if let Some(z) = res.itu_zone {
            if f.itu_zone_deny.contains(&z) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
            if !f.itu_zone_allow.is_empty() && !f.itu_zone_allow.contains(&z) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
        } else {
            let decision = if !f.itu_zone_allow.is_empty() {
                unknown_to_decision(policy, true)
            } else {
                unknown_to_decision(policy, false)
            };
            if let FilterDecision::Drop(r) = decision {
                return FilterDecision::Drop(r);
            }
        }
    }

    // Entity
    if !f.entity_allow.is_empty() || !f.entity_deny.is_empty() {
        if let Some(ent) = res.entity {
            if f.entity_deny.contains(ent) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
            if !f.entity_allow.is_empty() && !f.entity_allow.contains(ent) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
        } else {
            let decision = if !f.entity_allow.is_empty() {
                unknown_to_decision(policy, true)
            } else {
                unknown_to_decision(policy, false)
            };
            if let FilterDecision::Drop(r) = decision {
                return FilterDecision::Drop(r);
            }
        }
    }

    // Country
    if !f.country_allow.is_empty() || !f.country_deny.is_empty() {
        if let Some(cty) = res.country {
            if f.country_deny.contains(cty) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
            if !f.country_allow.is_empty() && !f.country_allow.contains(cty) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
        } else {
            let decision = if !f.country_allow.is_empty() {
                unknown_to_decision(policy, true)
            } else {
                unknown_to_decision(policy, false)
            };
            if let FilterDecision::Drop(r) = decision {
                return FilterDecision::Drop(r);
            }
        }
    }

    // State
    if !f.state_allow.is_empty() || !f.state_deny.is_empty() {
        if let Some(st) = res.state {
            if f.state_deny.contains(st) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
            if !f.state_allow.is_empty() && !f.state_allow.contains(st) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
        } else {
            let decision = if !f.state_allow.is_empty() {
                unknown_to_decision(policy, true)
            } else {
                unknown_to_decision(policy, false)
            };
            if let FilterDecision::Drop(r) = decision {
                return FilterDecision::Drop(r);
            }
        }
    }

    // Grid
    if !f.grid_deny.is_empty() || !f.grid_allow.is_empty() {
        if let Some(grid) = res.grid {
            if f.grid_deny.is_match(grid) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
            if !f.grid_allow.is_empty() && !f.grid_allow.is_match(grid) {
                return FilterDecision::Drop(FilterDropReason::GeoNotMet);
            }
        } else {
            let decision = if !f.grid_allow.is_empty() {
                unknown_to_decision(policy, true)
            } else {
                unknown_to_decision(policy, false)
            };
            if let FilterDecision::Drop(r) = decision {
                return FilterDecision::Drop(r);
            }
        }
    }

    FilterDecision::Allow
}

// ---------------------------------------------------------------------------
// Enrichment evaluation
// ---------------------------------------------------------------------------

fn eval_enrichment(view: &SpotView<'_>, cfg: &FilterConfig) -> FilterDecision {
    if let FilterDecision::Drop(r) =
        eval_tristate(view.lotw, cfg.enrichment.lotw, cfg.unknown_policy)
    {
        return FilterDecision::Drop(r);
    }
    if let FilterDecision::Drop(r) = eval_tristate(
        view.in_master_db,
        cfg.enrichment.in_master_db,
        cfg.unknown_policy,
    ) {
        return FilterDecision::Drop(r);
    }
    if let FilterDecision::Drop(r) = eval_tristate(
        view.in_callbook,
        cfg.enrichment.in_callbook,
        cfg.unknown_policy,
    ) {
        return FilterDecision::Drop(r);
    }

    if !cfg.enrichment.membership.is_empty() {
        let Some(have) = view.memberships else {
            return unknown_to_decision(cfg.unknown_policy, true);
        };
        for rule in &cfg.enrichment.membership {
            match rule {
                MembershipRuleSerde::Require(tag) => {
                    if !have.contains(tag) {
                        return FilterDecision::Drop(FilterDropReason::EnrichmentNotMet);
                    }
                }
                MembershipRuleSerde::Deny(tag) => {
                    if have.contains(tag) {
                        return FilterDecision::Drop(FilterDropReason::EnrichmentNotMet);
                    }
                }
            }
        }
    }

    FilterDecision::Allow
}

fn eval_tristate(
    value: Option<bool>,
    rule: TriState,
    policy: UnknownFieldPolicy,
) -> FilterDecision {
    match rule {
        TriState::Any => FilterDecision::Allow,
        TriState::RequireTrue => match value {
            Some(true) => FilterDecision::Allow,
            Some(false) => FilterDecision::Drop(FilterDropReason::EnrichmentNotMet),
            None => unknown_to_decision(policy, true),
        },
        TriState::RequireFalse => match value {
            Some(false) => FilterDecision::Allow,
            Some(true) => FilterDecision::Drop(FilterDropReason::EnrichmentNotMet),
            None => unknown_to_decision(policy, true),
        },
    }
}

/// Convert unknown field policy to a filter decision.
///
/// `is_allowlist_context`: true when we are checking an allowlist (unknown should
/// generally fail) vs a denylist (unknown should generally pass).
///
/// - `Neutral`: fail for allowlists (unknown doesn't prove membership), pass for denylists
/// - `FailClosed`: always drop
/// - `FailOpen`: always allow
fn unknown_to_decision(policy: UnknownFieldPolicy, is_allowlist_context: bool) -> FilterDecision {
    match policy {
        UnknownFieldPolicy::Neutral => {
            if is_allowlist_context {
                FilterDecision::Drop(FilterDropReason::UnknownFieldFailed)
            } else {
                FilterDecision::Allow
            }
        }
        UnknownFieldPolicy::FailClosed => {
            FilterDecision::Drop(FilterDropReason::UnknownFieldFailed)
        }
        UnknownFieldPolicy::FailOpen => FilterDecision::Allow,
    }
}

// ---------------------------------------------------------------------------
// Helper: count unique originators
// ---------------------------------------------------------------------------

/// Count unique originators from a list of (callsign, kind) pairs.
pub fn count_unique_originators(originators: &[(&str, OriginatorKind)], human_only: bool) -> u8 {
    let mut set = HashSet::<&str>::new();
    for (call, k) in originators {
        if human_only && *k != OriginatorKind::Human {
            continue;
        }
        set.insert(call);
    }
    u8::try_from(set.len()).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    use crate::filter::config::*;
    use crate::model::*;
    use chrono::Utc;
    use std::collections::BTreeSet;

    // Helper: compile a default config with modifications
    fn compile(f: impl FnOnce(&mut FilterConfigSerde)) -> FilterConfig {
        let mut cfg = FilterConfigSerde::default();
        f(&mut cfg);
        cfg.validate_and_compile().unwrap()
    }

    fn default_view() -> SpotView<'static> {
        SpotView::test_default(Utc::now(), "W1AW", 14_025_000, Band::B20, DxMode::CW)
    }

    // -----------------------------------------------------------------------
    // Default config allows everything
    // -----------------------------------------------------------------------

    #[test]
    fn default_config_allows_normal_spot() {
        let cfg = FilterConfigSerde::default().validate_and_compile().unwrap();
        let view = default_view();
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    // -----------------------------------------------------------------------
    // Age
    // -----------------------------------------------------------------------

    #[test]
    fn too_old_spot_dropped() {
        let cfg = compile(|c| c.max_age_secs = 60);
        let now = Utc::now();
        let mut view = SpotView::test_default(now, "W1AW", 14_025_000, Band::B20, DxMode::CW);
        view.last_seen = now - chrono::Duration::seconds(120);
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::TooOld)
        );
    }

    #[test]
    fn young_spot_passes_age() {
        let cfg = compile(|c| c.max_age_secs = 60);
        let now = Utc::now();
        let mut view = SpotView::test_default(now, "W1AW", 14_025_000, Band::B20, DxMode::CW);
        view.last_seen = now - chrono::Duration::seconds(30);
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    // -----------------------------------------------------------------------
    // Frequency sanity
    // -----------------------------------------------------------------------

    #[test]
    fn freq_below_sanity_dropped() {
        let cfg = compile(|_| {});
        let mut view = default_view();
        view.freq_hz = 1_000_000; // below 1.8 MHz
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::FreqOutOfRange)
        );
    }

    #[test]
    fn freq_above_sanity_dropped() {
        let cfg = compile(|_| {});
        let mut view = default_view();
        view.freq_hz = 60_000_000; // above 54 MHz
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::FreqOutOfRange)
        );
    }

    // -----------------------------------------------------------------------
    // Band allow/deny
    // -----------------------------------------------------------------------

    #[test]
    fn band_deny_drops() {
        let cfg = compile(|c| {
            c.rf.band_deny.insert(Band::B20);
        });
        assert_eq!(
            evaluate(&default_view(), &cfg),
            FilterDecision::Drop(FilterDropReason::BandDenied)
        );
    }

    #[test]
    fn band_allow_passes_listed() {
        let cfg = compile(|c| {
            c.rf.band_allow.insert(Band::B20);
        });
        assert_eq!(evaluate(&default_view(), &cfg), FilterDecision::Allow);
    }

    #[test]
    fn band_allow_drops_unlisted() {
        let cfg = compile(|c| {
            c.rf.band_allow.insert(Band::B40);
        });
        // view is B20, not in allowlist
        assert_eq!(
            evaluate(&default_view(), &cfg),
            FilterDecision::Drop(FilterDropReason::BandDenied)
        );
    }

    // -----------------------------------------------------------------------
    // Mode allow/deny
    // -----------------------------------------------------------------------

    #[test]
    fn mode_deny_drops() {
        let cfg = compile(|c| {
            c.rf.mode_deny.insert(DxMode::CW);
        });
        assert_eq!(
            evaluate(&default_view(), &cfg),
            FilterDecision::Drop(FilterDropReason::ModeDenied)
        );
    }

    #[test]
    fn mode_allow_drops_unlisted() {
        let cfg = compile(|c| {
            c.rf.mode_allow.insert(DxMode::SSB);
        });
        assert_eq!(
            evaluate(&default_view(), &cfg),
            FilterDecision::Drop(FilterDropReason::ModeDenied)
        );
    }

    // -----------------------------------------------------------------------
    // Frequency ranges
    // -----------------------------------------------------------------------

    #[test]
    fn freq_deny_range_drops() {
        let cfg = compile(|c| {
            c.rf.freq_deny_hz.push((14_020_000, 14_030_000));
        });
        assert_eq!(
            evaluate(&default_view(), &cfg),
            FilterDecision::Drop(FilterDropReason::FreqDenied)
        );
    }

    #[test]
    fn freq_allow_range_passes() {
        let cfg = compile(|c| {
            c.rf.freq_allow_hz.push((14_000_000, 14_070_000));
        });
        assert_eq!(evaluate(&default_view(), &cfg), FilterDecision::Allow);
    }

    #[test]
    fn freq_allow_range_drops_outside() {
        let cfg = compile(|c| {
            c.rf.freq_allow_hz.push((7_000_000, 7_040_000));
        });
        assert_eq!(
            evaluate(&default_view(), &cfg),
            FilterDecision::Drop(FilterDropReason::FreqNotAllowed)
        );
    }

    // -----------------------------------------------------------------------
    // Profiles
    // -----------------------------------------------------------------------

    #[test]
    fn profile_match_passes() {
        let cfg = compile(|c| {
            c.rf.profiles.push(RfProfileSerde {
                name: "CW".into(),
                ranges_hz: vec![(14_000_000, 14_070_000)],
            });
        });
        assert_eq!(evaluate(&default_view(), &cfg), FilterDecision::Allow);
    }

    #[test]
    fn profile_no_match_drops() {
        let cfg = compile(|c| {
            c.rf.profiles.push(RfProfileSerde {
                name: "SSB".into(),
                ranges_hz: vec![(14_150_000, 14_350_000)],
            });
        });
        assert_eq!(
            evaluate(&default_view(), &cfg),
            FilterDecision::Drop(FilterDropReason::ProfileNoMatch)
        );
    }

    // -----------------------------------------------------------------------
    // DX callsign
    // -----------------------------------------------------------------------

    #[test]
    fn dx_call_deny_drops() {
        let cfg = compile(|c| {
            c.dx.deny.regexes.push("^W1".into());
        });
        assert_eq!(
            evaluate(&default_view(), &cfg),
            FilterDecision::Drop(FilterDropReason::DxCallDenied)
        );
    }

    #[test]
    fn dx_call_allow_passes() {
        let cfg = compile(|c| {
            c.dx.allow.globs.push("W1*".into());
        });
        assert_eq!(evaluate(&default_view(), &cfg), FilterDecision::Allow);
    }

    #[test]
    fn dx_call_allow_drops_nonmatch() {
        let cfg = compile(|c| {
            c.dx.allow.globs.push("JA*".into());
        });
        assert_eq!(
            evaluate(&default_view(), &cfg),
            FilterDecision::Drop(FilterDropReason::DxCallNotAllowed)
        );
    }

    // -----------------------------------------------------------------------
    // Spotter kind
    // -----------------------------------------------------------------------

    #[test]
    fn human_denied_when_disabled() {
        let cfg = compile(|c| c.spotter.allow_human = false);
        assert_eq!(
            evaluate(&default_view(), &cfg),
            FilterDecision::Drop(FilterDropReason::OriginatorKindDenied)
        );
    }

    #[test]
    fn skimmer_denied_when_disabled() {
        let cfg = compile(|c| c.spotter.allow_skimmer = false);
        let mut view = default_view();
        view.originator_kind = OriginatorKind::Skimmer;
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::OriginatorKindDenied)
        );
    }

    #[test]
    fn unknown_kind_denied_when_disabled() {
        let cfg = compile(|c| c.spotter.allow_unknown_kind = false);
        let mut view = default_view();
        view.originator_kind = OriginatorKind::Unknown;
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::OriginatorKindDenied)
        );
    }

    // -----------------------------------------------------------------------
    // Spotter call/source
    // -----------------------------------------------------------------------

    #[test]
    fn spotter_source_deny_drops() {
        let cfg = compile(|c| {
            c.spotter.source_deny.insert("bad_node".into());
        });
        let mut view = default_view();
        view.spotter_source_id = Some("bad_node");
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::SpotterDenied)
        );
    }

    #[test]
    fn spotter_call_deny_drops() {
        let cfg = compile(|c| {
            c.spotter.callsign.deny.globs.push("W3LPL*".into());
        });
        let mut view = default_view();
        view.spotter_call_norm = Some("W3LPL-2");
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::SpotterDenied)
        );
    }

    // -----------------------------------------------------------------------
    // Content / CQ / split
    // -----------------------------------------------------------------------

    #[test]
    fn cq_only_drops_non_cq() {
        let cfg = compile(|c| c.content.cq_only = true);
        let mut view = default_view();
        view.comment = Some("599 NY");
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::CqRequired)
        );
    }

    #[test]
    fn cq_only_passes_cq_comment() {
        let cfg = compile(|c| c.content.cq_only = true);
        let mut view = default_view();
        view.comment = Some("CQ JA");
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    #[test]
    fn cq_only_passes_skimmer_cq_flag() {
        let cfg = compile(|c| c.content.cq_only = true);
        let mut view = default_view();
        view.is_skimmer_cq = true;
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    #[test]
    fn split_suppressed() {
        let cfg = compile(|c| c.content.suppress_split_qsy = true);
        let mut view = default_view();
        view.comment = Some("UP 5");
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::SplitQsySuppressed)
        );
    }

    #[test]
    fn comment_deny_drops() {
        let cfg = compile(|c| {
            c.content.comment_deny.regexes.push("(?i)test".into());
        });
        let mut view = default_view();
        view.comment = Some("TEST 599");
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::CommentDenied)
        );
    }

    // -----------------------------------------------------------------------
    // Correlation
    // -----------------------------------------------------------------------

    #[test]
    fn correlation_min_originators_drops() {
        let cfg = compile(|c| c.correlation.min_unique_originators = 3);
        let mut view = default_view();
        view.unique_originators = 2;
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::CorrelationNotMet)
        );
    }

    #[test]
    fn correlation_min_sources_drops() {
        let cfg = compile(|c| c.correlation.min_unique_sources = 2);
        let mut view = default_view();
        view.unique_sources = 1;
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::CorrelationNotMet)
        );
    }

    #[test]
    fn correlation_min_observations_drops() {
        let cfg = compile(|c| c.correlation.min_observations = 5);
        let mut view = default_view();
        view.total_observations_in_window = 3;
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::CorrelationNotMet)
        );
    }

    #[test]
    fn correlation_human_only_originators() {
        let cfg = compile(|c| {
            c.correlation.min_unique_originators = 2;
            c.correlation.originators_human_only = true;
        });
        let mut view = default_view();
        view.unique_originators = 5; // plenty total
        view.unique_human_originators = 1; // but only 1 human
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::CorrelationNotMet)
        );
    }

    // -----------------------------------------------------------------------
    // Skimmer metrics
    // -----------------------------------------------------------------------

    #[test]
    fn skimmer_snr_out_of_range_drops() {
        let cfg = compile(|c| c.skimmer.snr_db = Some((10, 40)));
        let mut view = default_view();
        view.originator_kind = OriginatorKind::Skimmer;
        view.snr_db = Some(5); // below min
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::SkimmerMetricNotMet)
        );
    }

    #[test]
    fn skimmer_snr_in_range_passes() {
        let cfg = compile(|c| c.skimmer.snr_db = Some((10, 40)));
        let mut view = default_view();
        view.originator_kind = OriginatorKind::Skimmer;
        view.snr_db = Some(20);
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    #[test]
    fn skimmer_wpm_out_of_range_drops() {
        let cfg = compile(|c| c.skimmer.wpm = Some((15, 45)));
        let mut view = default_view();
        view.originator_kind = OriginatorKind::Skimmer;
        view.wpm = Some(10); // below min
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::SkimmerMetricNotMet)
        );
    }

    #[test]
    fn skimmer_dupe_dropped() {
        let cfg = compile(|c| c.skimmer.drop_dupes = true);
        let mut view = default_view();
        view.originator_kind = OriginatorKind::Skimmer;
        view.is_skimmer_dupe = true;
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::SkimmerMetricNotMet)
        );
    }

    #[test]
    fn skimmer_require_cq_drops_non_cq() {
        let cfg = compile(|c| c.skimmer.require_cq = true);
        let mut view = default_view();
        view.originator_kind = OriginatorKind::Skimmer;
        view.is_skimmer_cq = false;
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::SkimmerMetricNotMet)
        );
    }

    #[test]
    fn skimmer_metrics_not_applied_to_humans() {
        let cfg = compile(|c| {
            c.skimmer.snr_db = Some((10, 40));
            c.skimmer.require_cq = true;
            c.skimmer.drop_dupes = true;
        });
        // Human originator — skimmer metrics should not apply
        let view = default_view();
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    // -----------------------------------------------------------------------
    // Geo: CQ zone (the critical fix)
    // -----------------------------------------------------------------------

    #[test]
    fn cq_zone_allow_passes() {
        let cfg = compile(|c| {
            c.geo.dx.cq_zone_allow.insert(5);
        });
        let mut view = default_view();
        view.dx_geo = GeoResolved {
            cq_zone: Some(5),
            ..Default::default()
        };
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    #[test]
    fn cq_zone_allow_drops_wrong_zone() {
        let cfg = compile(|c| {
            c.geo.dx.cq_zone_allow.insert(14);
        });
        let mut view = default_view();
        view.dx_geo = GeoResolved {
            cq_zone: Some(5),
            ..Default::default()
        };
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::GeoNotMet)
        );
    }

    #[test]
    fn cq_zone_deny_drops() {
        let cfg = compile(|c| {
            c.geo.dx.cq_zone_deny.insert(5);
        });
        let mut view = default_view();
        view.dx_geo = GeoResolved {
            cq_zone: Some(5),
            ..Default::default()
        };
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::GeoNotMet)
        );
    }

    // -----------------------------------------------------------------------
    // Geo: ITU zone
    // -----------------------------------------------------------------------

    #[test]
    fn itu_zone_allow_drops_wrong_zone() {
        let cfg = compile(|c| {
            c.geo.dx.itu_zone_allow.insert(8);
        });
        let mut view = default_view();
        view.dx_geo = GeoResolved {
            itu_zone: Some(45),
            ..Default::default()
        };
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::GeoNotMet)
        );
    }

    // -----------------------------------------------------------------------
    // Geo: continent
    // -----------------------------------------------------------------------

    #[test]
    fn continent_allow_passes() {
        let cfg = compile(|c| {
            c.geo.dx.continent_allow.insert(Continent::EU);
        });
        let mut view = default_view();
        view.dx_geo = GeoResolved {
            continent: Some(Continent::EU),
            ..Default::default()
        };
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    #[test]
    fn continent_allow_drops_wrong() {
        let cfg = compile(|c| {
            c.geo.dx.continent_allow.insert(Continent::EU);
        });
        let mut view = default_view();
        view.dx_geo = GeoResolved {
            continent: Some(Continent::NA),
            ..Default::default()
        };
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::GeoNotMet)
        );
    }

    // -----------------------------------------------------------------------
    // UnknownFieldPolicy: Neutral with allowlist
    // -----------------------------------------------------------------------

    #[test]
    fn neutral_policy_drops_unknown_on_allowlist() {
        let cfg = compile(|c| {
            c.unknown_policy = UnknownFieldPolicy::Neutral;
            c.geo.dx.continent_allow.insert(Continent::EU);
        });
        // No geo data at all — Neutral should drop for allowlists
        let view = default_view();
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::UnknownFieldFailed)
        );
    }

    #[test]
    fn neutral_policy_passes_unknown_on_denylist() {
        let cfg = compile(|c| {
            c.unknown_policy = UnknownFieldPolicy::Neutral;
            c.geo.dx.continent_deny.insert(Continent::EU);
        });
        // No geo data — Neutral should pass for denylists
        let view = default_view();
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    #[test]
    fn fail_open_passes_unknown_on_allowlist() {
        let cfg = compile(|c| {
            c.unknown_policy = UnknownFieldPolicy::FailOpen;
            c.geo.dx.continent_allow.insert(Continent::EU);
        });
        let view = default_view();
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    #[test]
    fn fail_closed_drops_unknown_on_denylist() {
        let cfg = compile(|c| {
            c.unknown_policy = UnknownFieldPolicy::FailClosed;
            c.geo.dx.continent_deny.insert(Continent::EU);
        });
        let view = default_view();
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::UnknownFieldFailed)
        );
    }

    // -----------------------------------------------------------------------
    // Enrichment
    // -----------------------------------------------------------------------

    #[test]
    fn lotw_require_true_passes() {
        let cfg = compile(|c| c.enrichment.lotw = TriState::RequireTrue);
        let mut view = default_view();
        view.lotw = Some(true);
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    #[test]
    fn lotw_require_true_drops_false() {
        let cfg = compile(|c| c.enrichment.lotw = TriState::RequireTrue);
        let mut view = default_view();
        view.lotw = Some(false);
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::EnrichmentNotMet)
        );
    }

    #[test]
    fn membership_require_drops_missing() {
        let cfg = compile(|c| {
            c.enrichment
                .membership
                .push(MembershipRuleSerde::Require("FOC".into()));
        });
        let mut view = default_view();
        let memberships = BTreeSet::new(); // empty
        view.memberships = Some(&memberships);
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::EnrichmentNotMet)
        );
    }

    #[test]
    fn membership_require_passes() {
        let cfg = compile(|c| {
            c.enrichment
                .membership
                .push(MembershipRuleSerde::Require("FOC".into()));
        });
        let mut view = default_view();
        let mut memberships = BTreeSet::new();
        memberships.insert("FOC".to_string());
        view.memberships = Some(&memberships);
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    #[test]
    fn membership_deny_drops_present() {
        let cfg = compile(|c| {
            c.enrichment
                .membership
                .push(MembershipRuleSerde::Deny("SPAM".into()));
        });
        let mut view = default_view();
        let mut memberships = BTreeSet::new();
        memberships.insert("SPAM".to_string());
        view.memberships = Some(&memberships);
        assert_eq!(
            evaluate(&view, &cfg),
            FilterDecision::Drop(FilterDropReason::EnrichmentNotMet)
        );
    }

    // -----------------------------------------------------------------------
    // count_unique_originators helper
    // -----------------------------------------------------------------------

    #[test]
    fn count_unique_originators_all() {
        let list = vec![
            ("W1AW", OriginatorKind::Human),
            ("K1TTT", OriginatorKind::Skimmer),
            ("W1AW", OriginatorKind::Human), // duplicate
        ];
        assert_eq!(count_unique_originators(&list, false), 2);
    }

    #[test]
    fn count_unique_originators_human_only() {
        let list = vec![
            ("W1AW", OriginatorKind::Human),
            ("K1TTT", OriginatorKind::Skimmer),
            ("VE3NEA", OriginatorKind::Human),
        ];
        assert_eq!(count_unique_originators(&list, true), 2);
    }

    // -----------------------------------------------------------------------
    // Combined: multiple filters
    // -----------------------------------------------------------------------

    #[test]
    fn combined_filters_all_pass() {
        let cfg = compile(|c| {
            c.rf.band_allow.insert(Band::B20);
            c.rf.mode_allow.insert(DxMode::CW);
            c.dx.allow.globs.push("W*".into());
            c.correlation.min_unique_originators = 1;
        });
        let view = default_view();
        assert_eq!(evaluate(&view, &cfg), FilterDecision::Allow);
    }

    #[test]
    fn combined_filters_first_failure_reported() {
        let cfg = compile(|c| {
            c.rf.band_deny.insert(Band::B20); // will fail first
            c.dx.deny.globs.push("W*".into()); // would also fail
        });
        // Band check comes before DX call check in evaluation order
        assert_eq!(
            evaluate(&default_view(), &cfg),
            FilterDecision::Drop(FilterDropReason::BandDenied)
        );
    }
}
