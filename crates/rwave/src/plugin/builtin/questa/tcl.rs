// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! The two pieces of Tcl this module cannot avoid: quoting a word on the way in,
//! and splitting a list on the way out.
//!
//! Quoting is a correctness requirement, not tidiness. A signal named
//! `res[7:0]` written bare into a Tcl command is *command substitution* — Tcl
//! runs `7:0` as a command — and vector names are the common case, not the
//! exotic one.

use super::err;

/// Quote `s` so Tcl passes it through as exactly one literal word.
///
/// Braces are preferred because they suppress every kind of substitution at
/// once; a word already containing a brace or backslash cannot use them, so it
/// falls back to escaping each metacharacter. A newline or NUL is rejected
/// rather than escaped: the wire protocol is line-based, and a name carrying a
/// newline would break framing no matter how it were quoted.
pub fn quote_word(s: &str) -> Result<String, String> {
    if s.contains(['\n', '\r', '\0']) {
        return Err(err(format!(
            "'{}' contains a newline or NUL, which cannot be sent to vsim",
            s.escape_debug()
        )));
    }
    if s.is_empty() {
        return Ok("{}".to_string());
    }
    if !s.contains(['{', '}', '\\']) {
        return Ok(format!("{{{s}}}"));
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(
            c,
            '{' | '}' | '[' | ']' | '$' | '"' | '\\' | ';' | ' ' | '\t'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    Ok(out)
}

/// Split one Tcl list into its elements.
///
/// Brace-nesting and quote aware, and performs no substitution — this reads
/// data that Questa produced, so running anything in it would be a bug. Never
/// panics: unbalanced input yields whatever was parsed up to that point, which
/// the caller rejects structurally rather than trusting.
pub fn split_list(s: &str) -> Vec<String> {
    let ch: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < ch.len() {
        while i < ch.len() && ch[i].is_whitespace() {
            i += 1;
        }
        if i >= ch.len() {
            break;
        }
        let mut item = String::new();
        match ch[i] {
            '{' => {
                let mut depth = 0usize;
                while i < ch.len() {
                    let c = ch[i];
                    if c == '{' {
                        depth += 1;
                        i += 1;
                        if depth > 1 {
                            item.push(c);
                        }
                        continue;
                    }
                    if c == '}' {
                        depth -= 1;
                        i += 1;
                        if depth == 0 {
                            break;
                        }
                        item.push(c);
                        continue;
                    }
                    item.push(c);
                    i += 1;
                }
            }
            '"' => {
                i += 1;
                while i < ch.len() && ch[i] != '"' {
                    if ch[i] == '\\' && i + 1 < ch.len() {
                        i += 1;
                    }
                    item.push(ch[i]);
                    i += 1;
                }
                if i < ch.len() {
                    i += 1;
                }
            }
            _ => {
                while i < ch.len() && !ch[i].is_whitespace() {
                    if ch[i] == '\\' && i + 1 < ch.len() {
                        i += 1;
                    }
                    item.push(ch[i]);
                    i += 1;
                }
            }
        }
        out.push(item);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vector_name_is_braced_so_tcl_cannot_substitute_it() {
        // Unquoted, `[7:0]` is command substitution and Tcl runs `7:0`.
        assert_eq!(quote_word("res[7:0]").unwrap(), "{res[7:0]}");
        assert_eq!(quote_word("/top/u/res").unwrap(), "{/top/u/res}");
        assert_eq!(quote_word("$x").unwrap(), "{$x}");
        assert_eq!(quote_word("").unwrap(), "{}");
    }

    #[test]
    fn a_name_holding_a_brace_falls_back_to_escaping() {
        assert_eq!(quote_word("a{b").unwrap(), "a\\{b");
        assert_eq!(quote_word("a}b").unwrap(), "a\\}b");
        // An escaped Verilog identifier reaches us with its backslash intact.
        assert_eq!(quote_word("\\foo.bar").unwrap(), "\\\\foo.bar");
    }

    #[test]
    fn a_newline_is_refused_rather_than_escaped() {
        let e = quote_word("a\nb").unwrap_err();
        assert!(e.contains("newline"), "{e}");
        assert!(quote_word("a\0b").is_err());
    }

    #[test]
    fn splits_the_row_shape_questa_actually_emits() {
        // Verbatim from `find drivers -possible -tcl` on Questa 10.7c.
        let rows = split_list("{Gate /top/u_core/u_alu {res_q[7:0]} dut.sv:4}");
        assert_eq!(rows, vec!["Gate /top/u_core/u_alu {res_q[7:0]} dut.sv:4"]);
        let fields = split_list(&rows[0]);
        assert_eq!(fields, vec!["Gate", "/top/u_core/u_alu", "res_q[7:0]", "dut.sv:4"]);
    }

    #[test]
    fn several_drivers_arrive_as_one_line_of_braced_rows() {
        let rows = split_list("{TRI /top <???> top.sv:13} {TRI /top <???> top.sv:14}");
        assert_eq!(rows.len(), 2);
        assert_eq!(split_list(&rows[1])[3], "top.sv:14");
    }

    #[test]
    fn handles_the_two_deep_braces_of_the_time_aware_shape() {
        let rows = split_list("{{55 ns} FF /top {res[7:0]} top.sv:9}");
        assert_eq!(rows.len(), 1);
        let fields = split_list(&rows[0]);
        assert_eq!(fields, vec!["55 ns", "FF", "/top", "res[7:0]", "top.sv:9"]);
    }

    #[test]
    fn unbalanced_input_yields_a_value_instead_of_panicking() {
        assert_eq!(split_list("{a b"), vec!["a b"]);
        assert_eq!(split_list(""), Vec::<String>::new());
        assert_eq!(split_list("   "), Vec::<String>::new());
        assert_eq!(split_list("\"a b\" c"), vec!["a b", "c"]);
    }
}
