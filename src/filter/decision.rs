//! Filter evaluation result types.

/// The result of evaluating a spot against the filter configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterDecision {
    Allow,
    Drop(FilterDropReason),
}

/// The specific reason a spot was filtered out.
#[derive(Debug, Clone, PartialEq, Eq)]
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
