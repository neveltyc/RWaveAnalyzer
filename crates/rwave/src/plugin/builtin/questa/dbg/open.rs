// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Opening a Questa `.dbg`.
//!
//! The file is SQLite with its 16-byte header replaced: `Modelsim dbg 1 \0`
//! where a normal database has `SQLite format 3\0`. Everything after those
//! bytes is stock SQLite, so the only thing standing between rwave and the
//! data is the magic.
//!
//! The user's file is never written to. It is opened where it lies, read-only,
//! through the VFS in [`super::vfs`], which corrects the header one read at a
//! time — no copy in memory, no temp copy on disk, no chance of touching a
//! simulation output. `RWAVE_DBG_OPEN=memory` selects the older path, which read
//! the whole file into memory with the header corrected on the way in and handed
//! it to `deserialize_read_exact`; it is kept for comparison.
//!
//! Every database carries its own schema version, and this reader was written
//! against exactly one. An unrecognised version is refused rather than parsed
//! on the assumption the layout held: reading a moved column would produce a
//! confident wrong answer, which is worse than no answer.

use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::vfs;
use crate::plugin::builtin::questa::err;

const QUESTA_MAGIC: &[u8; 16] = b"Modelsim dbg 1 \0";
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Which of the three database shapes a file is.
///
/// They are versioned differently, and the third is not versioned at all —
/// which is only apparent from a real database, not from the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The one beside the waveform: hierarchy, instances, wiring.
    Top,
    /// `__mti.dbg`: the index saying where each design unit's database is.
    Index,
    /// One per design unit: processes, shapes, and the signal-to-shape index.
    /// Carries no version of its own, so it is checked by structure instead.
    Unit,
}

impl Kind {
    /// `(table, key column, value column, the version this reader knows)`, for
    /// the kinds that state a version.
    fn version_probe(self) -> Option<(&'static str, &'static str, &'static str, i64)> {
        match self {
            Kind::Top => Some(("dbg_config_tbl", "property", "value", 6)),
            Kind::Index => Some(("dbg_options_tbl", "optionName", "value", 1)),
            Kind::Unit => None,
        }
    }
}

