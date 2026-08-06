// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Command-line interface: argument model and a small hand-rolled parser.
//!
//! The global flags are `--json`, `--limit`, `--verbose`, `--version`; the
//! per-command flags are `--begin`, `--end`, `--scope`, `--depth`, `--filter`,
//! `--exclude`, `--at`, `--condition`, `--show`. `--json`, `--limit`, and
//! `--verbose` may appear either before or after the subcommand. We avoid a
//! third-party arg parser to keep the static binary small and the error text
//! under our control.

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
    /// Hierarchy subtrees to restrict the selection to.
    pub scope: Option<String>,
    /// Maximum levels below the matched `--scope` root; requires `--scope`.
    pub depth: Option<i64>,
    pub filter: Option<String>,
    /// Subtractive counterpart of `--filter`: same pattern language, applied
    /// last. Usable on its own to carve noise out of a whole-file query.
    pub exclude: Option<String>,
    pub at: Option<String>,
    /// Search conditions. Each element is one `--condition` clause (a
    /// comma-separated AND list); repeating `--condition` ORs the clauses
    /// (OR-of-ANDs). Empty = no `--condition` given.
    pub condition: Vec<String>,
    pub show: Option<String>,
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
    pub scope: Option<String>,
    pub depth: Option<i64>,
    pub filter: Option<String>,
    pub exclude: Option<String>,
    pub at: Option<String>,
    /// Default `--condition` clause group; a batch line with any `--condition`
    /// of its own replaces this whole group (see [`parse_batch_line`]).
    pub condition: Vec<String>,
    pub show: Option<String>,
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
///
/// Written as a raw string: the source layout *is* the output layout, so the
/// column alignment below cannot drift from what a user sees.
pub fn help_text() -> String {
    format!(
        r#"rwave {ver} — AI-agent-friendly VCD/FST waveform analyzer

Usage: rwave [--json] [--limit N] [--verbose] <command> <file> [options]
       rwave --batch [--json] <file> [global-opts] < commands.txt

Commands:
  info      <file>                              File overview (timescale, signals, time span, scopes)
  list      <file> [selection]                  List signals (one row per alias path)
  dump      <file> [--begin T] [--end T] [selection]
                                                Print value-change events in time order
  summary   <file> [--begin T] [--end T] [selection]
                                                Per-signal stats: change count, edges, static detection
  snapshot  <file> --at T [selection]           Known signal values at a given time point
  compare   <file> --at T1,T2 [selection]       Diff signal values between two time points
  search    <file> --condition C [--condition C2 ...] [--show K1,K2] [--begin T] [--end T]
                                                Conditional search; comma = AND within a --condition, repeat --condition to OR the clauses;
                                                a changed(SIG) term fires at SIG's transitions (event mode)

Selection options (every command above except info):
  --scope P1,P2     Restrict to hierarchy subtrees. A name without a '.' matches an
                    instance name ('*' and '?' allowed); a dotted path matches as a
                    segment-aligned suffix of a scope path, so 'u_tx.u_fifo' finds
                    that subtree wherever it sits.
  --depth N         Keep signals at most N levels below the matched --scope root; a
                    signal sitting directly in the scope is depth 1. Requires --scope.
  --filter K1,K2    Keep signals matching any pattern; omit to keep all.
  --exclude K1,K2   Drop signals matching any pattern; applied last, and usable on
                    its own.

Patterns are comma-separated and case-insensitive. One without a '.' matches the
signal's leaf name, so 'tx_err' finds the signal and not the synchronizer instance
named after it; one containing a '.' matches the whole hierarchical path. Either
way, no '*' or '?' means substring, and '*'/'?' make it an anchored glob ('[' and
']' stay literal, for bus ranges). An empty value ('') means "not given".

Selection is decided per alias path: a signal is kept when any one of its paths
clears every option, and list prints only the paths that did. search resolves its
--condition and --show names within the same selection, which is often what makes
an ambiguous name unique; a name spelled as a full path bypasses selection.

Global options:
  --json        Output compact structured JSON instead of text
  --limit N     Max rows/records to emit; default {lim}; 0 = unlimited
  --verbose     Show extra fields; if --limit is omitted, disables truncation
  --batch       Load <file> once, then read commands (one per line) from stdin
  --version     Print version and exit
  -h, --help    Print this help and exit

Batch mode (--batch): each stdin line is a command minus the leading 'rwave';
results are emitted in input order. With --json each result is one NDJSON line
{{"id","ok","result"|"error"}}; without it, each result is preceded by a
'#label' header line. A trailing '#label' on an input line sets that result's
id (otherwise a 1-based sequence number is used); blank lines and lines starting
with '#' are skipped. [global-opts] become per-command defaults; each selection
option overrides its own default, and a line lifts one it does not want with an
empty value (--filter '').

Supports both VCD and FST inputs; the format is auto-detected.
Time values accept fs/ps/ns/us/ms/s suffixes (e.g. 17.5us); a bare integer is raw ticks.
"#,
        ver = crate::VERSION,
        lim = DEFAULT_LIMIT,
    )
}

