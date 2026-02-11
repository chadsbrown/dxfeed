//! DX spot line parser for telnet cluster protocols.
//!
//! Handles DXSpider, AR-Cluster v6, CC Cluster, VE7CC, and RBN line formats.
//! The parser is designed to never panic on any input.
//!
//! # DX Spot Format
//!
//! The standard format is:
//! ```text
//! DX de SP-CALL:    FREQ.F  DX-CALL  comment            HHMMz
//! ```
//!
//! Column positions vary between cluster implementations. The parser
//! locates the colon after the spotter call and works outward from there.

use chrono::NaiveTime;

/// A parsed DX spot with all fields extracted from a single line.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedSpot {
    /// The spotter (originator) callsign.
    pub spotter_call: String,
    /// The DX station callsign.
    pub dx_call: String,
    /// Frequency in Hz, parsed directly from the string to avoid f64 rounding.
    pub freq_hz: u64,
    /// Comment/info field (may contain SNR/WPM for RBN spots).
    pub comment: Option<String>,
    /// UTC time parsed from the line (HHMMz), if present.
    pub utc_time: Option<NaiveTime>,
}

/// The result of parsing a single line from a cluster connection.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedLine {
    /// A DX spot line.
    Spot(ParsedSpot),
    /// An announcement (To ALL de ..., To LOCAL de ...).
    Announce(String),
    /// A WWV or WCY propagation data line.
    Propagation(String),
    /// A login prompt or challenge.
    Prompt(String),
    /// A line that could not be classified.
    Unknown(String),
}

/// RBN-specific parsed fields from a spot comment.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RbnFields {
    pub snr_db: Option<i8>,
    pub wpm: Option<u16>,
    pub is_cq: bool,
}

/// Parse a single line from a DX cluster or RBN telnet connection.
///
/// Never panics, regardless of input. Unrecognized lines return
/// `ParsedLine::Unknown`.
pub fn parse_line(line: &str) -> ParsedLine {
    let trimmed = line.trim_end();

    if trimmed.is_empty() {
        return ParsedLine::Unknown(String::new());
    }

    // DX spot lines
    if let Some(spot) = try_parse_dx_spot(trimmed) {
        return ParsedLine::Spot(spot);
    }

    // Announce lines: "To ALL de ...", "To LOCAL de ..."
    if let Some(rest) = trimmed.strip_prefix("To ") {
        if rest.contains(" de ") {
            return ParsedLine::Announce(trimmed.to_string());
        }
    }

    // WWV/WCY propagation data
    if trimmed.starts_with("WWV de ") || trimmed.starts_with("WCY de ") {
        return ParsedLine::Propagation(trimmed.to_string());
    }

    // Login prompts / challenges
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("login:")
        || lower.contains("call:")
        || lower.contains("callsign:")
        || lower.contains("please enter your call")
        || lower.contains("password:")
        || (lower.ends_with('>') && trimmed.len() < 30)
    {
        return ParsedLine::Prompt(trimmed.to_string());
    }

    ParsedLine::Unknown(trimmed.to_string())
}

/// Try to parse a DX spot line. Returns None if it doesn't match.
fn try_parse_dx_spot(line: &str) -> Option<ParsedSpot> {
    // Must start with "DX de " (case-insensitive)
    let after_prefix = if line.len() >= 6 && line[..6].eq_ignore_ascii_case("DX de ") {
        &line[6..]
    } else {
        return None;
    };

    // Find the colon after the spotter call
    let colon_pos = after_prefix.find(':')?;
    if colon_pos == 0 {
        return None;
    }

    let spotter_call = after_prefix[..colon_pos].trim().to_string();
    if spotter_call.is_empty() {
        return None;
    }

    // Everything after the colon
    let after_colon = &after_prefix[colon_pos + 1..];

    // Parse frequency — first whitespace-delimited token that looks numeric
    let after_colon_trimmed = after_colon.trim_start();

    // Split into tokens to find frequency and dx_call
    let mut tokens = after_colon_trimmed.split_whitespace();

    // First token should be frequency (e.g., "14025.0" or "14025.1")
    let freq_token = tokens.next()?;
    let freq_hz = parse_freq_to_hz(freq_token)?;

    // Second token should be the DX callsign
    let dx_call = tokens.next()?.to_string();
    if dx_call.is_empty() {
        return None;
    }

    // The rest is comment + optional time
    // We need to find the comment and time from the remaining text.
    // The time is typically at the end: HHMMz or HHMM (4 digits + optional Z)
    let freq_end = after_colon_trimmed
        .find(&dx_call)
        .map(|pos| pos + dx_call.len())?;
    let remainder = after_colon_trimmed[freq_end..].trim();

    let (comment, utc_time) = extract_comment_and_time(remainder);

    Some(ParsedSpot {
        spotter_call,
        dx_call,
        freq_hz,
        comment,
        utc_time,
    })
}

