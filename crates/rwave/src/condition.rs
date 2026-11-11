// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Search condition parsing and value matching.
//!
//! A condition list is comma-separated AND terms. Each term is
//! `SIG=VAL`, `SIG==VAL`, `SIG!=VAL`, or the edge predicate `changed(SIG)`
//! (true at exactly the ticks where SIG transitions; its presence switches
//! `search` to event mode). Values may be decimal (`5`), hex
//! (`0xff`), binary (`b1010`, `0b1010`), 4-state (`b1x0z`), or a bare 4-state
//! literal (`1x0`). A binary literal may mark don't-care bits with `?`
//! (`b?????1??`), like `?` in a Verilog `casez` item. Numeric targets match by
//! numeric equality; 4-state targets match as (width-aware) bit patterns, a
//! mask on its cared bits only. `!=` does **not** match x/z/undefined.
//!
//! A negative decimal (`-1`) means its two's-complement pattern in the logic
//! signal's width, since VCD records no signedness. A number with a fraction or
//! exponent (`3.14`, `-1.5`, `1e-9`) targets a real signal, which compares any
//! decimal or real number numerically. [`resolve_target`] fits a target to its
//! signal once the signal is known, and rejects one the signal cannot hold.

#[derive(Debug, Clone)]
pub struct ConditionParseError(pub String);
#[derive(Debug, Clone)]
pub struct ValueParseError(pub String);

impl std::fmt::Display for ConditionParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::fmt::Display for ValueParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

const MAX_SIGNAL_WIDTH: usize = 65536;
const MAX_VALUE_ARG_LEN: usize = MAX_SIGNAL_WIDTH + 2;
const MAX_DECIMAL_VALUE_DIGITS: usize = 100;
const MAX_HEX_VALUE_DIGITS: usize = (MAX_SIGNAL_WIDTH + 3) / 4;

/// Comparison operator in a condition term.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    EqEq,
    Ne,
}

impl Op {
    pub fn as_str(&self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::EqEq => "==",
            Op::Ne => "!=",
        }
    }
}

/// A parsed (but not yet signal-resolved) condition target.
#[derive(Debug, Clone)]
pub struct Target {
    /// For non-numeric 4-state targets: the raw bit string (e.g. `1x0`, or
    /// `1??0` for a don't-care mask).
    /// For numeric targets: the lower-cased original (informational).
    pub raw: String,
    /// `Some(n)` for numeric targets matched by integer equality; `None` for
    /// 4-state bit-pattern targets. The magnitude when `neg`.
    pub int: Option<BigUint>,
    /// `int` was written with a minus sign; [`resolve_target`] folds it into
    /// the signal's two's complement.
    pub neg: bool,
    /// The number a real signal is compared with: set for a fraction/exponent
    /// spelling at parse time, and for any decimal once resolved to a real signal.
    pub real: Option<f64>,
}

impl Target {
    /// De-duplication key for a target: its value *as written* plus whether it
    /// is numeric vs a 4-state bit pattern. Deliberately keyed on the spelling,
    /// so different bases for one value (`5` vs `0x5`) are distinct — there is
    /// no cross-base normalization. Shared by the within-clause term de-dup and
    /// the cross-clause de-dup so the two can never drift apart.
    pub fn dedup_key(&self) -> String {
        let class = if self.real.is_some() {
            "real"
        } else if self.int.is_some() {
            "int"
        } else {
            "bits"
        };
        format!("{}:{class}", self.raw)
    }

    fn int(raw: String, n: BigUint, neg: bool) -> Target {
        Target { raw, int: Some(n), neg, real: None }
    }

    fn bits(raw: String) -> Target {
        Target { raw, int: None, neg: false, real: None }
    }

    /// Is this a don't-care mask, i.e. a bit pattern carrying a `?` bit?
    pub fn is_mask(&self) -> bool {
        self.int.is_none() && self.raw.contains('?')
    }
}

/// The body of one condition term.
#[derive(Debug, Clone)]
pub enum TermBody {
    /// `SIG op VAL`: compare the signal's current value against a target.
    Level {
        op: Op,
        target: Target,
        /// The value text as written (for the resolved label).
        value_text: String,
    },
    /// `changed(SIG)`: true at exactly the ticks where the signal transitions.
    Changed,
}

/// A parsed condition term prior to resolving the signal pattern.
#[derive(Debug, Clone)]
pub struct ParsedCondition {
    pub pattern: String,
    pub term: TermBody,
    /// Original term text (for labels).
    pub original: String,
}

/// Minimal arbitrary-precision unsigned integer for comparing wide bus values
/// without overflow. Stored as little-endian base-2^32 limbs. Only equality
/// and construction-from-string are needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigUint {
    limbs: Vec<u32>,
}

impl BigUint {
    fn zero() -> Self {
        BigUint { limbs: vec![] }
    }

    fn normalize(mut self) -> Self {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
        self
    }

    fn mul_small_add(&mut self, mul: u32, add: u32) {
        let mut carry = add as u64;
        for limb in self.limbs.iter_mut() {
            let v = (*limb as u64) * (mul as u64) + carry;
            *limb = (v & 0xffff_ffff) as u32;
            carry = v >> 32;
        }
        while carry > 0 {
            self.limbs.push((carry & 0xffff_ffff) as u32);
            carry >>= 32;
        }
    }

