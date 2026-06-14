// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Command-line interface: argument model and a small hand-rolled parser.
//!
//! The global flags are `--json`, `--limit`, `--verbose`, `--version`; the
//! per-command flags are `--begin`, `--end`, `--filter`, `--at`, `--condition`,
//! `--show`, `--changed`. `--json`, `--limit`, and `--verbose` may appear
//! either before or after the subcommand. We avoid a third-party arg parser to
//! keep the static binary small and the error text under our control.

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

/// Global default options for a batch run, applied to every command unless a
/// per-command line overrides the same option. Mirrors the optional fields of
/// [`Args`] (minus `command`/`file`/`json`): `--json` does not participate in
/// the merge because batch output framing is fixed by the top-level invocation.
#[derive(Debug, Clone, Default)]
pub struct Defaults {
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

/// A fully parsed `--batch` invocation: the file to load once, the output
/// framing (`json` = NDJSON, else text), and the per-command default options.
#[derive(Debug, Clone)]
pub struct BatchInvocation {
    pub file: String,
    pub json: bool,
    pub defaults: Defaults,
}

/// Outcome of parsing argv.
pub enum ParseOutcome {
    /// Run with these arguments.
    Run(Args),
    /// Run in batch mode: load the file once, read commands from stdin.
    Batch(BatchInvocation),
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
       rwave --batch [--json] <file> [global-opts] < commands.txt\n\
\n\
Commands:\n\
  info      <file>                              File overview (timescale, signals, time span, scopes)\n\
  list      <file> [--filter K1,K2]             List signals (filter matches any alias path)\n\
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
  --batch       Load <file> once, then read commands (one per line) from stdin\n\
  --version     Print version and exit\n\
  -h, --help    Print this help and exit\n\
\n\
Batch mode (--batch): each stdin line is a command minus the leading 'rwave';\n\
results are emitted in input order. With --json each result is one NDJSON line\n\
{{\"id\",\"ok\",\"result\"|\"error\"}}; without it, each result is preceded by a\n\
'#label' header line. A trailing '#label' on an input line sets that result's\n\
id (otherwise a 1-based sequence number is used); blank lines and lines starting\n\
with '#' are skipped. [global-opts] become per-command defaults.\n\
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
    // values of preceding value-taking flags. The same pass notes whether
    // --batch is present, so the main parse knows up front that the lone
    // positional is the file (not a subcommand) regardless of token order.
    let mut skip_next = false;
    let mut batch_mode = false;
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
        if a == "--batch" {
            batch_mode = true;
        }
        if VALUE_FLAGS.iter().any(|f| f == a) {
            skip_next = true;
        }
    }
    if argv.is_empty() {
        return ParseOutcome::Print(help_text());
    }
    match parse_inner(argv, batch_mode) {
        Ok(outcome) => outcome,
        Err(msg) => ParseOutcome::Error(msg),
    }
}

/// Accumulated flags, command, and positionals from one token stream. Shared by
/// the single-command path, the `--batch` top-level parse, and per-line batch
/// parsing — so every path interprets flags through the exact same code.
#[derive(Default)]
struct Acc {
    json: bool,
    batch: bool,
    limit: Option<i64>,
    verbose: bool,
    begin: Option<String>,
    end: Option<String>,
    filter: Option<String>,
    at: Option<String>,
    condition: Option<String>,
    show: Option<String>,
    changed: Option<String>,
    command: Option<Command>,
    positionals: Vec<String>,
}

/// Run the token loop over `argv`, filling `acc`. When `batch_mode` is set there
/// is no CLI subcommand, so every non-flag token is a positional (the file);
/// otherwise the first non-flag token is interpreted as the subcommand. Returns
/// a usage-error message on the first malformed token.
fn accumulate(argv: &[String], acc: &mut Acc, batch_mode: bool) -> Result<(), String> {
    let mut i = 0;
    while i < argv.len() {
        let tok = &argv[i];
        match tok.as_str() {
            "--json" => acc.json = true,
            "--batch" => acc.batch = true,
            "--verbose" => acc.verbose = true,
            "--limit" => {
                i += 1;
                let v = argv
                    .get(i)
                    .ok_or_else(|| "--limit requires a value".to_string())?;
                match v.parse::<i64>() {
                    Ok(n) => acc.limit = Some(n),
                    Err(_) => {
                        return Err(format!("argument --limit: invalid int value: '{v}'"));
                    }
                }
            }
            "--begin" => {
                i += 1;
                acc.begin = Some(require_value(argv, i, "--begin")?);
            }
            "--end" => {
                i += 1;
                acc.end = Some(require_value(argv, i, "--end")?);
            }
            "--filter" => {
                i += 1;
                acc.filter = Some(require_value(argv, i, "--filter")?);
            }
            "--at" => {
                i += 1;
                acc.at = Some(require_value(argv, i, "--at")?);
            }
            "--condition" => {
                i += 1;
                acc.condition = Some(require_value(argv, i, "--condition")?);
            }
            "--show" => {
                i += 1;
                acc.show = Some(require_value(argv, i, "--show")?);
            }
            "--changed" => {
                i += 1;
                acc.changed = Some(require_value(argv, i, "--changed")?);
            }
            s if s.starts_with("--") => {
                return Err(format!("unrecognized argument: {s}"));
            }
            s if s.starts_with('-') && s.len() > 1 && acc.command.is_some() => {
                return Err(format!("unrecognized argument: {s}"));
            }
            other => {
                if batch_mode {
                    // No subcommand on the CLI in batch mode; the only positional
                    // is the file. Commands come from stdin.
                    acc.positionals.push(other.to_string());
                } else if acc.command.is_none() {
                    match Command::from_str(other) {
                        Some(c) => acc.command = Some(c),
                        None => {
                            return Err(format!(
                                "invalid command: '{other}' (choose from info, list, dump, \
                                 summary, snapshot, compare, search)"
                            ));
                        }
                    }
                } else {
                    acc.positionals.push(other.to_string());
                }
            }
        }
        i += 1;
    }
    Ok(())
}

/// Required-argument check, shared by the single-command and batch-line paths so
/// a missing `--at`/`--condition` fails identically in both.
fn check_required(
    command: &Command,
    at: &Option<String>,
    condition: &Option<String>,
) -> Result<(), String> {
    match command {
        Command::Snapshot if at.is_none() => {
            Err("the following arguments are required: --at".into())
        }
        Command::Compare if at.is_none() => {
            Err("the following arguments are required: --at".into())
        }
        Command::Search if condition.is_none() => {
            Err("the following arguments are required: --condition".into())
        }
        _ => Ok(()),
    }
}

fn check_limit(limit: Option<i64>) -> Result<(), String> {
    if let Some(n) = limit {
        if n < 0 {
            return Err(format!("limit must be non-negative; got {n}"));
        }
    }
    Ok(())
}

/// Inner parse returning a `Result<_, String>` so the `?` operator can
/// short-circuit on errors (mapped to `ParseOutcome::Error` by the caller).
/// On success it yields a `Run`, `Batch`, or `Print` outcome.
fn parse_inner(argv: &[String], batch_mode: bool) -> Result<ParseOutcome, String> {
    let mut acc = Acc::default();
    accumulate(argv, &mut acc, batch_mode)?;
    if acc.batch {
        resolve_batch(acc)
    } else {
        resolve_single(acc)
    }
}

/// Resolve an ordinary single-command invocation.
fn resolve_single(acc: Acc) -> Result<ParseOutcome, String> {
    let command = match acc.command {
        Some(c) => c,
        None => return Ok(ParseOutcome::Print(help_text())),
    };
    if acc.positionals.is_empty() {
        return Err(format!(
            "the following arguments are required: <file> (for '{}')",
            cmd_name(&command)
        ));
    }
    if acc.positionals.len() > 1 {
        return Err(format!(
            "unexpected extra arguments: {}",
            acc.positionals[1..].join(" ")
        ));
    }
    let file = acc.positionals.into_iter().next().unwrap();
    check_required(&command, &acc.at, &acc.condition)?;
    check_limit(acc.limit)?;
    Ok(ParseOutcome::Run(Args {
        command,
        file,
        json: acc.json,
        limit: acc.limit,
        verbose: acc.verbose,
        begin: acc.begin,
        end: acc.end,
        filter: acc.filter,
        at: acc.at,
        condition: acc.condition,
        show: acc.show,
        changed: acc.changed,
    }))
}

/// Resolve a `--batch` invocation: a file positional, no subcommand, and the
/// remaining flags captured as per-command defaults.
fn resolve_batch(acc: Acc) -> Result<ParseOutcome, String> {
    // In batch mode the CLI carries no subcommand (it's read from stdin); a bare
    // command name among the positionals means the user combined `--batch` with
    // a subcommand, e.g. `rwave --batch info file`.
    if acc.positionals.iter().any(|p| Command::from_str(p).is_some()) {
        return Err(
            "--batch cannot be combined with a subcommand; commands are read from stdin".into(),
        );
    }
    if acc.positionals.is_empty() {
        return Err("the following arguments are required: <file>".into());
    }
    if acc.positionals.len() > 1 {
        return Err(format!(
            "unexpected extra arguments: {}",
            acc.positionals[1..].join(" ")
        ));
    }
    let file = acc.positionals.into_iter().next().unwrap();
    check_limit(acc.limit)?;
    Ok(ParseOutcome::Batch(BatchInvocation {
        file,
        json: acc.json,
        defaults: Defaults {
            limit: acc.limit,
            verbose: acc.verbose,
            begin: acc.begin,
            end: acc.end,
            filter: acc.filter,
            at: acc.at,
            condition: acc.condition,
            show: acc.show,
            changed: acc.changed,
        },
    }))
}

/// Parse one batch input line's tokens into a full [`Args`], injecting the
/// already-loaded `file` and filling unset options from `defaults` (a per-line
/// option overrides the same default; `--verbose` is additive). Required-argument
/// and limit validation match the single-command path, so a line's success or
/// failure mirrors the equivalent `rwave <cmd> <file> …` invocation exactly.
pub fn parse_batch_line(tokens: &[String], file: &str, defaults: &Defaults) -> Result<Args, String> {
    let mut acc = Acc::default();
    accumulate(tokens, &mut acc, false)?;
    let command = match acc.command {
        Some(c) => c,
        None => {
            return Err("missing command (each line must start with a subcommand: info, list, \
                 dump, summary, snapshot, compare, search)"
                .into());
        }
    };
    if !acc.positionals.is_empty() {
        return Err(format!(
            "unexpected argument: {} (the waveform file is given once on the --batch line, \
             not per command)",
            acc.positionals.join(" ")
        ));
    }
    let limit = acc.limit.or(defaults.limit);
    let verbose = acc.verbose || defaults.verbose;
    let begin = acc.begin.or_else(|| defaults.begin.clone());
    let end = acc.end.or_else(|| defaults.end.clone());
    let filter = acc.filter.or_else(|| defaults.filter.clone());
    let at = acc.at.or_else(|| defaults.at.clone());
    let condition = acc.condition.or_else(|| defaults.condition.clone());
    let show = acc.show.or_else(|| defaults.show.clone());
    let changed = acc.changed.or_else(|| defaults.changed.clone());
    check_required(&command, &at, &condition)?;
    check_limit(limit)?;
    Ok(Args {
        command,
        file: file.to_string(),
        json: false,
        limit,
        verbose,
        begin,
        end,
        filter,
        at,
        condition,
        show,
        changed,
    })
}

/// Split one batch input line into `(tokens, label)`. Tokenization is
/// quote-aware (shell-like): whitespace separates tokens; `'…'` and `"…"`
/// group (single quotes are fully literal, double quotes honor `\"`/`\\`); a
/// backslash outside quotes escapes the next character; an unquoted `#` begins
/// the trailing label (the rest of the line, trimmed; empty → no label). An
/// empty `tokens` means the line was blank or a comment / label-only line and
/// should be skipped. Returns `Err` on an unterminated quote or a dangling
/// backslash.
pub fn split_line(line: &str) -> Result<(Vec<String>, Option<String>), String> {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut label: Option<String> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                if in_token {
                    tokens.push(std::mem::take(&mut cur));
                    in_token = false;
                }
                i += 1;
            }
            '#' => {
                // Unquoted '#': the rest of the line is the trailing label. We
                // return immediately, so there's no need to reset `in_token`.
                if in_token {
                    tokens.push(std::mem::take(&mut cur));
                }
                let rest: String = chars[i + 1..].iter().collect();
                let trimmed = rest.trim();
                if !trimmed.is_empty() {
                    label = Some(trimmed.to_string());
                }
                return Ok((tokens, label));
            }
            '\'' => {
                in_token = true;
                i += 1;
                let mut closed = false;
                while i < chars.len() {
                    if chars[i] == '\'' {
                        closed = true;
                        i += 1;
                        break;
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
                if !closed {
                    return Err("unterminated single quote".into());
                }
            }
            '"' => {
                in_token = true;
                i += 1;
                let mut closed = false;
                while i < chars.len() {
                    let d = chars[i];
                    if d == '"' {
                        closed = true;
                        i += 1;
                        break;
                    }
                    if d == '\\' && i + 1 < chars.len() && matches!(chars[i + 1], '"' | '\\') {
                        cur.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    cur.push(d);
                    i += 1;
                }
                if !closed {
                    return Err("unterminated double quote".into());
                }
            }
            '\\' => {
                if i + 1 < chars.len() {
                    in_token = true;
                    cur.push(chars[i + 1]);
                    i += 2;
                } else {
                    return Err("line ends with an unescaped backslash".into());
                }
            }
            _ => {
                in_token = true;
                cur.push(c);
                i += 1;
            }
        }
    }
    if in_token {
        tokens.push(cur);
    }
    Ok((tokens, label))
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
fn require_value(argv: &[String], i: usize, flag: &str) -> Result<String, String> {
    match argv.get(i) {
        Some(v) => Ok(v.clone()),
        None => Err(format!("{flag} requires a value")),
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
            other => panic!("unexpected outcome: {}", outcome_kind(&other)),
        }
        match p(&["dump", "x.vcd", "--begin", "--help"]) {
            ParseOutcome::Run(_) | ParseOutcome::Error(_) => {}
            other => panic!("unexpected outcome: {}", outcome_kind(&other)),
        }
        // A genuine --version anywhere still works.
        match p(&["--filter", "clk", "--version", "info", "x.vcd"]) {
            ParseOutcome::Print(s) => assert!(s.contains(crate::VERSION)),
            _ => panic!("expected version print"),
        }
    }

    /// A short label for a `ParseOutcome` variant, for assertion messages.
    fn outcome_kind(o: &ParseOutcome) -> &'static str {
        match o {
            ParseOutcome::Run(_) => "Run",
            ParseOutcome::Batch(_) => "Batch",
            ParseOutcome::Print(_) => "Print",
            ParseOutcome::Error(_) => "Error",
        }
    }

