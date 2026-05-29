// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Search condition parsing and value matching, mirroring `vcd_analyzer.py`.
//!
//! A condition list is comma-separated AND terms. Each term is
//! `SIG=VAL`, `SIG==VAL`, or `SIG!=VAL`. Values may be decimal (`5`), hex
//! (`0xff`), binary (`b1010`, `0b1010`), 4-state (`b1x0z`), or a bare 4-state
//! literal (`1x0`). Numeric targets match by numeric equality; 4-state targets
//! match as (width-aware) bit patterns. `!=` does **not** match x/z/undefined.

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
    /// For non-numeric 4-state targets: the raw bit string (e.g. `1x0`).
    /// For numeric targets: the lower-cased original (informational).
    pub raw: String,
    /// `Some(n)` for numeric targets matched by integer equality; `None` for
    /// 4-state bit-pattern targets.
    pub int: Option<BigUint>,
}

/// A parsed condition term prior to resolving the signal pattern.
#[derive(Debug, Clone)]
pub struct ParsedCondition {
    pub pattern: String,
    pub op: Op,
    pub target: Target,
    /// Original `SIG op VAL` text (for labels).
    pub original: String,
    /// The value text as written (for the resolved label).
    pub value_text: String,
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

/// Parse a target value string into [`Target`], matching
/// `vcd_analyzer.py::_parse_target_value`.
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
    if raw.starts_with('-') {
        return Err(ValueParseError(
            "negative target values are not supported for waveform matching".into(),
        ));
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
            Some(n) => Ok(Target { raw: raw.clone(), int: Some(n) }),
            None => Err(ValueParseError(format!(
                "invalid hex target {}; x/z literals must use binary form like b1x0z", crate::format::pyrepr(text)
            ))),
        };
    }
    if let Some(body) = raw.strip_prefix("0b") {
        return parse_binary_body(body, &raw, text);
    }
    if let Some(body) = raw.strip_prefix('b') {
        return parse_binary_body(body, &raw, text);
    }
    if raw.starts_with('+') {
        return Err(ValueParseError(
            "signed target values are not supported; write unsigned values".into(),
        ));
    }
    // Bare: decimal if all digits, else 4-state literal.
    if raw.bytes().all(|b| b.is_ascii_digit()) {
        if raw.len() > MAX_DECIMAL_VALUE_DIGITS {
            return Err(ValueParseError(format!(
                "decimal target too long; max digits is {MAX_DECIMAL_VALUE_DIGITS}"
            )));
        }
        let n = BigUint::from_decimal(&raw).unwrap();
        return Ok(Target { raw: raw.clone(), int: Some(n) });
    }
    // Not decimal: must be a 4-state literal.
    if raw.len() > MAX_SIGNAL_WIDTH {
        return Err(ValueParseError(
            "literal target too wide; max characters is MAX_SIGNAL_WIDTH".into(),
        ));
    }
    if raw.bytes().all(|b| matches!(b, b'0' | b'1' | b'x' | b'z')) {
        Ok(Target { raw, int: None })
    } else {
        Err(ValueParseError(format!(
            "invalid target {}; expected decimal, 0x.., b.., or 0/1/x/z literal", crate::format::pyrepr(text)
        )))
    }
}

fn parse_binary_body(body: &str, raw: &str, text: &str) -> Result<Target, ValueParseError> {
    if body.is_empty() {
        return Err(ValueParseError("binary target must contain at least one bit".into()));
    }
    if body.len() > MAX_SIGNAL_WIDTH {
        return Err(ValueParseError(format!(
            "binary target too wide; max bits is {MAX_SIGNAL_WIDTH}"
        )));
    }
    if let Some(n) = BigUint::from_binary(body) {
        Ok(Target { raw: body.to_string(), int: Some(n) })
    } else if body.bytes().all(|b| matches!(b, b'0' | b'1' | b'x' | b'z')) {
        Ok(Target { raw: body.to_string(), int: None })
    } else {
        let _ = raw;
        Err(ValueParseError(format!(
            "invalid binary target {}; expected only 0/1/x/z", crate::format::pyrepr(text)
        )))
    }
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
        let (sig, op, val) = split_condition(item)
            .ok_or_else(|| ConditionParseError(format!(
                "invalid condition {}; expected SIG=VAL, SIG==VAL, or SIG!=VAL", crate::format::pyrepr(item)
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
            op,
            target,
            original: item.to_string(),
            value_text: val.to_string(),
        });
    }
    if out.is_empty() {
        return Err(ConditionParseError("search requires at least one condition".into()));
    }
    Ok(out)
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
        // 4-state bit-pattern target.
        match value_bits {
            Some(bits) => {
                let vb = normalize_4state(bits);
                let tb = &target.raw;
                if (tb.len() as u32) > width {
                    return false;
                }
                left_extend(&vb, width) == left_extend(tb, width)
            }
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
            // x/z/undef do NOT satisfy !=.
            if has_unknown(value_bits) {
                return false;
            }
            !value_matches(value_bits, raw, target, width)
        }
    }
}

fn has_unknown(value_bits: Option<&str>) -> bool {
    match value_bits {
        None => true,
        Some(b) => b.chars().any(|c| {
            let c = c.to_ascii_lowercase();
            !matches!(c, '0' | '1')
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
    bits.chars()
        .map(|c| match c.to_ascii_lowercase() {
            '0' => '0',
            '1' => '1',
            'z' => 'z',
            'h' => '1',
            'l' => '0',
            'x' => 'x',
            _ => 'x',
        })
        .collect()
}

/// Left-extend a 4-state bit string to `width` per VCD rules (x->x, z->z,
/// else 0). If already >= width, returned unchanged.
fn left_extend(bits: &str, width: u32) -> String {
    let width = width as usize;
    if bits.len() >= width {
        return bits.to_string();
    }
    let msb = bits.chars().next().unwrap_or('0');
    let pad = if msb == 'x' || msb == 'z' { msb } else { '0' };
    let mut s = String::with_capacity(width);
    for _ in 0..(width - bits.len()) {
        s.push(pad);
    }
    s.push_str(bits);
    s
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
        assert_eq!(conds[0].op, Op::Eq);
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
