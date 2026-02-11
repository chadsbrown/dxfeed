//! Filter configuration validation errors.

#[derive(Debug, thiserror::Error)]
pub enum FilterConfigError {
    #[error("invalid glob pattern: {0}")]
    InvalidGlob(String),
    #[error("invalid regex pattern: {0}")]
    InvalidRegex(String),
    #[error("invalid frequency range: {0}..{1}")]
    InvalidFreqRange(u64, u64),
    #[error("max_age must be > 0")]
    MaxAgeZero,
}
