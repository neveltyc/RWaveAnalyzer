// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Command-line interface: argument model and a small hand-rolled parser.
//!
//! The flag surface mirrors `vcd_analyzer.py` exactly (global `--json`,
//! `--limit`, `--verbose`, `--version`; per-command `--begin`, `--end`,
//! `--filter`, `--at`, `--condition`, `--show`, `--changed`). `--json`,
//! `--limit`, and `--verbose` may appear either before or after the
//! subcommand. We avoid a third-party arg parser to keep the static binary
//! small and the error text under our control.

/// Default result limit when neither `--limit` nor `--verbose` is given.
pub const DEFAULT_LIMIT: usize = 200;

/// Which subcommand to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Info,
    List,
    Dump,
    Summary,
    Snapshot,
    Compare,
    Search,
}

impl Command {
    fn from_str(s: &str) -> Option<Command> {
        Some(match s {
            "info" => Command::Info,
            "list" => Command::List,
            "dump" => Command::Dump,
            "summary" => Command::Summary,
            "snapshot" => Command::Snapshot,
            "compare" => Command::Compare,
            "search" => Command::Search,
            _ => return None,
        })
    }
}

/// Fully parsed CLI invocation.
#[derive(Debug, Clone)]
pub struct Args {
    pub command: Command,
    pub file: String,
    pub json: bool,
    /// `None` = not given (limit defaults applied later); `Some(n)` = explicit.
    pub limit: Option<i64>,
    pub verbose: bool,
    pub begin: Option<String>,
    pub end: Option<String>,
    pub filter: Option<String>,
    pub at: Option<String>,
    pub condition: Option<String>,
    pub show: Option<String>,
    pub changed: Option<String>,
}

/// Outcome of parsing argv.
pub enum ParseOutcome {
    /// Run with these arguments.
    Run(Args),
    /// Print this text to stdout and exit 0 (e.g. `--version`, `--help`).
    Print(String),
    /// Print this error to stderr and exit 2.
    Error(String),
}

/// Top-level help text (shown for `--help` / no command).
pub fn help_text() -> String {
    format!(
        "rwave {ver} — AI-agent-friendly VCD/FST waveform analyzer\n\
\n\
Usage: rwave [--json] [--limit N] [--verbose] <command> <file> [options]\n\
\n\
Commands:\n\
  info      <file>                              File overview (timescale, signals, time span, scopes)\n\
  list      <file> [--filter K1,K2]             List signals with path and bit width\n\
  dump      <file> [--begin T] [--end T] [--filter K1,K2]\n\
                                                Print value-change events in time order\n\
  summary   <file> [--begin T] [--end T] [--filter K1,K2]\n\
                                                Per-signal stats: change count, edges, static detection\n\
  snapshot  <file> --at T [--filter K1,K2]      Known signal values at a given time point\n\
  compare   <file> --at T1,T2 [--filter K1,K2]  Diff signal values between two time points\n\
  search    <file> --condition C [--show K1,K2] [--changed K] [--begin T] [--end T]\n\
                                                Conditional search and associated signal observation\n\
\n\
Global options:\n\
  --json        Output compact structured JSON instead of text\n\
  --limit N     Max rows/records to emit; default {lim}; 0 = unlimited\n\
  --verbose     Show extra fields; if --limit is omitted, disables truncation\n\
  --version     Print version and exit\n\
  -h, --help    Print this help and exit\n\
\n\
Supports both VCD and FST inputs; the format is auto-detected.\n\
Time values accept fs/ps/ns/us/ms/s suffixes (e.g. 17.5us); a bare integer is raw ticks.\n",
        ver = crate::VERSION,
        lim = DEFAULT_LIMIT,
    )
}

/// Flags that consume the following argv token as their value. Used by the
/// `--version` / `--help` pre-scan to avoid mistaking a flag *value* for a
/// help/version request (e.g. `--filter --version` should be "missing value
/// for --filter", not "print version").
const VALUE_FLAGS: &[&str] = &[
    "--limit", "--begin", "--end", "--filter", "--at",
    "--condition", "--show", "--changed",
];