/// An open, read-only Questa database.
pub struct Db {
    conn: Connection,
    path: PathBuf,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Connection` is not Debug, and the path is the only identifying part.
        write!(f, "Db({})", self.path.display())
    }
}

impl Db {
    /// Read `path` into memory, correct the header, and hand it to SQLite.
    pub fn open(path: &Path, kind: Kind) -> Result<Db, String> {
        let mut f = std::fs::File::open(path)
            .map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
        let len = f
            .metadata()
            .map_err(|e| err(format!("cannot size {}: {e}", path.display())))?
            .len() as usize;
        if len < QUESTA_MAGIC.len() {
            return Err(err(format!("{} is too short to be a database", path.display())));
        }

        let mut magic = [0u8; 16];
        f.read_exact(&mut magic)
            .map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
        if &magic != QUESTA_MAGIC {
            return Err(err(format!(
                "{} is not a Questa debug database (header {:?})",
                path.display(),
                String::from_utf8_lossy(&magic).trim_end_matches('\0')
            )));
        }

        // The file itself, read through a VFS that corrects the header a read at
        // a time. Nothing is copied: SQLite pages the database off disk and
        // resident memory is its page cache. `RWAVE_DBG_OPEN=memory` selects the
        // older path — the whole file in memory — so the two can be compared on
        // the same database.
        if !matches!(std::env::var("RWAVE_DBG_OPEN").as_deref(), Ok("memory")) {
            let db = Self::open_through_vfs(path)?;
            db.check_version(kind)?;
            return Ok(db);
        }

        // The corrected header followed by the rest of the file, as one stream.
        // SQLite owns the allocation and frees it with the connection.
        let mut conn = Connection::open_in_memory()
            .map_err(|e| err(format!("cannot create an in-memory database: {e}")))?;
        conn.deserialize_read_exact(rusqlite::MAIN_DB, SQLITE_MAGIC.chain(f), len, true)
            .map_err(|e| {
                err(format!(
                    "{} did not open as a database: {e}. It should be SQLite behind a \
                     Questa header; a truncated or in-progress file is the usual cause.",
                    path.display()
                ))
            })?;

        let db = Db { conn, path: path.to_path_buf() };
        db.check_version(kind)?;
        Ok(db)
    }

    /// Open the file where it lies, through the header-correcting VFS.
    ///
    /// SQLite opens lazily, so the first page is not touched until something
    /// asks for it. A file that is not a database would otherwise be reported by
    /// whichever query happened to run first, in that query's words; the read is
    /// forced here so the diagnosis stays where it was.
    fn open_through_vfs(path: &Path) -> Result<Db, String> {
        let vfs = vfs::register()
            .map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
        // An absolute path, so SQLite cannot read the name as one of the URIs it
        // accepts in a filename's place.
        let real = std::fs::canonicalize(path)
            .map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
        let conn = Connection::open_with_flags_and_vfs(
            &real,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            vfs,
        )
        .and_then(|c| {
            // Forces page 1 through the VFS, which is where a file that is not
            // SQLite behind the header stops being one.
            c.query_row("PRAGMA schema_version", [], |r| r.get::<_, i64>(0)).map(|_| c)
        })
        .map_err(|e| {
            err(format!(
                "{} did not open as a database: {e}. It should be SQLite behind a \
                 Questa header; a truncated or in-progress file is the usual cause.",
                path.display()
            ))
        })?;
        Ok(Db { conn, path: path.to_path_buf() })
    }

    fn check_version(&self, kind: Kind) -> Result<(), String> {
        let Some((table, key, val, known)) = kind.version_probe() else {
            // A per-unit database states no version, so the check is that it
            // holds the two tables a trace reads. A layout change drastic
            // enough to drop one of them fails here rather than later, as an
            // empty answer.
            for t in ["signal_tbl", "shape_tbl"] {
                let n: i64 = self
                    .conn
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                        [t],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                if n == 0 {
                    return Err(err(format!(
                        "{} has no {t}; it is not a design-unit database.",
                        self.path.display()
                    )));
                }
            }
            return Ok(());
        };
        let got: Option<i64> = self
            .conn
            .query_row(
                &format!("SELECT {val} FROM {table} WHERE {key} = 'schema'"),
                [],
                |r| r.get(0),
            )
            .ok();
        match got {
            Some(v) if v == known => Ok(()),
            Some(v) => Err(err(format!(
                "{} is schema version {v}; rwave reads version {known}{}. \
                 Refusing rather than reading it as though the layout were unchanged.",
                self.path.display(),
                self.writer().map(|w| format!(" (this file was written by {w})")).unwrap_or_default()
            ))),
            None => Err(err(format!(
                "{} carries no schema version in {table}; it is not a database this \
                 reader recognises.",
                self.path.display()
            ))),
        }
    }

    /// The Questa version that wrote this database, when it says.
    pub fn writer(&self) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM dbg_config_tbl WHERE property = 'MTI_VERSION'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a file that is SQLite behind a Questa header, the way Questa
    /// writes one. Fixtures are generated rather than captured: a committed
    /// vendor file would pin one design's shapes, and this way a test can ask
    /// for exactly the rows it needs.
    pub(crate) fn write_dbg(path: &Path, kind: Kind, schema: i64, extra: &[&str]) {
        {
            let c = Connection::open(path).unwrap();
            let (table, key, val, _) = kind
                .version_probe()
                .expect("only the versioned kinds are written by this helper");
            c.execute_batch(&format!(
                "CREATE TABLE {table} ({key}, {val});
                 INSERT INTO {table} VALUES ('schema', {schema});"
            ))
            .unwrap();
            if kind == Kind::Top {
                c.execute(
                    "INSERT INTO dbg_config_tbl VALUES ('MTI_VERSION', '10.7c')",
                    [],
                )
                .unwrap();
            }
            for sql in extra {
                c.execute_batch(sql).unwrap();
            }
        }
        // Swap SQLite's magic for Questa's, which is the only difference.
        let mut bytes = std::fs::read(path).unwrap();
        bytes[..16].copy_from_slice(QUESTA_MAGIC);
        std::fs::write(path, bytes).unwrap();
    }

    pub(crate) fn tmp(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("rwave-dbg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn opens_a_database_behind_the_questa_header() {
        let d = tmp("open");
        let p = d.join("run.dbg");
        write_dbg(&p, Kind::Top, 6, &["CREATE TABLE t (a); INSERT INTO t VALUES (42);"]);
        let db = Db::open(&p, Kind::Top).unwrap();
        let got: i64 = db.conn().query_row("SELECT a FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(got, 42);
        assert_eq!(db.writer().as_deref(), Some("10.7c"));
    }

    #[test]
    fn the_users_file_is_left_exactly_as_it_was() {
        let d = tmp("readonly");
        let p = d.join("run.dbg");
        write_dbg(&p, Kind::Top, 6, &[]);
        let before = std::fs::read(&p).unwrap();
        let db = Db::open(&p, Kind::Top).unwrap();
        drop(db);
        assert_eq!(std::fs::read(&p).unwrap(), before, "a simulation output was modified");
    }

    #[test]
    fn an_unknown_schema_version_is_refused_not_guessed() {
        let d = tmp("version");
        let p = d.join("run.dbg");
        write_dbg(&p, Kind::Top, 7, &[]);
        let e = Db::open(&p, Kind::Top).unwrap_err();
        assert!(e.contains("schema version 7"), "{e}");
        assert!(e.contains("reads version 6"), "{e}");
        // The Questa version is worth naming: it is what tells the user which
        // tool produced something this reader has not seen.
        assert!(e.contains("10.7c"), "{e}");
    }

    #[test]
    fn a_file_that_is_not_a_questa_database_says_so() {
        let d = tmp("magic");
        let p = d.join("not.dbg");
        std::fs::write(&p, b"SQLite format 3\0and then some padding to be long enough").unwrap();
        let e = Db::open(&p, Kind::Top).unwrap_err();
        assert!(e.contains("not a Questa debug database"), "{e}");

        let short = d.join("short.dbg");
        std::fs::write(&short, b"tiny").unwrap();
        assert!(Db::open(&short, Kind::Top).unwrap_err().contains("too short"));

        assert!(Db::open(&d.join("absent.dbg"), Kind::Top).unwrap_err().contains("cannot read"));
    }

    #[test]
    fn the_two_database_kinds_are_versioned_separately() {
        let d = tmp("kinds");
        let m = d.join("__mti.dbg");
        // A module database is version 1 while a top-level one is 6; checking a
        // module against the top-level table would reject every valid file.
        write_dbg(&m, Kind::Index, 1, &[]);
        assert!(Db::open(&m, Kind::Index).is_ok());
        assert!(Db::open(&m, Kind::Top).is_err());
    }
}
