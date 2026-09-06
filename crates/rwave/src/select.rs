// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Signal selection: which signals a command works on.
//!
//! Four options narrow a query, and they compose as one pipeline over each
//! **alias path** in turn:
//!
//! ```text
//! alias path → --scope: inside the subtree?
//!            → --depth: near enough to its root?
//!            → --filter: name matches?
//!            → --exclude: not dropped?
//! ```
//!
//! A signal is selected when *any one* of its alias paths clears every gate.
//! Judging paths rather than signals is what makes exclusion safe on a net
//! visible at several points in the hierarchy: a status bit whose only excluded
//! path is the copy wired into a synchronizer survives through its clean path,
//! while the synchronizer's own internal nets — which have no path outside it —
//! drop out. Deciding per signal would throw away the very bit that was asked
//! for.
//!
//! Gates are ANDed and `--exclude` is applied last, so it always wins.

use crate::filter::{compile_glob, glob_match, Filters, GlobTok, MatchMode, SEPARATORS};
use crate::filter::{MAX_FILTER_PATTERN_LEN, MAX_FILTER_WILDCARDS};
use crate::model::{leaf_of, SignalInfo};

// Scope paths are segmented on [`SEPARATORS`], the same set that decides
// whether a `--filter` pattern addresses a leaf or a path. A scope whose *name*
// contains a separator (an escaped identifier) segments wrongly here —
// documented in docs/PLUGIN.md, and harmless for the leaf name, which is never
// derived by splitting.

/// One compiled `--scope` value.
#[derive(Debug, Clone)]
enum ScopePat {
    /// No `.` in the value: match one scope's own name (its last segment).
    Instance(SegPat),
    /// Dotted value: match as a segment-aligned suffix, so `u_tx.u_fifo`
    /// selects that subtree wherever it sits, and a path written out from the
    /// root matches as its own suffix.
    Suffix(Vec<SegPat>),
}

/// A pattern for a single hierarchy segment. Anchored: `u_fifo` never matches
/// the segment `u_fifo_ctrl`, which is the point of matching segment-wise
/// instead of by substring.
#[derive(Debug, Clone)]
enum SegPat {
    Lit(String),
    Glob(Vec<GlobTok>),
}

impl SegPat {
    fn parse(raw: &str) -> Result<SegPat, String> {
        if raw.len() > MAX_FILTER_PATTERN_LEN {
            return Err(format!(
                "scope pattern too long; max length is {MAX_FILTER_PATTERN_LEN}"
            ));
        }
        let lower = raw.to_lowercase();
        let wildcards = lower.chars().filter(|c| *c == '*' || *c == '?').count();
        if wildcards > MAX_FILTER_WILDCARDS {
            return Err(format!(
                "too many wildcard characters in scope pattern; max is {MAX_FILTER_WILDCARDS}"
            ));
        }
        Ok(if wildcards > 0 {
            SegPat::Glob(compile_glob(&lower))
        } else {
            SegPat::Lit(lower)
        })
    }

    /// `seg` must already be lower-cased.
    fn matches(&self, seg: &str) -> bool {
        match self {
            SegPat::Lit(s) => s == seg,
            SegPat::Glob(toks) => glob_match(toks, seg),
        }
    }
}

/// The compiled `--filter` / `--scope` / `--depth` / `--exclude` of one
/// invocation.
#[derive(Debug, Clone)]
pub struct Selection {
    /// `None` = no `--scope` (every path is in range).
    scope: Option<Vec<ScopePat>>,
    /// `None` = no `--depth`. Always accompanied by a scope (the CLI rejects
    /// the pairing otherwise), since depth is counted from the scope root.
    depth: Option<u32>,
    /// `None` = no `--filter` (every path clears the include gate).
    include: Option<Filters>,
    /// `None` = no `--exclude` (no path is dropped).
    exclude: Option<Filters>,
}

