//! Callsign normalization, portable suffix handling, and skimmer detection.

use crate::domain::PortablePolicy;
use crate::filter::config::CallsignNormalizationSerde;

/// Known portable suffixes that `StripCommonSuffixes` will remove.
const COMMON_SUFFIXES: &[&str] = &["P", "M", "QRP", "MM", "AM"];

/// Normalize a callsign according to the given policy.
///
/// Steps applied in order:
/// 1. Uppercase (if `policy.uppercase`)
/// 2. Strip non-alphanumeric characters except `/` (if `policy.strip_non_alnum`)
/// 3. Apply portable policy
pub fn normalize_callsign(call: &str, policy: &CallsignNormalizationSerde) -> String {
    let mut result = if policy.uppercase {
        call.to_ascii_uppercase()
    } else {
        call.to_string()
    };

    if policy.strip_non_alnum {
        result.retain(|c| c.is_ascii_alphanumeric() || c == '/');
    }

    let keep_len = strip_portable_suffix(&result, policy.portable_policy).len();
    result.truncate(keep_len);

    result
}

/// Strip portable suffixes from a callsign according to the given policy.
///
/// - `KeepAsIs`: no change
/// - `StripCommonSuffixes`: removes trailing `/P`, `/M`, `/QRP`, `/MM`, `/AM`
/// - `StripAllAfterSlash`: removes everything after (and including) the last `/`
///
/// Note: `StripAllAfterSlash` on `"DL/W1AW"` returns `"DL"` (the portable
/// prefix, not the base callsign). Use [`base_callsign`] to extract the actual
/// callsign from compound calls with portable prefixes.
pub fn strip_portable_suffix(call: &str, policy: PortablePolicy) -> &str {
    match policy {
        PortablePolicy::KeepAsIs => call,
        PortablePolicy::StripCommonSuffixes => {
            if let Some(pos) = call.rfind('/') {
                let suffix = &call[pos + 1..];
                let suffix_upper: String = suffix.to_ascii_uppercase();
                if COMMON_SUFFIXES.contains(&suffix_upper.as_str()) {
                    &call[..pos]
                } else {
                    call
                }
            } else {
                call
            }
        }
        PortablePolicy::StripAllAfterSlash => {
            if let Some(pos) = call.rfind('/') {
                &call[..pos]
            } else {
                call
            }
        }
    }
}

/// Extract the base callsign, stripping portable prefixes and suffixes.
///
/// For compound callsigns with `/`, picks the segment most likely to be the
/// real callsign: the longest segment containing a digit.
///
/// - `"DL/W1AW/P"` → `"W1AW"`
/// - `"VE3NEA/P"` → `"VE3NEA"`
/// - `"W1AW/7"` → `"W1AW"`
/// - `"4X/W1AW"` → `"W1AW"`
pub fn base_callsign(call: &str) -> &str {
    if !call.contains('/') {
        return call;
    }

    call.split('/')
        .filter(|s| !s.is_empty() && s.chars().any(|c| c.is_ascii_digit()))
        .max_by_key(|s| s.len())
        .unwrap_or(call)
}

