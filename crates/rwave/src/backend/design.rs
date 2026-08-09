// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Design-connectivity queries: "who drives this signal, and what does it drive".
//!
//! This is deliberately *not* part of the C plugin ABI. A waveform file records
//! values over time and says nothing about connectivity; answering a driver
//! question needs an elaborated design database (Verdi's KDB) alongside it. Only
//! the built-in Verdi NPI backend can do that, so the capability lives here as a
//! plain Rust trait that backend reaches through
//! [`WaveformBackend::design_query`](super::WaveformBackend::design_query).
//! Every other backend — wellen's VCD/FST/GHW, WLF, and every external plugin —
//! inherits the default `None` and the command layer reports a clean
//! "unsupported" instead.
//!
//! Keeping this out of the vtable is a correctness requirement, not just tidiness:
//! `RwaveBackend` is a C struct whose length is fixed by the plugin that was
//! compiled against it, so appending capability slots is what makes a new host
//! read past the end of an older plugin's vtable. See `docs/PLUGIN.md`.

use std::path::{Path, PathBuf};

/// What kind of construct drives (or loads) a signal.
///
/// Derived from the NPI object type reported for the driving *statement*, not
/// guessed from names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HopKind {
    /// `assign lhs = rhs` — a continuous assignment.
    ContAssign,
    /// A procedural assignment inside an `always`/`initial` block.
    Procedural,
    /// A module/interface port the signal passes through; the real driver is on
    /// the other side of the hierarchy boundary.
    Port,
    /// An enclosing `if`/`case`/`while`/`@(...)` — this gates the assignment
    /// rather than supplying its data.
    Control,
    /// A literal or parameter.
    Constant,
    /// Reported by NPI but not one of the shapes above.
    Other,
}

impl HopKind {
    pub fn tag(&self) -> &'static str {
        match self {
            HopKind::ContAssign => "assign",
            HopKind::Procedural => "procedural",
            HopKind::Port => "port",
            HopKind::Control => "control",
            HopKind::Constant => "constant",
            HopKind::Other => "other",
        }
    }
}

/// One endpoint of a trace: a statement that drives (or reads) the queried
/// signal, plus the signals that statement itself depends on.
#[derive(Debug, Clone)]
pub struct Hop {
    /// 1-based group number as reported by NPI, so several hops can share a
    /// source and stay grouped in output.
    pub group: usize,
    pub kind: HopKind,
    /// NPI's own object type (`npiContAssign`, `npiPort`, …), passed through so
    /// a caller can tell exactly what NPI saw even when `kind` folds several
    /// types together.
    pub npi_type: String,
    /// The statement's source text, e.g. `assign res = res_q`.
    pub statement: String,
    /// The scope the statement lives in.
    pub scope: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    /// True when this hop crossed a hierarchy port boundary to get here.
    pub boundary: bool,
    /// Signals the statement reads (for a driver) or the target it writes (for
    /// a load), as full hierarchical paths.
    pub signals: Vec<String>,
}

/// Overall confidence in a trace result. Reporting "I cannot see it" beats
/// reporting a driver that is really just a reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceStatus {
    /// Structural drivers/loads were found.
    Resolved,
    /// Nothing at all came back.
    NotFound,
    /// Only port boundaries came back: the signal is driven from outside the
    /// part of the hierarchy NPI could follow.
    BoundaryOnly,
    /// Every driver candidate is also a load of the same net, which is what a
    /// testbench driving through a virtual interface looks like from an RTL
    /// point of view. The real driver is in class-based testbench code that the
    /// RTL fan-in cannot see.
    TestbenchDriven,
}

impl TraceStatus {
    pub fn tag(&self) -> &'static str {
        match self {
            TraceStatus::Resolved => "resolved",
            TraceStatus::NotFound => "no_driver_found",
            TraceStatus::BoundaryOnly => "boundary_only",
            TraceStatus::TestbenchDriven => "testbench_driven",
        }
    }
}

