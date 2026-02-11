//! Frequency-to-band mapping, mode inference, and frequency bucketing.

use crate::domain::{Band, DxMode, ModePolicy};

/// Map a frequency in Hz to an amateur band.
///
/// Uses the widest amateur allocation across all three ITU regions.
pub fn freq_to_band(freq_hz: u64) -> Band {
    match freq_hz {
        1_800_000..=2_000_000 => Band::B160,
        3_500_000..=4_000_000 => Band::B80,
        5_250_000..=5_450_000 => Band::B60,
        7_000_000..=7_300_000 => Band::B40,
        10_100_000..=10_150_000 => Band::B30,
        14_000_000..=14_350_000 => Band::B20,
        18_068_000..=18_168_000 => Band::B17,
        21_000_000..=21_450_000 => Band::B15,
        24_890_000..=24_990_000 => Band::B12,
        28_000_000..=29_700_000 => Band::B10,
        50_000_000..=54_000_000 => Band::B6,
        144_000_000..=148_000_000 => Band::B2,
        _ => Band::Unknown,
    }
}

/// Return the band edge frequencies (Hz) for a given band.
pub fn band_edges(band: Band) -> Option<(u64, u64)> {
    match band {
        Band::B160 => Some((1_800_000, 2_000_000)),
        Band::B80 => Some((3_500_000, 4_000_000)),
        Band::B60 => Some((5_250_000, 5_450_000)),
        Band::B40 => Some((7_000_000, 7_300_000)),
        Band::B30 => Some((10_100_000, 10_150_000)),
        Band::B20 => Some((14_000_000, 14_350_000)),
        Band::B17 => Some((18_068_000, 18_168_000)),
        Band::B15 => Some((21_000_000, 21_450_000)),
        Band::B12 => Some((24_890_000, 24_990_000)),
        Band::B10 => Some((28_000_000, 29_700_000)),
        Band::B6 => Some((50_000_000, 54_000_000)),
        Band::B2 => Some((144_000_000, 148_000_000)),
        Band::Unknown => None,
    }
}

/// Best-effort mode inference from frequency using IARU band plans.
///
/// This is a rough approximation. Contest operators should use the
/// `ModePolicy` system to control how mode is determined.
pub fn freq_to_mode(freq_hz: u64) -> DxMode {
    match freq_hz {
        // 160m
        1_800_000..=1_843_000 => DxMode::CW,
        1_843_001..=2_000_000 => DxMode::SSB,

        // 80m
        3_500_000..=3_570_000 => DxMode::CW,
        3_570_001..=3_600_000 => DxMode::DIG,
        3_600_001..=4_000_000 => DxMode::SSB,

        // 60m (mixed usage, mostly digital/SSB)
        5_250_000..=5_450_000 => DxMode::DIG,

        // 40m
        7_000_000..=7_040_000 => DxMode::CW,
        7_040_001..=7_060_000 => DxMode::DIG,
        7_060_001..=7_300_000 => DxMode::SSB,

        // 30m (CW and digital only, no SSB)
        10_100_000..=10_130_000 => DxMode::CW,
        10_130_001..=10_150_000 => DxMode::DIG,

        // 20m
        14_000_000..=14_070_000 => DxMode::CW,
        14_070_001..=14_099_000 => DxMode::DIG,
        14_099_001..=14_350_000 => DxMode::SSB,

        // 17m
        18_068_000..=18_095_000 => DxMode::CW,
        18_095_001..=18_109_000 => DxMode::DIG,
        18_109_001..=18_168_000 => DxMode::SSB,

        // 15m
        21_000_000..=21_070_000 => DxMode::CW,
        21_070_001..=21_110_000 => DxMode::DIG,
        21_110_001..=21_450_000 => DxMode::SSB,

        // 12m
        24_890_000..=24_915_000 => DxMode::CW,
        24_915_001..=24_931_000 => DxMode::DIG,
        24_931_001..=24_990_000 => DxMode::SSB,

        // 10m
        28_000_000..=28_070_000 => DxMode::CW,
        28_070_001..=28_190_000 => DxMode::DIG,
        28_190_001..=29_700_000 => DxMode::SSB,

        // 6m
        50_000_000..=50_100_000 => DxMode::CW,
        50_100_001..=50_500_000 => DxMode::SSB,
        50_500_001..=54_000_000 => DxMode::DIG,

        // 2m
        144_000_000..=144_150_000 => DxMode::CW,
        144_150_001..=144_400_000 => DxMode::SSB,
        144_400_001..=148_000_000 => DxMode::DIG,

        _ => DxMode::Unknown,
    }
}

