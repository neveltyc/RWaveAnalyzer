// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Design-connectivity queries for the built-in Verdi NPI FSDB backend.
//!
//! Loads an elaborated design database (KDB) into the same NPI session that
//! already has the waveform open, then answers driver/load questions from it.
//! The two are independent inputs: the FSDB supplies values over time, the KDB
//! supplies structure, and only together can `trace` say both who drives a
//! signal and what it was carrying.
//!
//! The connectivity walk itself is Verdi's (`npi_trace_*_dump2` in
//! `libnpiL1.so`), which is what makes hierarchy crossing and pass-through
//! correct without reimplementing elaboration. Those entry points report
//! through a `FILE*`; see [`super::npi_design_sys`] for why that variant is the
//! one we can call, and [`parse_dump`] for the grammar.

use std::ffi::{c_char, c_int, c_void, CString};
use std::path::{Path, PathBuf};

use crate::backend::design::{DesignQuery, Direction, TraceOutcome};
use crate::plugin::builtin::npi_dump::{classify, parse_dump};
use crate::plugin::builtin::diag::bridge_err;

use super::backend::FsdbBackend;
use super::npi_design_sys::{self, TrcOption};
use super::fsdb_sys;

const ERR_PREFIX: &str = "rwave-fsdb";

/// Design-load state for one session.
///
/// `npi_load_design` is process-global in NPI and a failed load discards
/// whatever was loaded before, so the identity of what is currently loaded is
/// tracked here and a repeat request for the same database is a no-op. rwave
/// serves one waveform per process, so one loaded design per process is the
/// natural fit.
#[derive(Default)]
pub struct DesignSession {
    loaded: Option<(PathBuf, Option<String>)>,
    /// The argv handed to `npi_load_design`, kept alive for as long as the
    /// design is loaded.
    ///
    /// `npi_load_design` takes `char**` by value, which is consistent with
    /// parse-and-copy, but NPI is closed-source and the sibling `npi_init` is
    /// documented in this crate as retaining its argv. Outliving the design
    /// costs a few dozen bytes and removes the question entirely.
    _argv: Vec<CString>,
}

impl FsdbBackend {
    /// Load `kdb` unless it is already the loaded design.
    fn load_design(&mut self, kdb: &Path, top: Option<&str>) -> Result<(), String> {
        let want = (kdb.to_path_buf(), top.map(str::to_string));
        if self.design.loaded.as_ref() == Some(&want) {
            return Ok(());
        }
        // A failed load leaves NPI with no design, so drop our record of one
        // before trying: if this fails the session must not claim to still hold
        // the previous database.
        self.design.loaded = None;

        let kdb_str = kdb.to_str().ok_or_else(|| {
            bridge_err(ERR_PREFIX, format!("--kdb path is not valid UTF-8: {}", kdb.display()))
        })?;
        // argv[0] is a real executable path, not a placeholder: NPI inspects it
        // to locate the running module (see the note on `npi_init` in
        // `fsdb_sys`, which aborts the process on a placeholder).
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.into_os_string().into_string().ok())
            .unwrap_or_else(|| "rwave".to_string());
        let mut argv_owned: Vec<CString> = vec![
            CString::new(exe.replace('\0', "?")).unwrap_or_else(|_| CString::new("rwave").expect("static")),
            CString::new("-simflow").expect("static"),
            CString::new("-dbdir").expect("static"),
            CString::new(kdb_str)
                .map_err(|_| bridge_err(ERR_PREFIX, "--kdb path contains a NUL byte"))?,
        ];
        if let Some(t) = top.map(str::trim).filter(|s| !s.is_empty()) {
            argv_owned.push(CString::new("-top").expect("static"));
            argv_owned.push(
                CString::new(t)
                    .map_err(|_| bridge_err(ERR_PREFIX, "--top contains a NUL byte"))?,
            );
        }
        let argc = argv_owned.len() as c_int;
        // NULL-terminated like a real `main` argv: `argc` says how many there
        // are, but an option loop that walks to NULL instead would otherwise
        // run off the end of the allocation.
        let mut argv: Vec<*mut c_char> = argv_owned
            .iter()
            .map(|s| s.as_ptr() as *mut c_char)
            .chain(std::iter::once(std::ptr::null_mut()))
            .collect();

