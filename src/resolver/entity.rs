//! Entity (DXCC) resolver trait and data types.

use crate::domain::Continent;

/// Resolved entity/DXCC information for a callsign.
#[derive(Debug, Clone)]
pub struct EntityInfo {
    /// DXCC entity name (e.g., "United States", "Japan").
    pub entity_name: String,
    pub continent: Continent,
    pub cq_zone: u8,
    pub itu_zone: u8,
    pub lat: f64,
    pub lon: f64,
    /// Primary DXCC prefix (e.g., "K", "JA").
    pub primary_prefix: String,
}

/// Trait for resolving callsigns to DXCC entity information.
///
/// Implementations might use cty.dat, a database, or an API.
/// The library provides [`crate::resolver::cty::CtyResolver`] behind
/// the `cty` feature flag.
pub trait EntityResolver: Send + Sync {
    fn resolve(&self, callsign: &str) -> Option<EntityInfo>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockResolver;

    impl EntityResolver for MockResolver {
        fn resolve(&self, callsign: &str) -> Option<EntityInfo> {
            match callsign {
                "W1AW" => Some(EntityInfo {
                    entity_name: "United States".into(),
                    continent: Continent::NA,
                    cq_zone: 5,
                    itu_zone: 8,
                    lat: 37.53,
                    lon: -97.0,
                    primary_prefix: "K".into(),
                }),
                _ => None,
            }
        }
    }

    #[test]
    fn mock_resolver_returns_info() {
        let resolver = MockResolver;
        let info = resolver.resolve("W1AW").unwrap();
        assert_eq!(info.entity_name, "United States");
        assert_eq!(info.continent, Continent::NA);
        assert_eq!(info.cq_zone, 5);
    }

    #[test]
    fn mock_resolver_returns_none_for_unknown() {
        let resolver = MockResolver;
        assert!(resolver.resolve("GARBAGE").is_none());
    }
}