impl Selection {
    /// Compile the raw option values. A value that is empty or all blanks
    /// (`''`, `","`) is treated as absent, so it widens the selection rather
    /// than matching nothing — which is also how a `--batch` line lifts an
    /// inherited default.
    pub fn parse(
        scope: &Option<String>,
        depth: Option<i64>,
        filter: &Option<String>,
        exclude: &Option<String>,
    ) -> Result<Selection, String> {
        Selection::parse_mode(scope, depth, filter, exclude, MatchMode::Substring)
    }

    /// [`parse`](Self::parse) with the `--filter` / `--exclude` matching mode
    /// chosen by the caller (`--exact` picks [`MatchMode::Exact`]). The mode
    /// applies to both pattern gates: a run that asks for exact names wants
    /// them on the side that drops rows too, or `--exclude` would keep quietly
    /// removing more than it names.
    pub fn parse_mode(
        scope: &Option<String>,
        depth: Option<i64>,
        filter: &Option<String>,
        exclude: &Option<String>,
        mode: MatchMode,
    ) -> Result<Selection, String> {
        let compile = |raw: &Option<String>| -> Result<Option<Filters>, String> {
            match raw {
                Some(r) => {
                    let f = Filters::parse_csv_mode(r, mode).map_err(|e| e.0)?;
                    Ok(if f.is_empty() { None } else { Some(f) })
                }
                None => Ok(None),
            }
        };
        let scope = match scope {
            Some(raw) => {
                let mut pats = Vec::new();
                for value in raw.split(',') {
                    let value = value.trim();
                    if value.is_empty() {
                        continue;
                    }
                    let segs: Vec<&str> =
                        value.split(SEPARATORS).filter(|s| !s.is_empty()).collect();
                    if segs.is_empty() {
                        continue;
                    }
                    pats.push(if segs.len() == 1 && !value.contains(SEPARATORS) {
                        ScopePat::Instance(SegPat::parse(segs[0])?)
                    } else {
                        ScopePat::Suffix(
                            segs.iter().map(|s| SegPat::parse(s)).collect::<Result<_, _>>()?,
                        )
                    });
                }
                if pats.is_empty() {
                    None
                } else {
                    Some(pats)
                }
            }
            None => None,
        };
        let depth = match depth {
            // A depth without a scope cannot be honored and is rejected at the
            // CLI layer; ignore it here rather than filtering on a base that
            // does not exist. Saturate rather than cast: a depth past u32 means
            // "as deep as it goes", and wrapping would turn it into a small
            // number — or into 0, which matches nothing — and answer with a
            // quietly wrong result set instead of everything.
            Some(n) if n > 0 && scope.is_some() => {
                Some(u32::try_from(n).unwrap_or(u32::MAX))
            }
            _ => None,
        };
        Ok(Selection {
            scope,
            depth,
            include: compile(filter)?,
            exclude: compile(exclude)?,
        })
    }

    /// True when no option is active, i.e. every signal is selected. Callers
    /// use this to skip per-signal work entirely.
    pub fn is_all(&self) -> bool {
        self.scope.is_none()
            && self.depth.is_none()
            && self.include.is_none()
            && self.exclude.is_none()
    }

    /// Names the options actually in force, for error messages that need to
    /// explain why a plainly-present signal was not found.
    pub fn active_gates(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.scope.is_some() {
            parts.push("--scope");
        }
        if self.depth.is_some() {
            parts.push("--depth");
        }
        if self.include.is_some() {
            parts.push("--filter");
        }
        if self.exclude.is_some() {
            parts.push("--exclude");
        }
        parts.join(", ")
    }

    /// The `--scope` and `--depth` gates, which depend only on where a path
    /// sits. Returns false when the path is out of range.
    fn structural_ok(&self, scope_path: &str) -> bool {
        let pats = match &self.scope {
            Some(p) => p,
            // No scope means no depth either (see `parse`), so nothing to check.
            None => return true,
        };
        let segs: Vec<String> = scope_path
            .split(SEPARATORS)
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase)
            .collect();

