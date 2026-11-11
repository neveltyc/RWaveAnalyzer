// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Filter pattern matching.
//!
//! Patterns are comma-separated and case-insensitive. Each one is matched
//! against either the signal's **leaf name** or its **whole hierarchical path**,
//! chosen by the pattern itself:
//!
//! * A pattern with no `.` matches the **leaf name** — the local variable name,
//!   with no scope around it. `tx_fifo_push_err` finds the status bit and not
//!   the nets inside a synchronizer instance named after it.
//! * A pattern containing a `.` matches the **whole path**, so `top.u_dma.*`
//!   and `dma.req` still select by hierarchy position.
//!
//! Within the chosen haystack, a pattern with no `*`/`?` names a signal
//! **exactly** when some haystack in reach equals it ([`Filters::prefer_exact`];
//! a trailing bus range is optional, so `state` names `state[2:0]`), and is a
//! **substring** match otherwise: `top.sig3` selects that signal and not
//! `top.sig30`, while `fifo` with no signal of that name finds every `*fifo*`.
//! A pattern containing `*` or `?` is an anchored **glob-lite** match where only
//! `*` (any span) and `?` (one char) are special; every other character —
//! notably `[` and `]` in bus ranges such as `data[7:0]` — is literal. This
//! intentionally differs from shell `fnmatch`.
//!
//! [`MatchMode::Exact`] drops the substring fallback: a wildcard-free pattern
//! must be the whole haystack or it matches nothing.

/// Hierarchy separators. Wellen normalizes VCD/FST/GHW paths to `.`; the
/// built-in FSDB backend emits either `.` or `/`. Both a pattern's
/// leaf-versus-path decision and `select`'s scope segmentation key off this one
/// set, so the two cannot disagree about what counts as a hierarchy.
pub(crate) const SEPARATORS: [char; 2] = ['.', '/'];

/// Maximum length of a single filter pattern (DoS guard).
pub(crate) const MAX_FILTER_PATTERN_LEN: usize = 256;
/// Maximum number of wildcard chars in one pattern (regex-blowup guard).
pub(crate) const MAX_FILTER_WILDCARDS: usize = 16;

#[derive(Debug, Clone)]
pub struct FilterParseError(pub String);

impl std::fmt::Display for FilterParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// How a wildcard-free pattern matches its haystack.
///
/// `Substring` is the default; `Exact` is what `--exact` selects. Only the
/// wildcard-free case differs — a glob is anchored under both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchMode {
    /// The whole haystack when one in reach equals the pattern, else any
    /// haystack containing it.
    #[default]
    Substring,
    /// The pattern must be the whole haystack.
    Exact,
}

/// A single compiled pattern: which haystack it applies to, and how it matches.
#[derive(Debug, Clone)]
struct Pat {
    domain: Domain,
    kind: PatKind,
}

/// Which string a pattern is matched against. Decided once, at parse time, by
/// whether the pattern contains a `.`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Domain {
    /// The whole hierarchical path.
    Path,
    /// The leaf (local variable) name only.
    Leaf,
}

#[derive(Debug, Clone)]
enum PatKind {
    /// Lower-cased wildcard-free pattern: whole-name equality when `exact`,
    /// else substring.
    Plain { lit: String, exact: bool },
    /// Glob-lite: a sequence of tokens to match against a lower-cased haystack.
    Glob(Vec<GlobTok>),
}

#[derive(Debug, Clone)]
pub(crate) enum GlobTok {
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
    /// layer, or split here from a single comma-joined string), matching by
    /// substring.
    pub fn parse<S: AsRef<str>>(raw_patterns: &[S]) -> Result<Filters, FilterParseError> {
        Filters::parse_mode(raw_patterns, MatchMode::Substring)
    }