/// Flags that consume the following argv token as their value. Used by the
/// `--version` / `--help` pre-scan to avoid mistaking a flag *value* for a
/// help/version request (e.g. `--filter --version` should be "missing value
/// for --filter", not "print version").
const VALUE_FLAGS: &[&str] = &[
    "--limit", "--begin", "--end", "--scope", "--depth", "--filter", "--exclude",
    "--at", "--condition", "--show",
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
    scope: Option<String>,
    depth: Option<i64>,
    filter: Option<String>,
    exclude: Option<String>,
    at: Option<String>,
    condition: Vec<String>,
    show: Option<String>,
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
            "--scope" => {
                i += 1;
                acc.scope = Some(require_value(argv, i, "--scope")?);
            }
            "--depth" => {
                i += 1;
                let v = argv
                    .get(i)
                    .ok_or_else(|| "--depth requires a value".to_string())?;
                match v.parse::<i64>() {
                    Ok(n) => acc.depth = Some(n),
                    Err(_) => {
                        return Err(format!("argument --depth: invalid int value: '{v}'"));
                    }
                }
            }
            "--filter" => {
                i += 1;
                acc.filter = Some(require_value(argv, i, "--filter")?);
            }
            "--exclude" => {
                i += 1;
                acc.exclude = Some(require_value(argv, i, "--exclude")?);
            }
            "--at" => {
                i += 1;
                acc.at = Some(require_value(argv, i, "--at")?);
            }
            "--condition" => {
                i += 1;
                // Repeatable: each occurrence is one OR clause (see `Args::condition`).
                acc.condition.push(require_value(argv, i, "--condition")?);
            }
            "--show" => {
                i += 1;
                acc.show = Some(require_value(argv, i, "--show")?);
            }
            "--changed" => {
                return Err(
                    "--changed is not available; did you mean --condition \"changed(SIG)\"?"
                        .into(),
                );
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
    condition: &[String],
) -> Result<(), String> {
    match command {
        Command::Snapshot if at.is_none() => {
            Err("the following arguments are required: --at".into())
        }
        Command::Compare if at.is_none() => {
            Err("the following arguments are required: --at".into())
        }
        Command::Search if condition.is_empty() => {
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

/// Validate `--depth`, which is measured *from* the `--scope` root and so has
/// no meaning without one. Called after the batch merge, so a line inheriting a
/// default `--depth` without ever naming a scope fails on that line — and a
/// line clearing an inherited scope with `--scope ''` fails the same way, since
/// a blank value means "not given" everywhere in the selection flags.
fn check_depth(depth: Option<i64>, scope: &Option<String>) -> Result<(), String> {
    let n = match depth {
        Some(n) => n,
        None => return Ok(()),
    };
    if n <= 0 {
        return Err(format!("depth must be positive; got {n}"));
    }
    if !scope.as_deref().is_some_and(|s| !s.trim().is_empty()) {
        return Err("--depth requires --scope (depth is counted from the scope root)".into());
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
    check_depth(acc.depth, &acc.scope)?;
    Ok(ParseOutcome::Run(Args {
        command,
        file,
        json: acc.json,
        limit: acc.limit,
        verbose: acc.verbose,
        begin: acc.begin,
        end: acc.end,
        scope: acc.scope,
        depth: acc.depth,
        filter: acc.filter,
        exclude: acc.exclude,
        at: acc.at,
        condition: acc.condition,
        show: acc.show,
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
    // Only the value is checked here, not the `--depth` / `--scope` pairing: a
    // default `--depth` is legitimate when the lines supply their own scopes.
    // The pairing is enforced per line, on the merged options.
    if let Some(n) = acc.depth {
        if n <= 0 {
            return Err(format!("depth must be positive; got {n}"));
        }
    }
    Ok(ParseOutcome::Batch(BatchInvocation {
        file,
        json: acc.json,
        defaults: Defaults {
            limit: acc.limit,
            verbose: acc.verbose,
            begin: acc.begin,
            end: acc.end,
            scope: acc.scope,
            depth: acc.depth,
            filter: acc.filter,
            exclude: acc.exclude,
            at: acc.at,
            condition: acc.condition,
            show: acc.show,
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
    // Each selection flag overrides its own default independently. A line can
    // therefore lift one inherited flag without disturbing the others by
    // passing it empty (`--filter ''`), which reads as "not given".
    let scope = acc.scope.or_else(|| defaults.scope.clone());
    let depth = acc.depth.or(defaults.depth);
    let filter = acc.filter.or_else(|| defaults.filter.clone());
    let exclude = acc.exclude.or_else(|| defaults.exclude.clone());
    let at = acc.at.or_else(|| defaults.at.clone());
    // A line carrying any `--condition` of its own replaces the entire default
    // clause group; with none, the default group is inherited wholesale.
    let condition = if acc.condition.is_empty() {
        defaults.condition.clone()
    } else {
        acc.condition
    };
    let show = acc.show.or_else(|| defaults.show.clone());
    check_required(&command, &at, &condition)?;
    check_limit(limit)?;
    check_depth(depth, &scope)?;
    Ok(Args {
        command,
        file: file.to_string(),
        json: false,
        limit,
        verbose,
        begin,
        end,
        scope,
        depth,
        filter,
        exclude,
        at,
        condition,
        show,
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
    fn changed_flag_removed_with_hint() {
        match p(&["search", "x.vcd", "--condition", "a=1", "--changed", "b"]) {
            ParseOutcome::Error(e) => {
                assert!(e.contains("--condition \"changed(SIG)\""), "{e}");
            }
            _ => panic!("expected error for removed --changed flag"),
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
        // Every value flag must be in VALUE_FLAGS, the selection ones included.
        for flag in ["--scope", "--depth", "--exclude"] {
            match p(&["list", "x.vcd", flag, "--version"]) {
                ParseOutcome::Run(_) | ParseOutcome::Error(_) => {}
                other => panic!("{flag} hijacked by --version: {}", outcome_kind(&other)),
            }
        }
        // A genuine --version anywhere still works.
        match p(&["--filter", "clk", "--version", "info", "x.vcd"]) {
            ParseOutcome::Print(s) => assert!(s.contains(crate::VERSION)),
            _ => panic!("expected version print"),
        }
    }

    #[test]
    fn selection_flags_parse() {
        match p(&[
            "list", "x.vcd", "--scope", "u_tx", "--depth", "2", "--filter", "err", "--exclude",
            "clk",
        ]) {
            ParseOutcome::Run(a) => {
                assert_eq!(a.scope.as_deref(), Some("u_tx"));
                assert_eq!(a.depth, Some(2));
                assert_eq!(a.filter.as_deref(), Some("err"));
                assert_eq!(a.exclude.as_deref(), Some("clk"));
            }
            other => panic!("expected Run, got {}", outcome_kind(&other)),
        }
        // --exclude stands alone.
        match p(&["summary", "x.vcd", "--exclude", "*_sync_*"]) {
            ParseOutcome::Run(a) => {
                assert!(a.filter.is_none());
                assert_eq!(a.exclude.as_deref(), Some("*_sync_*"));
            }
            other => panic!("expected Run, got {}", outcome_kind(&other)),
        }
        for flag in ["--scope", "--exclude", "--depth"] {
            match p(&["list", "x.vcd", flag]) {
                ParseOutcome::Error(e) => assert!(e.contains(flag), "{e}"),
                other => panic!("expected Error, got {}", outcome_kind(&other)),
            }
        }
    }

    #[test]
    fn depth_is_positive_and_needs_a_scope() {
        // Depth counts from the scope root, so it means nothing without one.
        match p(&["list", "x.vcd", "--depth", "2"]) {
            ParseOutcome::Error(e) => assert!(e.contains("--depth requires --scope"), "{e}"),
            other => panic!("expected Error, got {}", outcome_kind(&other)),
        }
        // A blank scope is "not given" — it must not satisfy the pairing.
        match p(&["list", "x.vcd", "--depth", "2", "--scope", "  "]) {
            ParseOutcome::Error(e) => assert!(e.contains("--depth requires --scope"), "{e}"),
            other => panic!("expected Error, got {}", outcome_kind(&other)),
        }
        for bad in ["0", "-3"] {
            match p(&["list", "x.vcd", "--scope", "u_tx", "--depth", bad]) {
                ParseOutcome::Error(e) => assert!(e.contains("depth must be positive"), "{e}"),
                other => panic!("expected Error, got {}", outcome_kind(&other)),
            }
        }
        match p(&["list", "x.vcd", "--scope", "u_tx", "--depth", "x"]) {
            ParseOutcome::Error(e) => assert!(e.contains("invalid int value"), "{e}"),
            other => panic!("expected Error, got {}", outcome_kind(&other)),
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
    fn batch_selection_defaults_merge_per_flag() {
        let defaults = match p(&[
            "--batch", "x.vcd", "--scope", "u_tx", "--depth", "2", "--filter", "err", "--exclude",
            "clk",
        ]) {
            ParseOutcome::Batch(b) => b.defaults,
            other => panic!("expected Batch, got {}", outcome_kind(&other)),
        };
        // A bare line inherits every default.
        let a = parse_batch_line(&["list".into()], "f.vcd", &defaults).unwrap();
        assert_eq!(a.scope.as_deref(), Some("u_tx"));
        assert_eq!(a.depth, Some(2));
        assert_eq!(a.filter.as_deref(), Some("err"));
        assert_eq!(a.exclude.as_deref(), Some("clk"));

        // Overriding one leaves the others alone.
        let toks: Vec<String> = ["list", "--scope", "u_rx"].iter().map(|s| s.to_string()).collect();
        let b = parse_batch_line(&toks, "f.vcd", &defaults).unwrap();
        assert_eq!(b.scope.as_deref(), Some("u_rx"));
        assert_eq!(b.filter.as_deref(), Some("err"));

        // An empty value lifts an inherited default without touching the rest —
        // the escape hatch for a line that wants the whole file back.
        let toks: Vec<String> = ["list", "--filter", ""].iter().map(|s| s.to_string()).collect();
        let c = parse_batch_line(&toks, "f.vcd", &defaults).unwrap();
        assert_eq!(c.filter.as_deref(), Some(""));
        assert_eq!(c.exclude.as_deref(), Some("clk"));
    }

    #[test]
    fn batch_depth_pairing_is_enforced_per_line() {
        // A default --depth is legal on its own: lines may bring their own
        // scopes. Only a line that ends up scope-less is an error.
        let defaults = match p(&["--batch", "x.vcd", "--depth", "2"]) {
            ParseOutcome::Batch(b) => b.defaults,
            other => panic!("expected Batch, got {}", outcome_kind(&other)),
        };
        assert_eq!(defaults.depth, Some(2));
        let e = parse_batch_line(&["list".into()], "f.vcd", &defaults).unwrap_err();
        assert!(e.contains("--depth requires --scope"), "{e}");
        let toks: Vec<String> = ["list", "--scope", "u_tx"].iter().map(|s| s.to_string()).collect();
        assert!(parse_batch_line(&toks, "f.vcd", &defaults).is_ok());

        // Clearing an inherited scope re-exposes the inherited depth.
        let paired = match p(&["--batch", "x.vcd", "--depth", "2", "--scope", "u_tx"]) {
            ParseOutcome::Batch(b) => b.defaults,
            other => panic!("expected Batch, got {}", outcome_kind(&other)),
        };
        let toks: Vec<String> = ["list", "--scope", ""].iter().map(|s| s.to_string()).collect();
        let e = parse_batch_line(&toks, "f.vcd", &paired).unwrap_err();
        assert!(e.contains("--depth requires --scope"), "{e}");

        // A non-positive default is rejected up front, at the --batch line.
        match p(&["--batch", "x.vcd", "--depth", "0"]) {
            ParseOutcome::Error(e) => assert!(e.contains("depth must be positive"), "{e}"),
            other => panic!("expected Error, got {}", outcome_kind(&other)),
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
