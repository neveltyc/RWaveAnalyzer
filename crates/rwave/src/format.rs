// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Value and time formatting/parsing.
//!
//! These routines reproduce the observable behaviour of the reference
//! `vcd_analyzer.py` (IEEE 1364-2005 §18.2.2 value formatting and the tool's
//! time-string grammar) so that `rwave` output matches the Python tool field
//! for field, while sourcing the underlying data from `wellen`.

/// Quote a string the way Python's `repr()` does for error messages: single
/// quotes by default, switching to double quotes only when the value contains a
/// single quote but no double quote. Backslashes and ASCII control characters
/// are always escaped using the same conventions CPython's `unicode_repr` uses
/// (`\\`, `\n`, `\r`, `\t`, `\xNN`), so `rwave`'s error text is byte-identical
/// to the Python analyzer's.
pub fn pyrepr(s: &str) -> String {
    let has_single = s.contains('\'');
    let has_double = s.contains('"');
    // Switch to double quotes only when the string contains a single quote but
    // not a double quote (Python's heuristic).
    let dquote = has_single && !has_double;
    let quote_ch = if dquote { '"' } else { '\'' };

    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote_ch);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote_ch => {
                out.push('\\');
                out.push(c);
            }
            // ASCII C0 controls + DEL → \xNN (matches CPython repr).
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote_ch);
    out
}

/// Multipliers (in seconds) for the time-unit suffixes accepted on the CLI.
const UNITS: &[(&str, f64)] = &[
    ("fs", 1e-15),
    ("ps", 1e-12),
    ("ns", 1e-9),
    ("us", 1e-6),
    ("ms", 1e-3),
    ("s", 1.0),
];

/// Maximum accepted length of a time argument string (CPU-DoS guard).
const MAX_TIME_ARG_LEN: usize = 100;
/// int64 max — keeps downstream tick arithmetic safe.
pub const MAX_TIME_TICKS: i64 = i64::MAX;

/// Error raised while parsing a time argument.
#[derive(Debug, Clone)]
pub struct TimeParseError(pub String);

impl std::fmt::Display for TimeParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn unit_factor(unit: &str) -> Option<f64> {
    UNITS.iter().find(|(u, _)| *u == unit).map(|(_, f)| *f)
}

/// A signal's formatting-relevant metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// Fixed-width logic vector (wire/reg/logic/...). `width` bits.
    Bits,
    /// Real / realtime (printed verbatim).
    Real,
    /// String-valued variable (printed verbatim).
    Str,
    /// Event variable (always prints `triggered`).
    Event,
}

/// Format a recorded value the way `vcd_analyzer.py`'s `fmt_val` does.
///
/// * `event`            -> `triggered`
/// * `real`/`string`    -> the value verbatim
/// * 1-bit logic        -> `0` / `1` / `x` / `z` (lower-cased)
/// * multi-bit clean    -> `<decimal> (0x<hex>)`, hex zero-padded to width
/// * multi-bit w/ x or z -> `b<bits>`
///
/// `raw` for a logic vector is the MSB-first bit string from wellen
/// (e.g. `"0010"`, possibly containing `x`/`z`). For real/string it is the
/// literal text.
pub fn fmt_val(raw: &str, kind: ValueKind, width: u32) -> String {
    match kind {
        ValueKind::Event => "triggered".to_string(),
        ValueKind::Real | ValueKind::Str => raw.to_string(),
        ValueKind::Bits => fmt_bits(raw, width),
    }
}