/// Parse a slice of argv tokens (excluding argv[0]).
pub fn parse(argv: &[String]) -> ParseOutcome {
    // Pre-scan for --version / --help anywhere, skipping tokens that are the
    // values of preceding value-taking flags.
    let mut skip_next = false;
    for a in argv {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "--version" {
            return ParseOutcome::Print(format!("rwave {}", crate::VERSION));
        }
        if a == "-h" || a == "--help" {
            return ParseOutcome::Print(help_text());
        }
        if VALUE_FLAGS.iter().any(|f| f == a) {
            skip_next = true;
        }
    }
    if argv.is_empty() {
        return ParseOutcome::Print(help_text());
    }
    match parse_inner(argv) {
        Ok(outcome) => outcome,
        Err(e) => e,
    }
}

/// Inner parse returning a `Result` so the `?` operator can short-circuit on
/// errors (mapped to `ParseOutcome::Error`). On success it yields either a
/// `Run` or a `Print` outcome.
fn parse_inner(argv: &[String]) -> Result<ParseOutcome, ParseOutcome> {
    let mut json = false;
    let mut limit: Option<i64> = None;
    let mut verbose = false;
    let mut command: Option<Command> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut begin = None;
    let mut end = None;
    let mut filter = None;
    let mut at = None;
    let mut condition = None;
    let mut show = None;
    let mut changed = None;

    let mut i = 0;
    while i < argv.len() {
        let tok = &argv[i];
        match tok.as_str() {
            "--json" => json = true,
            "--verbose" => verbose = true,
            "--limit" => {
                i += 1;
                let v = match argv.get(i) {
                    Some(v) => v,
                    None => return Err(ParseOutcome::Error("--limit requires a value".into())),
                };
                match v.parse::<i64>() {
                    Ok(n) => limit = Some(n),
                    Err(_) => {
                        return Err(ParseOutcome::Error(format!(
                            "argument --limit: invalid int value: '{v}'"
                        )));
                    }
                }
            }
            "--begin" => {
                i += 1;
                begin = Some(require_value(argv, i, "--begin")?);
            }
            "--end" => {
                i += 1;
                end = Some(require_value(argv, i, "--end")?);
            }
            "--filter" => {
                i += 1;
                filter = Some(require_value(argv, i, "--filter")?);
            }
            "--at" => {
                i += 1;
                at = Some(require_value(argv, i, "--at")?);
            }
            "--condition" => {
                i += 1;
                condition = Some(require_value(argv, i, "--condition")?);
            }
            "--show" => {
                i += 1;
                show = Some(require_value(argv, i, "--show")?);
            }
            "--changed" => {
                i += 1;
                changed = Some(require_value(argv, i, "--changed")?);
            }
            s if s.starts_with("--") => {
                return Err(ParseOutcome::Error(format!("unrecognized argument: {s}")));
            }
            s if s.starts_with('-') && s.len() > 1 && command.is_some() => {
                return Err(ParseOutcome::Error(format!("unrecognized argument: {s}")));
            }
            other => {
                if command.is_none() {
                    match Command::from_str(other) {
                        Some(c) => command = Some(c),
                        None => {
                            return Err(ParseOutcome::Error(format!(
                                "invalid command: '{other}' (choose from info, list, dump, \
                                 summary, snapshot, compare, search)"
                            )));
                        }
                    }
                } else {
                    positionals.push(other.to_string());
                }
            }
        }
        i += 1;
    }

    let command = match command {
        Some(c) => c,
        None => return Ok(ParseOutcome::Print(help_text())),
    };

    if positionals.is_empty() {
        return Err(ParseOutcome::Error(format!(
            "the following arguments are required: <file> (for '{}')",
            cmd_name(&command)
        )));
    }
    if positionals.len() > 1 {
        return Err(ParseOutcome::Error(format!(
            "unexpected extra arguments: {}",
            positionals[1..].join(" ")
        )));
    }
    let file = positionals.into_iter().next().unwrap();

    match command {
        Command::Snapshot if at.is_none() => {
            return Err(ParseOutcome::Error(
                "the following arguments are required: --at".into(),
            ));
        }
        Command::Compare if at.is_none() => {
            return Err(ParseOutcome::Error(
                "the following arguments are required: --at".into(),
            ));
        }
        Command::Search if condition.is_none() => {
            return Err(ParseOutcome::Error(
                "the following arguments are required: --condition".into(),
            ));
        }
        _ => {}
    }

    if let Some(n) = limit {
        if n < 0 {
            return Err(ParseOutcome::Error(format!(
                "limit must be non-negative; got {n}"
            )));
        }
    }

    Ok(ParseOutcome::Run(Args {
        command,
        file,
        json,
        limit,
        verbose,
        begin,
        end,
        filter,
        at,
        condition,
        show,
        changed,
    }))
}