    /// Parse a decimal string.
    pub fn from_decimal(s: &str) -> Option<Self> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut n = BigUint::zero();
        for b in s.bytes() {
            n.mul_small_add(10, (b - b'0') as u32);
        }
        Some(n.normalize())
    }

    /// Parse a hex string (no `0x`).
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut n = BigUint::zero();
        for b in s.bytes() {
            let d = (b as char).to_digit(16).unwrap();
            n.mul_small_add(16, d);
        }
        Some(n.normalize())
    }

    /// Number of significant bits (0 for zero).
    pub fn bit_len(&self) -> u32 {
        match self.limbs.last() {
            Some(top) => (self.limbs.len() as u32 - 1) * 32 + (32 - top.leading_zeros()),
            None => 0,
        }
    }

    /// `2^bits - self`, for `self <= 2^bits`: the two's-complement pattern of
    /// `-self` in a `bits`-wide signal.
    fn negate_in(&self, bits: u32) -> BigUint {
        let n = bits as usize / 32 + 1;
        let mut out = vec![0u32; n];
        out[bits as usize / 32] = 1 << (bits % 32);
        let mut borrow = 0i64;
        for (i, limb) in out.iter_mut().enumerate() {
            let sub = *self.limbs.get(i).unwrap_or(&0) as i64 + borrow;
            let mut v = *limb as i64 - sub;
            borrow = 0;
            if v < 0 {
                v += 1 << 32;
                borrow = 1;
            }
            *limb = v as u32;
        }
        let r = BigUint { limbs: out }.normalize();
        // 2^bits - 0 is 2^bits itself, one bit too wide: -0 is 0.
        if r.bit_len() > bits {
            BigUint::zero()
        } else {
            r
        }
    }

    /// Is this exactly `2^k` for some `k`?
    fn is_power_of_two(&self) -> bool {
        self.limbs.iter().map(|l| l.count_ones()).sum::<u32>() == 1
    }

    /// Parse a pure binary string (only 0/1).
    pub fn from_binary(s: &str) -> Option<Self> {
        if s.is_empty() || !s.bytes().all(|b| b == b'0' || b == b'1') {
            return None;
        }
        let mut n = BigUint::zero();
        for b in s.bytes() {
            n.mul_small_add(2, (b - b'0') as u32);
        }
        Some(n.normalize())
    }
}

/// Parse a target value string into [`Target`].
pub fn parse_target_value(text: &str) -> Result<Target, ValueParseError> {
    let raw = text.trim().to_lowercase();
    if raw.is_empty() {
        return Err(ValueParseError("target value must not be empty".into()));
    }
    if raw.len() > MAX_VALUE_ARG_LEN {
        return Err(ValueParseError(format!(
            "target value too long; max length is {MAX_VALUE_ARG_LEN}"
        )));
    }
    if let Some(body) = raw.strip_prefix('-') {
        return match parse_bare_number(body)? {
            Some(BareNumber::Int(n)) => Ok(Target::int(raw.clone(), n, true)),
            Some(BareNumber::Real(r)) => Ok(Target { real: Some(-r), ..Target::bits(raw.clone()) }),
            None => Err(ValueParseError(format!(
                "invalid negative target {}; only negative decimal integers (-5) and real numbers (-1.5) are supported", crate::format::pyrepr(text)
            ))),
        };
    }
    if let Some(body) = raw.strip_prefix("0x") {
        if body.is_empty() {
            return Err(ValueParseError("hex target must contain at least one digit".into()));
        }
        if body.len() > MAX_HEX_VALUE_DIGITS {
            return Err(ValueParseError(format!(
                "hex target too wide; max hex digits is {MAX_HEX_VALUE_DIGITS}"
            )));
        }
        return match BigUint::from_hex(body) {
            Some(n) => Ok(Target::int(raw.clone(), n, false)),
            None => Err(ValueParseError(format!(
                "invalid hex target {}; x/z and ? literals must use binary form like b1x0z or b1??0", crate::format::pyrepr(text)
            ))),
        };
    }
    if let Some(body) = raw.strip_prefix("0b") {
        return parse_binary_body(body, text);
    }
    if let Some(body) = raw.strip_prefix('b') {
        return parse_binary_body(body, text);
    }
    if raw.starts_with('+') {
        return Err(ValueParseError(
            "a leading '+' is not supported; write the value without it".into(),
        ));
    }
    // Bare, in order: decimal integer, real number, 4-state literal.
    match parse_bare_number(&raw)? {
        Some(BareNumber::Int(n)) => return Ok(Target::int(raw.clone(), n, false)),
        Some(BareNumber::Real(r)) => return Ok(Target { real: Some(r), ..Target::bits(raw.clone()) }),
        None => {}
    }
    // Not decimal: must be a 4-state literal.
    if raw.len() > MAX_SIGNAL_WIDTH {
        return Err(ValueParseError(
            format!("literal target too wide; max characters is {MAX_SIGNAL_WIDTH}"),
        ));
    }
    if raw.bytes().all(|b| matches!(b, b'0' | b'1' | b'x' | b'z')) {
        Ok(Target::bits(raw))
    } else if is_mask_bits(&raw) {
        // A bare `1??0` is not guessed at: `?` needs the binary prefix.
        Err(ValueParseError(format!(
            "don't-care target {} needs a binary prefix, e.g. b{raw}", crate::format::pyrepr(text)
        )))
    } else {
        Err(ValueParseError(format!(
            "invalid target {}; expected decimal (5, -5), 0x.., b.., 0/1/x/z literal, or real number (3.14)", crate::format::pyrepr(text)
        )))
    }
}

enum BareNumber {
    Int(BigUint),
    Real(f64),
}

