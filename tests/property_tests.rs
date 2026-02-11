//! Property-like tests verifying robustness invariants.
//!
//! These tests exercise the parser, filter, and aggregator with
//! adversarial inputs to ensure no panics or undefined behavior.

use dxfeed::domain::{Band, DxMode};
use dxfeed::filter::config::FilterConfigSerde;
use dxfeed::filter::evaluate::evaluate;
use dxfeed::model::SpotView;
use dxfeed::parser::spot::parse_line;

// ---------------------------------------------------------------------------
// Parser: never panics on arbitrary input
// ---------------------------------------------------------------------------

#[test]
fn parser_never_panics_on_random_bytes() {
    // Deterministic pseudo-random via simple LCG
    let mut seed: u64 = 12345;
    for _ in 0..10_000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let len = (seed % 200) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|i| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                ((seed >> 33) ^ (i as u64)) as u8
            })
            .collect();

        // Some bytes may not be valid UTF-8, so use lossy conversion
        let line = String::from_utf8_lossy(&bytes);
        let _ = parse_line(&line); // must not panic
    }
}

#[test]
fn parser_never_panics_on_adversarial_strings() {
    let adversarial = [
        "",
        " ",
        "\0",
        "\x00\x01\x02\x03",
        "DX de ",
        "DX de :     ",
        "DX de W1AW:",
        "DX de W1AW:     ",
        "DX de W1AW:     14025.0",
        "DX de W1AW:     14025.0  ",
        "DX de W1AW:     14025.0  JA1ABC",
        "DX de W1AW:     0.0  JA1ABC       CQ",
        "DX de W1AW:     99999999.0  JA1ABC       CQ",
        "DX de W1AW:     -1.0  JA1ABC       CQ",
        "DX de W1AW:     NaN  JA1ABC       CQ",
        "DX de W1AW:     Inf  JA1ABC       CQ",
        "DX de W1AW:     14025.0  JA1ABC       CQ                         9999Z",
        "DX de W1AW:     14025.0  JA1ABC       CQ                         0000Z",
        "DX de W1AW:     14025.0  JA1ABC       CQ                         2500Z",
        &"A".repeat(10000),
        &format!("DX de {}:", "A".repeat(1000)),
        &format!("DX de W1AW:     14025.0  {}       CQ", "B".repeat(1000)),
        "DX de W1AW:     14025.0  JA1ABC       CQ\r\n",
        "DX de W1AW:     14025.0  JA1ABC       CQ\n",
        "DX de W1AW:\t14025.0\tJA1ABC\tCQ",
        "DX de W1AW:     14025.000000000000000000000000001  JA1ABC       CQ",
        // Unicode
        "DX de W1AW:     14025.0  JA1ABC       \u{1F4E1} satellite",
        "DX de W1AW:     14025.0  \u{00E9}\u{00F1}  CQ",
    ];

    for line in &adversarial {
        let _ = parse_line(line); // must not panic
    }
}

// ---------------------------------------------------------------------------
// Filter: default config allows all normal spots
// ---------------------------------------------------------------------------

#[test]
fn default_filter_allows_any_valid_spot() {
    use chrono::Utc;
    use dxfeed::filter::decision::FilterDecision;

    let filter = FilterConfigSerde::default()
        .validate_and_compile()
        .unwrap();
    let now = Utc::now();

    let bands = [Band::B160, Band::B80, Band::B40, Band::B20, Band::B15, Band::B10];
    let modes = [DxMode::CW, DxMode::SSB, DxMode::DIG];
    let calls = ["W1AW", "JA1ABC", "DL1ABC", "VK3MO", "PY2SEX"];

    for &band in &bands {
        for &mode in &modes {
            for &call in &calls {
                let freq = match band {
                    Band::B160 => 1_825_000,
                    Band::B80 => 3_525_000,
                    Band::B40 => 7_025_000,
                    Band::B20 => 14_025_000,
                    Band::B15 => 21_025_000,
                    Band::B10 => 28_025_000,
                    _ => 14_025_000,
                };

                let view = SpotView::test_default(now, call, freq, band, mode);
                let decision = evaluate(&view, &filter);
                assert!(
                    matches!(decision, FilterDecision::Allow),
                    "default filter should allow {call} on {band:?}/{mode:?}, got {decision:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Filter config: validate_and_compile never panics
// ---------------------------------------------------------------------------

#[test]
fn filter_validate_never_panics() {
    // Various potentially problematic configs
    let configs = vec![
        FilterConfigSerde::default(),
        {
            let mut c = FilterConfigSerde::default();
            c.freq_sanity_hz = Some((0, u64::MAX));
            c
        },
        {
            let mut c = FilterConfigSerde::default();
            c.rf.band_allow.insert(Band::B20);
            c.rf.band_deny.insert(Band::B20); // conflicting
            c
        },
        {
            let mut c = FilterConfigSerde::default();
            c.max_age_secs = 0; // zero age
            c
        },
    ];

    for config in configs {
        // May return Ok or Err, but must not panic
        let _ = config.validate_and_compile();
    }
}

// ---------------------------------------------------------------------------
// Band/mode round-trip
// ---------------------------------------------------------------------------

#[test]
fn freq_to_band_round_trip() {
    use dxfeed::freq::freq_to_band;

    // For frequencies within known band edges, freq_to_band should return
    // the correct band (not Unknown).
    let test_cases: &[(u64, Band)] = &[
        (1_800_000, Band::B160),
        (1_999_000, Band::B160),
        (3_500_000, Band::B80),
        (3_999_000, Band::B80),
        (7_000_000, Band::B40),
        (7_299_000, Band::B40),
        (10_100_000, Band::B30),
        (14_000_000, Band::B20),
        (14_349_000, Band::B20),
        (18_068_000, Band::B17),
        (21_000_000, Band::B15),
        (24_890_000, Band::B12),
        (28_000_000, Band::B10),
        (50_000_000, Band::B6),
        (144_000_000, Band::B2),
    ];

    for &(freq, expected_band) in test_cases {
        let band = freq_to_band(freq);
        assert_eq!(
            band, expected_band,
            "freq_to_band({freq}) should be {expected_band:?}, got {band:?}"
        );
    }
}

#[test]
fn resolve_mode_for_all_bands() {
    use dxfeed::domain::ModePolicy;
    use dxfeed::freq::resolve_mode;

    // CW portions
    for freq in [1_810_000u64, 3_525_000, 7_025_000, 14_025_000, 21_025_000, 28_025_000] {
        let mode = resolve_mode(freq, None, ModePolicy::InferFromFrequency);
        assert_eq!(mode, DxMode::CW, "CW expected for freq {freq}");
    }

    // SSB portions
    for freq in [1_860_000u64, 3_780_000, 7_180_000, 14_200_000, 21_300_000, 28_500_000] {
        let mode = resolve_mode(freq, None, ModePolicy::InferFromFrequency);
        assert_eq!(mode, DxMode::SSB, "SSB expected for freq {freq}");
    }
}
