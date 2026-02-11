//! Pluggable resolvers for entity (DXCC) and enrichment data.

pub mod entity;

#[cfg(feature = "cty")]
pub mod cty;