/// Resolve operating mode using the configured policy.
pub fn resolve_mode(freq_hz: u64, upstream_mode: Option<DxMode>, policy: ModePolicy) -> DxMode {
    match policy {
        ModePolicy::TrustUpstream => upstream_mode.unwrap_or(DxMode::Unknown),
        ModePolicy::InferFromFrequency => freq_to_mode(freq_hz),
        ModePolicy::PreferUpstreamFallbackInfer => {
            match upstream_mode {
                Some(m) if m != DxMode::Unknown => m,
                _ => freq_to_mode(freq_hz),
            }
        }
    }
}

/// Truncate a frequency to its bucket boundary for deduplication.
///
/// # Arguments
/// * `freq_hz` - The frequency in Hz
/// * `mode` - The operating mode (determines bucket size)
/// * `cw_bucket_hz` - Bucket size for CW (default: 10)
/// * `ssb_bucket_hz` - Bucket size for SSB (default: 1000)
/// * `dig_bucket_hz` - Bucket size for DIG (default: 100)
pub fn freq_bucket(
    freq_hz: u64,
    mode: DxMode,
    cw_bucket_hz: u64,
    ssb_bucket_hz: u64,
    dig_bucket_hz: u64,
) -> u64 {
    let bucket = match mode {
        DxMode::CW => cw_bucket_hz,
        DxMode::SSB => ssb_bucket_hz,
        DxMode::DIG => dig_bucket_hz,
        // For AM, FM, Unknown: use SSB bucket as a reasonable default
        _ => ssb_bucket_hz,
    };

    if bucket == 0 {
        return freq_hz;
    }

    (freq_hz / bucket) * bucket
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // freq_to_band
    // -----------------------------------------------------------------------

    #[test]
    fn freq_to_band_hf_contest_bands() {
        assert_eq!(freq_to_band(1_825_000), Band::B160);
        assert_eq!(freq_to_band(3_525_000), Band::B80);
        assert_eq!(freq_to_band(7_001_000), Band::B40);
        assert_eq!(freq_to_band(10_110_000), Band::B30);
        assert_eq!(freq_to_band(14_025_000), Band::B20);
        assert_eq!(freq_to_band(18_080_000), Band::B17);
        assert_eq!(freq_to_band(21_025_000), Band::B15);
        assert_eq!(freq_to_band(24_900_000), Band::B12);
        assert_eq!(freq_to_band(28_025_000), Band::B10);
        assert_eq!(freq_to_band(50_125_000), Band::B6);
        assert_eq!(freq_to_band(144_200_000), Band::B2);
    }

    #[test]
    fn freq_to_band_edges() {
        assert_eq!(freq_to_band(1_800_000), Band::B160);
        assert_eq!(freq_to_band(2_000_000), Band::B160);
        assert_eq!(freq_to_band(14_000_000), Band::B20);
        assert_eq!(freq_to_band(14_350_000), Band::B20);
    }

    #[test]
    fn freq_to_band_out_of_band() {
        assert_eq!(freq_to_band(0), Band::Unknown);
        assert_eq!(freq_to_band(1_000_000), Band::Unknown);
        assert_eq!(freq_to_band(100_000_000), Band::Unknown);
    }

    // -----------------------------------------------------------------------
    // band_edges
    // -----------------------------------------------------------------------

    #[test]
    fn band_edges_20m() {
        assert_eq!(band_edges(Band::B20), Some((14_000_000, 14_350_000)));
    }

    #[test]
    fn band_edges_unknown() {
        assert_eq!(band_edges(Band::Unknown), None);
    }

    // -----------------------------------------------------------------------
    // freq_to_mode
    // -----------------------------------------------------------------------

    #[test]
    fn freq_to_mode_cw() {
        assert_eq!(freq_to_mode(14_025_000), DxMode::CW);
        assert_eq!(freq_to_mode(7_010_000), DxMode::CW);
        assert_eq!(freq_to_mode(21_025_000), DxMode::CW);
    }

    #[test]
    fn freq_to_mode_ssb() {
        assert_eq!(freq_to_mode(14_200_000), DxMode::SSB);
        assert_eq!(freq_to_mode(7_100_000), DxMode::SSB);
        assert_eq!(freq_to_mode(3_750_000), DxMode::SSB);
    }

    #[test]
    fn freq_to_mode_dig() {
        assert_eq!(freq_to_mode(14_074_000), DxMode::DIG);
        assert_eq!(freq_to_mode(7_050_000), DxMode::DIG);
        assert_eq!(freq_to_mode(10_140_000), DxMode::DIG);
    }

    #[test]
    fn freq_to_mode_unknown() {
        assert_eq!(freq_to_mode(0), DxMode::Unknown);
        assert_eq!(freq_to_mode(100_000_000), DxMode::Unknown);
    }

    // -----------------------------------------------------------------------
    // resolve_mode
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_mode_trust_upstream() {
        assert_eq!(
            resolve_mode(14_025_000, Some(DxMode::SSB), ModePolicy::TrustUpstream),
            DxMode::SSB
        );
        assert_eq!(
            resolve_mode(14_025_000, None, ModePolicy::TrustUpstream),
            DxMode::Unknown
        );
    }

    #[test]
    fn resolve_mode_infer() {
        assert_eq!(
            resolve_mode(14_025_000, Some(DxMode::SSB), ModePolicy::InferFromFrequency),
            DxMode::CW
        );
    }

    #[test]
    fn resolve_mode_prefer_upstream_fallback() {
        // Has upstream: use it
        assert_eq!(
            resolve_mode(14_025_000, Some(DxMode::SSB), ModePolicy::PreferUpstreamFallbackInfer),
            DxMode::SSB
        );
        // Upstream is Unknown: infer
        assert_eq!(
            resolve_mode(14_025_000, Some(DxMode::Unknown), ModePolicy::PreferUpstreamFallbackInfer),
            DxMode::CW
        );
        // No upstream: infer
        assert_eq!(
            resolve_mode(14_025_000, None, ModePolicy::PreferUpstreamFallbackInfer),
            DxMode::CW
        );
    }

    // -----------------------------------------------------------------------
    // freq_bucket
    // -----------------------------------------------------------------------

    #[test]
    fn freq_bucket_cw_10hz() {
        assert_eq!(freq_bucket(14_025_130, DxMode::CW, 10, 1000, 100), 14_025_130);
        assert_eq!(freq_bucket(14_025_135, DxMode::CW, 10, 1000, 100), 14_025_130);
        assert_eq!(freq_bucket(14_025_139, DxMode::CW, 10, 1000, 100), 14_025_130);
        assert_eq!(freq_bucket(14_025_140, DxMode::CW, 10, 1000, 100), 14_025_140);
    }

    #[test]
    fn freq_bucket_ssb_1khz() {
        assert_eq!(freq_bucket(14_200_750, DxMode::SSB, 10, 1000, 100), 14_200_000);
        assert_eq!(freq_bucket(14_200_000, DxMode::SSB, 10, 1000, 100), 14_200_000);
        assert_eq!(freq_bucket(14_200_999, DxMode::SSB, 10, 1000, 100), 14_200_000);
    }

    #[test]
    fn freq_bucket_dig_100hz() {
        assert_eq!(freq_bucket(14_074_050, DxMode::DIG, 10, 1000, 100), 14_074_000);
        assert_eq!(freq_bucket(14_074_099, DxMode::DIG, 10, 1000, 100), 14_074_000);
        assert_eq!(freq_bucket(14_074_100, DxMode::DIG, 10, 1000, 100), 14_074_100);
    }

    #[test]
    fn freq_bucket_two_spots_same_cw_bucket() {
        let a = freq_bucket(14_025_100, DxMode::CW, 10, 1000, 100);
        let b = freq_bucket(14_025_105, DxMode::CW, 10, 1000, 100);
        assert_eq!(a, b); // both in 14025100 bucket
    }

    #[test]
    fn freq_bucket_two_spots_different_cw_bucket() {
        let a = freq_bucket(14_025_100, DxMode::CW, 10, 1000, 100);
        let b = freq_bucket(14_025_115, DxMode::CW, 10, 1000, 100);
        assert_ne!(a, b); // 14025100 vs 14025110
    }

    #[test]
    fn freq_bucket_zero_bucket_size_returns_raw() {
        assert_eq!(freq_bucket(14_025_130, DxMode::CW, 0, 0, 0), 14_025_130);
    }
}
