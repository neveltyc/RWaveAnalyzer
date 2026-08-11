// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! [`DesignQuery`] for a `.wlf`, answered from Questa's debug database.
//!
//! The waveform itself has no connectivity; Questa keeps it in a `.dbg` written
//! by `vopt -debugdb`. That file is SQLite behind a replaced header, so rwave
//! reads it directly — see [`super::super::questa::dbg`].

use std::path::{Path, PathBuf};

use crate::backend::design::{DesignQuery, Direction, TraceOutcome};
use crate::plugin::builtin::questa::source::SourceCache;
use crate::plugin::builtin::questa::{err, to_questa};

use super::backend::WlfBackend;

/// What the backend keeps between queries.
#[derive(Default)]
pub struct DesignSession {
    design: Option<crate::plugin::builtin::questa::dbg::Design>,
    /// Where the debug database is. Reported to the user and used to write the
    /// message when it turns out not to be usable.
    dbg: Option<PathBuf>,
    source: SourceCache,
}

impl WlfBackend {
    /// The waveform's own directory, which is what a source path is relative to.
    fn wlf_dir(&self) -> PathBuf {
        Path::new(self.path())
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

impl DesignQuery for WlfBackend {
    /// Questa names the debug database by the waveform's basename, falling back
    /// to `vsim.dbg`.
    fn locate_design(&mut self, cli_kdb: Option<&str>) -> Result<PathBuf, String> {
        if cli_kdb.is_some_and(|s| !s.trim().is_empty()) {
            return Err(err(
                "--kdb names a Verdi design library, which a WLF does not have. Questa \
                 loads the debug database with the same basename as the waveform \
                 (<name>.wlf -> <name>.dbg), so there is nothing to point at.",
            ));
        }
        let dbg = Path::new(self.path()).with_extension("dbg");
        if !dbg.is_file() {
            let alt = self.wlf_dir().join("vsim.dbg");
            let fallback = if alt.is_file() {
                format!(" ({} exists, but it belongs to another run)", alt.display())
            } else {
                String::new()
            };
            return Err(err(format!(
                "{} has no debug database beside it{fallback}. Connectivity is not in the \
                 waveform; generate it with:\n  \
                 vopt +acc <top> -o <opt> -debugdb\n  \
                 vsim -postsimdataflow -debugdb={} -wlf {} <opt>",
                self.path(),
                dbg.display(),
                self.path()
            )));
        }
        self.design.dbg = Some(dbg.clone());
        Ok(dbg)
    }

    fn ensure_design(&mut self, db: &Path, top: Option<&str>) -> Result<(), String> {
        if top.is_some_and(|t| !t.trim().is_empty()) {
            return Err(err(
                "--top selects a top module in a Verdi design library. A WLF session opens \
                 the dataset the waveform already names, so there is nothing to choose.",
            ));
        }
        if self.design.design.is_some() {
            return Ok(());
        }
        self.design.design = Some(crate::plugin::builtin::questa::dbg::Design::open(db, None)?);
        Ok(())
    }

    fn trace(
        &mut self,
        signal: &str,
        scope: &str,
        dir: Direction,
        control: bool,
    ) -> Result<TraceOutcome, String> {
        let questa_path = to_questa(signal, scope);
        let d = self
            .design
            .design
            .as_mut()
            .ok_or_else(|| err("internal: ensure_design must run first"))?;
        // A name the design does not carry is a different fact from a name with
        // no drivers, and an empty answer cannot say which.
        if !d.resolves(&questa_path) {
            return Err(err(format!(
                "the design database does not hold '{questa_path}' (rwave path '{signal}'). \
                 The waveform carries that signal, so this is a name-translation mismatch \
                 rather than a missing signal."
            )));
        }
        let mut hops = d.trace(&questa_path, dir, control)?;
        self.fill_statements(&mut hops);
        let status = crate::plugin::builtin::npi_dump::classify(&hops);
        Ok(TraceOutcome { hops, status })
    }
}

impl WlfBackend {
    /// Replace each endpoint's placeholder statement with the source line it
    /// came from, when that file can be read. Questa stores a location but no
    /// text.
    fn fill_statements(&mut self, hops: &mut [crate::backend::design::Hop]) {
        let dir = self.wlf_dir();
        for h in hops.iter_mut() {
            if let (Some(f), Some(l)) = (h.file.clone(), h.line)
                && let Some(text) = self.design.source.line(&dir, &f, l)
            {
                h.statement = text;
            }
        }
    }
}