/// Classify an unsigned bare number: all ASCII digits is an integer; digits
/// with a fraction and/or exponent (`3.14`, `.5`, `1e-9`) is a finite real.
/// `None` when it is neither — the caller then tries a 4-state literal. The
/// grammar is spelled out rather than left to `f64::from_str`, which also
/// takes `inf`, `nan` and `infinity`.
fn parse_bare_number(s: &str) -> Result<Option<BareNumber>, ValueParseError> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        if s.len() > MAX_DECIMAL_VALUE_DIGITS {
            return Err(ValueParseError(format!(
                "decimal target too long; max digits is {MAX_DECIMAL_VALUE_DIGITS}"
            )));
        }
        return Ok(Some(BareNumber::Int(BigUint::from_decimal(s).unwrap())));
    }
    if s.len() > MAX_DECIMAL_VALUE_DIGITS + 32 || !is_real_literal(s) {
        return Ok(None);
    }
    Ok(s.parse::<f64>().ok().filter(|r| r.is_finite()).map(BareNumber::Real))
}

/// `digits [. digits] [e [+-] digits]` or `. digits [...]`, with a fraction or
/// an exponent present (a bare digit run is an integer).
fn is_real_literal(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |mut i: usize| {
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        (i, i - start)
    };
    let (mut i, int_digits) = digits(0);
    let mut frac_digits = 0;
    let mut marked = false;
    if i < b.len() && b[i] == b'.' {
        marked = true;
        (i, frac_digits) = digits(i + 1);
    }
    if int_digits + frac_digits == 0 {
        return false;
    }
    if i < b.len() && b[i] == b'e' {
        marked = true;
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let (end, exp_digits) = digits(i);
        if exp_digits == 0 {
            return false;
        }
        i = end;
    }
    marked && i == b.len()
}

/// Fit a level target to the signal it compares against, once the signal is
/// known, or say why the signal cannot hold it. `Err` carries the reason, to
/// follow `condition 'TERM': `.
///
/// * event: no level at all — `changed()` is the question to ask.
/// * real: a decimal or real number compares numerically (`dac=100` asks what
///   `dac=100.0` does); a bit pattern or a hex/binary spelling is an error.
/// * logic: a real number is an error. A negative decimal becomes its
///   two's-complement pattern in `width` bits, VCD recording no signedness. A
///   number of more than `width` bits, or a pattern with a 1/x/z above bit
///   `width-1`, can never be equal — and `!=` would always hold — so it is an
///   error rather than a plausible empty answer.
/// * string: compares as text, the number as written included (`s=5`, `s=-1`).
pub fn resolve_target(
    mut t: Target,
    kind: crate::format::ValueKind,
    width: u32,
    path: &str,
    value_text: &str,
) -> Result<Target, String> {
    use crate::format::ValueKind;
    let shown = crate::format::pyrepr(value_text);
    match kind {
        ValueKind::Event => Err(format!(
            "targets event variable {path}, which has no level; use changed({path}) to fire on each trigger"
        )),
        ValueKind::Real => {
            // A binary/hex spelling describes bits, which a real does not have;
            // only a decimal or real number names one.
            let spelled_bits = t.raw.starts_with('b') || t.raw.starts_with("0x");
            if t.real.is_none() && (t.int.is_none() || spelled_bits) {
                return Err(format!(
                    "compares real signal {path} against a bit pattern; write a decimal or real number, e.g. {path}=3.14"
                ));
            }
            if t.real.is_none() {
                // A decimal integer, as written: the f64 nearest to it.
                t.real = t.raw.parse::<f64>().ok();
                t.int = None;
                t.neg = false;
            }
            Ok(t)
        }
        ValueKind::Bits if width > 0 => {
            if t.real.is_some() {
                return Err(format!(
                    "compares logic signal {path} (width {width}) against the real number {shown}; use a decimal, hex, binary, or 4-state target"
                ));
            }
            let Some(n) = &t.int else {
                let excess = t.raw.len().saturating_sub(width as usize);
                if t.raw.as_bytes()[..excess].iter().any(|b| !matches!(b, b'0' | b'?')) {
                    return Err(format!(
                        "{shown} has a 1/x/z above bit {} and cannot match signal {path} (width {width})",
                        width - 1
                    ));
                }
                return Ok(t);
            };
            if !t.neg {
                if n.bit_len() > width {
                    return Err(format!(
                        "{shown} does not fit signal {path} (width {width}); the range is {}",
                        unsigned_range(width)
                    ));
                }
                return Ok(t);
            }
            // -n fits when n <= 2^(width-1).
            let fits = n.bit_len() < width || (n.bit_len() == width && n.is_power_of_two());
            if !fits {
                return Err(format!(
                    "{shown} does not fit signal {path} (width {width}) as a two's-complement value; the signed range is {}",
                    signed_range(width)
                ));
            }
            t.int = Some(n.negate_in(width));
            t.neg = false;
            Ok(t)
        }
        ValueKind::Str => Ok(Target { int: None, neg: false, real: None, ..t }),
        // A zero-width logic signal has no bits to hold a two's complement.
        _ if t.neg => Err(format!(
            "negative target {shown} needs a signal with a width; {path} has none"
        )),
        _ => Ok(t),
    }
}

fn unsigned_range(width: u32) -> String {
    let signed = if width > 1 { format!(" (or {} as two's complement)", neg_range(width)) } else { String::new() };
    if width < 128 {
        format!("0..{}{signed}", (1u128 << width) - 1)
    } else {
        format!("0..2^{width}-1{signed}")
    }
}

fn neg_range(width: u32) -> String {
    if width <= 127 {
        format!("-{}..-1", 1u128 << (width - 1))
    } else {
        format!("-2^{}..-1", width - 1)
    }
}

