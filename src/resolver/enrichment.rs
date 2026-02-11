//! Enrichment resolver traits for LoTW, master DB, callbook, and club memberships.
//!
//! These are trait-only definitions in the core crate. Actual implementations
//! (e.g., loading the LoTW user list, parsing SCP files) would be in separate
//! crates or behind feature flags. The library provides [`NullEnrichmentResolver`]
//! as a default that returns `None` for all queries.

use std::collections::BTreeSet;

/// Trait for resolving enrichment data about callsigns.
///
/// Implementations might load data from LoTW user lists, super-check-partial
/// databases, callbook APIs, or club membership files.
pub trait EnrichmentResolver: Send + Sync {
    /// Whether the callsign is a confirmed LoTW user.
    fn lotw_user(&self, callsign: &str) -> Option<bool>;

    /// Whether the callsign appears in the contest master database (SCP).
    fn in_master_db(&self, callsign: &str) -> Option<bool>;

    /// Whether the callsign appears in a callbook database.
    fn in_callbook(&self, callsign: &str) -> Option<bool>;

    /// Club memberships for the callsign (e.g., "ARRL", "SKCC").
    fn memberships(&self, callsign: &str) -> Option<BTreeSet<String>>;
}

/// Null implementation that returns `None` for all queries.
///
/// Used as the default when no enrichment data is available. Combined with
/// `UnknownFieldPolicy::Neutral`, enrichment-related filters will not block
/// any spots.
pub struct NullEnrichmentResolver;

impl EnrichmentResolver for NullEnrichmentResolver {
    fn lotw_user(&self, _callsign: &str) -> Option<bool> {
        None
    }

    fn in_master_db(&self, _callsign: &str) -> Option<bool> {
        None
    }

    fn in_callbook(&self, _callsign: &str) -> Option<bool> {
        None
    }

    fn memberships(&self, _callsign: &str) -> Option<BTreeSet<String>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_resolver_returns_none() {
        let resolver = NullEnrichmentResolver;
        assert!(resolver.lotw_user("W1AW").is_none());
        assert!(resolver.in_master_db("W1AW").is_none());
        assert!(resolver.in_callbook("W1AW").is_none());
        assert!(resolver.memberships("W1AW").is_none());
    }

    #[test]
    fn mock_resolver_returns_values() {
        struct MockResolver;

        impl EnrichmentResolver for MockResolver {
            fn lotw_user(&self, callsign: &str) -> Option<bool> {
                Some(callsign == "W1AW")
            }

            fn in_master_db(&self, _callsign: &str) -> Option<bool> {
                Some(true)
            }

            fn in_callbook(&self, _callsign: &str) -> Option<bool> {
                Some(true)
            }

            fn memberships(&self, callsign: &str) -> Option<BTreeSet<String>> {
                if callsign == "W1AW" {
                    let mut set = BTreeSet::new();
                    set.insert("ARRL".into());
                    Some(set)
                } else {
                    None
                }
            }
        }

        let resolver = MockResolver;
        assert_eq!(resolver.lotw_user("W1AW"), Some(true));
        assert_eq!(resolver.lotw_user("JA1ABC"), Some(false));
        assert_eq!(resolver.in_master_db("W1AW"), Some(true));
        assert_eq!(resolver.in_callbook("W1AW"), Some(true));

        let memberships = resolver.memberships("W1AW").unwrap();
        assert!(memberships.contains("ARRL"));

        assert!(resolver.memberships("JA1ABC").is_none());
    }

    #[test]
    fn null_resolver_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NullEnrichmentResolver>();
    }
}
