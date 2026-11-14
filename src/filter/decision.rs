//! Filter evaluation result types.

/// The result of evaluating a spot against the filter configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterDecision {
    Allow,
    Drop(FilterDropReason),
}

/// The specific reason a spot was filtered out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FilterDropReason {
    TooOld,
    FreqOutOfRange,
    BandDenied,
    ModeDenied,
    ProfileNoMatch,
    FreqDenied,
    FreqNotAllowed,
    DxCallDenied,
    DxCallNotAllowed,
    SpotterDenied,
    SpotterNotAllowed,
    OriginatorKindDenied,
    CommentDenied,
    CommentNotAllowed,
    CqRequired,
    SplitQsySuppressed,
    CorrelationNotMet,
    SkimmerMetricNotMet,
    GeoNotMet,
    EnrichmentNotMet,
    UnknownFieldFailed,
}

impl FilterDropReason {
    /// Every reason, in declaration order (used to index per-reason metrics).
    pub const ALL: [FilterDropReason; 21] = [
        Self::TooOld,
        Self::FreqOutOfRange,
        Self::BandDenied,
        Self::ModeDenied,
        Self::ProfileNoMatch,
        Self::FreqDenied,
        Self::FreqNotAllowed,
        Self::DxCallDenied,
        Self::DxCallNotAllowed,
        Self::SpotterDenied,
        Self::SpotterNotAllowed,
        Self::OriginatorKindDenied,
        Self::CommentDenied,
        Self::CommentNotAllowed,
        Self::CqRequired,
        Self::SplitQsySuppressed,
        Self::CorrelationNotMet,
        Self::SkimmerMetricNotMet,
        Self::GeoNotMet,
        Self::EnrichmentNotMet,
        Self::UnknownFieldFailed,
    ];
}