fn fmt_bits(raw: &str, width: u32) -> String {
    let width = width.max(1) as usize;
    // wellen normalises bit chars to lower-case h/u/w/l/x/z/0/1; we treat any
    // non-0/1 bit as "unknown" for display, mirroring the Python 4-state model
    // which only knows 0/1/x/z. Map the 9-state chars down to x/z so that the
    // textual output stays in the documented {0,1,x,z} alphabet.
    let value = normalize_bits(raw);

    // Malformed inputs may carry more 4-state bits than the declared width.
    // Don't truncate to the LSBs (that fabricates a plausible value); show
    // explicit unknowns instead.
    let value = if value.len() > width && is_4state(&value) {
        "x".repeat(width)
    } else {
        value
    };

    if width == 1 {
        return value;
    }

    // Left-extend short vectors per IEEE Table 18-1: x extends x, z extends z,
    // otherwise 0.
    let value = if value.len() < width {
        let msb = value.chars().next().unwrap_or('0');
        let pad = if msb == 'x' || msb == 'z' { msb } else { '0' };
        let mut s = String::with_capacity(width);
        for _ in 0..(width - value.len()) {
            s.push(pad);
        }
        s.push_str(&value);
        s
    } else {
        value
    };

    if value.contains('x') || value.contains('z') {
        let mut s = String::with_capacity(value.len() + 1);
        s.push('b');
        s.push_str(&value);
        return s;
    }

    // Clean binary -> decimal (0xhex). Use big integer math via string parsing
    // so wide buses don't overflow u64.
    match bits_to_decimal_and_hex(&value, width) {
        Some((dec, hex)) => format!("{dec} (0x{hex})"),
        None => {
            let mut s = String::with_capacity(value.len() + 1);
            s.push('b');
            s.push_str(&value);
            s
        }
    }
}

/// Lower-case bit chars and collapse the 9-state alphabet to the 4-state set
/// used for display. `h`->`1`, `l`->`0`, `u`/`w`/`-` and anything unexpected
/// become `x`; `z` stays `z`.
fn normalize_bits(raw: &str) -> String {
    raw.chars()
        .map(|c| match c.to_ascii_lowercase() {
            '0' => '0',
            '1' => '1',
            'z' => 'z',
            'x' => 'x',
            'h' => '1',
            'l' => '0',
            // u (uninitialized), w (weak unknown), '-' (don't care) -> unknown
            _ => 'x',
        })
        .collect()
}

fn is_4state(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| matches!(c, '0' | '1' | 'x' | 'z'))
}

/// Convert a clean binary string (only 0/1) to (decimal, lower-hex) strings.
/// The hex is zero-padded to `ceil(width/4)` digits. Returns `None` if the
/// string is not pure binary.
fn bits_to_decimal_and_hex(bits: &str, width: usize) -> Option<(String, String)> {
    if bits.is_empty() || !bits.chars().all(|c| c == '0' || c == '1') {
        return None;
    }
    // Decimal via repeated *2 + bit on a base-10 big number (Vec of digits,
    // least-significant first). Adequate for arbitrary widths.
    let dec = binary_to_decimal_string(bits);
    let hex = binary_to_hex_string(bits, width);
    Some((dec, hex))
}

/// Big-number binary->decimal. Returns the decimal string with no leading
/// zeros (except "0").
fn binary_to_decimal_string(bits: &str) -> String {
    // digits: little-endian base-10
    let mut digits: Vec<u8> = vec![0];
    for b in bits.chars() {
        // multiply by 2
        let mut carry = 0u8;
        for d in digits.iter_mut() {
            let v = *d * 2 + carry;
            *d = v % 10;
            carry = v / 10;
        }
        while carry > 0 {
            digits.push(carry % 10);
            carry /= 10;
        }
        // add the bit
        if b == '1' {
            let mut carry = 1u8;
            for d in digits.iter_mut() {
                if carry == 0 {
                    break;
                }
                let v = *d + carry;
                *d = v % 10;
                carry = v / 10;
            }
            while carry > 0 {
                digits.push(carry % 10);
                carry /= 10;
            }
        }
    }
    let mut s: String = digits.iter().rev().map(|d| (b'0' + d) as char).collect();
    // strip leading zeros
    let trimmed = s.trim_start_matches('0');
    if trimmed.is_empty() {
        s = "0".to_string();
    } else {
        s = trimmed.to_string();
    }
    s
}

