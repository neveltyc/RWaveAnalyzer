// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Derive the NPI library paths from a Verdi install pointed at by `$VERDI_HOME`.
//!
//! A licensed Verdi install needs nothing more than `$VERDI_HOME` to read FSDB:
//! `libNPI.so` carries `RPATH=$ORIGIN`, so its own dependencies (incl.
//! `libvfs.so`, its only non-standard one) resolve with no `LD_LIBRARY_PATH`,
//! and `npi_init` locates the rest of the Verdi tree from `$VERDI_HOME` itself.
//! So when the explicit override env vars are unset, we point rwave straight at
//! `$VERDI_HOME/share/NPI/lib/<arch>/lib{NPI,npiL1}.so`.

use std::path::{Path, PathBuf};

/// Per-arch lib subdir under `share/NPI/lib`, in preference order. The casing
/// and gcc-suffix vary across Verdi releases — a single tree ships several
/// (`linux64`, `LINUX64`, …) — so probe rather than hardcode one name.
const NPI_LIB_DIRS: &[&str] =
    &["linux64", "LINUX64", "LINUXAMD64", "linux64_gcc920", "LINUX64_GNU_920"];

const NPI_FILENAME: &str = "libNPI.so";
const L1_FILENAME: &str = "libnpiL1.so";

/// `$VERDI_HOME`, if set and pointing at a directory.
fn verdi_home() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("VERDI_HOME")?);
    p.is_dir().then_some(p)
}

/// The `share/NPI/lib/<arch>` directory under `home` that actually holds
/// `libNPI.so`, probing the known arch-dir names in preference order.
fn npi_lib_dir_in(home: &Path) -> Option<PathBuf> {
    let base = home.join("share").join("NPI").join("lib");
    NPI_LIB_DIRS
        .iter()
        .map(|d| base.join(d))
        .find(|dir| dir.join(NPI_FILENAME).is_file())
}

fn npi_lib_in(home: &Path) -> Option<PathBuf> {
    Some(npi_lib_dir_in(home)?.join(NPI_FILENAME))
}

fn npi_l1_lib_in(home: &Path) -> Option<PathBuf> {
    let p = npi_lib_dir_in(home)?.join(L1_FILENAME);
    p.is_file().then_some(p)
}

/// `libNPI.so` under `$VERDI_HOME`, or `None` when `VERDI_HOME` is unset or
/// carries no NPI lib. A `None` (or a stale VERDI_HOME set for other tools)
/// simply falls through to the next locate tier — it is never an error here.
pub fn npi_lib() -> Option<PathBuf> {
    npi_lib_in(&verdi_home()?)
}

/// `libnpiL1.so` beside the resolved `libNPI.so` (same build dir), for `trace`.
pub fn npi_l1_lib() -> Option<PathBuf> {
    npi_l1_lib_in(&verdi_home()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("rwave-verdi-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Lay down `<home>/share/NPI/lib/<arch>/<files...>` and return `home`.
    fn install(home: &Path, arch: &str, files: &[&str]) {
        let dir = home.join("share").join("NPI").join("lib").join(arch);
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), b"").unwrap();
        }
    }

    #[test]
    fn finds_npi_and_l1_under_lowercase_linux64() {
        let home = tmpdir("basic");
        install(&home, "linux64", &[NPI_FILENAME, L1_FILENAME]);
        assert_eq!(
            npi_lib_in(&home),
            Some(home.join("share/NPI/lib/linux64").join(NPI_FILENAME))
        );
        assert_eq!(
            npi_l1_lib_in(&home),
            Some(home.join("share/NPI/lib/linux64").join(L1_FILENAME))
        );
    }

    #[test]
    fn probes_casing_variants_when_lowercase_absent() {
        let home = tmpdir("casing");
        install(&home, "LINUXAMD64", &[NPI_FILENAME, L1_FILENAME]);
        assert_eq!(
            npi_lib_in(&home),
            Some(home.join("share/NPI/lib/LINUXAMD64").join(NPI_FILENAME))
        );
    }

    #[test]
    fn prefers_linux64_when_several_dirs_coexist() {
        let home = tmpdir("prefer");
        // A real Verdi tree ships both; the preference order must win.
        install(&home, "LINUX64", &[NPI_FILENAME]);
        install(&home, "linux64", &[NPI_FILENAME]);
        assert_eq!(
            npi_lib_in(&home),
            Some(home.join("share/NPI/lib/linux64").join(NPI_FILENAME))
        );
    }

    #[test]
    fn missing_install_yields_none_not_error() {
        let home = tmpdir("empty");
        assert_eq!(npi_lib_in(&home), None);
        assert_eq!(npi_l1_lib_in(&home), None);
    }

    #[test]
    fn l1_absent_beside_npi_is_none() {
        let home = tmpdir("no-l1");
        install(&home, "linux64", &[NPI_FILENAME]);
        assert!(npi_lib_in(&home).is_some());
        assert_eq!(npi_l1_lib_in(&home), None);
    }
}