/// Heuristic check for whether a callsign looks like an automated skimmer.
///
/// RBN skimmers typically have a `-N` suffix (e.g., `W3LPL-2`, `DK8JP-1`).
/// This is best-effort; the definitive originator kind is set by the source
/// connector based on whether the spot came from RBN.
pub fn is_likely_skimmer(call: &str) -> bool {
    if let Some(pos) = call.rfind('-') {
        let after = &call[pos + 1..];
        !after.is_empty() && after.len() <= 2 && after.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_policy() -> CallsignNormalizationSerde {
        CallsignNormalizationSerde::default()
    }

    // -----------------------------------------------------------------------
    // normalize_callsign
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_uppercase() {
        assert_eq!(normalize_callsign("w1aw", &default_policy()), "W1AW");
    }

    #[test]
    fn normalize_no_uppercase() {
        let mut p = default_policy();
        p.uppercase = false;
        assert_eq!(normalize_callsign("w1aw", &p), "w1aw");
    }

    #[test]
    fn normalize_strip_common_suffixes() {
        let mut p = default_policy();
        p.portable_policy = PortablePolicy::StripCommonSuffixes;
        assert_eq!(normalize_callsign("VE3NEA/P", &p), "VE3NEA");
        assert_eq!(normalize_callsign("W1AW/QRP", &p), "W1AW");
        assert_eq!(normalize_callsign("W1AW/MM", &p), "W1AW");
        assert_eq!(normalize_callsign("W1AW/AM", &p), "W1AW");
        assert_eq!(normalize_callsign("W1AW/M", &p), "W1AW");
    }

    #[test]
    fn normalize_strip_common_keeps_non_suffix() {
        let mut p = default_policy();
        p.portable_policy = PortablePolicy::StripCommonSuffixes;
        assert_eq!(normalize_callsign("DL/W1AW", &p), "DL/W1AW");
    }

    #[test]
    fn normalize_strip_all_after_slash() {
        let mut p = default_policy();
        p.portable_policy = PortablePolicy::StripAllAfterSlash;
        assert_eq!(normalize_callsign("VE3NEA/P", &p), "VE3NEA");
        assert_eq!(normalize_callsign("DL/W1AW", &p), "DL");
        assert_eq!(normalize_callsign("DL/W1AW/P", &p), "DL/W1AW");
    }

    #[test]
    fn normalize_strip_non_alnum() {
        let mut p = default_policy();
        p.strip_non_alnum = true;
        assert_eq!(normalize_callsign("W3LPL-2", &p), "W3LPL2");
        assert_eq!(normalize_callsign("W1AW", &p), "W1AW");
    }

    #[test]
    fn normalize_empty() {
        assert_eq!(normalize_callsign("", &default_policy()), "");
    }

    #[test]
    fn normalize_non_ascii_no_strip() {
        let result = normalize_callsign("w1aw\u{00e9}", &default_policy());
        assert!(result.starts_with("W1AW"));
    }

    #[test]
    fn normalize_non_ascii_stripped() {
        let mut p = default_policy();
        p.strip_non_alnum = true;
        assert_eq!(normalize_callsign("w1aw\u{00e9}", &p), "W1AW");
    }

    #[test]
    fn normalize_combined_uppercase_strip_suffix() {
        let mut p = default_policy();
        p.portable_policy = PortablePolicy::StripCommonSuffixes;
        assert_eq!(normalize_callsign("ve3nea/p", &p), "VE3NEA");
    }

    // -----------------------------------------------------------------------
    // strip_portable_suffix
    // -----------------------------------------------------------------------

    #[test]
    fn strip_keep_as_is() {
        assert_eq!(
            strip_portable_suffix("W1AW/P", PortablePolicy::KeepAsIs),
            "W1AW/P"
        );
    }

    #[test]
    fn strip_common_suffix_p() {
        assert_eq!(
            strip_portable_suffix("VE3NEA/P", PortablePolicy::StripCommonSuffixes),
            "VE3NEA"
        );
    }

    #[test]
    fn strip_common_suffix_case_insensitive() {
        assert_eq!(
            strip_portable_suffix("VE3NEA/p", PortablePolicy::StripCommonSuffixes),
            "VE3NEA"
        );
        assert_eq!(
            strip_portable_suffix("W1AW/qrp", PortablePolicy::StripCommonSuffixes),
            "W1AW"
        );
    }

    #[test]
    fn strip_common_no_slash() {
        assert_eq!(
            strip_portable_suffix("W1AW", PortablePolicy::StripCommonSuffixes),
            "W1AW"
        );
    }

    #[test]
    fn strip_common_non_matching_suffix() {
        assert_eq!(
            strip_portable_suffix("DL/W1AW", PortablePolicy::StripCommonSuffixes),
            "DL/W1AW"
        );
        assert_eq!(
            strip_portable_suffix("W1AW/7", PortablePolicy::StripCommonSuffixes),
            "W1AW/7"
        );
    }

    #[test]
    fn strip_all_after_last_slash() {
        assert_eq!(
            strip_portable_suffix("DL/W1AW", PortablePolicy::StripAllAfterSlash),
            "DL"
        );
        assert_eq!(
            strip_portable_suffix("DL/W1AW/P", PortablePolicy::StripAllAfterSlash),
            "DL/W1AW"
        );
    }

    #[test]
    fn strip_all_no_slash() {
        assert_eq!(
            strip_portable_suffix("W1AW", PortablePolicy::StripAllAfterSlash),
            "W1AW"
        );
    }

    // -----------------------------------------------------------------------
    // base_callsign
    // -----------------------------------------------------------------------

    #[test]
    fn base_simple() {
        assert_eq!(base_callsign("W1AW"), "W1AW");
    }

    #[test]
    fn base_with_portable_suffix() {
        assert_eq!(base_callsign("VE3NEA/P"), "VE3NEA");
    }

    #[test]
    fn base_with_portable_prefix() {
        assert_eq!(base_callsign("DL/W1AW"), "W1AW");
    }

    #[test]
    fn base_with_prefix_and_suffix() {
        assert_eq!(base_callsign("DL/W1AW/P"), "W1AW");
    }

    #[test]
    fn base_district_override() {
        assert_eq!(base_callsign("W1AW/7"), "W1AW");
    }

    #[test]
    fn base_numeric_prefix() {
        assert_eq!(base_callsign("4X/W1AW"), "W1AW");
    }

    #[test]
    fn base_empty() {
        assert_eq!(base_callsign(""), "");
    }

    #[test]
    fn base_just_slash() {
        assert_eq!(base_callsign("/"), "/");
    }

    #[test]
    fn base_no_digits_anywhere() {
        assert_eq!(base_callsign("AB/CD"), "AB/CD");
    }

    // -----------------------------------------------------------------------
    // is_likely_skimmer
    // -----------------------------------------------------------------------

    #[test]
    fn skimmer_dash_single_digit() {
        assert!(is_likely_skimmer("W3LPL-2"));
        assert!(is_likely_skimmer("DK8JP-1"));
    }

    #[test]
    fn skimmer_dash_two_digits() {
        assert!(is_likely_skimmer("W3LPL-12"));
    }

    #[test]
    fn not_skimmer_plain_call() {
        assert!(!is_likely_skimmer("W1AW"));
        assert!(!is_likely_skimmer("VE3NEA"));
    }

    #[test]
    fn not_skimmer_dash_letters() {
        assert!(!is_likely_skimmer("W3LPL-AB"));
    }

    #[test]
    fn not_skimmer_empty() {
        assert!(!is_likely_skimmer(""));
    }

    #[test]
    fn not_skimmer_dash_three_digits() {
        assert!(!is_likely_skimmer("W3LPL-123"));
    }

    #[test]
    fn not_skimmer_trailing_dash() {
        assert!(!is_likely_skimmer("W3LPL-"));
    }
}