/// Big-number binary->hex, zero-padded to ceil(width/4) digits.
fn binary_to_hex_string(bits: &str, width: usize) -> String {
    let hw = ((width + 3) / 4).max(1);
    // Pad bits on the left to a multiple of 4 so we can chunk into nibbles.
    let pad = (4 - (bits.len() % 4)) % 4;
    let mut padded = String::with_capacity(bits.len() + pad);
    for _ in 0..pad {
        padded.push('0');
    }
    padded.push_str(bits);
    let mut hex = String::with_capacity(padded.len() / 4);
    let chars: Vec<char> = padded.chars().collect();
    for chunk in chars.chunks(4) {
        let mut nib = 0u8;
        for &c in chunk {
            nib = (nib << 1) | if c == '1' { 1 } else { 0 };
        }
        hex.push(std::char::from_digit(nib as u32, 16).unwrap());
    }
    // strip leading zeros, then left-pad to hw
    let trimmed = hex.trim_start_matches('0');
    let core = if trimmed.is_empty() { "0" } else { trimmed };
    if core.len() >= hw {
        core.to_string()
    } else {
        let mut s = String::with_capacity(hw);
        for _ in 0..(hw - core.len()) {
            s.push('0');
        }
        s.push_str(core);
        s
    }
}

/// Parse a time string to internal ticks, given the timescale in seconds.
///
/// Grammar (matching `vcd_analyzer.py`):
///   * bare non-negative integer -> raw ticks
///   * value with `fs|ps|ns|us|ms|s` suffix -> scaled by timescale, rounded
///   * bare fractional (e.g. `10.5`) is rejected; use a unit suffix
///   * no space allowed between number and unit
///   * negative non-zero rejected; `-0` treated as 0
pub fn parse_time(s: &str, ts_sec: f64) -> Result<i64, TimeParseError> {
    if s.len() > MAX_TIME_ARG_LEN {
        return Err(TimeParseError(format!(
            "time value too long; max length is {MAX_TIME_ARG_LEN}"
        )));
    }
    let stripped = s.trim();

    // Try to match  sign? number unit?
    if let Some((sign, num, unit)) = match_time(stripped) {
        if sign == '-' && !num.trim_matches(['0', '.']).is_empty() {
            return Err(TimeParseError(format!(
                "time must be non-negative; got {}", pyrepr(s)
            )));
        }
        match unit {
            None => {
                if num.contains('.') {
                    return Err(TimeParseError(format!(
                        "bare numeric time must be integer ticks; got {}. \
                         Use a unit suffix for fractional times, e.g. {num}ns", pyrepr(s)
                    )));
                }
                let v = parse_ticks_decimal(num, s)?;
                check_time_range(v, s)
            }
            Some(unit) => {
                if ts_sec <= 0.0 {
                    return Err(TimeParseError(
                        "cannot convert time with unit because timescale is 0 or invalid"
                            .to_string(),
                    ));
                }
                let factor = unit_factor(unit).ok_or_else(|| {
                    TimeParseError(format!("invalid time unit in {}", pyrepr(s)))
                })?;
                let val: f64 = num.parse().map_err(|_| {
                    TimeParseError(format!("invalid time value {}", pyrepr(s)))
                })?;
                let scaled = val * factor / ts_sec;
                if !scaled.is_finite() {
                    return Err(TimeParseError(format!(
                        "time value {} is not finite", pyrepr(s)
                    )));
                }
                let rounded = round_half_even(scaled);
                // A scaled time beyond i64 range would saturate on `as i64`,
                // silently fabricating a value; reject it as "too large" to
                // match the reference's range check.
                //
                // `i64::MAX as f64` rounds *up* to 2^63 (i64::MAX itself isn't
                // exactly representable as f64), so `>=` is required: a `rounded`
                // value equal to that f64 would cast to a saturated i64::MAX,
                // off by one (or more) from the intended tick count.
                if rounded < 0.0 {
                    return Err(TimeParseError(format!(
                        "time must be non-negative; got {}", pyrepr(s)
                    )));
                }
                if rounded >= MAX_TIME_TICKS as f64 {
                    return Err(TimeParseError(format!(
                        "time value too large; got {}, max ticks is {}",
                        pyrepr(s),
                        MAX_TIME_TICKS
                    )));
                }
                check_time_range(rounded as i64, s)
            }
        }
    } else {
        // Fall back to bare integer.
        let v = parse_ticks_decimal(stripped, s)?;
        check_time_range(v, s)
    }
}