fn signed_range(width: u32) -> String {
    if width <= 127 {
        let h = 1u128 << (width - 1);
        format!("-{h}..{}", h - 1)
    } else {
        format!("-2^{0}..2^{0}-1", width - 1)
    }
}

fn parse_binary_body(body: &str, text: &str) -> Result<Target, ValueParseError> {
    if body.is_empty() {
        return Err(ValueParseError("binary target must contain at least one bit".into()));
    }
    if body.len() > MAX_SIGNAL_WIDTH {
        return Err(ValueParseError(format!(
            "binary target too wide; max bits is {MAX_SIGNAL_WIDTH}"
        )));
    }
    if let Some(n) = BigUint::from_binary(body) {
        // Keep the base: `raw` is what the de-dup key reads, and a bare body
        // would fold `b1010` (10) into the decimal `1010`. `b` and `0b` are
        // one base, so both spell it `b`.
        Ok(Target::int(format!("b{body}"), n, false))
    } else if is_mask_bits(body) {
        Ok(Target::bits(body.to_string()))
    } else {
        Err(ValueParseError(format!(
            "invalid binary target {}; expected only 0/1/x/z, or ? for a don't-care bit", crate::format::pyrepr(text)
        )))
    }
}

/// A 4-state bit string that may also hold `?` don't-care bits.
fn is_mask_bits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| matches!(b, b'0' | b'1' | b'x' | b'z' | b'?'))
}

/// Parse a comma-separated condition list into [`ParsedCondition`]s.
pub fn parse_conditions(text: &str) -> Result<Vec<ParsedCondition>, ConditionParseError> {
    if text.trim().is_empty() {
        return Err(ConditionParseError("search requires --condition".into()));
    }
    let mut out = Vec::new();
    for item in text.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if let Some(term) = parse_changed_term(item)? {
            out.push(term);
            continue;
        }
        let (sig, op, val) = split_condition(item)
            .ok_or_else(|| ConditionParseError(format!(
                "invalid condition {}; expected SIG=VAL, SIG==VAL, SIG!=VAL, or changed(SIG)", crate::format::pyrepr(item)
            )))?;
        if sig.is_empty() || val.is_empty() {
            return Err(ConditionParseError(format!(
                "invalid empty signal/value in condition {}", crate::format::pyrepr(item)
            )));
        }
        let target = parse_target_value(val)
            .map_err(|e| ConditionParseError(e.0))?;
        out.push(ParsedCondition {
            pattern: sig.to_string(),
            term: TermBody::Level { op, target, value_text: val.to_string() },
            original: item.to_string(),
        });
    }
    if out.is_empty() {
        return Err(ConditionParseError("search requires at least one condition".into()));
    }
    Ok(out)
}

/// Recognize the `changed(SIG)` edge-predicate form. Any term starting with
/// `changed(` (case-insensitive) is claimed by this syntax. A bare term (no
/// operator) was never valid before, so no working bare condition changes
/// meaning; the one shadowed spelling is a `changed(...)=VAL` level term on an
/// escaped identifier that itself begins with `changed(`, which stays
/// reachable through a scope-qualified pattern (`tb.changed(a)=1` does not
/// start with the prefix). Returns `Ok(None)` when the term does not start
/// with the prefix; malformed `changed(...` shapes get targeted errors.
fn parse_changed_term(item: &str) -> Result<Option<ParsedCondition>, ConditionParseError> {
    const PREFIX_LEN: usize = "changed(".len();
    if item.len() < PREFIX_LEN || !item.as_bytes()[..PREFIX_LEN].eq_ignore_ascii_case(b"changed(") {
        return Ok(None);
    }
    let body = &item[PREFIX_LEN..];
    if let Some(inner) = body.strip_suffix(')') {
        let inner = inner.trim();
        if inner.is_empty() {
            return Err(ConditionParseError(
                "changed() requires a signal, e.g. changed(req)".into(),
            ));
        }
        return Ok(Some(ParsedCondition {
            pattern: inner.to_string(),
            term: TermBody::Changed,
            original: item.to_string(),
        }));
    }
    if body.contains(')') {
        // e.g. `changed(req)=1` — trailing text after the closing paren.
        return Err(ConditionParseError(format!(
            "invalid term {}: changed(SIG) is a predicate and takes no comparison", crate::format::pyrepr(item)
        )));
    }
    // No closing paren — a plain missing `)`, or `changed(a,b)` cut apart by
    // the AND comma.
    Err(ConditionParseError(format!(
        "invalid term {}: unclosed changed( — missing ')'? note changed() takes exactly one signal: a comma inside the parens is read as an AND separator, so \"both changed\" is changed(a),changed(b)", crate::format::pyrepr(item)
    )))
}

/// Split `SIG op VAL`, finding the operator. Order matters: check `==`/`!=`
/// before `=`. Returns `(sig, op, val)` trimmed.
fn split_condition(item: &str) -> Option<(&str, Op, &str)> {
    // Find the first operator occurrence. `!=` and `==` are two chars; `=` one.
    // We scan for `!=` or `==` first, then a lone `=`.
    if let Some(pos) = item.find("!=") {
        let sig = item[..pos].trim();
        let val = item[pos + 2..].trim();
        return Some((sig, Op::Ne, val));
    }
    if let Some(pos) = item.find("==") {
        let sig = item[..pos].trim();
        let val = item[pos + 2..].trim();
        return Some((sig, Op::EqEq, val));
    }
    if let Some(pos) = item.find('=') {
        let sig = item[..pos].trim();
        let val = item[pos + 1..].trim();
        return Some((sig, Op::Eq, val));
    }
    None
}