        let npi = fsdb_sys::npi();
        // Loading prints progress and license chatter to stdout/stderr, which
        // would corrupt --json output.
        let rc = {
            let _silence = fsdb_sys::silence_stdio();
            unsafe { (npi.load_design)(argc, argv.as_mut_ptr()) }
        };
        if rc != 1 {
            return Err(bridge_err(
                ERR_PREFIX,
                format!(
                    "npi_load_design failed (rc={rc}) for {}. Check --top.",
                    kdb.display()
                ),
            ));
        }
        self.design.loaded = Some(want);
        self.design._argv = argv_owned;
        Ok(())
    }

    /// Whether `name` resolves in the loaded design.
    ///
    /// The trace dump alone cannot separate "no such signal" from "no drivers"
    /// — both come back empty — and keying that distinction on the wording of
    /// NPI's output would break the day Verdi rephrases a header.
    fn resolves_in_design(&self, name: &str) -> bool {
        let Ok(c) = CString::new(name) else {
            return false;
        };
        let npi = fsdb_sys::npi();
        let _silence = fsdb_sys::silence_stdio();
        let h = unsafe { (npi.handle_by_name)(c.as_ptr(), std::ptr::null_mut()) };
        if h.is_null() {
            return false;
        }
        unsafe { (npi.release_handle)(h) };
        true
    }
}

impl DesignQuery for FsdbBackend {
    /// Read `simvDaidirPath` out of the FSDB header.
    ///
    /// VCS records the absolute path of the design library it dumped from, so
    /// the file can name its own design instead of us assuming co-location.
    /// `npi_waveform_info` takes a filename, not a session handle, so this is
    /// independent of the open session.
    fn recorded_design_dir(&mut self) -> Option<PathBuf> {
        let path = CString::new(self.path()).ok()?;
        let npi = fsdb_sys::npi();
        let mut info = fsdb_sys::NpiWaveformInfo::default();
        let rc = {
            let _silence = fsdb_sys::silence_stdio();
            unsafe { (npi.waveform_info)(path.as_ptr(), &mut info) }
        };
        if rc == 0 || info.simv_daidir_path.is_null() {
            return None;
        }
        // SAFETY: non-NULL and NUL-terminated per the API; owned by NPI, so we
        // copy rather than retain it.
        let s = unsafe { std::ffi::CStr::from_ptr(info.simv_daidir_path) }
            .to_string_lossy()
            .into_owned();
        (!s.trim().is_empty()).then(|| PathBuf::from(s))
    }

    fn ensure_design(&mut self, kdb: &Path, top: Option<&str>) -> Result<(), String> {
        // Resolve L1 first: without it there is nothing to answer with, and
        // failing here gives a better message than a successful design load
        // followed by "no such symbol".
        npi_design_sys::ensure_loaded()?;
        self.load_design(kdb, top)
    }

    fn trace(
        &mut self,
        signal: &str,
        dir: Direction,
        control: bool,
    ) -> Result<TraceOutcome, String> {
        if self.design.loaded.is_none() {
            return Err(bridge_err(ERR_PREFIX, "no design loaded; call ensure_design first"));
        }
        let l1 = npi_design_sys::ensure_loaded()?;
        let opts = TrcOption::new(control);
        let sig = CString::new(signal)
            .map_err(|_| bridge_err(ERR_PREFIX, "signal name contains a NUL byte"))?;

        let run = |f: unsafe extern "C" fn(
            *const c_char,
            *mut c_void,
            bool,
            *mut c_void,
            *const TrcOption,
        ) -> c_int| {
            npi_design_sys::capture_dump(|file| {
                let _silence = fsdb_sys::silence_stdio();
                // isPassThrough = true so the walk crosses hierarchy port
                // boundaries instead of stopping at the enclosing module;
                // NULL boundary vector = we do not restrict where it may go.
                unsafe { f(sig.as_ptr(), file, true, std::ptr::null_mut(), &opts) }
            })
        };

        // Check resolvability up front so an unknown name is reported as such
        // rather than as an empty (and apparently successful) trace.
        if !self.resolves_in_design(signal) {
            return Err(bridge_err(
                ERR_PREFIX,
                format!(
                    "'{signal}' is not in the design database. \
                     Pass --top if the design's top module differs from the waveform's."
                ),
            ));
        }

        let text = match dir {
            Direction::Driver => run(l1.trace_driver_dump)?,
            Direction::Load => run(l1.trace_load_dump)?,
        };
        let hops = parse_dump(&text);

        let status = classify(&hops, dir, |d| {
            let text = match d {
                Direction::Driver => run(l1.trace_driver_dump),
                Direction::Load => run(l1.trace_load_dump),
            };
            // A failed cross-check query must not fail the whole trace: it can
            // only downgrade the verdict from testbench_driven to resolved.
            text.map(|t| parse_dump(&t)).unwrap_or_default()
        });
        Ok(TraceOutcome { hops, status })
    }
}