/// Parse a bare integer-ticks string into `i64`, distinguishing the cases the
/// reference tool treats differently: a value made only of digits but exceeding
/// the int64 range is reported as "too large" (not "invalid"), a negative value
/// as "non-negative", and anything else as "invalid". This mirrors
/// `vcd_analyzer.py`, where ticks come from Python's `int()` (arbitrary
/// precision, and accepting `_` digit-group separators between digits).
fn parse_ticks_decimal(num: &str, original: &str) -> Result<i64, TimeParseError> {
    // Accept Python-style underscores (only *between* digits), matching the
    // reference's int() parsing; reject leading/trailing/doubled underscores.
    let cleaned = match strip_int_underscores(num) {
        Some(c) => c,
        None => {
            return Err(TimeParseError(format!(
                "invalid time value {}; expected integer ticks or value \
                 with fs/ps/ns/us/ms/s suffix",
                pyrepr(original)
            )));
        }
    };
    let body = cleaned.strip_prefix('-').unwrap_or(&cleaned);
    let is_neg = cleaned.starts_with('-');
    let all_digits = !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit());
    match cleaned.parse::<i64>() {
        Ok(v) => Ok(v),
        Err(_) if all_digits && !is_neg => {
            // Pure non-negative digits that don't fit i64: "too large".
            Err(TimeParseError(format!(
                "time value too large; got {}, max ticks is {}",
                pyrepr(original),
                MAX_TIME_TICKS
            )))
        }
        Err(_) => Err(TimeParseError(format!(
            "invalid time value {}; expected integer ticks or value \
             with fs/ps/ns/us/ms/s suffix",
            pyrepr(original)
        ))),
    }
}

/// Validate and remove Python-style `_` digit separators from an integer
/// literal. Returns the underscore-free string if `s` is a well-formed integer
/// (optional leading sign, then digits with single underscores only between
/// two digits), else `None`. Examples: `1_000`->`1000`, `1_0_0_0`->`1000`,
/// while `_1`, `1_`, `1__0`, and `` are rejected. A string with no underscores
/// is returned unchanged (still validated as sign+digits).
fn strip_int_underscores(s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut out = String::with_capacity(s.len());
    if bytes[0] == b'+' || bytes[0] == b'-' {
        out.push(bytes[0] as char);
        i = 1;
    }
    let digits_start = i;
    let mut prev_was_digit = false;
    while i < bytes.len() {
        match bytes[i] {
            b'0'..=b'9' => {
                out.push(bytes[i] as char);
                prev_was_digit = true;
            }
            b'_' => {
                // An underscore is only legal immediately between two digits:
                // the previous char must be a digit and the next must be one.
                if !prev_was_digit {
                    return None;
                }
                let next_is_digit = bytes.get(i + 1).is_some_and(|b| b.is_ascii_digit());
                if !next_is_digit {
                    return None;
                }
                prev_was_digit = false; // the '_' itself is not a digit
                // do not push the underscore
            }
            _ => return None,
        }
        i += 1;
    }
    // Must have at least one digit after the optional sign.
    if i == digits_start {
        return None;
    }
    Some(out)
}

/// Round to nearest integer, ties to even — matching Python 3's built-in
/// `round()` (banker's rounding), which `vcd_analyzer.py` uses when scaling a
/// unit-suffixed time to ticks. Rust's `f64::round` rounds half away from zero,
/// which would disagree on exact `.5` cases (e.g. 0.5 -> 0 here, not 1).
pub fn round_half_even(x: f64) -> f64 {
    let r = x.round(); // half away from zero
    if (x - x.trunc()).abs() == 0.5 {
        // Exactly halfway: pick the even neighbour.
        let floor = x.floor();
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        r
    }
}

fn check_time_range(v: i64, original: &str) -> Result<i64, TimeParseError> {
    if v < 0 {
        return Err(TimeParseError(format!(
            "time must be non-negative; got {}", pyrepr(original)
        )));
    }
    // The upper bound (MAX_TIME_TICKS == i64::MAX) cannot be exceeded by an i64,
    // so it is enforced at parse time instead (see parse_ticks_decimal and the
    // unit-scaled path), where the source value may be larger than i64 range.
    Ok(v)
}