/// Does a recorded value (canonical raw string + kind) match a target?
///
/// `value_bits` is the MSB-first bit string for a logic value, or `None` if the
/// value is not a logic vector (real/string). `width` is the signal width for
/// width-aware 4-state extension.
pub fn value_matches(value_bits: Option<&str>, raw_value: &str, target: &Target, width: u32) -> bool {
    if let Some(tr) = target.real {
        // Resolved to a real signal: numeric equality on the recorded value.
        return raw_value.parse::<f64>().is_ok_and(|v| v == tr);
    }
    if let Some(ti) = &target.int {
        // Numeric target: compare by integer equality. Only logic vectors with
        // no x/z can be numeric.
        match value_bits {
            Some(bits) if is_clean_binary(bits) => {
                match BigUint::from_binary(&normalize_4state(bits)) {
                    Some(v) => &v == ti,
                    None => false,
                }
            }
            _ => false,
        }
    } else {
        // 4-state bit-pattern target; a plain literal is a mask with no `?`.
        match value_bits {
            Some(bits) => pattern_compare(bits, &target.raw, width).equal,
            None => raw_value == target.raw,
        }
    }
}

/// Evaluate one condition against a recorded value.
pub fn condition_match(
    value_bits: Option<&str>,
    raw_value: Option<&str>,
    op: Op,
    target: &Target,
    width: u32,
) -> bool {
    let raw = match raw_value {
        Some(r) => r,
        None => return false, // undefined never matches
    };
    match op {
        Op::Eq | Op::EqEq => value_matches(value_bits, raw, target, width),
        Op::Ne => {
            // x/z values do NOT satisfy != (truly-unknown bits are not evidence
            // of inequality). Weak-strength `h`/`l` *are* defined (1/0) and so
            // are not "unknown" here. Non-logic signals (real/string/event)
            // fall through to the literal-compare path inside value_matches.
            if let (None, Some(bits)) = (&target.int, value_bits) {
                // Bit-pattern target: only the cared bits count. An x under a
                // `?` is not evidence either way, so it does not block `!=`.
                let m = pattern_compare(bits, &target.raw, width);
                return !m.cared_unknown && !m.equal;
            }
            if has_unknown(value_bits) {
                return false;
            }
            !value_matches(value_bits, raw, target, width)
        }
    }
}

/// Outcome of comparing a logic value against a bit-pattern target.
struct PatternCmp {
    /// Every cared bit equals the value's bit (an x cared bit matches only x).
    equal: bool,
    /// Some cared bit of the value is x/z.
    cared_unknown: bool,
}

/// Compare a value's bits with a pattern's cared (non-`?`) bits, LSB-aligned.
/// A plain 4-state literal is a pattern with every bit cared, so equality and
/// the `!=` unknown rule share this one comparison.
///
/// Both sides are left-extended to `width` by the VCD rule first (an x/z MSB
/// pads with itself, anything else — `?` included — with `0`), so `b1??` on 8
/// bits still requires the top five bits to be 0; write every bit, e.g.
/// `b?????1??`, to leave them free. A pattern bit above the width meets a value
/// bit that does not exist and reads as 0: a `0` or `?` there constrains
/// nothing, while a `1`/`x`/`z` never matches (and `!=` then holds once the
/// cared in-width bits are known). A value longer than its declared width is
/// malformed and reads as all-x, as `fmt_bits` displays it. Allocation-free:
/// it runs once per signal per tick in interval mode.
fn pattern_compare(value_bits: &str, pattern: &str, width: u32) -> PatternCmp {
    let v = value_bits.as_bytes();
    let p = pattern.as_bytes();
    let width = width as usize;
    let over_long = v.len() > width;
    let v_pad = match v.first().map(|b| norm_bit(*b)) {
        Some(c @ (b'x' | b'z')) => c,
        _ => b'0',
    };
    let p_pad = match p.first() {
        Some(c @ (b'x' | b'z')) => *c,
        _ => b'0',
    };
    let mut equal = true;
    let mut cared_unknown = false;
    for off in 0..width.max(p.len()) {
        let pb = if off < p.len() { p[p.len() - 1 - off] } else { p_pad };
        if pb == b'?' {
            continue;
        }
        let vb = if off >= width {
            b'0'
        } else if over_long {
            b'x'
        } else if off < v.len() {
            norm_bit(v[v.len() - 1 - off])
        } else {
            v_pad
        };
        if vb == b'x' || vb == b'z' {
            cared_unknown = true;
        }
        if vb != pb {
            equal = false;
        }
    }
    PatternCmp { equal, cared_unknown }
}

/// One value bit in the 4-state alphabet: h/l are weak 1/0, and any other
/// non-0/1/z character (the 9-state u/w/-, or anything malformed) is x.
fn norm_bit(b: u8) -> u8 {
    match b.to_ascii_lowercase() {
        b'0' | b'l' => b'0',
        b'1' | b'h' => b'1',
        b'z' => b'z',
        _ => b'x',
    }
}

/// Are any bits "unknown" in the strict sense — `x`/`z` or any other
/// non-binary, non-strength character? Logic values containing only
/// `0`/`1`/`h`/`l` are considered well-defined (h/l map to 1/0 elsewhere).
/// Non-bit values (real/string/event, value_bits=None) are NOT unknown — the
/// raw value is fully defined, just not a logic vector.
fn has_unknown(value_bits: Option<&str>) -> bool {
    match value_bits {
        None => false,
        Some(b) => b.chars().any(|c| {
            let c = c.to_ascii_lowercase();
            !matches!(c, '0' | '1' | 'h' | 'l')
        }),
    }
}