/// Parse a frequency string (in kHz) directly to Hz as u64.
///
/// Handles formats like "14025.0", "14025.1", "14025", "7001.5".
/// Avoids f64 to prevent rounding issues.
///
/// Strategy: split on '.', parse integer and fractional parts separately.
fn parse_freq_to_hz(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Handle possible leading/trailing non-numeric chars (some clusters add spaces)
    let s = s.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');

    if let Some(dot_pos) = s.find('.') {
        // Has decimal point: integer part is kHz, fractional part is sub-kHz
        let int_part = &s[..dot_pos];
        let frac_part = &s[dot_pos + 1..];

        let khz: u64 = if int_part.is_empty() {
            0
        } else {
            int_part.parse().ok()?
        };

        // Convert kHz to Hz
        let mut hz = khz * 1_000;

        // Parse fractional kHz part
        // "1" = 100 Hz, "10" = 10 Hz, "100" = 1 Hz, etc.
        if !frac_part.is_empty() {
            // Take up to 3 decimal places (we only care about Hz precision)
            let frac_str: String = frac_part.chars().take(3).collect();
            let frac_val: u64 = frac_str.parse().ok()?;

            // Scale based on number of digits
            let scale = match frac_str.len() {
                1 => 100, // .1 = 100 Hz
                2 => 10,  // .10 = 10 Hz
                3 => 1,   // .100 = 1 Hz
                _ => return None,
            };
            hz += frac_val * scale;
        }

        Some(hz)
    } else {
        // No decimal point: whole kHz
        let khz: u64 = s.parse().ok()?;
        Some(khz * 1_000)
    }
}

/// Extract comment text and UTC time from the remainder of a spot line.
///
/// The time is typically at the end of the line in the format `HHMMz` or `HHMM`.
fn extract_comment_and_time(remainder: &str) -> (Option<String>, Option<NaiveTime>) {
    if remainder.is_empty() {
        return (None, None);
    }

    // Look for time pattern at the end: 4 digits optionally followed by 'Z' or 'z'
    // The time is typically in the last 5-6 characters
    let bytes = remainder.as_bytes();
    let len = bytes.len();

    // Try to find HHMMz at the end
    let (comment_end, time) = if len >= 5 {
        // Check for HHMMz pattern (4 digits + 'Z'/'z')
        let last5 = &remainder[len - 5..];
        if last5.ends_with('Z') || last5.ends_with('z') {
            if let Some(t) = parse_hhmm(&last5[..4]) {
                (len - 5, Some(t))
            } else {
                (len, None)
            }
        } else if len >= 4 {
            // Try HHMM at the very end (no Z)
            let last4 = &remainder[len - 4..];
            if last4.chars().all(|c| c.is_ascii_digit()) {
                if let Some(t) = parse_hhmm(last4) {
                    (len - 4, Some(t))
                } else {
                    (len, None)
                }
            } else {
                (len, None)
            }
        } else {
            (len, None)
        }
    } else if len >= 4 {
        // Short remainder, try HHMM
        let last4 = &remainder[len - 4..];
        if last4.chars().all(|c| c.is_ascii_digit()) {
            if let Some(t) = parse_hhmm(last4) {
                (len - 4, Some(t))
            } else {
                (len, None)
            }
        } else {
            (len, None)
        }
    } else {
        (len, None)
    };

    let comment_str = remainder[..comment_end].trim();
    let comment = if comment_str.is_empty() {
        None
    } else {
        Some(comment_str.to_string())
    };

    (comment, time)
}

/// Parse a 4-character HHMM string into a NaiveTime.
fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    if s.len() != 4 || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hour: u32 = s[..2].parse().ok()?;
    let min: u32 = s[2..].parse().ok()?;
    NaiveTime::from_hms_opt(hour, min, 0)
}