fn cmd_name(c: &Command) -> &'static str {
    match c {
        Command::Info => "info",
        Command::List => "list",
        Command::Dump => "dump",
        Command::Summary => "summary",
        Command::Snapshot => "snapshot",
        Command::Compare => "compare",
        Command::Search => "search",
    }
}

/// Helper: fetch the value at argv[i], erroring if missing.
fn require_value(argv: &[String], i: usize, flag: &str) -> Result<String, ParseOutcome> {
    match argv.get(i) {
        Some(v) => Ok(v.clone()),
        None => Err(ParseOutcome::Error(format!("{flag} requires a value"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> ParseOutcome {
        let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse(&v)
    }

    #[test]
    fn version() {
        match p(&["--version"]) {
            ParseOutcome::Print(s) => assert!(s.contains(crate::VERSION)),
            _ => panic!(),
        }
    }

    #[test]
    fn info_basic() {
        match p(&["info", "x.vcd"]) {
            ParseOutcome::Run(a) => {
                assert_eq!(a.command, Command::Info);
                assert_eq!(a.file, "x.vcd");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn json_before_or_after() {
        match p(&["--json", "info", "x.vcd"]) {
            ParseOutcome::Run(a) => assert!(a.json),
            _ => panic!(),
        }
        match p(&["info", "x.vcd", "--json"]) {
            ParseOutcome::Run(a) => assert!(a.json),
            _ => panic!(),
        }
    }

    #[test]
    fn search_requires_condition() {
        match p(&["search", "x.vcd"]) {
            ParseOutcome::Error(e) => assert!(e.contains("--condition")),
            _ => panic!(),
        }
    }

    #[test]
    fn snapshot_requires_at() {
        match p(&["snapshot", "x.vcd"]) {
            ParseOutcome::Error(e) => assert!(e.contains("--at")),
            _ => panic!(),
        }
    }

    #[test]
    fn version_and_help_not_hijacked_by_value_flags() {
        // `--filter --version` should be "missing value for --filter", not the
        // version string. The pre-scan must skip tokens that are values of a
        // value-taking flag.
        match p(&["info", "x.vcd", "--filter", "--version"]) {
            ParseOutcome::Run(_) | ParseOutcome::Error(_) => {}
            ParseOutcome::Print(s) => panic!("unexpectedly printed: {s}"),
        }
        match p(&["dump", "x.vcd", "--begin", "--help"]) {
            ParseOutcome::Run(_) | ParseOutcome::Error(_) => {}
            ParseOutcome::Print(s) => panic!("unexpectedly printed: {s}"),
        }
        // A genuine --version anywhere still works.
        match p(&["--filter", "clk", "--version", "info", "x.vcd"]) {
            ParseOutcome::Print(s) => assert!(s.contains(crate::VERSION)),
            _ => panic!("expected version print"),
        }
    }
}