fn is_clean_binary(bits: &str) -> bool {
    !bits.is_empty()
        && bits.chars().all(|c| {
            let c = c.to_ascii_lowercase();
            matches!(c, '0' | '1' | 'h' | 'l')
        })
}

/// Normalize a 9-state bit string to the 4-state alphabet used by matching.
fn normalize_4state(bits: &str) -> String {
    bits.bytes().map(|b| norm_bit(b) as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_decimal_hex_bin() {
        assert!(parse_target_value("5").unwrap().int.is_some());
        assert!(parse_target_value("0xff").unwrap().int.is_some());
        assert!(parse_target_value("b1010").unwrap().int.is_some());
        assert!(parse_target_value("0b1010").unwrap().int.is_some());
        // 4-state literal
        let t = parse_target_value("b1x0z").unwrap();
        assert!(t.int.is_none());
        assert_eq!(t.raw, "1x0z");
    }

    #[test]
    fn numeric_matches_value() {
        let t = parse_target_value("5").unwrap();
        // 3-bit 101 = 5
        assert!(value_matches(Some("101"), "101", &t, 3));
        // 8-bit short form still equals 5
        assert!(value_matches(Some("00000101"), "00000101", &t, 8));
        assert!(!value_matches(Some("100"), "100", &t, 3));
    }

    #[test]
    fn numeric_does_not_collide_with_bits() {
        // target 10 (decimal) must NOT match a 2-bit value "10" (=2).
        let t = parse_target_value("10").unwrap();
        assert!(!value_matches(Some("10"), "10", &t, 2));
        // but 4-bit 1010 = 10 matches
        assert!(value_matches(Some("1010"), "1010", &t, 4));
    }

    #[test]
    fn four_state_pattern() {
        let t = parse_target_value("b1x0").unwrap();
        // width 4: target extends to 01x0? no: msb is '1' -> pad with 0 -> 01x0
        assert!(value_matches(Some("01x0"), "01x0", &t, 4));
        assert!(!value_matches(Some("0100"), "0100", &t, 4));
    }

    #[test]
    fn ne_does_not_match_unknown() {
        let t = parse_target_value("0").unwrap();
        // value x, op != 0 -> false (unknown is not evidence of difference)
        assert!(!condition_match(Some("x"), Some("x"), Op::Ne, &t, 1));
        // value 1, != 0 -> true
        assert!(condition_match(Some("1"), Some("1"), Op::Ne, &t, 1));
    }

    #[test]
    fn eq_x() {
        let t = parse_target_value("x").unwrap();
        assert!(condition_match(Some("x"), Some("x"), Op::Eq, &t, 1));
    }

    #[test]
    fn condition_list() {
        let conds = parse_conditions("valid=1,ready=1").unwrap();
        assert_eq!(conds.len(), 2);
        assert_eq!(conds[0].pattern, "valid");
        assert!(matches!(conds[0].term, TermBody::Level { op: Op::Eq, .. }));
    }

    #[test]
    fn changed_term_parses() {
        let conds = parse_conditions("changed(req),ready=0").unwrap();
        assert_eq!(conds.len(), 2);
        assert_eq!(conds[0].pattern, "req");
        assert!(matches!(conds[0].term, TermBody::Changed));
        assert_eq!(conds[0].original, "changed(req)");
        assert!(matches!(conds[1].term, TermBody::Level { op: Op::Eq, .. }));
        // Case-insensitive prefix; inner whitespace trimmed.
        let conds = parse_conditions("Changed( top.req )").unwrap();
        assert_eq!(conds[0].pattern, "top.req");
        assert!(matches!(conds[0].term, TermBody::Changed));
    }

    #[test]
    fn changed_term_errors_are_targeted() {
        // Empty signal.
        let e = parse_conditions("changed()").unwrap_err();
        assert!(e.0.contains("requires a signal"), "{}", e.0);
        // Comma inside the parens is cut by the AND split; the orphan
        // `changed(a` gets the one-signal guidance (parse short-circuits, so
        // the stray `b)` never produces a second confusing error).
        let e = parse_conditions("changed(a,b)").unwrap_err();
        assert!(e.0.contains("exactly one signal"), "{}", e.0);
        // Trailing comparison after the closing paren.
        let e = parse_conditions("changed(req)=1").unwrap_err();
        assert!(e.0.contains("takes no comparison"), "{}", e.0);
        // The generic error now advertises the form.
        let e = parse_conditions("req").unwrap_err();
        assert!(e.0.contains("changed(SIG)"), "{}", e.0);
    }

    #[test]
    fn ne_with_weak_strength_chars_h_and_l() {
        // h/l are well-defined logic levels (h=1, l=0), so a value carrying
        // them should NOT be treated as "unknown" for the != path.
        let t = parse_target_value("0").unwrap();   // numeric 0
        // "1h" normalizes to "11" = 3; 3 != 0, so this should be true.
        assert!(condition_match(Some("1h"), Some("1h"), Op::Ne, &t, 2),
            "1h != 0 should be true (h is a defined 1)");
        // "0l" normalizes to "00" = 0; 0 != 0 is false (they are equal).
        assert!(!condition_match(Some("0l"), Some("0l"), Op::Ne, &t, 2));
        // A genuine unknown still poisons the path.
        assert!(!condition_match(Some("1x"), Some("1x"), Op::Ne, &t, 2));
    }

    #[test]
    fn ne_with_no_raw_value_returns_false() {
        // condition_match's early return for raw_value == None must continue
        // to win over the has_unknown(None)==false change: a signal with no
        // recorded value at all should not match any condition (Eq or Ne).
        let t = parse_target_value("0").unwrap();
        assert!(!condition_match(None, None, Op::Eq, &t, 1));
        assert!(!condition_match(None, None, Op::Ne, &t, 1));
        assert!(!condition_match(Some("0"), None, Op::Eq, &t, 1));
        assert!(!condition_match(Some("0"), None, Op::Ne, &t, 1));
    }

    #[test]
    fn ne_on_non_logic_signals_no_longer_silently_false() {
        // Pre-fix, has_unknown(None) returned true, so Op::Ne against any
        // non-logic signal (real/string/event) silently returned false even
        // when they obviously differ from the target. Post-fix, Eq runs and
        // Ne mirrors its negation. A real signal with value "3.14" against
        // numeric target 0 has no bits → Eq returns false → Ne returns true.
        let nt = parse_target_value("0").unwrap();
        assert!(condition_match(None, Some("3.14"), Op::Ne, &nt, 64),
            "real signal `3.14 != 0` should be true (was silently false)");
        // For a non-logic value vs pattern target, the raw==raw branch decides
        // Eq, and Ne is its negation. Equal raw → Ne false.
        let p = parse_target_value("x").unwrap();   // 4-state literal "x"
        assert!(!condition_match(None, Some("x"), Op::Ne, &p, 1));
    }

    fn mask(text: &str) -> Target {
        let t = parse_target_value(text).unwrap();
        assert!(t.is_mask(), "{text} should parse as a mask");
        t
    }

    #[test]
    fn mask_parses_to_a_raw_target() {
        let t = mask("b1??0");
        assert_eq!(t.raw, "1??0");
        assert!(t.int.is_none());
        assert_eq!(mask("0B?1").raw, "?1");
        // A plain literal is not a mask.
        assert!(!parse_target_value("b1x0z").unwrap().is_mask());
    }

    #[test]
    fn malformed_masks_are_rejected() {
        for (text, msg) in [
            ("1??0", "needs a binary prefix"),
            ("0x?a", "must use binary form"),
            ("b1?2", "don't-care bit"),
        ] {
            let e = parse_target_value(text).unwrap_err();
            assert!(e.0.contains(msg), "{text}: {}", e.0);
        }
    }

    #[test]
    fn mask_compares_only_cared_bits() {
        let t = mask("b?????1??");
        assert!(value_matches(Some("00000100"), "", &t, 8));
        assert!(value_matches(Some("11111111"), "", &t, 8));
        assert!(!value_matches(Some("11111011"), "", &t, 8));
        // A compressed dump (iverilog writes b1xxxx for 0001_xxxx): bit 4 is
        // a known 1 beside the x run, bit 3 is x and so not a 1.
        assert!(value_matches(Some("1xxxx"), "", &mask("b???1????"), 8));
        assert!(!value_matches(Some("1xxxx"), "", &mask("b????1???"), 8));
        assert!(value_matches(Some("1xxxx"), "", &mask("b????x???"), 8));
        // Weak strengths read as their levels.
        assert!(value_matches(Some("h0"), "", &mask("b1?"), 2));
    }

    #[test]
    fn short_mask_pads_with_zero() {
        // b1??? on 8 bits is 0000_1???: the high nibble must be 0.
        let t = mask("b1???");
        assert!(value_matches(Some("1010"), "", &t, 8));
        assert!(!value_matches(Some("10001010"), "", &t, 8));
        // An x MSB pads with x, as for a plain literal.
        assert!(value_matches(Some("xx1"), "", &mask("bx?"), 3));
    }

    #[test]
    fn over_wide_pattern() {
        // `?` or `0` above the width constrains nothing, mask or plain literal.
        assert!(value_matches(Some("10"), "", &mask("b??1?"), 2));
        assert!(value_matches(Some("01"), "", &mask("b0?1"), 2));
        assert!(value_matches(Some("x1"), "", &parse_target_value("b00x1").unwrap(), 2));
        // A 1/x/z bit above the width never matches, so `!=` holds once the
        // cared in-width bits are known.
        let t = mask("b1?1");
        assert!(!value_matches(Some("01"), "", &t, 2));
        assert!(condition_match(Some("01"), Some("01"), Op::Ne, &t, 2));
        let plain = parse_target_value("b1x1").unwrap();
        assert!(!value_matches(Some("01"), "", &plain, 2));
        assert!(condition_match(Some("01"), Some("01"), Op::Ne, &plain, 2));
    }

    #[test]
    fn ne_against_mask_ignores_x_under_dont_care() {
        // Low nibble x, bit 4 (cared) a known 1: equal, so != fails.
        assert!(!condition_match(Some("1xxxx"), Some("1xxxx"), Op::Ne, &mask("b???1????"), 8));
        // Bit 4 a known 1 against a cared 0: != holds despite the x run.
        assert!(condition_match(Some("1xxxx"), Some("1xxxx"), Op::Ne, &mask("b???0????"), 8));
        // An x in a cared bit is not evidence of difference.
        assert!(!condition_match(Some("1xxxx"), Some("1xxxx"), Op::Ne, &mask("b????0???"), 8));
        // Undefined still never matches.
        assert!(!condition_match(None, None, Op::Ne, &mask("b?1"), 2));
    }

    #[test]
    fn over_long_value_reads_as_unknown() {
        // A 3-char value on a 2-bit signal is malformed; it displays as xx.
        let t = parse_target_value("bxx").unwrap();
        assert!(value_matches(Some("101"), "", &t, 2));
        assert!(!condition_match(Some("101"), Some("101"), Op::Ne, &mask("b?1"), 2));
    }

    #[test]
    fn mask_never_matches_a_non_logic_value() {
        let t = mask("b1?");
        // (search rejects a mask on a non-logic signal before it gets here.)
        assert!(!condition_match(None, Some("3.0"), Op::Eq, &t, 64));
    }

    #[test]
    fn dedup_key_keeps_the_base() {
        // b1010 is 10, 1010 is one thousand and ten: never the same term.
        let bin = parse_target_value("b1010").unwrap();
        let dec = parse_target_value("1010").unwrap();
        assert_ne!(bin.dedup_key(), dec.dedup_key());
        // The same base still folds, whichever binary prefix spells it.
        assert_eq!(bin.dedup_key(), parse_target_value("B1010").unwrap().dedup_key());
        assert_eq!(bin.dedup_key(), parse_target_value("0b1010").unwrap().dedup_key());
    }

    fn fit(text: &str, kind: crate::format::ValueKind, width: u32) -> Result<Target, String> {
        resolve_target(parse_target_value(text).unwrap(), kind, width, "top.s", text)
    }

    #[test]
    fn negative_decimal_is_twos_complement_in_the_width() {
        use crate::format::ValueKind::Bits;
        let int = |t: &str, w| fit(t, Bits, w).unwrap().int.unwrap();
        assert_eq!(int("-1", 4), BigUint::from_binary("1111").unwrap());
        assert_eq!(int("-8", 4), BigUint::from_binary("1000").unwrap());
        assert_eq!(int("-1", 1), BigUint::from_binary("1").unwrap());
        assert_eq!(int("-0", 4), BigUint::zero());
        assert_eq!(int("-1", 32), BigUint::from_decimal("4294967295").unwrap());
        assert_eq!(int("-2147483648", 32), BigUint::from_decimal("2147483648").unwrap());
        assert_eq!(int("-1", 65), BigUint::from_binary(&"1".repeat(65)).unwrap());
        assert!(fit("-9", Bits, 4).unwrap_err().contains("signed range is -8..7"));
        assert!(fit("-2", Bits, 1).is_err());
        assert!(fit("-2147483649", Bits, 32).is_err());
    }

    #[test]
    fn a_target_wider_than_the_signal_is_an_error() {
        use crate::format::ValueKind::Bits;
        assert!(fit("15", Bits, 4).is_ok());
        assert!(fit("16", Bits, 4).unwrap_err().contains("range is 0..15 (or -8..-1"));
        assert!(fit("2", Bits, 1).unwrap_err().contains("range is 0..1"));
        assert!(fit("0x1f", Bits, 4).is_err());
        assert!(fit("b10000", Bits, 4).is_err());
        assert!(fit("b00001111", Bits, 4).is_ok());
        assert!(fit("b0?1x0", Bits, 4).is_ok());
        assert!(fit("bx0000", Bits, 4).unwrap_err().contains("above bit 3"));
        assert!(fit("b1?000", Bits, 4).is_err());
        assert!(fit("1.5", Bits, 4).unwrap_err().contains("real number"));
    }

    #[test]
    fn real_signals_compare_numbers() {
        use crate::format::ValueKind::{Event, Real, Str};
        let t = fit("100", Real, 64).unwrap();
        assert!(value_matches(None, "100", &t, 64));
        assert!(!value_matches(None, "4", &t, 64));
        let t = fit("-1.5", Real, 64).unwrap();
        assert!(value_matches(None, "-1.5", &t, 64));
        assert!(condition_match(None, Some("NaN"), Op::Ne, &t, 64));
        assert!(value_matches(None, "0.001", &fit("1e-3", Real, 64).unwrap(), 64));
        for bits in ["b101", "0x5", "1x", "0b11"] {
            assert!(fit(bits, Real, 64).unwrap_err().contains("bit pattern"), "{bits}");
        }
        assert!(fit("1", Event, 0).unwrap_err().contains("changed(top.s)"));
        for text in ["5", "-1", "1.5", "0x5"] {
            let t = fit(text, Str, 0).unwrap();
            assert!(value_matches(None, &text.to_lowercase(), &t, 0), "{text}");
            assert!(!value_matches(None, "6", &t, 0), "{text}");
        }
        assert!(fit("-1", crate::format::ValueKind::Bits, 0).unwrap_err().contains("has none"));
    }

    #[test]
    fn real_literal_grammar() {
        for ok in ["3.14", ".5", "5.", "1e9", "1E-9", "2.5e+3"] {
            assert!(parse_target_value(ok).unwrap().real.is_some(), "{ok}");
        }
        for bad in ["inf", "nan", "1e", "e5", ".", "1.2.3", "1_000.0", "٣.5", "²"] {
            assert!(parse_target_value(bad).is_err(), "{bad}");
        }
        assert!(parse_target_value("-1.5").unwrap().real == Some(-1.5));
        assert!(parse_target_value("--1").is_err());
        assert!(parse_target_value("-b1").is_err());
        assert!(parse_target_value("+1").is_err());
    }

    #[test]
    fn spellings_of_different_classes_never_fold() {
        let k = |t: &str| parse_target_value(t).unwrap().dedup_key();
        assert_ne!(k("1"), k("1.0"));
        assert_ne!(k("-1"), k("1"));
        assert_ne!(k("10"), k("b10"));
    }

    #[test]
    fn bignum_wide() {
        // 64-bit all ones
        let a = BigUint::from_binary(&"1".repeat(64)).unwrap();
        let b = BigUint::from_decimal("18446744073709551615").unwrap();
        assert_eq!(a, b);
        let c = BigUint::from_hex("ffffffffffffffff").unwrap();
        assert_eq!(a, c);
    }
}
