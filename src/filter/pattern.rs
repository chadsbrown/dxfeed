//! Compiled pattern sets (glob + regex) for hot-path matching.

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use regex::RegexSet;

use super::config::PatternSetSerde;
use super::error::FilterConfigError;

/// A compiled set of glob and regex patterns, ready for fast matching.
#[derive(Debug, Clone)]
pub struct CompiledPatternSet {
    pub raw: PatternSetSerde,
    globset: Option<GlobSet>,
    regexset: Option<RegexSet>,
}

impl CompiledPatternSet {
    /// Compile a `PatternSetSerde` into a ready-to-match pattern set.
    pub fn compile(raw: PatternSetSerde) -> Result<Self, FilterConfigError> {
        let globset = if raw.globs.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for g in &raw.globs {
                let glob = if raw.case_insensitive {
                    GlobBuilder::new(g)
                        .case_insensitive(true)
                        .build()
                        .map_err(|_| FilterConfigError::InvalidGlob(g.clone()))?
                } else {
                    Glob::new(g).map_err(|_| FilterConfigError::InvalidGlob(g.clone()))?
                };
                builder.add(glob);
            }
            Some(
                builder
                    .build()
                    .map_err(|_| FilterConfigError::InvalidGlob("<globset>".into()))?,
            )
        };

        let regexset = if raw.regexes.is_empty() {
            None
        } else {
            let patterns: Vec<String> = if raw.case_insensitive {
                raw.regexes.iter().map(|r| format!("(?i:{})", r)).collect()
            } else {
                raw.regexes.clone()
            };
            Some(
                RegexSet::new(patterns)
                    .map_err(|e| FilterConfigError::InvalidRegex(e.to_string()))?,
            )
        };

        Ok(Self {
            raw,
            globset,
            regexset,
        })
    }

    /// Returns true if ANY pattern matches the input string.
    /// Returns false if there are no patterns.
    pub fn is_match(&self, s: &str) -> bool {
        if let Some(gs) = &self.globset {
            if gs.is_match(s) {
                return true;
            }
        }
        if let Some(rs) = &self.regexset {
            if rs.is_match(s) {
                return true;
            }
        }
        false
    }

    /// Returns true if no patterns are configured.
    pub fn is_empty(&self) -> bool {
        self.raw.globs.is_empty() && self.raw.regexes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(globs: &[&str], regexes: &[&str], case_insensitive: bool) -> PatternSetSerde {
        PatternSetSerde {
            globs: globs.iter().map(|s| s.to_string()).collect(),
            regexes: regexes.iter().map(|s| s.to_string()).collect(),
            case_insensitive,
        }
    }

    #[test]
    fn empty_pattern_set() {
        let compiled = CompiledPatternSet::compile(PatternSetSerde::default()).unwrap();
        assert!(compiled.is_empty());
        assert!(!compiled.is_match("anything"));
    }

    #[test]
    fn glob_matching() {
        let compiled = CompiledPatternSet::compile(ps(&["W1*"], &[], false)).unwrap();
        assert!(!compiled.is_empty());
        assert!(compiled.is_match("W1AW"));
        assert!(compiled.is_match("W1ABC"));
        assert!(!compiled.is_match("K1TTT"));
        assert!(!compiled.is_match("w1aw")); // case sensitive
    }

    #[test]
    fn glob_case_insensitive() {
        let compiled = CompiledPatternSet::compile(ps(&["W1*"], &[], true)).unwrap();
        assert!(compiled.is_match("W1AW"));
        assert!(compiled.is_match("w1aw"));
        assert!(compiled.is_match("w1AW"));
        assert!(!compiled.is_match("K1TTT"));
    }

    #[test]
    fn regex_matching() {
        let compiled = CompiledPatternSet::compile(ps(&[], &["^(VE|VA)"], false)).unwrap();
        assert!(compiled.is_match("VE3NEA"));
        assert!(compiled.is_match("VA3ABC"));
        assert!(!compiled.is_match("W1AW"));
        assert!(!compiled.is_match("ve3nea")); // case sensitive
    }

    #[test]
    fn regex_case_insensitive() {
        let compiled = CompiledPatternSet::compile(ps(&[], &["^(VE|VA)"], true)).unwrap();
        assert!(compiled.is_match("VE3NEA"));
        assert!(compiled.is_match("ve3nea"));
        assert!(!compiled.is_match("W1AW"));
    }

    #[test]
    fn mixed_glob_and_regex() {
        let compiled = CompiledPatternSet::compile(ps(&["W1*"], &["^JA"], false)).unwrap();
        assert!(compiled.is_match("W1AW"));
        assert!(compiled.is_match("JA1ABC"));
        assert!(!compiled.is_match("VE3NEA"));
    }

    #[test]
    fn invalid_glob() {
        let result = CompiledPatternSet::compile(ps(&["[invalid"], &[], false));
        assert!(result.is_err());
        match result.unwrap_err() {
            FilterConfigError::InvalidGlob(_) => {}
            other => panic!("expected InvalidGlob, got {:?}", other),
        }
    }

    #[test]
    fn invalid_regex() {
        let result = CompiledPatternSet::compile(ps(&[], &["(unclosed"], false));
        assert!(result.is_err());
        match result.unwrap_err() {
            FilterConfigError::InvalidRegex(_) => {}
            other => panic!("expected InvalidRegex, got {:?}", other),
        }
    }

    #[test]
    fn multiple_globs_any_match() {
        let compiled = CompiledPatternSet::compile(ps(&["W1*", "K3*"], &[], false)).unwrap();
        assert!(compiled.is_match("W1AW"));
        assert!(compiled.is_match("K3LR"));
        assert!(!compiled.is_match("N1MM"));
    }

    #[test]
    fn multiple_regexes_any_match() {
        let compiled =
            CompiledPatternSet::compile(ps(&[], &["^W1", "^K3"], false)).unwrap();
        assert!(compiled.is_match("W1AW"));
        assert!(compiled.is_match("K3LR"));
        assert!(!compiled.is_match("N1MM"));
    }
}