/// Result of one driver/load query. Truncation is the command layer's job, so
/// this carries everything found.
#[derive(Debug, Clone)]
pub struct TraceOutcome {
    pub hops: Vec<Hop>,
    pub status: TraceStatus,
}

/// Which way to walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Driver,
    Load,
}

impl Direction {
    pub fn tag(&self) -> &'static str {
        match self {
            Direction::Driver => "driver",
            Direction::Load => "load",
        }
    }
}

/// Connectivity queries against an elaborated design database.
///
/// Implemented only by the built-in Verdi NPI FSDB backend. Loading a design is
/// expensive and checks out a license, so [`ensure_design`](Self::ensure_design)
/// is idempotent: a `--batch` session pays for it once and every later query is
/// an in-memory lookup.
pub trait DesignQuery {
    /// Load `kdb` (idempotent — a repeat call with the same database is a no-op).
    /// On failure the session must stay usable so the caller can retry with a
    /// different `--kdb`.
    fn ensure_design(&mut self, kdb: &Path, top: Option<&str>) -> Result<(), String>;

    /// The design library this waveform records having been produced from, if
    /// the format carries it.
    ///
    /// Not a guess: FSDB stores the absolute `simv.daidir` path in its header,
    /// so the file can say which design it belongs to instead of us assuming it
    /// sits in the same directory. Returns `None` when the field is absent
    /// (a dump from another simulator, or one written without it).
    fn recorded_design_dir(&mut self) -> Option<PathBuf> {
        None
    }

    /// Trace `signal` in the given direction. `control` includes the enclosing
    /// `if`/`case`/clock-edge dependencies; with it false NPI omits them at the
    /// source, which is how clock/reset noise is suppressed without ever
    /// pattern-matching on signal names.
    fn trace(
        &mut self,
        signal: &str,
        dir: Direction,
        control: bool,
    ) -> Result<TraceOutcome, String>;
}

/// Why a design database could not be located. One variant per cause, since
/// each has a different fix.
#[derive(Debug)]
pub enum KdbMiss {
    /// The waveform records no design directory and none was given.
    NotRecorded,
    /// The waveform names a design library that is not reachable.
    RecordedGone(PathBuf),
    /// An explicit `--kdb` that holds no elaborated database.
    ExplicitMissing(PathBuf),
    /// A path holding `work.lib++` but no `kdb.elab++`.
    NotElaborated(PathBuf),
}

impl KdbMiss {
    /// The error text. State what is wrong and point at the fix; no diagnosis.
    pub fn into_error(self) -> String {
        match self {
            KdbMiss::NotRecorded => "this waveform records no design library. \
                 Pass --kdb <simv.daidir>, or build one with vcs -kdb."
                .to_string(),
            KdbMiss::RecordedGone(p) => format!(
                "the design library recorded in this waveform is not accessible: {}. \
                 Pass --kdb <simv.daidir>.",
                p.display()
            ),
            KdbMiss::ExplicitMissing(p) => {
                format!("--kdb {} holds no kdb.elab++.", p.display())
            }
            KdbMiss::NotElaborated(p) => format!(
                "{} is not elaborated: it holds work.lib++ but no kdb.elab++. \
                 Run elabcom -elab kdb.",
                p.display()
            ),
        }
    }
}

/// Normalize a path to the elaborated database. Accepts a `simv.daidir` or the
/// `kdb.elab++` inside it. `work.lib++` gets its own variant: NPI loads it
/// without complaint and then resolves nothing, so the symptom is unreadable.
fn normalize_kdb(p: &Path) -> Result<PathBuf, KdbMiss> {
    if p.file_name().is_some_and(|n| n == "kdb.elab++") && p.exists() {
        return Ok(p.to_path_buf());
    }
    let elab = p.join("kdb.elab++");
    if elab.exists() {
        return Ok(elab);
    }
    if p.join("work.lib++").exists() {
        return Err(KdbMiss::NotElaborated(p.to_path_buf()));
    }
    Err(KdbMiss::NotRecorded) // caller replaces this with the right variant
}