/// Anchored match of `sign? (digits.digits? | .digits | digits) unit?`.
/// Returns `(sign_char, number_str, unit)` where sign is '+' if absent.
fn match_time(s: &str) -> Option<(char, &str, Option<&str>)> {
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut sign = '+';
    if bytes[i] == b'+' || bytes[i] == b'-' {
        sign = bytes[i] as char;
        i += 1;
    }
    let num_start = i;
    let mut saw_digit = false;
    let mut saw_dot = false;
    while i < bytes.len() {
        match bytes[i] {
            b'0'..=b'9' => {
                saw_digit = true;
                i += 1;
            }
            b'.' if !saw_dot => {
                saw_dot = true;
                i += 1;
            }
            _ => break,
        }
    }
    // Must be one of: digits, digits.digits?, .digits
    let num = &s[num_start..i];
    let valid_num = match (saw_digit, saw_dot) {
        (true, _) => true,
        (false, true) => num.len() > 1, // ".5" ok, "." not
        (false, false) => false,
    };
    if !valid_num {
        return None;
    }
    let rest = &s[i..];
    if rest.is_empty() {
        return Some((sign, num, None));
    }
    if unit_factor(rest).is_some() {
        return Some((sign, num, Some(rest)));
    }
    None
}

/// Format internal ticks to a human-readable string (matching `fmt_time`).
///
/// Picks the smallest unit u where |scaled| < 1000 (preferring natural
/// boundaries), e.g. timescale 1ns -> tick 5 prints `5ns`, tick 17534700
/// prints `17.5347us`. Tick 0 always prints `0s`.
pub fn fmt_time(ticks: i64, ts_sec: f64) -> String {
    if ticks == 0 {
        return "0s".to_string();
    }
    if !ts_sec.is_finite() || ts_sec <= 0.0 {
        return "?".to_string();
    }
    let sec = ticks as f64 * ts_sec;
    if !sec.is_finite() {
        return "?".to_string();
    }
    for (u, f) in UNITS {
        let scaled = sec / f;
        if (-1000.0 < scaled && scaled < 1000.0) || *u == "s" {
            return format!("{}{}", fmt_g(scaled), u);
        }
    }
    format!("{}s", fmt_g(sec))
}