    /// [`parse`](Self::parse) with the wildcard-free matching mode chosen by
    /// the caller.
    pub fn parse_mode<S: AsRef<str>>(
        raw_patterns: &[S],
        mode: MatchMode,
    ) -> Result<Filters, FilterParseError> {
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
            // A separator in the pattern is the user reaching for the hierarchy;
            // with none, they are naming a signal and mean the leaf. Both
            // separators count: the built-in FSDB backend emits '/'-separated
            // paths, and a pattern like `top/u_dma/*` addresses a hierarchy just
            // as plainly as its dotted equivalent.
            let domain = if lower.contains(SEPARATORS) { Domain::Path } else { Domain::Leaf };
            let kind = if lower.contains('*') || lower.contains('?') {
                PatKind::Glob(compile_glob(&lower))
            } else {
                PatKind::Plain { lit: lower, exact: mode == MatchMode::Exact }
            };
            pats.push(Pat { domain, kind });
        }
        Ok(Filters { pats })
    }

    /// Parse from a single comma-joined string (e.g. the raw `--filter` value).
    pub fn parse_csv(value: &str) -> Result<Filters, FilterParseError> {
        Filters::parse_csv_mode(value, MatchMode::Substring)
    }

    /// [`parse_csv`](Self::parse_csv) with an explicit matching mode.
    pub fn parse_csv_mode(value: &str, mode: MatchMode) -> Result<Filters, FilterParseError> {
        let parts: Vec<&str> = value.split(',').collect();
        Filters::parse_mode(&parts, mode)
    }

    pub fn is_empty(&self) -> bool {
        self.pats.is_empty()
    }

    /// Does any pattern match? Both haystacks must already be lower-cased —
    /// and lower-cased *separately*: `to_lowercase` can change a string's byte
    /// length, so a leaf can never be sliced out of an already-lowered path
    /// using offsets measured on the original.
    pub fn matches_lower(&self, path_lower: &str, leaf_lower: &str) -> bool {
        for p in &self.pats {
            let hay = match p.domain {
                Domain::Path => path_lower,
                Domain::Leaf => leaf_lower,
            };
            let hit = match &p.kind {
                PatKind::Plain { lit, exact: true } => names_exactly(hay, lit),
                PatKind::Plain { lit, exact: false } => hay.contains(lit.as_str()),
                PatKind::Glob(toks) => glob_match(toks, hay),
            };
            if hit {
                return true;
            }
        }
        false
    }

    /// Switch each wildcard-free pattern to whole-name matching when one of
    /// `names` — the lower-cased `(path, leaf)` pairs the pattern can reach —
    /// equals it. Asking for `top.sig3` then means that signal, not
    /// `top.sig30`..`top.sig39` too; a pattern no name equals keeps its
    /// substring reach, so exploring with a fragment still works.
    pub fn prefer_exact(&mut self, names: &[(String, String)]) {
        for p in &mut self.pats {
            if let PatKind::Plain { lit, exact: exact @ false } = &mut p.kind {
                *exact = names.iter().any(|(path, leaf)| {
                    let hay = match p.domain {
                        Domain::Path => path,
                        Domain::Leaf => leaf,
                    };
                    names_exactly(hay, lit)
                });
            }
        }
    }

    /// Does any wildcard-free pattern still wait for [`prefer_exact`](Self::prefer_exact)?
    pub fn needs_names(&self) -> bool {
        self.pats.iter().any(|p| matches!(p.kind, PatKind::Plain { exact: false, .. }))
    }

    /// [`matches_lower`](Self::matches_lower) for callers that hold the
    /// original-case strings and are not matching in a hot loop.
    pub fn matches_path_leaf(&self, path: &str, leaf: &str) -> bool {
        self.matches_lower(&path.to_lowercase(), &leaf.to_lowercase())
    }
}