        // Find where the scope matched. Every match site is a *prefix* of the
        // path's own scope chain, so a hit means the signal sits in that scope
        // or below it. Several patterns (or one pattern at several depths) can
        // match; take the deepest, which is the most permissive reading of
        // "how far below the selected root is this signal".
        let mut base: Option<usize> = None;
        for k in 1..=segs.len() {
            let hit = pats.iter().any(|p| match p {
                ScopePat::Instance(sp) => sp.matches(&segs[k - 1]),
                ScopePat::Suffix(sps) => {
                    k >= sps.len()
                        && sps.iter().zip(&segs[k - sps.len()..k]).all(|(sp, s)| sp.matches(s))
                }
            });
            if hit {
                base = Some(k);
            }
        }
        let base = match base {
            Some(b) => b,
            None => return false,
        };
        match self.depth {
            // Depth counts the leaf as 1, so a signal sitting directly in the
            // matched scope is depth 1. Derived from the scope chain, never by
            // counting separators in the path — an escaped identifier carries
            // its own dots and would inflate the count.
            Some(max) => (segs.len() - base + 1) as u32 <= max,
            None => true,
        }
    }

    /// The gates that decide whether a path is *shown*: everything except
    /// `--filter`. `list` prints one row per alias, and a row the user asked to
    /// exclude (or scoped away) must not come back on a signal that some other
    /// path selected — otherwise `--exclude` would still display what it was
    /// meant to hide. `--filter` deliberately does not hide rows: it says which
    /// signals are of interest, and the other paths of such a signal are
    /// information, not noise.
    pub fn displays_alias(&self, path: &str, scope: &str) -> bool {
        if !self.structural_ok(scope) {
            return false;
        }
        match &self.exclude {
            Some(e) => !e.matches_path_leaf(path, leaf_of(path, scope)),
            None => true,
        }
    }

    /// Does this alias path clear every gate?
    pub fn keeps_alias(&self, path: &str, scope: &str) -> bool {
        if !self.structural_ok(scope) {
            return false;
        }
        // Lower-case once and share the result across both pattern gates.
        // Separately, never by slicing a lowered path: `to_lowercase` can
        // change a string's byte length.
        let path_lower = path.to_lowercase();
        let leaf_lower = leaf_of(path, scope).to_lowercase();
        if let Some(inc) = &self.include {
            if !inc.matches_lower(&path_lower, &leaf_lower) {
                return false;
            }
        }
        match &self.exclude {
            Some(e) => !e.matches_lower(&path_lower, &leaf_lower),
            None => true,
        }
    }

    /// Is this signal selected — does any of its alias paths clear every gate?
    pub fn keeps_signal(&self, info: &SignalInfo) -> bool {
        info.alias_pairs().any(|(p, sc)| self.keeps_alias(p, sc))
    }

    /// The alias path that actually cleared the gates, or `None` when the
    /// signal is not selected.
    ///
    /// [`keeps_signal`](Self::keeps_signal) answers *whether*; this answers
    /// *through which name*. Output rows are labelled with the signal's
    /// canonical path, so on a waveform where one signal is declared under
    /// several names the two can differ — and reporting only the canonical one
    /// answers a question the user did not ask.
    pub fn matched_alias<'a>(&self, info: &'a SignalInfo) -> Option<&'a str> {
        info.alias_pairs()
            .find(|(p, sc)| self.keeps_alias(p, sc))
            .map(|(p, _)| p)
    }

    /// As [`keeps_signal`](Self::keeps_signal), with `pat` as one more term on
    /// the *same* alias. Used to resolve a `search` name inside the current
    /// selection: the path that matches the name must itself be a path the
    /// selection kept, not merely belong to a signal selected via some other
    /// path.
    pub fn keeps_signal_matching(&self, info: &SignalInfo, pat: &Filters) -> bool {
        info.alias_pairs().any(|(p, sc)| {
            if !self.keeps_alias(p, sc) {
                return false;
            }
            pat.matches_path_leaf(p, leaf_of(p, sc))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(scope: Option<&str>, depth: Option<i64>, f: Option<&str>, x: Option<&str>) -> Selection {
        Selection::parse(
            &scope.map(str::to_string),
            depth,
            &f.map(str::to_string),
            &x.map(str::to_string),
        )
        .expect("options compile")
    }

    fn sel_exact(f: Option<&str>, x: Option<&str>) -> Selection {
        Selection::parse_mode(
            &None,
            None,
            &f.map(str::to_string),
            &x.map(str::to_string),
            MatchMode::Exact,
        )
        .expect("options compile")
    }

    // The motivating hierarchy: a status bit, the CDC synchronizer instance
    // named after it, and an unrelated signal carrying the name.
    const STATUS: (&str, &str) = ("top.status", "top");
    const SYNC_CLK: (&str, &str) = ("top.u_bcm21_sync_status.clk_d", "top.u_bcm21_sync_status");
    const SYNC_D: (&str, &str) = ("top.u_bcm21_sync_status.d_p", "top.u_bcm21_sync_status");
    const DMA_Q: (&str, &str) = ("top.u_dma.status_q", "top.u_dma");

    fn keeps(s: &Selection, sig: (&str, &str)) -> bool {
        s.keeps_alias(sig.0, sig.1)
    }

    #[test]
    fn no_options_selects_everything() {
        let s = sel(None, None, None, None);
        assert!(s.is_all());
        assert!(keeps(&s, STATUS) && keeps(&s, SYNC_CLK));
        assert_eq!(s.active_gates(), "");
    }

    #[test]
    fn blank_values_widen_rather_than_match_nothing() {
        assert!(sel(None, None, Some(" , "), None).is_all());
        assert!(sel(None, None, None, Some("")).is_all());
        assert!(sel(Some(""), None, None, None).is_all());
    }

    // -- --filter / --exclude ------------------------------------------------

    #[test]
    fn filter_matches_the_leaf_and_skips_the_scope_named_after_it() {
        let s = sel(None, None, Some("status"), None);
        assert!(keeps(&s, STATUS));
        assert!(keeps(&s, DMA_Q));
        assert!(!keeps(&s, SYNC_CLK));
    }

    #[test]
    fn exclude_is_applied_after_filter_and_wins() {
        let s = sel(None, None, Some("clk"), Some("*_sync_*.*"));
        assert!(!keeps(&s, SYNC_CLK));
    }

    #[test]
    fn dropping_a_whole_subtree_takes_a_dotted_pattern() {
        // Consequence of leaf matching worth stating outright: a dot-free
        // `--exclude u_bcm21_sync_status` drops nothing, because no *leaf* is
        // called that. Naming a subtree means naming a path, and the readable
        // way to do it is a trailing dot — "anything under this".
        let bare = sel(None, None, None, Some("u_bcm21_sync_status"));
        assert!(keeps(&bare, SYNC_CLK), "a scope name is not a leaf name");

        for pat in ["u_bcm21_sync_status.", "*_sync_*.*"] {
            let s = sel(None, None, None, Some(pat));
            assert!(!keeps(&s, SYNC_CLK), "{pat} should drop the subtree");
            assert!(keeps(&s, STATUS), "{pat} should spare everything else");
            assert!(keeps(&s, DMA_Q), "{pat} should spare everything else");
        }
    }

    #[test]
    fn exclude_works_on_its_own() {
        let s = sel(None, None, None, Some("*_sync_*.*"));
        assert!(!s.is_all());
        assert!(keeps(&s, STATUS) && keeps(&s, DMA_Q));
        assert!(!keeps(&s, SYNC_CLK));
    }

    #[test]
    fn a_signal_survives_through_one_clean_alias() {
        // `top.status` and the synchronizer's `d_p` are the same net. Excluding
        // the synchronizer hides that path but must not lose the signal.
        let s = sel(None, None, None, Some("*_sync_*.*"));
        assert!(keeps(&s, STATUS));
        assert!(!keeps(&s, SYNC_D));
        assert!(!s.displays_alias(SYNC_D.0, SYNC_D.1));
        assert!(s.displays_alias(STATUS.0, STATUS.1));
    }

    #[test]
    fn filter_does_not_hide_rows_but_exclude_and_scope_do() {
        let s = sel(None, None, Some("nothing_matches_this"), None);
        assert!(s.displays_alias(STATUS.0, STATUS.1), "--filter never hides a row");
        let s = sel(Some("u_dma"), None, None, None);
        assert!(!s.displays_alias(STATUS.0, STATUS.1), "--scope hides out-of-range rows");
    }

    // -- --scope -------------------------------------------------------------

    #[test]
    fn instance_form_matches_a_scope_name_exactly() {
        let s = sel(Some("u_dma"), None, None, None);
        assert!(keeps(&s, DMA_Q));
        assert!(!keeps(&s, STATUS));
        // Anchored per segment: a longer instance name is a different scope.
        let s = sel(Some("u_dm"), None, None, None);
        assert!(!keeps(&s, DMA_Q));
    }

    #[test]
    fn instance_form_takes_wildcards() {
        let s = sel(Some("u_bcm21*"), None, None, None);
        assert!(keeps(&s, SYNC_CLK));
        assert!(!keeps(&s, DMA_Q));
        let s = sel(Some("u_?ma"), None, None, None);
        assert!(keeps(&s, DMA_Q));
    }

    #[test]
    fn instance_form_matches_at_any_level_and_includes_descendants() {
        let s = sel(Some("u_m0"), None, None, None);
        assert!(s.keeps_alias("root.u_m0.cnt", "root.u_m0"));
        assert!(s.keeps_alias("root.u_m0.u_a.cnt", "root.u_m0.u_a"), "descendants included");
        assert!(!s.keeps_alias("root.u_m1.u_a.cnt", "root.u_m1.u_a"));
    }

    #[test]
    fn dotted_form_matches_a_segment_aligned_suffix() {
        let s = sel(Some("u_m0.u_a"), None, None, None);
        assert!(s.keeps_alias("root.u_m0.u_a.cnt", "root.u_m0.u_a"));
        assert!(s.keeps_alias("root.u_m0.u_a.u_x.cnt", "root.u_m0.u_a.u_x"));
        assert!(!s.keeps_alias("root.u_m0.u_b.cnt", "root.u_m0.u_b"));
        assert!(!s.keeps_alias("root.u_m1.u_a.cnt", "root.u_m1.u_a"));
    }

    #[test]
    fn a_full_path_from_the_root_matches_as_its_own_suffix() {
        let s = sel(Some("root.u_m0"), None, None, None);
        assert!(s.keeps_alias("root.u_m0.cnt", "root.u_m0"));
        assert!(!s.keeps_alias("other.u_m0.cnt", "other.u_m0"));
    }

    #[test]
    fn segment_alignment_prevents_partial_name_matches() {
        // The whole point of segment-wise matching: `fifo` is not `u_fifo`.
        let s = sel(Some("fifo"), None, None, None);
        assert!(!s.keeps_alias("top.u_fifo.cnt", "top.u_fifo"));
        let s = sel(Some("u_fifo"), None, None, None);
        assert!(!s.keeps_alias("top.u_fifo_ctrl.cnt", "top.u_fifo_ctrl"));
    }

    #[test]
    fn several_scopes_are_ored() {
        let s = sel(Some("u_dma,u_bcm21_sync_status"), None, None, None);
        assert!(keeps(&s, DMA_Q) && keeps(&s, SYNC_CLK));
        assert!(!keeps(&s, STATUS));
    }

    #[test]
    fn a_top_level_signal_is_outside_every_scope() {
        let s = sel(Some("top"), None, None, None);
        assert!(!s.keeps_alias("clk", ""), "no scope path, so nothing to match");
        assert!(keeps(&s, STATUS));
    }

    #[test]
    fn slash_separated_hierarchies_segment_too() {
        // The built-in FSDB backend emits '/' as a separator.
        let s = sel(Some("u_dma"), None, None, None);
        assert!(s.keeps_alias("top/u_dma/req", "top/u_dma"));
        let s = sel(Some("top.u_dma"), None, None, None);
        assert!(s.keeps_alias("top/u_dma/req", "top/u_dma"), "pattern separator is free");
    }

    #[test]
    fn filter_and_exclude_reach_slash_hierarchies_too() {
        // The gate that used to be blind to '/': only `--scope` segmented on
        // both separators, so on an FSDB hierarchy a path-shaped --filter or
        // --exclude was matched against the leaf name and could never hit.
        const REQ: (&str, &str) = ("top/u_dma/req", "top/u_dma");
        const OTHER: (&str, &str) = ("top/u_cpu/req", "top/u_cpu");

        let s = sel(None, None, Some("top/u_dma/*"), None);
        assert!(keeps(&s, REQ));
        assert!(!keeps(&s, OTHER));

        let s = sel(None, None, None, Some("u_dma/"));
        assert!(!keeps(&s, REQ), "the subtree is dropped");
        assert!(keeps(&s, OTHER));

        // A bare name still means the leaf, so it spans both subtrees.
        let s = sel(None, None, Some("req"), None);
        assert!(keeps(&s, REQ) && keeps(&s, OTHER));
    }

    // -- --depth -------------------------------------------------------------

    #[test]
    fn depth_one_keeps_only_the_scopes_own_signals() {
        let s = sel(Some("u_m0"), Some(1), None, None);
        assert!(s.keeps_alias("root.u_m0.cnt", "root.u_m0"), "directly in scope = depth 1");
        assert!(!s.keeps_alias("root.u_m0.u_a.cnt", "root.u_m0.u_a"), "one level down = 2");
    }

    #[test]
    fn depth_two_reaches_one_level_of_children() {
        let s = sel(Some("u_m0"), Some(2), None, None);
        assert!(s.keeps_alias("root.u_m0.cnt", "root.u_m0"));
        assert!(s.keeps_alias("root.u_m0.u_a.cnt", "root.u_m0.u_a"));
        assert!(!s.keeps_alias("root.u_m0.u_a.u_x.cnt", "root.u_m0.u_a.u_x"));
    }

    #[test]
    fn depth_is_measured_from_the_deepest_matching_scope() {
        // Both `u_m0` and its child `u_a` match; measuring from the deeper one
        // is the permissive reading, and keeps the signal at depth 1.
        let s = sel(Some("u_m0,u_a"), Some(1), None, None);
        assert!(s.keeps_alias("root.u_m0.u_a.cnt", "root.u_m0.u_a"));
    }

    #[test]
    fn depth_counts_scope_segments_not_dots_in_the_path() {
        // An escaped identifier carries dots inside the *leaf*; counting them
        // would put this signal a level deeper than it is.
        let s = sel(Some("tb"), Some(1), None, None);
        assert!(s.keeps_alias(r"tb.\foo.bar", "tb"));
    }

    #[test]
    fn an_enormous_depth_saturates_instead_of_wrapping() {
        // A depth past u32 means "as deep as it goes". Casting wrapped it —
        // 2^32 became 0, which matches nothing — so a larger depth returned
        // fewer signals than a smaller one.
        for n in [u32::MAX as i64 + 1, u32::MAX as i64 + 2, i64::MAX] {
            let s = sel(Some("u_m0"), Some(n), None, None);
            assert!(
                s.keeps_alias("root.u_m0.u_a.u_x.cnt", "root.u_m0.u_a.u_x"),
                "--depth {n} should keep everything under the scope"
            );
        }
    }

    #[test]
    fn depth_without_a_scope_is_inert() {
        // The CLI rejects the combination; the type must not filter on a base
        // it never established.
        let s = sel(None, Some(1), None, None);
        assert!(s.is_all());
        assert!(s.keeps_alias("a.b.c.d", "a.b.c"));
    }

    // -- combinations --------------------------------------------------------

    #[test]
    fn gates_are_anded() {
        let s = sel(Some("u_m0"), Some(2), Some("cnt"), Some("*_dbg"));
        assert!(s.keeps_alias("root.u_m0.u_a.cnt", "root.u_m0.u_a"));
        assert!(!s.keeps_alias("root.u_m0.u_a.cnt_dbg", "root.u_m0.u_a"), "excluded");
        assert!(!s.keeps_alias("root.u_m0.u_a.flag", "root.u_m0.u_a"), "filtered out");
        assert!(!s.keeps_alias("root.u_m1.u_a.cnt", "root.u_m1.u_a"), "out of scope");
        assert!(!s.keeps_alias("root.u_m0.u_a.u_x.cnt", "root.u_m0.u_a.u_x"), "too deep");
        assert_eq!(s.active_gates(), "--scope, --depth, --filter, --exclude");
    }

    #[test]
    fn keeps_signal_needs_one_alias_to_clear_everything() {
        let info = crate::model::test_signal(
            &[STATUS, SYNC_D],
        );
        let s = sel(None, None, None, Some("*_sync_*.*"));
        assert!(s.keeps_signal(&info), "kept via the clean path");
        let s = sel(Some("u_bcm21_sync_status"), None, None, Some("*_sync_*.*"));
        assert!(!s.keeps_signal(&info), "the only in-scope path is excluded");
    }

    #[test]
    fn keeps_signal_matching_needs_one_alias_to_do_both() {
        // The name must match a path the selection kept — not a path that was
        // scoped away on a signal some *other* path selected.
        let info = crate::model::test_signal(&[STATUS, SYNC_D]);
        let pat = Filters::parse_csv("d_p").expect("pattern");
        let s = sel(None, None, None, None);
        assert!(s.keeps_signal_matching(&info, &pat));
        let s = sel(None, None, None, Some("*_sync_*.*"));
        assert!(
            !s.keeps_signal_matching(&info, &pat),
            "the matching path is excluded, even though the signal survives"
        );
    }

    /// `--exact` reaches both pattern gates. An exclusion that still matched by
    /// substring would keep dropping more than it names, which is the quieter
    /// half of the same problem.
    #[test]
    fn exact_applies_to_filter_and_exclude() {
        let sig = crate::model::test_signal(&[("tb.req", "tb")]);
        let strobe = crate::model::test_signal(&[("tb.req_strobe", "tb")]);

        let s = sel_exact(Some("req"), None);
        assert!(s.keeps_signal(&sig));
        assert!(!s.keeps_signal(&strobe));

        let s = sel_exact(None, Some("req"));
        assert!(!s.keeps_signal(&sig));
        assert!(s.keeps_signal(&strobe), "exclude is anchored too");

        // Substring mode is unchanged: both are caught either way.
        let s = sel(None, None, Some("req"), None);
        assert!(s.keeps_signal(&sig) && s.keeps_signal(&strobe));
    }

    /// `matched_alias` answers *through which name*, which is what lets a
    /// command say so when the row it prints is labelled differently.
    #[test]
    fn matched_alias_names_the_path_that_matched() {
        let info = crate::model::test_signal(&[("tb.foo", "tb"), ("tb.foo_copy", "tb")]);
        let s = sel_exact(Some("foo_copy"), None);
        assert_eq!(s.matched_alias(&info), Some("tb.foo_copy"));
        assert_ne!(s.matched_alias(&info), Some(info.path.as_str()));

        let s = sel_exact(Some("foo"), None);
        assert_eq!(s.matched_alias(&info), Some("tb.foo"));

        let s = sel_exact(Some("nothing"), None);
        assert_eq!(s.matched_alias(&info), None);
    }
}
