/// Amateur radio frequency bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Band {
    B160,
    B80,
    B60,
    B40,
    B30,
    B20,
    B17,
    B15,
    B12,
    B10,
    B6,
    B2,
    Unknown,
}

/// Operating modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DxMode {
    CW,
    SSB,
    DIG,
    AM,
    FM,
    Unknown,
}

/// ITU continents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Continent {
    AF,
    AN,
    AS,
    EU,
    NA,
    OC,
    SA,
    Unknown,
}

/// Whether a spot originator is a human operator or automated skimmer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OriginatorKind {
    Human,
    Skimmer,
    Unknown,
}

/// Policy for handling unknown/missing fields during filter evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum UnknownFieldPolicy {
    /// For denylists: allow (field not in deny set). For allowlists: deny (field not proven in allow set).
    Neutral,
    /// Unknown fields are treated as not matching any filter (drops on allowlists, passes denylists).
    FailClosed,
    /// Unknown fields pass all filters.
    FailOpen,
}

impl Default for UnknownFieldPolicy {
    fn default() -> Self {
        Self::Neutral
    }
}

/// Three-state filter requirement for boolean enrichment flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TriState {
    Any,
    RequireTrue,
    RequireFalse,
}

impl Default for TriState {
    fn default() -> Self {
        Self::Any
    }
}

/// Policy for handling portable suffixes (e.g., /P, /M, /QRP) on callsigns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PortablePolicy {
    KeepAsIs,
    StripCommonSuffixes,
    StripAllAfterSlash,
}

impl Default for PortablePolicy {
    fn default() -> Self {
        Self::KeepAsIs
    }
}

/// Policy for determining operating mode from spot data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ModePolicy {
    /// Trust whatever mode the upstream cluster/skimmer reports.
    TrustUpstream,
    /// Ignore upstream mode, infer from frequency using band plan.
    InferFromFrequency,
    /// Use upstream mode if present, otherwise infer from frequency.
    PreferUpstreamFallbackInfer,
}

impl Default for ModePolicy {
    fn default() -> Self {
        Self::PreferUpstreamFallbackInfer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tristate_default_is_any() {
        assert_eq!(TriState::default(), TriState::Any);
    }

    #[test]
    fn unknown_field_policy_default_is_neutral() {
        assert_eq!(UnknownFieldPolicy::default(), UnknownFieldPolicy::Neutral);
    }

    #[test]
    fn portable_policy_default_is_keep() {
        assert_eq!(PortablePolicy::default(), PortablePolicy::KeepAsIs);
    }

    #[test]
    fn mode_policy_default_is_prefer_upstream() {
        assert_eq!(
            ModePolicy::default(),
            ModePolicy::PreferUpstreamFallbackInfer
        );
    }

    #[test]
    fn band_ord_follows_declaration_order() {
        assert!(Band::B160 < Band::B80);
        assert!(Band::B80 < Band::B40);
        assert!(Band::B10 < Band::B6);
        assert!(Band::B6 < Band::Unknown);
    }

    #[test]
    fn continent_equality() {
        assert_eq!(Continent::NA, Continent::NA);
        assert_ne!(Continent::NA, Continent::EU);
    }

    #[test]
    fn originator_kind_variants() {
        let kinds = [
            OriginatorKind::Human,
            OriginatorKind::Skimmer,
            OriginatorKind::Unknown,
        ];
        for k in &kinds {
            assert_eq!(*k, *k);
        }
    }

    #[cfg(feature = "serde")]
    mod serde_tests {
        use super::*;

        fn round_trip_json<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(val: &T) {
            let json = serde_json::to_string(val).unwrap();
            let back: T = serde_json::from_str(&json).unwrap();
            assert_eq!(*val, back);
        }

        #[test]
        fn band_serde_round_trip() {
            for band in [Band::B160, Band::B80, Band::B60, Band::B40, Band::B30,
                         Band::B20, Band::B17, Band::B15, Band::B12, Band::B10,
                         Band::B6, Band::B2, Band::Unknown] {
                round_trip_json(&band);
            }
        }

        #[test]
        fn dx_mode_serde_round_trip() {
            for mode in [DxMode::CW, DxMode::SSB, DxMode::DIG, DxMode::AM,
                         DxMode::FM, DxMode::Unknown] {
                round_trip_json(&mode);
            }
        }

        #[test]
        fn continent_serde_round_trip() {
            for c in [Continent::AF, Continent::AN, Continent::AS, Continent::EU,
                      Continent::NA, Continent::OC, Continent::SA, Continent::Unknown] {
                round_trip_json(&c);
            }
        }

        #[test]
        fn tristate_serde_round_trip() {
            for t in [TriState::Any, TriState::RequireTrue, TriState::RequireFalse] {
                round_trip_json(&t);
            }
        }

        #[test]
        fn unknown_field_policy_serde_round_trip() {
            for p in [UnknownFieldPolicy::Neutral, UnknownFieldPolicy::FailClosed,
                      UnknownFieldPolicy::FailOpen] {
                round_trip_json(&p);
            }
        }

        #[test]
        fn portable_policy_serde_round_trip() {
            for p in [PortablePolicy::KeepAsIs, PortablePolicy::StripCommonSuffixes,
                      PortablePolicy::StripAllAfterSlash] {
                round_trip_json(&p);
            }
        }

        #[test]
        fn mode_policy_serde_round_trip() {
            for p in [ModePolicy::TrustUpstream, ModePolicy::InferFromFrequency,
                      ModePolicy::PreferUpstreamFallbackInfer] {
                round_trip_json(&p);
            }
        }
    }
}