/// Is `hay` the name `lit` spells, with or without a trailing bus range? A
/// vector's name carries its range (`state[2:0]`), which the user should not
/// have to type to name it.
fn names_exactly(hay: &str, lit: &str) -> bool {
    if hay == lit {
        return true;
    }
    match hay.strip_suffix(']').and_then(|h| h.rfind('[').map(|i| &hay[..i])) {
        Some(base) => base == lit,
        None => false,
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

/// Compile a lower-cased glob-lite pattern into tokens. Shared with `select`,
/// which matches the same syntax against one hierarchy segment at a time.
pub(crate) fn compile_glob(pat: &str) -> Vec<GlobTok> {
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
pub(crate) fn glob_match(toks: &[GlobTok], hay: &str) -> bool {
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

    /// Match a path whose leaf is everything after the last '.', which is what
    /// the ordinary (non-escaped) signals in these cases have. Real callers
    /// take the leaf from the signal table, never by splitting.
    fn m(f: &Filters, path: &str) -> bool {
        let leaf = path.rsplit('.').next().unwrap_or(path);
        f.matches_path_leaf(path, leaf)
    }

    #[test]
    fn substring_default() {
        let f = Filters::parse_csv("clk,rst").unwrap();
        assert!(m(&f, "tb.clk"));
        assert!(m(&f, "tb.rst_n"));
        assert!(!m(&f, "tb.data"));
    }

    #[test]
    fn case_insensitive() {
        let f = Filters::parse_csv("CLK").unwrap();
        assert!(m(&f, "tb.clk"));
    }

    #[test]
    fn glob_suffix() {
        let f = Filters::parse_csv("*_valid,*_ready").unwrap();
        assert!(m(&f, "tb.a_valid"));
        assert!(m(&f, "tb.b_ready"));
        assert!(!m(&f, "tb.valid_x"));
    }

    #[test]
    fn glob_scope_prefix() {
        let f = Filters::parse_csv("top.u_dma.*").unwrap();
        assert!(m(&f, "top.u_dma.req"));
        assert!(!m(&f, "top.u_cpu.req"));
    }

    #[test]
    fn brackets_are_literal() {
        // The '[' is literal; '*data[7:0]' matches the leaf 'data[7:0]'.
        let f = Filters::parse_csv("*data[7:0]").unwrap();
        assert!(m(&f, "tb.data[7:0]"));
        assert!(!m(&f, "tb.data[3:0]"));
    }

    #[test]
    fn question_mark() {
        let f = Filters::parse_csv("d?ta").unwrap();
        assert!(m(&f, "data"));
        assert!(m(&f, "dxta"));
        assert!(!m(&f, "dta"));
    }

    #[test]
    fn dotless_pattern_matches_the_leaf_not_the_scope() {
        // The motivating case: a CDC synchronizer instance is named after the
        // signal it synchronizes, so a whole-path match would drag in every net
        // inside it.
        let f = Filters::parse_csv("status").unwrap();
        assert!(m(&f, "top.status"));
        assert!(m(&f, "top.u_dma.status_q"));
        assert!(!m(&f, "top.u_bcm21_sync_status.clk_d"));
        assert!(!m(&f, "top.u_bcm21_sync_status.q_p"));
    }

    #[test]
    fn dotless_glob_anchors_to_the_leaf() {
        // Anchored against the leaf alone, so it cannot swallow scopes.
        let f = Filters::parse_csv("tb*").unwrap();
        assert!(m(&f, "tb_done"));
        assert!(!m(&f, "tb.clk"));
    }

    #[test]
    fn dotted_pattern_matches_the_whole_path() {
        let f = Filters::parse_csv("dma.req").unwrap();
        assert!(m(&f, "top.u_dma.req"));
        assert!(!m(&f, "top.u_cpu.req"));
        // A trailing dot is the way to ask for "anywhere under this name".
        let f = Filters::parse_csv("u_bcm21_sync_status.").unwrap();
        assert!(m(&f, "top.u_bcm21_sync_status.clk_d"));
    }

    #[test]
    fn leaf_matching_ignores_a_scope_that_shares_the_name() {
        // `clk` must not select every signal under a scope called `clk_gen`.
        let f = Filters::parse_csv("clk").unwrap();
        assert!(m(&f, "top.clk_div"));
        assert!(!m(&f, "top.clk_gen.enable"));
    }

    #[test]
    fn a_slash_separated_pattern_addresses_the_path() {
        // The built-in FSDB backend emits '/'-separated hierarchies. A pattern
        // written that way is reaching for the hierarchy just as plainly as a
        // dotted one, so it must not be matched against the leaf name.
        let path = "top/u_dma/req";
        let leaf = "req";
        for pat in ["top/u_dma/*", "u_dma/", "u_dma/req"] {
            let f = Filters::parse_csv(pat).unwrap();
            assert!(f.matches_path_leaf(path, leaf), "{pat} should match {path}");
        }
        // A different subtree is still excluded.
        let f = Filters::parse_csv("u_cpu/").unwrap();
        assert!(!f.matches_path_leaf(path, leaf));
        // And a bare name still means the leaf, on either separator style.
        let f = Filters::parse_csv("req").unwrap();
        assert!(f.matches_path_leaf(path, leaf));
        let f = Filters::parse_csv("u_dma").unwrap();
        assert!(!f.matches_path_leaf(path, leaf), "a scope name is not a leaf name");
    }

    fn primed(pat: &str, names: &[(&str, &str)]) -> Filters {
        let mut f = Filters::parse_csv(pat).unwrap();
        let names: Vec<(String, String)> =
            names.iter().map(|(p, l)| (p.to_string(), l.to_string())).collect();
        f.prefer_exact(&names);
        f
    }

    #[test]
    fn a_name_that_exists_is_matched_whole() {
        let names = [("top.sig3", "sig3"), ("top.sig30", "sig30")];
        for pat in ["top.sig3", "sig3", "SIG3"] {
            let f = primed(pat, &names);
            assert!(m(&f, "top.sig3"), "{pat}");
            assert!(!m(&f, "top.sig30"), "{pat}");
        }
        // No signal is named `sig`: the fragment keeps its substring reach.
        let f = primed("sig", &names);
        assert!(m(&f, "top.sig3") && m(&f, "top.sig30"));
    }

    #[test]
    fn a_bus_range_is_optional_in_a_whole_name() {
        let names = [("top.state[2:0]", "state[2:0]"), ("top.state_next[2:0]", "state_next[2:0]")];
        let f = primed("state", &names);
        assert!(m(&f, "top.state[2:0]"));
        assert!(!m(&f, "top.state_next[2:0]"));
        let f = primed("top.state", &names);
        assert!(m(&f, "top.state[2:0]") && !m(&f, "top.state_next[2:0]"));
        assert!(names_exactly("mem[3]", "mem"));
        assert!(!names_exactly("mem[3]x", "mem"));
    }

    #[test]
    fn each_pattern_is_primed_on_its_own() {
        let names = [("top.sig3", "sig3"), ("top.sig30", "sig30"), ("top.ctrl_a", "ctrl_a")];
        let f = primed("sig3,ctrl", &names);
        assert!(m(&f, "top.sig3") && !m(&f, "top.sig30") && m(&f, "top.ctrl_a"));
    }

    /// The motivating case for `--exact`: a substring pattern also names every
    /// signal that merely starts with it, and a strobe sharing the prefix is
    /// exactly the sort of thing that comes back looking like the answer.
    #[test]
    fn exact_mode_requires_the_whole_leaf() {
        let sub = Filters::parse_csv("DtsmTrainVal0Min").unwrap();
        assert!(m(&sub, "tb.DtsmTrainVal0Min"));
        assert!(m(&sub, "tb.DtsmTrainVal0Min_strobe"), "substring catches the strobe");

        let exact = Filters::parse_csv_mode("DtsmTrainVal0Min", MatchMode::Exact).unwrap();
        assert!(m(&exact, "tb.DtsmTrainVal0Min"));
        assert!(!m(&exact, "tb.DtsmTrainVal0Min_strobe"));
        assert!(!m(&exact, "tb.pre_DtsmTrainVal0Min"));
    }

    /// Exact mode picks the haystack the same way substring mode does: a
    /// separator in the pattern still means "match the whole path".
    #[test]
    fn exact_mode_keeps_the_leaf_versus_path_split() {
        let f = Filters::parse_csv_mode("tb.clk", MatchMode::Exact).unwrap();
        assert!(f.matches_path_leaf("tb.clk", "clk"));
        assert!(!f.matches_path_leaf("top.tb.clk", "clk"), "anchored, not a suffix");
        let f = Filters::parse_csv_mode("clk", MatchMode::Exact).unwrap();
        assert!(f.matches_path_leaf("top.tb.clk", "clk"), "bare name still means the leaf");
    }

    /// A pattern that already carries a wildcard is anchored either way, so
    /// `--exact` leaves it alone rather than turning `*` into a literal.
    #[test]
    fn exact_mode_does_not_change_a_glob() {
        for mode in [MatchMode::Substring, MatchMode::Exact] {
            let f = Filters::parse_csv_mode("*_valid", mode).unwrap();
            assert!(m(&f, "tb.a_valid"), "{mode:?}");
            assert!(!m(&f, "tb.a_valid_q"), "{mode:?}");
        }
    }

    /// Exactness is about anchoring, not about case: every other pattern in the
    /// tool folds case, and one that did not would be a trap of its own.
    #[test]
    fn exact_mode_is_still_case_insensitive() {
        let f = Filters::parse_csv_mode("CLK", MatchMode::Exact).unwrap();
        assert!(m(&f, "tb.clk"));
    }

    #[test]
    fn escaped_identifier_leaf_is_matched_whole() {
        // The leaf comes from the signal table, dots and all — so a pattern
        // naming part of it matches, and one naming the scope does not.
        let f = Filters::parse_csv("bar").unwrap();
        assert!(f.matches_path_leaf(r"tb.\foo.bar", r"\foo.bar"));
        let f = Filters::parse_csv("tb").unwrap();
        assert!(!f.matches_path_leaf(r"tb.\foo.bar", r"\foo.bar"));
    }
}