/// Locate the design database. Two sources only: `--kdb`, taken literally and
/// never falling back, and `recorded`, the `simv.daidir` path VCS writes into
/// the FSDB header. No directory scan: a neighbouring build is not evidence
/// that it is the right build, and using the wrong one answers from the wrong
/// design without ever failing.
pub fn probe_kdb(cli_kdb: Option<&str>, recorded: Option<&Path>) -> Result<PathBuf, KdbMiss> {
    if let Some(k) = cli_kdb.map(str::trim).filter(|s| !s.is_empty()) {
        let p = PathBuf::from(k);
        return normalize_kdb(&p).map_err(|e| match e {
            KdbMiss::NotRecorded => KdbMiss::ExplicitMissing(p),
            other => other,
        });
    }
    match recorded {
        None => Err(KdbMiss::NotRecorded),
        Some(r) => normalize_kdb(r).map_err(|e| match e {
            KdbMiss::NotRecorded => KdbMiss::RecordedGone(r.to_path_buf()),
            other => other,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("rwave-kdb-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn the_path_recorded_in_the_waveform_is_used_without_any_argument() {
        let d = tmpdir("recorded");
        let real = d.join("build/simv.daidir");
        std::fs::create_dir_all(real.join("kdb.elab++")).unwrap();
        assert_eq!(probe_kdb(None, Some(&real)).unwrap(), real.join("kdb.elab++"));
    }

    #[test]
    fn an_explicit_kdb_is_taken_literally_and_never_falls_back() {
        let d = tmpdir("explicit");
        let recorded = d.join("build/simv.daidir");
        std::fs::create_dir_all(recorded.join("kdb.elab++")).unwrap();
        // A perfectly good database is recorded in the file, but the user named
        // somewhere else. Silently using the other one would answer from a
        // different design than the one they asked about.
        let err = probe_kdb(Some(d.join("nope").to_str().unwrap()), Some(&recorded)).unwrap_err();
        assert!(matches!(err, KdbMiss::ExplicitMissing(_)));
    }

    #[test]
    fn a_dump_moved_away_from_its_build_fails_with_that_diagnosis() {
        let d = tmpdir("moved");
        // Deliberately no fallback scan: a database sitting next to the dump is
        // not evidence that it is the *right* one.
        std::fs::create_dir_all(d.join("simv.daidir/kdb.elab++")).unwrap();
        let err = probe_kdb(None, Some(Path::new("/gone/simv.daidir"))).unwrap_err();
        assert!(matches!(err, KdbMiss::RecordedGone(_)));
        let msg = err.into_error();
        assert!(msg.contains("/gone/simv.daidir"));
        assert!(msg.contains("--kdb"));
    }

    #[test]
    fn a_waveform_that_records_nothing_says_so() {
        let err = probe_kdb(None, None).unwrap_err();
        assert!(matches!(err, KdbMiss::NotRecorded));
        assert!(err.into_error().contains("records no design library"));
    }

    #[test]
    fn accepts_the_elab_directory_named_directly() {
        let d = tmpdir("direct");
        std::fs::create_dir_all(d.join("kdb.elab++")).unwrap();
        let got = probe_kdb(Some(d.join("kdb.elab++").to_str().unwrap()), None).unwrap();
        assert_eq!(got, d.join("kdb.elab++"));
    }

    #[test]
    fn a_non_elaborated_library_is_named_as_such() {
        let d = tmpdir("notelab");
        std::fs::create_dir_all(d.join("work.lib++")).unwrap();
        let err = probe_kdb(Some(d.to_str().unwrap()), None).unwrap_err();
        assert!(matches!(err, KdbMiss::NotElaborated(_)));
        let msg = err.into_error();
        assert!(msg.contains("not elaborated"));
        assert!(msg.contains("elabcom"));
    }
}
