// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! A tiny, dependency-free JSON value model and serializer.
//!
//! Output is byte-compatible with Python's
//! `json.dumps(obj, ensure_ascii=False, separators=(',', ':'))`, which is what
//! the reference `vcd_analyzer.py` uses for its `--json` mode:
//!   * no spaces after `:` or `,`
//!   * non-ASCII characters emitted literally (UTF-8), not `\uXXXX`-escaped
//!   * object key order is insertion order (we use a `Vec` of pairs)
//!   * integers render without a decimal point; floats use Rust's shortest
//!     round-trip representation, which is sufficient for the few float fields
//!     we emit (none, in practice — all numbers here are integers or strings).

use std::fmt::Write as _;

/// A JSON value. Object member order is preserved (insertion order).
#[derive(Debug, Clone)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn str<S: Into<String>>(s: S) -> Json {
        Json::Str(s.into())
    }

    /// Build a `Str` from an `Option<&str>`, mapping `None` to JSON `null`.
    pub fn opt_str(s: Option<&str>) -> Json {
        match s {
            Some(v) => Json::Str(v.to_string()),
            None => Json::Null,
        }
    }

    /// Build an `Int` from an `Option`, mapping `None` to JSON `null`.
    pub fn opt_int(v: Option<i64>) -> Json {
        match v {
            Some(v) => Json::Int(v),
            None => Json::Null,
        }
    }

    /// Serialize to a compact string.
    pub fn to_compact_string(&self) -> String {
        let mut out = String::new();
        self.write_compact(&mut out);
        out
    }

    fn write_compact(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Json::Str(s) => write_json_string(s, out),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_compact(out);
                }
                out.push(']');
            }
            Json::Object(members) => {
                out.push('{');
                for (i, (k, v)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(k, out);
                    out.push(':');
                    v.write_compact(out);
                }
                out.push('}');
            }
        }
    }
}

/// Write a JSON string literal, escaping per RFC 8259 while leaving non-ASCII
/// bytes as literal UTF-8 (matching `ensure_ascii=False`).
fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            // Other C0 control characters must be \u-escaped.
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Convenience builder for JSON objects that keeps call sites terse.
pub struct Obj(Vec<(String, Json)>);

impl Obj {
    pub fn new() -> Self {
        Obj(Vec::new())
    }

    pub fn push<K: Into<String>>(mut self, key: K, value: Json) -> Self {
        self.0.push((key.into(), value));
        self
    }

    /// Append all members of another object table (used to merge the shared
    /// `total`/`total_is_exact` fields onto a result object).
    pub fn extend(mut self, members: Vec<(String, Json)>) -> Self {
        self.0.extend(members);
        self
    }

    pub fn build(self) -> Json {
        Json::Object(self.0)
    }
}

impl Default for Obj {
    fn default() -> Self {
        Obj::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_object() {
        let j = Obj::new()
            .push("a", Json::Int(1))
            .push("b", Json::str("x"))
            .push("c", Json::Bool(true))
            .build();
        assert_eq!(j.to_compact_string(), r#"{"a":1,"b":"x","c":true}"#);
    }

    #[test]
    fn nested_array() {
        let j = Json::Array(vec![Json::Int(1), Json::Null, Json::str("z")]);
        assert_eq!(j.to_compact_string(), r#"[1,null,"z"]"#);
    }

    #[test]
    fn string_escapes() {
        let j = Json::str("a\"b\\c\nd");
        assert_eq!(j.to_compact_string(), r#""a\"b\\c\nd""#);
    }

    #[test]
    fn unicode_is_literal() {
        // ensure_ascii=False: emit UTF-8 bytes directly.
        let j = Json::str("ns·µ");
        assert_eq!(j.to_compact_string(), "\"ns·µ\"");
    }
}