    #[test]
    fn batch_basic_file_is_positional() {
        // `--batch <file>` parses to a Batch with the file, no subcommand.
        match p(&["--batch", "x.vcd"]) {
            ParseOutcome::Batch(b) => {
                assert_eq!(b.file, "x.vcd");
                assert!(!b.json);
            }
            other => panic!("expected Batch, got {}", outcome_kind(&other)),
        }
        // Order-independent: --batch after the file still works.
        match p(&["x.vcd", "--batch", "--json"]) {
            ParseOutcome::Batch(b) => {
                assert_eq!(b.file, "x.vcd");
                assert!(b.json);
            }
            other => panic!("expected Batch, got {}", outcome_kind(&other)),
        }
    }

    #[test]
    fn batch_globals_become_defaults() {
        match p(&["--batch", "x.vcd", "--limit", "0", "--verbose", "--filter", "clk"]) {
            ParseOutcome::Batch(b) => {
                assert_eq!(b.defaults.limit, Some(0));
                assert!(b.defaults.verbose);
                assert_eq!(b.defaults.filter.as_deref(), Some("clk"));
            }
            other => panic!("expected Batch, got {}", outcome_kind(&other)),
        }
    }

    #[test]
    fn batch_conflicts_with_subcommand() {
        match p(&["--batch", "info", "x.vcd"]) {
            ParseOutcome::Error(e) => assert!(e.contains("cannot be combined")),
            other => panic!("expected Error, got {}", outcome_kind(&other)),
        }
    }