/// Render a float the way Python's `%g` (default precision 6) does, which is
/// how `vcd_analyzer.py` prints scaled times. This means up to 6 significant
/// digits, trailing zeros stripped, and exponent form for very large/small
/// magnitudes.
pub fn fmt_g(x: f64) -> String {
    if x == 0.0 {
        return "0".to_string();
    }
    // %g uses exponent form when exp < -4 or exp >= precision (6).
    let formatted = format!("{:.*e}", 5, x); // 6 sig digits in scientific form
    // Parse mantissa/exponent from Rust's "d.ddddde±N" form.
    let (mantissa, exp) = split_exp(&formatted);
    if exp < -4 || exp >= 6 {
        // Scientific form, mimic %g: strip trailing zeros in mantissa, use
        // e±0N with at least 2 exponent digits.
        let m = strip_trailing_zeros(&mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{}e{}{:02}", m, sign, exp.abs());
    }
    // Fixed form with (6 - 1 - exp) fractional digits, then strip zeros.
    let frac_digits = (6 - 1 - exp).max(0) as usize;
    let fixed = format!("{:.*}", frac_digits, x);
    strip_trailing_zeros(&fixed)
}

fn split_exp(sci: &str) -> (String, i32) {
    // sci looks like "1.23456e2" or "1.23456e-3"
    if let Some(pos) = sci.find('e') {
        let mantissa = sci[..pos].to_string();
        let exp: i32 = sci[pos + 1..].parse().unwrap_or(0);
        (mantissa, exp)
    } else {
        (sci.to_string(), 0)
    }
}

fn strip_trailing_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_underscores_match_python_int() {
        // Underscores between digits are accepted (bare integer ticks only).
        assert_eq!(parse_time("1_000", 1e-9).unwrap(), 1000);
        assert_eq!(parse_time("1_0_0_0", 1e-9).unwrap(), 1000);
        // Leading/trailing/doubled underscores are rejected.
        for bad in ["1__000", "_1000", "1000_", "_", "1_"] {
            assert!(parse_time(bad, 1e-9).is_err(), "should reject {bad}");
        }
        // Underscores are NOT accepted with a unit suffix or in hex (the
        // reference's numeric+unit form has no underscores).
        assert!(parse_time("1_000ns", 1e-9).is_err());
        assert!(parse_time("10_00ns", 1e-9).is_err());
        assert!(parse_time("0x1_0", 1e-9).is_err());
    }

    #[test]
    fn time_overflow_reports_too_large() {
        // Bare integer beyond i64 range -> "too large" (not "invalid"), matching
        // the reference whose ticks are arbitrary-precision.
        let e = parse_time("99999999999999999999", 1e-9).unwrap_err();
        assert!(e.0.contains("too large"), "got: {}", e.0);
        assert!(e.0.contains("9223372036854775807"), "got: {}", e.0);
        // i64::MAX + 1.
        let e = parse_time("9223372036854775808", 1e-9).unwrap_err();
        assert!(e.0.contains("too large"), "got: {}", e.0);
        // Exactly i64::MAX parses fine.
        assert_eq!(parse_time("9223372036854775807", 1e-9).unwrap(), i64::MAX);
        // Non-digit garbage stays "invalid".
        let e = parse_time("abc", 1e-9).unwrap_err();
        assert!(e.0.contains("invalid time value"), "got: {}", e.0);
        // Negative stays "non-negative".
        let e = parse_time("-5", 1e-9).unwrap_err();
        assert!(e.0.contains("non-negative"), "got: {}", e.0);
    }

    #[test]
    fn time_unit_overflow_rejects_at_f64_boundary() {
        // `9.223372036854775808e18 ns` at 1ns timescale scales to f64
        // `9.223372036854776e18` (== i64::MAX as f64 == 2^63), which casts
        // *saturating* to i64::MAX. The check must reject this as "too large"
        // rather than silently produce i64::MAX - 1.
        let e = parse_time("9223372036854775808.0ns", 1e-9).unwrap_err();
        assert!(e.0.contains("too large"), "got: {}", e.0);
        // Values well above i64::MAX in floating-point scaling also rejected.
        // (Scientific notation isn't accepted by match_time; use a long literal.)
        let e = parse_time("99999999999999999999.0s", 1.0).unwrap_err();
        assert!(e.0.contains("too large"), "got: {}", e.0);
    }

    #[test]
    fn pyrepr_matches_python_for_quoting_and_escapes() {
        // Single-quote default, double-quote when string has a single-quote
        // and no double-quote, mirroring Python's repr().
        assert_eq!(pyrepr("hello"), "'hello'");
        assert_eq!(pyrepr("it's"), "\"it's\"");
        assert_eq!(pyrepr("\""), "'\"'");
        assert_eq!(pyrepr("'\""), "'\\'\"'");
        // Backslashes always escape, in both quoting modes.
        assert_eq!(pyrepr("a\\b"), "'a\\\\b'");
        assert_eq!(pyrepr("it's a\\b"), "\"it's a\\\\b\"");
        // ASCII control characters escape to Python's named escapes / \xNN.
        assert_eq!(pyrepr("a\nb"), "'a\\nb'");
        assert_eq!(pyrepr("a\tb"), "'a\\tb'");
        assert_eq!(pyrepr("a\rb"), "'a\\rb'");
        assert_eq!(pyrepr("\x01"), "'\\x01'");
        assert_eq!(pyrepr("\x7f"), "'\\x7f'");
    }

    #[test]
    fn one_bit() {
        assert_eq!(fmt_val("0", ValueKind::Bits, 1), "0");
        assert_eq!(fmt_val("1", ValueKind::Bits, 1), "1");
        assert_eq!(fmt_val("x", ValueKind::Bits, 1), "x");
        assert_eq!(fmt_val("z", ValueKind::Bits, 1), "z");
    }

    #[test]
    fn multibit_clean() {
        // 8-bit 0x11 = 17
        assert_eq!(fmt_val("00010001", ValueKind::Bits, 8), "17 (0x11)");
        // 3-bit value 2
        assert_eq!(fmt_val("010", ValueKind::Bits, 3), "2 (0x2)");
        // 8-bit 0x22 = 34
        assert_eq!(fmt_val("00100010", ValueKind::Bits, 8), "34 (0x22)");
    }

    #[test]
    fn multibit_short_is_left_extended() {
        // "1" in a 3-bit signal -> 001 -> 1 (0x1)
        assert_eq!(fmt_val("1", ValueKind::Bits, 3), "1 (0x1)");
        // "10" in 8-bit -> 2, hex zero-padded to ceil(8/4)=2 digits -> 0x02
        assert_eq!(fmt_val("10", ValueKind::Bits, 8), "2 (0x02)");
    }

    #[test]
    fn multibit_unknown() {
        assert_eq!(fmt_val("01x0", ValueKind::Bits, 4), "b01x0");
        // short with x MSB extends with x
        assert_eq!(fmt_val("x0", ValueKind::Bits, 4), "bxxx0");
    }

    #[test]
    fn wide_bus_no_overflow() {
        // 64 ones = 0xffffffffffffffff = 18446744073709551615
        let bits = "1".repeat(64);
        assert_eq!(
            fmt_val(&bits, ValueKind::Bits, 64),
            "18446744073709551615 (0xffffffffffffffff)"
        );
        // 128-bit: 1 followed by 127 zeros
        let mut b = String::from("1");
        b.push_str(&"0".repeat(127));
        let out = fmt_val(&b, ValueKind::Bits, 128);
        assert!(out.starts_with("170141183460469231731687303715884105728 (0x8"));
    }

    #[test]
    fn event_and_real() {
        assert_eq!(fmt_val("", ValueKind::Event, 1), "triggered");
        assert_eq!(fmt_val("3.14", ValueKind::Real, 64), "3.14");
        assert_eq!(fmt_val("hello", ValueKind::Str, 0), "hello");
    }

    #[test]
    fn nine_state_maps_to_four() {
        // h -> 1, l -> 0
        assert_eq!(fmt_val("hl", ValueKind::Bits, 2), "2 (0x2)");
        // u -> x
        assert_eq!(fmt_val("u", ValueKind::Bits, 1), "x");
    }

    #[test]
    fn time_parse_bare_and_units() {
        // timescale 1ns => 1e-9 s
        let ts = 1e-9;
        assert_eq!(parse_time("0", ts).unwrap(), 0);
        assert_eq!(parse_time("100", ts).unwrap(), 100);
        assert_eq!(parse_time("100ns", ts).unwrap(), 100);
        assert_eq!(parse_time("1us", ts).unwrap(), 1000);
        assert_eq!(parse_time("17.5us", ts).unwrap(), 17500);
        // Banker's rounding (ties to even): 0.5 -> 0, 1.5 -> 2.
        assert_eq!(parse_time(".5ns", ts).unwrap(), 0);
        assert_eq!(parse_time("1.5ns", ts).unwrap(), 2);
        assert_eq!(parse_time("2.5ns", ts).unwrap(), 2);
    }

    #[test]
    fn time_parse_rejects() {
        let ts = 1e-9;
        assert!(parse_time("10.5", ts).is_err()); // bare fractional
        assert!(parse_time("5 ns", ts).is_err()); // space
        assert!(parse_time("-5", ts).is_err()); // negative
        assert_eq!(parse_time("-0", ts).unwrap(), 0); // -0 ok
    }

    #[test]
    fn fmt_time_examples() {
        let ts = 1e-9; // 1ns
        assert_eq!(fmt_time(0, ts), "0s");
        assert_eq!(fmt_time(5, ts), "5ns");
        assert_eq!(fmt_time(1000, ts), "1us");
        // 17_534_700 ticks * 1ns = 1.75347e-2 s = 17.5347 ms.
        assert_eq!(fmt_time(17534700, ts), "17.5347ms");
        assert_eq!(fmt_time(100, ts), "100ns");
    }

    #[test]
    fn fmt_g_matches_python() {
        assert_eq!(fmt_g(5.0), "5");
        assert_eq!(fmt_g(17.5347), "17.5347");
        assert_eq!(fmt_g(1.0), "1");
        assert_eq!(fmt_g(0.5), "0.5");
        assert_eq!(fmt_g(1000.0), "1000");
        assert_eq!(fmt_g(0.001), "0.001");
    }
}
