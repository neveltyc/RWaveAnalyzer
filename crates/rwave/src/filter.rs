// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Filter pattern matching, mirroring `vcd_analyzer.py`.
//!
//! Patterns are comma-separated. A pattern with no `*`/`?` is a
//! case-insensitive **substring** match. A pattern containing `*` or `?` is a
//! **glob-lite** match where only `*` (any span) and `?` (one char) are
//! special; every other character — notably `[` and `]` in bus ranges such as
//! `data[7:0]` — is literal. This intentionally differs from shell `fnmatch`.

/// Maximum length of a single filter pattern (DoS guard).
const MAX_FILTER_PATTERN_LEN: usize = 256;
/// Maximum number of wildcard chars in one pattern (regex-blowup guard).
const MAX_FILTER_WILDCARDS: usize = 16;

#[derive(Debug, Clone)]
pub struct FilterParseError(pub String);

impl std::fmt::Display for FilterParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A single compiled pattern.
#[derive(Debug, Clone)]
enum Pat {
    /// Lower-cased substring.
    Substr(String),
    /// Glob-lite: a sequence of tokens to match against a lower-cased haystack.
    Glob(Vec<GlobTok>),
}

#[derive(Debug, Clone)]
enum GlobTok {
    /// `*` — match any (possibly empty) span.
    Star,
    /// `?` — match exactly one character.
    Any,
    /// A literal run of characters (already lower-cased).
    Lit(String),
}

/// A set of compiled filter patterns. `None` semantics (match-all) are handled
/// by the caller; an empty `Filters` matches nothing.
#[derive(Debug, Clone)]
pub struct Filters {
    pats: Vec<Pat>,
}

impl Filters {
    /// Parse a list of raw pattern strings (already split on commas by the CLI
    /// layer, or split here from a single comma-joined string).
    pub fn parse<S: AsRef<str>>(raw_patterns: &[S]) -> Result<Filters, FilterParseError> {
        let mut pats = Vec::new();
        for raw in raw_patterns {
            let pat = raw.as_ref().trim();
            if pat.is_empty() {
                continue;
            }
            if pat.len() > MAX_FILTER_PATTERN_LEN {
                return Err(FilterParseError(format!(
                    "filter pattern too long; max length is {MAX_FILTER_PATTERN_LEN}"
                )));
            }
            // Collapse runs of '*' into a single '*'.
            let collapsed = collapse_stars(pat);
            let wildcards = collapsed.chars().filter(|c| *c == '*' || *c == '?').count();
            if wildcards > MAX_FILTER_WILDCARDS {
                return Err(FilterParseError(format!(
                    "too many wildcard characters in filter pattern; max is {MAX_FILTER_WILDCARDS}"
                )));
            }
            let lower = collapsed.to_lowercase();
            if lower.contains('*') || lower.contains('?') {
                pats.push(Pat::Glob(compile_glob(&lower)));
            } else {
                pats.push(Pat::Substr(lower));
            }
        }
        Ok(Filters { pats })
    }

    /// Parse from a single comma-joined string (e.g. the raw `--filter` value).
    pub fn parse_csv(value: &str) -> Result<Filters, FilterParseError> {
        let parts: Vec<&str> = value.split(',').collect();
        Filters::parse(&parts)
    }

    pub fn is_empty(&self) -> bool {
        self.pats.is_empty()
    }

    /// Does any pattern match the given path? Matching is case-insensitive.
    pub fn matches(&self, path: &str) -> bool {
        let hay = path.to_lowercase();
        for p in &self.pats {
            match p {
                Pat::Substr(s) => {
                    if hay.contains(s.as_str()) {
                        return true;
                    }
                }
                Pat::Glob(toks) => {
                    if glob_match(toks, &hay) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

fn collapse_stars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_star = false;
    for c in s.chars() {
        if c == '*' {
            if !prev_star {
                out.push('*');
            }
            prev_star = true;
        } else {
            out.push(c);
            prev_star = false;
        }
    }
    out
}

fn compile_glob(pat: &str) -> Vec<GlobTok> {
    let mut toks = Vec::new();
    let mut lit = String::new();
    for c in pat.chars() {
        match c {
            '*' => {
                if !lit.is_empty() {
                    toks.push(GlobTok::Lit(std::mem::take(&mut lit)));
                }
                toks.push(GlobTok::Star);
            }
            '?' => {
                if !lit.is_empty() {
                    toks.push(GlobTok::Lit(std::mem::take(&mut lit)));
                }
                toks.push(GlobTok::Any);
            }
            other => lit.push(other),
        }
    }
    if !lit.is_empty() {
        toks.push(GlobTok::Lit(lit));
    }
    toks
}

/// Anchored glob match (the whole string must match), supporting `*` and `?`.
/// Implemented as a backtracking matcher over char slices; pattern size is
/// bounded by the parser so worst-case cost is acceptable.
fn glob_match(toks: &[GlobTok], hay: &str) -> bool {
    let hay: Vec<char> = hay.chars().collect();
    glob_rec(toks, 0, &hay, 0)
}

fn glob_rec(toks: &[GlobTok], ti: usize, hay: &[char], hi: usize) -> bool {
    if ti == toks.len() {
        return hi == hay.len();
    }
    match &toks[ti] {
        GlobTok::Star => {
            // Try to consume 0..=remaining chars.
            let remaining = hay.len().saturating_sub(hi);
            for skip in 0..=remaining {
                if glob_rec(toks, ti + 1, hay, hi + skip) {
                    return true;
                }
            }
            false
        }
        GlobTok::Any => {
            if hi < hay.len() {
                glob_rec(toks, ti + 1, hay, hi + 1)
            } else {
                false
            }
        }
        GlobTok::Lit(s) => {
            let lit: Vec<char> = s.chars().collect();
            if hi + lit.len() > hay.len() {
                return false;
            }
            for (k, &lc) in lit.iter().enumerate() {
                if hay[hi + k] != lc {
                    return false;
                }
            }
            // Advance one *token*, but consume `lit.len()` haystack chars.
            glob_rec(toks, ti + 1, hay, hi + lit.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_default() {
        let f = Filters::parse_csv("clk,rst").unwrap();
        assert!(f.matches("tb.clk"));
        assert!(f.matches("tb.rst_n"));
        assert!(!f.matches("tb.data"));
    }

    #[test]
    fn case_insensitive() {
        let f = Filters::parse_csv("CLK").unwrap();
        assert!(f.matches("tb.clk"));
    }

    #[test]
    fn glob_suffix() {
        let f = Filters::parse_csv("*_valid,*_ready").unwrap();
        assert!(f.matches("tb.a_valid"));
        assert!(f.matches("tb.b_ready"));
        assert!(!f.matches("tb.valid_x"));
    }

    #[test]
    fn glob_scope_prefix() {
        let f = Filters::parse_csv("top.u_dma.*").unwrap();
        assert!(f.matches("top.u_dma.req"));
        assert!(!f.matches("top.u_cpu.req"));
    }

    #[test]
    fn brackets_are_literal() {
        // The '[' is literal; '*data[7:0]' matches 'tb.data[7:0]'.
        let f = Filters::parse_csv("*data[7:0]").unwrap();
        assert!(f.matches("tb.data[7:0]"));
        assert!(!f.matches("tb.data[3:0]"));
    }

    #[test]
    fn question_mark() {
        let f = Filters::parse_csv("d?ta").unwrap();
        assert!(f.matches("data"));
        assert!(f.matches("dxta"));
        assert!(!f.matches("dta"));
    }
}