/// Parse RBN-specific fields from a spot comment.
///
/// RBN comments typically look like: "15 dB  22 WPM  CQ"
pub fn parse_rbn_comment(comment: &str) -> RbnFields {
    let mut fields = RbnFields::default();

    // Look for "NN dB" pattern
    let parts: Vec<&str> = comment.split_whitespace().collect();
    for i in 0..parts.len().saturating_sub(1) {
        if parts[i + 1].eq_ignore_ascii_case("dB") {
            if let Ok(snr) = parts[i].parse::<i8>() {
                fields.snr_db = Some(snr);
            }
        }
        if parts[i + 1].eq_ignore_ascii_case("WPM") {
            if let Ok(wpm) = parts[i].parse::<u16>() {
                fields.wpm = Some(wpm);
            }
        }
    }

    // Check for CQ
    fields.is_cq = parts
        .iter()
        .any(|p| p.eq_ignore_ascii_case("CQ"));

    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // DX spot parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_standard_dxspider_spot() {
        let line = "DX de W3LPL:     14025.0  JA1ABC       CQ JA UP 1                    0230Z";
        match parse_line(line) {
            ParsedLine::Spot(s) => {
                assert_eq!(s.spotter_call, "W3LPL");
                assert_eq!(s.dx_call, "JA1ABC");
                assert_eq!(s.freq_hz, 14_025_000);
                assert!(s.comment.as_deref().unwrap().contains("CQ JA"));
                assert_eq!(
                    s.utc_time,
                    Some(NaiveTime::from_hms_opt(2, 30, 0).unwrap())
                );
            }
            other => panic!("expected Spot, got {:?}", other),
        }
    }

    #[test]
    fn parse_spot_with_decimal_freq() {
        let line = "DX de K1TTT:     14025.1  W1AW         CQ                            1234Z";
        match parse_line(line) {
            ParsedLine::Spot(s) => {
                assert_eq!(s.freq_hz, 14_025_100);
                assert_eq!(s.dx_call, "W1AW");
            }
            other => panic!("expected Spot, got {:?}", other),
        }
    }

    #[test]
    fn parse_spot_two_decimal_places() {
        let line = "DX de VE3NEA:    7001.50  DL1ABC       599                            0800Z";
        match parse_line(line) {
            ParsedLine::Spot(s) => {
                assert_eq!(s.freq_hz, 7_001_500);
                assert_eq!(s.spotter_call, "VE3NEA");
            }
            other => panic!("expected Spot, got {:?}", other),
        }
    }

    #[test]
    fn parse_spot_three_decimal_places() {
        let line = "DX de N1MM:      14025.123 JA1XYZ      TEST                           1500Z";
        match parse_line(line) {
            ParsedLine::Spot(s) => {
                // 14025.123 kHz = 14025123 Hz
                assert_eq!(s.freq_hz, 14_025_123);
            }
            other => panic!("expected Spot, got {:?}", other),
        }
    }

    #[test]
    fn parse_spot_no_decimal() {
        let line = "DX de W1AW:      14025  K3LR          CQ                             2100Z";
        match parse_line(line) {
            ParsedLine::Spot(s) => {
                assert_eq!(s.freq_hz, 14_025_000);
            }
            other => panic!("expected Spot, got {:?}", other),
        }
    }

    #[test]
    fn parse_spot_no_time() {
        let line = "DX de W1AW:      14025.0  K3LR         CQ contest";
        match parse_line(line) {
            ParsedLine::Spot(s) => {
                assert_eq!(s.dx_call, "K3LR");
                // "CQ contest" doesn't end with a valid time
                assert!(s.utc_time.is_none());
                assert!(s.comment.as_deref().unwrap().contains("CQ contest"));
            }
            other => panic!("expected Spot, got {:?}", other),
        }
    }

    #[test]
    fn parse_spot_case_insensitive_prefix() {
        let line = "dx de W1AW:      14025.0  K3LR         CQ                             1200Z";
        assert!(matches!(parse_line(line), ParsedLine::Spot(_)));

        let line2 = "Dx de W1AW:      14025.0  K3LR         CQ                             1200Z";
        assert!(matches!(parse_line(line2), ParsedLine::Spot(_)));
    }

    #[test]
    fn parse_spot_trailing_whitespace() {
        let line = "DX de W1AW:      14025.0  K3LR         CQ                             1200Z   \r\n";
        match parse_line(line) {
            ParsedLine::Spot(s) => {
                assert_eq!(s.dx_call, "K3LR");
            }
            other => panic!("expected Spot, got {:?}", other),
        }
    }

    #[test]
    fn parse_spot_minimal() {
        // Minimal valid spot line
        let line = "DX de A:1.0 B";
        match parse_line(line) {
            ParsedLine::Spot(s) => {
                assert_eq!(s.spotter_call, "A");
                assert_eq!(s.dx_call, "B");
                assert_eq!(s.freq_hz, 1_000);
            }
            other => panic!("expected Spot, got {:?}", other),
        }
    }

    #[test]
    fn parse_rbn_format_line() {
        let line = "DX de W3OA-2:    14025.0  JA1ABC       15 dB  22 WPM  CQ              0230Z";
        match parse_line(line) {
            ParsedLine::Spot(s) => {
                assert_eq!(s.spotter_call, "W3OA-2");
                assert_eq!(s.dx_call, "JA1ABC");
                let comment = s.comment.as_deref().unwrap();
                let rbn = parse_rbn_comment(comment);
                assert_eq!(rbn.snr_db, Some(15));
                assert_eq!(rbn.wpm, Some(22));
                assert!(rbn.is_cq);
            }
            other => panic!("expected Spot, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Frequency parsing (direct to Hz)
    // -----------------------------------------------------------------------

    #[test]
    fn freq_parsing_various() {
        assert_eq!(parse_freq_to_hz("14025.0"), Some(14_025_000));
        assert_eq!(parse_freq_to_hz("14025.1"), Some(14_025_100));
        assert_eq!(parse_freq_to_hz("14025.10"), Some(14_025_100));
        assert_eq!(parse_freq_to_hz("14025.12"), Some(14_025_120));
        assert_eq!(parse_freq_to_hz("14025.123"), Some(14_025_123));
        assert_eq!(parse_freq_to_hz("7001"), Some(7_001_000));
        assert_eq!(parse_freq_to_hz("7001.5"), Some(7_001_500));
        assert_eq!(parse_freq_to_hz("3500.0"), Some(3_500_000));
        assert_eq!(parse_freq_to_hz("50125.0"), Some(50_125_000));
    }

    #[test]
    fn freq_parsing_edge_cases() {
        assert_eq!(parse_freq_to_hz(""), None);
        assert_eq!(parse_freq_to_hz("abc"), None);
        assert_eq!(parse_freq_to_hz("0"), Some(0));
        assert_eq!(parse_freq_to_hz("0.0"), Some(0));
    }

    // -----------------------------------------------------------------------
    // Announce lines
    // -----------------------------------------------------------------------

    #[test]
    fn parse_announce() {
        let line = "To ALL de W1AW: Hello everyone";
        assert!(matches!(parse_line(line), ParsedLine::Announce(_)));
    }

    #[test]
    fn parse_local_announce() {
        let line = "To LOCAL de K3LR: Testing";
        assert!(matches!(parse_line(line), ParsedLine::Announce(_)));
    }

    // -----------------------------------------------------------------------
    // Propagation data
    // -----------------------------------------------------------------------

    #[test]
    fn parse_wwv() {
        let line = "WWV de VE7CC <21> :   SFI=150, A=8, K=2, No Storms -> No Storms";
        assert!(matches!(parse_line(line), ParsedLine::Propagation(_)));
    }

    #[test]
    fn parse_wcy() {
        let line = "WCY de DK0WCY-1 <18> :   K=2 expK=2 A=8 R=60 SFI=120 SA=qui GMF=qui Au=no";
        assert!(matches!(parse_line(line), ParsedLine::Propagation(_)));
    }

    // -----------------------------------------------------------------------
    // Prompts
    // -----------------------------------------------------------------------

    #[test]
    fn parse_login_prompt() {
        let line = "login:";
        assert!(matches!(parse_line(line), ParsedLine::Prompt(_)));
    }

    #[test]
    fn parse_callsign_prompt() {
        let line = "Please enter your call:";
        assert!(matches!(parse_line(line), ParsedLine::Prompt(_)));
    }

    #[test]
    fn parse_password_prompt() {
        let line = "Password:";
        assert!(matches!(parse_line(line), ParsedLine::Prompt(_)));
    }

    #[test]
    fn parse_cluster_prompt() {
        let line = "W1AW-2>";
        assert!(matches!(parse_line(line), ParsedLine::Prompt(_)));
    }

    // -----------------------------------------------------------------------
    // Unknown
    // -----------------------------------------------------------------------

    #[test]
    fn parse_empty_line() {
        assert!(matches!(parse_line(""), ParsedLine::Unknown(_)));
    }

    #[test]
    fn parse_random_text() {
        let line = "Some random cluster MOTD text here";
        assert!(matches!(parse_line(line), ParsedLine::Unknown(_)));
    }

    #[test]
    fn parse_truncated_spot() {
        let line = "DX de W1AW:";
        assert!(matches!(parse_line(line), ParsedLine::Unknown(_)));
    }

    // -----------------------------------------------------------------------
    // RBN comment parsing
    // -----------------------------------------------------------------------

    #[test]
    fn rbn_comment_full() {
        let fields = parse_rbn_comment("15 dB  22 WPM  CQ");
        assert_eq!(fields.snr_db, Some(15));
        assert_eq!(fields.wpm, Some(22));
        assert!(fields.is_cq);
    }

    #[test]
    fn rbn_comment_beacon() {
        let fields = parse_rbn_comment("8 dB  18 WPM  BEACON");
        assert_eq!(fields.snr_db, Some(8));
        assert_eq!(fields.wpm, Some(18));
        assert!(!fields.is_cq);
    }

    #[test]
    fn rbn_comment_no_cq() {
        let fields = parse_rbn_comment("20 dB  25 WPM  NCDXF");
        assert_eq!(fields.snr_db, Some(20));
        assert_eq!(fields.wpm, Some(25));
        assert!(!fields.is_cq);
    }

    #[test]
    fn rbn_comment_malformed() {
        let fields = parse_rbn_comment("random text");
        assert_eq!(fields.snr_db, None);
        assert_eq!(fields.wpm, None);
        assert!(!fields.is_cq);
    }

    #[test]
    fn rbn_comment_empty() {
        let fields = parse_rbn_comment("");
        assert_eq!(fields, RbnFields::default());
    }

    // -----------------------------------------------------------------------
    // Never panics (fuzz-like edge cases)
    // -----------------------------------------------------------------------

    #[test]
    fn never_panics_on_weird_input() {
        let cases = [
            "DX de :14025.0 W1AW",
            "DX de",
            "DX de W1AW",
            "DX de W1AW: W1AW",
            "DX de W1AW: abc W1AW",
            "DX de W1AW:      ",
            "DX de W1AW: 0.0 ",
            "DX de :0.0 B",
            "\x00\x01\x02DX de W1AW: 14025.0 K3LR",
            "DX de W1AW: 999999999.999 K3LR",
            &"x".repeat(1000),
            "DX de W1AW: 14025.0 K3LR         comment 9999",
            "DX de W1AW: 14025.0 K3LR         comment 2500Z",
        ];

        for case in &cases {
            let _ = parse_line(case); // should not panic
        }
    }

    // -----------------------------------------------------------------------
    // Time extraction
    // -----------------------------------------------------------------------

    #[test]
    fn extract_time_with_z() {
        let (comment, time) = extract_comment_and_time("CQ JA UP 1                    0230Z");
        assert_eq!(time, Some(NaiveTime::from_hms_opt(2, 30, 0).unwrap()));
        assert!(comment.unwrap().contains("CQ JA"));
    }

    #[test]
    fn extract_time_without_z() {
        let (_, time) = extract_comment_and_time("CQ                             1200");
        assert_eq!(time, Some(NaiveTime::from_hms_opt(12, 0, 0).unwrap()));
    }

    #[test]
    fn extract_no_time() {
        let (comment, time) = extract_comment_and_time("CQ contest");
        assert!(time.is_none());
        assert_eq!(comment.as_deref(), Some("CQ contest"));
    }

    #[test]
    fn extract_invalid_time() {
        let (_, time) = extract_comment_and_time("CQ 2500Z");
        // 25:00 is invalid
        assert!(time.is_none());
    }
}
