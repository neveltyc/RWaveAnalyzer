// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Reading the driving statement out of the source file.
//!
//! Questa's debug database stores a `file:line` but no statement text, so
//! without this a driver row reads `gate  dut.sv:4  res_q[7:0]` — a location and
//! a name, but not the thing that was written. The FSDB backend gets statement
//! text from the KDB, and the same line of output should mean the same thing on
//! both.
//!
//! Best-effort by construction: paths are as the compiler saw them, so they
//! resolve only when rwave runs where the design was built. A miss leaves the
//! hop exactly as the parser produced it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Reads numbered lines out of source files, once per file.
#[derive(Default)]
pub struct SourceCache {
    files: HashMap<PathBuf, Option<Vec<String>>>,
}

/// Longest line worth showing. A generated file can hold a single line of many
/// kilobytes, and the statement column is one line of a table.
const MAX_LEN: usize = 200;

impl SourceCache {
    /// The text at `line` of `file`, resolved relative to `base`.
    ///
    /// `None` when the file cannot be read, the line does not exist, or the text
    /// is too long to belong in a column.
    pub fn line(&mut self, base: &Path, file: &str, line: u32) -> Option<String> {
        if line == 0 {
            return None;
        }
        let path = {
            let p = Path::new(file);
            if p.is_absolute() { p.to_path_buf() } else { base.join(p) }
        };
        let content = self
            .files
            .entry(path.clone())
            .or_insert_with(|| {
                std::fs::read_to_string(&path)
                    .ok()
                    .map(|s| s.lines().map(str::to_string).collect())
            })
            .as_ref()?;
        let text = content.get(line as usize - 1)?.trim();
        if text.is_empty() || text.chars().count() > MAX_LEN {
            return None;
        }
        Some(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("rwave-questa-src-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn reads_the_statement_at_the_reported_line() {
        let d = tmpdir("read");
        std::fs::write(
            d.join("dut.sv"),
            "module alu;\n  logic [7:0] res_q;\n  always_ff @(posedge clk) res_q <= a + b;\n  assign res = res_q;\nendmodule\n",
        )
        .unwrap();
        let mut c = SourceCache::default();
        assert_eq!(c.line(&d, "dut.sv", 4).as_deref(), Some("assign res = res_q;"));
        // Cached: a second read of the same file must give the same answer.
        assert_eq!(c.line(&d, "dut.sv", 2).as_deref(), Some("logic [7:0] res_q;"));
    }

    #[test]
    fn a_path_that_does_not_resolve_is_not_an_error() {
        let d = tmpdir("miss");
        let mut c = SourceCache::default();
        assert_eq!(c.line(&d, "nowhere.sv", 3), None);
        assert_eq!(c.line(&d, "nowhere.sv", 3), None, "the miss is cached too");
    }

    #[test]
    fn out_of_range_and_blank_lines_yield_nothing() {
        let d = tmpdir("range");
        std::fs::write(d.join("a.v"), "one\n\nthree\n").unwrap();
        let mut c = SourceCache::default();
        assert_eq!(c.line(&d, "a.v", 0), None);
        assert_eq!(c.line(&d, "a.v", 2), None, "blank line");
        assert_eq!(c.line(&d, "a.v", 9), None);
        assert_eq!(c.line(&d, "a.v", 3).as_deref(), Some("three"));
    }

    #[test]
    fn a_very_long_line_is_left_out_of_the_column() {
        let d = tmpdir("long");
        std::fs::write(d.join("gen.v"), format!("{}\n", "x".repeat(MAX_LEN + 1))).unwrap();
        let mut c = SourceCache::default();
        assert_eq!(c.line(&d, "gen.v", 1), None);
    }
}