    #[test]
    fn batch_requires_file() {
        match p(&["--batch"]) {
            ParseOutcome::Error(e) => assert!(e.contains("<file>")),
            other => panic!("expected Error, got {}", outcome_kind(&other)),
        }
    }

    #[test]
    fn batch_line_merges_and_overrides() {
        let defaults = Defaults {
            limit: Some(0),
            filter: Some("clk".into()),
            ..Defaults::default()
        };
        // Line with no limit/filter inherits the defaults.
        let a = parse_batch_line(&["dump".into()], "f.vcd", &defaults).unwrap();
        assert_eq!(a.command, Command::Dump);
        assert_eq!(a.file, "f.vcd");
        assert_eq!(a.limit, Some(0));
        assert_eq!(a.filter.as_deref(), Some("clk"));
        // Line's own flags override the defaults.
        let toks: Vec<String> = ["dump", "--limit", "5", "--filter", "rst"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let b = parse_batch_line(&toks, "f.vcd", &defaults).unwrap();
        assert_eq!(b.limit, Some(5));
        assert_eq!(b.filter.as_deref(), Some("rst"));
    }

    #[test]
    fn batch_line_missing_command_and_required_args() {
        let d = Defaults::default();
        assert!(parse_batch_line(&["--filter".into(), "clk".into()], "f.vcd", &d).is_err());
        // snapshot still requires --at when no default supplies it.
        let e = parse_batch_line(&["snapshot".into()], "f.vcd", &d).unwrap_err();
        assert!(e.contains("--at"));
        // a default --at satisfies it.
        let d2 = Defaults {
            at: Some("5ns".into()),
            ..Defaults::default()
        };
        assert!(parse_batch_line(&["snapshot".into()], "f.vcd", &d2).is_ok());
    }

    #[test]
    fn split_line_tokenizes_and_labels() {
        // Plain command, no label.
        let (t, l) = split_line("dump --filter state").unwrap();
        assert_eq!(t, vec!["dump", "--filter", "state"]);
        assert_eq!(l, None);
        // Trailing label.
        let (t, l) = split_line("list --filter clk,rst   #my label").unwrap();
        assert_eq!(t, vec!["list", "--filter", "clk,rst"]);
        assert_eq!(l.as_deref(), Some("my label"));
        // Blank and comment-only lines yield no tokens.
        assert!(split_line("   ").unwrap().0.is_empty());
        assert!(split_line("   # just a comment").unwrap().0.is_empty());
        // Quotes group and are stripped; quoted '#' is literal.
        let (t, _) = split_line(r#"search --condition "a=1,b=2" --show "x#y""#).unwrap();
        assert_eq!(t, vec!["search", "--condition", "a=1,b=2", "--show", "x#y"]);
        // Unterminated quote is an error.
        assert!(split_line(r#"dump --filter "oops"#).is_err());
    }
}
