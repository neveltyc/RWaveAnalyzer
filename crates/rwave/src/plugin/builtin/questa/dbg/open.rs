// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Opening a Questa `.dbg`.
//!
//! The file is SQLite with its 16-byte header replaced: `Modelsim dbg 1 \0`
//! where a normal database has `SQLite format 3\0`. Everything after those
//! bytes is stock SQLite, so the only thing standing between rwave and the
//! data is the magic.
//!
//! The user's file is never written to. The bytes are read into memory with the
//! header corrected on the way in, and handed to SQLite through
//! `deserialize_read_exact` as a read-only database — no temp copy on disk, no
//! chance of touching a simulation output.
//!
//! Every database carries its own schema version, and this reader was written
//! against exactly one. An unrecognised version is refused rather than parsed
//! on the assumption the layout held: reading a moved column would produce a
//! confident wrong answer, which is worse than no answer.

use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::plugin::builtin::questa::err;

const QUESTA_MAGIC: &[u8; 16] = b"Modelsim dbg 1 \0";
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Which of the two database shapes a file is. They differ in more than
/// content: the version lives in a differently named table, under a different
/// column name, and is numbered independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The one beside the waveform: hierarchy, instances, declarations.
    Top,
    /// One per design unit, under `<lib>/_dbcontainer/<opt>/`: processes,
    /// shapes, and the signal-to-shape index.
    Module,
}

impl Kind {
    /// `(table, key column, value column, the version this reader knows)`.
    fn version_probe(self) -> (&'static str, &'static str, &'static str, i64) {
        match self {
            Kind::Top => ("dbg_config_tbl", "property", "value", 6),
            Kind::Module => ("dbg_options_tbl", "optionName", "value", 1),
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

    fn check_version(&self, kind: Kind) -> Result<(), String> {
        let (table, key, val, known) = kind.version_probe();
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
            let (table, key, val, _) = kind.version_probe();
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
        write_dbg(&m, Kind::Module, 1, &[]);
        assert!(Db::open(&m, Kind::Module).is_ok());
        assert!(Db::open(&m, Kind::Top).is_err());
    }
}
