//! CTY.DAT parser and entity resolver.
//!
//! Parses the standard cty.dat file format (from country-files.com) and
//! provides callsign-to-entity resolution via longest prefix match.
//!
//! # Usage
//!
//! ```ignore
//! let resolver = CtyResolver::from_file("cty.dat")?;
//! let info = resolver.resolve("W1AW");
//! ```

use std::collections::HashMap;
use std::path::Path;

use crate::domain::Continent;

use super::entity::{EntityInfo, EntityResolver};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CtyParseError {
    #[error("invalid entity header: {0}")]
    InvalidHeader(String),
    #[error("unknown continent code: {0}")]
    UnknownContinent(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CtyEntity {
    name: String,
    continent: Continent,
    cq_zone: u8,
    itu_zone: u8,
    lat: f64,
    lon: f64,
    primary_prefix: String,
}

#[derive(Debug, Clone)]
struct PrefixEntry {
    entity_idx: usize,
    cq_zone_override: Option<u8>,
    itu_zone_override: Option<u8>,
}

// ---------------------------------------------------------------------------
// CtyResolver
// ---------------------------------------------------------------------------

/// Entity resolver backed by a parsed cty.dat file.
///
/// Uses `HashMap` for O(1) prefix lookup and iterates from longest to
/// shortest prefix for each callsign (O(L) where L = callsign length).
pub struct CtyResolver {
    entities: Vec<CtyEntity>,
    prefix_map: HashMap<String, PrefixEntry>,
    exact_map: HashMap<String, PrefixEntry>,
}

impl CtyResolver {
    /// Parse cty.dat content from a string.
    pub fn from_cty_dat(data: &str) -> Result<Self, CtyParseError> {
        let mut entities = Vec::new();
        let mut prefix_map = HashMap::new();
        let mut exact_map = HashMap::new();

        let mut current_header: Option<String> = None;
        let mut prefix_text = String::new();

        for line in data.lines() {
            let trimmed = line.trim();

            // Skip empty lines and comments
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            if !line.starts_with(' ') && !line.starts_with('\t') {
                // New entity header — process previous record if any
                if let Some(header) = current_header.take() {
                    process_record(
                        &header,
                        &prefix_text,
                        &mut entities,
                        &mut prefix_map,
                        &mut exact_map,
                    )?;
                    prefix_text.clear();
                }
                current_header = Some(trimmed.to_string());
            } else {
                // Prefix continuation line
                if !prefix_text.is_empty() {
                    prefix_text.push(' ');
                }
                prefix_text.push_str(trimmed);
            }
        }

        // Process final record
        if let Some(header) = current_header {
            process_record(
                &header,
                &prefix_text,
                &mut entities,
                &mut prefix_map,
                &mut exact_map,
            )?;
        }

        Ok(Self {
            entities,
            prefix_map,
            exact_map,
        })
    }

    /// Parse a cty.dat file from disk.
    pub fn from_file(path: &Path) -> Result<Self, CtyParseError> {
        let data = std::fs::read_to_string(path)?;
        Self::from_cty_dat(&data)
    }

    /// Number of DXCC entities loaded.
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// Number of prefixes (including exact matches) loaded.
    pub fn prefix_count(&self) -> usize {
        self.prefix_map.len() + self.exact_map.len()
    }

    fn build_info(&self, entity: &CtyEntity, pe: &PrefixEntry) -> EntityInfo {
        EntityInfo {
            entity_name: entity.name.clone(),
            continent: entity.continent,
            cq_zone: pe.cq_zone_override.unwrap_or(entity.cq_zone),
            itu_zone: pe.itu_zone_override.unwrap_or(entity.itu_zone),
            lat: entity.lat,
            lon: entity.lon,
            primary_prefix: entity.primary_prefix.clone(),
        }
    }
}

impl EntityResolver for CtyResolver {
    fn resolve(&self, callsign: &str) -> Option<EntityInfo> {
        let call = callsign.to_uppercase();

        // Check exact matches first
        if let Some(pe) = self.exact_map.get(&call) {
            let entity = &self.entities[pe.entity_idx];
            return Some(self.build_info(entity, pe));
        }

        // Longest prefix match: try progressively shorter prefixes
        for len in (1..=call.len()).rev() {
            if let Some(pe) = self.prefix_map.get(&call[..len]) {
                let entity = &self.entities[pe.entity_idx];
                return Some(self.build_info(entity, pe));
            }
        }

        None
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn process_record(
    header: &str,
    prefix_text: &str,
    entities: &mut Vec<CtyEntity>,
    prefix_map: &mut HashMap<String, PrefixEntry>,
    exact_map: &mut HashMap<String, PrefixEntry>,
) -> Result<(), CtyParseError> {
    let entity_idx = entities.len();
    let entity = parse_header(header)?;

    // Add primary prefix
    let primary = entity.primary_prefix.to_uppercase();
    prefix_map.entry(primary).or_insert(PrefixEntry {
        entity_idx,
        cq_zone_override: None,
        itu_zone_override: None,
    });

    // Parse prefix list
    for raw_prefix in prefix_text.split(',') {
        let raw = raw_prefix.trim().trim_end_matches(';');
        if raw.is_empty() {
            continue;
        }

        let parsed = parse_prefix_modifiers(raw);
        let entry = PrefixEntry {
            entity_idx,
            cq_zone_override: parsed.cq_zone_override,
            itu_zone_override: parsed.itu_zone_override,
        };

        let key = parsed.prefix.to_uppercase();
        if key.is_empty() {
            continue;
        }

        if parsed.exact {
            exact_map.entry(key).or_insert(entry);
        } else {
            prefix_map.entry(key).or_insert(entry);
        }
    }

    entities.push(entity);
    Ok(())
}

fn parse_header(line: &str) -> Result<CtyEntity, CtyParseError> {
    // Format: "Entity Name:  CQ ITU Cont Lat Lon UTC Prefix:"
    let (name_part, rest) = line
        .split_once(':')
        .ok_or_else(|| CtyParseError::InvalidHeader(line.to_string()))?;

    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() < 7 {
        return Err(CtyParseError::InvalidHeader(line.to_string()));
    }

    // Last field is "Prefix:" — strip trailing ':'
    let primary_prefix = fields[fields.len() - 1].trim_end_matches(':');

    let cq_zone: u8 = fields[0]
        .parse()
        .map_err(|_| CtyParseError::InvalidHeader(line.to_string()))?;
    let itu_zone: u8 = fields[1]
        .parse()
        .map_err(|_| CtyParseError::InvalidHeader(line.to_string()))?;
    let continent = parse_continent(fields[2])?;
    let lat: f64 = fields[3]
        .parse()
        .map_err(|_| CtyParseError::InvalidHeader(line.to_string()))?;
    let lon: f64 = fields[4]
        .parse()
        .map_err(|_| CtyParseError::InvalidHeader(line.to_string()))?;

    Ok(CtyEntity {
        name: name_part.trim().trim_start_matches('*').to_string(),
        continent,
        cq_zone,
        itu_zone,
        lat,
        lon,
        primary_prefix: primary_prefix.to_string(),
    })
}

fn parse_continent(code: &str) -> Result<Continent, CtyParseError> {
    match code {
        "AF" => Ok(Continent::AF),
        "AN" => Ok(Continent::AN),
        "AS" => Ok(Continent::AS),
        "EU" => Ok(Continent::EU),
        "NA" => Ok(Continent::NA),
        "OC" => Ok(Continent::OC),
        "SA" => Ok(Continent::SA),
        _ => Err(CtyParseError::UnknownContinent(code.to_string())),
    }
}

struct ParsedPrefix {
    prefix: String,
    cq_zone_override: Option<u8>,
    itu_zone_override: Option<u8>,
    exact: bool,
}

fn parse_prefix_modifiers(raw: &str) -> ParsedPrefix {
    let exact = raw.starts_with('=');
    let s = if exact { &raw[1..] } else { raw };

    let mut prefix = String::new();
    let mut cq_zone_override = None;
    let mut itu_zone_override = None;

    let mut in_paren = false;
    let mut in_bracket = false;
    let mut in_angle = false;
    let mut in_brace = false;
    let mut paren_buf = String::new();
    let mut bracket_buf = String::new();

    for ch in s.chars() {
        match ch {
            '(' => {
                in_paren = true;
                paren_buf.clear();
            }
            ')' => {
                in_paren = false;
                cq_zone_override = paren_buf.parse().ok();
            }
            '[' => {
                in_bracket = true;
                bracket_buf.clear();
            }
            ']' => {
                in_bracket = false;
                itu_zone_override = bracket_buf.parse().ok();
            }
            '<' => in_angle = true,
            '>' => in_angle = false,
            '{' => in_brace = true,
            '}' => in_brace = false,
            '~' => break, // ignore rest (UTC override)
            _ => {
                if in_paren {
                    paren_buf.push(ch);
                } else if in_bracket {
                    bracket_buf.push(ch);
                } else if !in_angle && !in_brace {
                    prefix.push(ch);
                }
            }
        }
    }

    ParsedPrefix {
        prefix,
        cq_zone_override,
        itu_zone_override,
        exact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CTY_DATA: &str = "\
Sov Mil Ord of Malta:         15  28  EU  41.90   12.43  -1.0  1A:
    1A;
United States:                05  08  NA  37.53  -97.00  5.0  K:
    AA,AB,AC,AD,AE,AF,AG,K,N,W,K5(4)[7];
Japan:                        25  45  AS  35.68  139.68  -9.0  JA:
    JA,JB,JC,JD,JE,JF,JG,JH,JI,JJ,JK,JL,JM,JN,JO,JP,JQ,JR,JS,
    7J,7K,7L,7M,7N,8J,8K,8L,8M,8N;
Anguilla:                     08  11  NA  18.20  -63.00  4.0  VP2E:
    VP2E;
ITU HQ:                       14  28  EU  46.17    6.05  -1.0  4U:
    4U;
United Nations HQ:            05  08  NA  40.75  -73.97  5.0  4U1U:
    4U1U;
*Sealand:                     14  27  EU  51.53    1.28   0.0  HZ:
    =HZ1AB;
";

    fn test_resolver() -> CtyResolver {
        CtyResolver::from_cty_dat(TEST_CTY_DATA).unwrap()
    }

    #[test]
    fn parse_basic_entities() {
        let resolver = test_resolver();
        assert!(resolver.entity_count() >= 6);
        assert!(resolver.prefix_count() > 10);
    }

    #[test]
    fn resolve_us_callsign() {
        let resolver = test_resolver();
        let info = resolver.resolve("W1AW").unwrap();
        assert_eq!(info.entity_name, "United States");
        assert_eq!(info.continent, Continent::NA);
        assert_eq!(info.cq_zone, 5);
        assert_eq!(info.itu_zone, 8);
        assert_eq!(info.primary_prefix, "K");
    }

    #[test]
    fn resolve_japan_callsign() {
        let resolver = test_resolver();
        let info = resolver.resolve("JA1ABC").unwrap();
        assert_eq!(info.entity_name, "Japan");
        assert_eq!(info.continent, Continent::AS);
        assert_eq!(info.cq_zone, 25);
        assert_eq!(info.itu_zone, 45);
    }

    #[test]
    fn resolve_anguilla() {
        let resolver = test_resolver();
        let info = resolver.resolve("VP2EHC").unwrap();
        assert_eq!(info.entity_name, "Anguilla");
        assert_eq!(info.continent, Continent::NA);
        assert_eq!(info.cq_zone, 8);
    }

    #[test]
    fn longest_prefix_match_un_hq() {
        let resolver = test_resolver();
        // 4U1UN should match 4U1U (United Nations HQ) not 4U (ITU HQ)
        let info = resolver.resolve("4U1UN").unwrap();
        assert_eq!(info.entity_name, "United Nations HQ");
        assert_eq!(info.cq_zone, 5);
    }

    #[test]
    fn shortest_prefix_match_itu() {
        let resolver = test_resolver();
        // 4U2ABC should match 4U (ITU HQ) since 4U1U doesn't match
        let info = resolver.resolve("4U2ABC").unwrap();
        assert_eq!(info.entity_name, "ITU HQ");
    }

    #[test]
    fn unknown_callsign_returns_none() {
        let resolver = test_resolver();
        assert!(resolver.resolve("GARBAGE").is_none());
        assert!(resolver.resolve("").is_none());
    }

    #[test]
    fn case_insensitive_lookup() {
        let resolver = test_resolver();
        let info = resolver.resolve("ja1abc").unwrap();
        assert_eq!(info.entity_name, "Japan");
    }

    #[test]
    fn cq_zone_override() {
        let resolver = test_resolver();
        // K5 has CQ zone override (4) and ITU zone override [7]
        let info = resolver.resolve("K5ABC").unwrap();
        assert_eq!(info.entity_name, "United States");
        assert_eq!(info.cq_zone, 4);
        assert_eq!(info.itu_zone, 7);
    }

    #[test]
    fn no_override_uses_entity_defaults() {
        let resolver = test_resolver();
        // W prefix has no override, uses entity defaults
        let info = resolver.resolve("W3LPL").unwrap();
        assert_eq!(info.cq_zone, 5);
        assert_eq!(info.itu_zone, 8);
    }

    #[test]
    fn exact_match_prefix() {
        let resolver = test_resolver();
        // =HZ1AB is an exact match entry for Sealand
        let info = resolver.resolve("HZ1AB").unwrap();
        assert_eq!(info.entity_name, "Sealand");
    }

    #[test]
    fn deleted_entity_prefix_stripped() {
        let resolver = test_resolver();
        // *Sealand has the star stripped from the name
        let info = resolver.resolve("HZ1AB").unwrap();
        assert!(!info.entity_name.starts_with('*'));
    }

    #[test]
    fn japan_7j_prefix() {
        let resolver = test_resolver();
        let info = resolver.resolve("7J1ABC").unwrap();
        assert_eq!(info.entity_name, "Japan");
    }

    #[test]
    fn single_char_prefix() {
        let resolver = test_resolver();
        let info = resolver.resolve("K1ABC").unwrap();
        assert_eq!(info.entity_name, "United States");
    }

    #[test]
    fn single_char_prefix_n() {
        let resolver = test_resolver();
        let info = resolver.resolve("N1MM").unwrap();
        assert_eq!(info.entity_name, "United States");
    }

    #[test]
    fn sov_mil_ord_malta() {
        let resolver = test_resolver();
        let info = resolver.resolve("1A0ABC").unwrap();
        assert_eq!(info.entity_name, "Sov Mil Ord of Malta");
        assert_eq!(info.continent, Continent::EU);
    }

    // -----------------------------------------------------------------------
    // Prefix modifier parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_simple_prefix() {
        let p = parse_prefix_modifiers("W");
        assert_eq!(p.prefix, "W");
        assert!(!p.exact);
        assert!(p.cq_zone_override.is_none());
        assert!(p.itu_zone_override.is_none());
    }

    #[test]
    fn parse_exact_prefix() {
        let p = parse_prefix_modifiers("=W1AW");
        assert_eq!(p.prefix, "W1AW");
        assert!(p.exact);
    }

    #[test]
    fn parse_cq_override() {
        let p = parse_prefix_modifiers("K5(4)");
        assert_eq!(p.prefix, "K5");
        assert_eq!(p.cq_zone_override, Some(4));
    }

    #[test]
    fn parse_itu_override() {
        let p = parse_prefix_modifiers("K5[7]");
        assert_eq!(p.prefix, "K5");
        assert_eq!(p.itu_zone_override, Some(7));
    }

    #[test]
    fn parse_both_overrides() {
        let p = parse_prefix_modifiers("K5(4)[7]");
        assert_eq!(p.prefix, "K5");
        assert_eq!(p.cq_zone_override, Some(4));
        assert_eq!(p.itu_zone_override, Some(7));
    }

    #[test]
    fn parse_with_location_override() {
        let p = parse_prefix_modifiers("W5<30.00/-90.00>");
        assert_eq!(p.prefix, "W5");
    }

    #[test]
    fn parse_with_continent_override() {
        let p = parse_prefix_modifiers("VP2E{NA}");
        assert_eq!(p.prefix, "VP2E");
    }
}
