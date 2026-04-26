//! Pre-parse env-var interpolation over raw scenario YAML.
//!
//! Syntax:
//! - `${VAR}` — required; errors if unset.
//! - `${VAR:-default}` — optional; default is the literal text up to the next `}`.
//! - `$$` — literal `$` (the only escape).
//!
//! Variable names must match `[A-Z_][A-Z0-9_]*`.

use std::env;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum InterpolateError {
    #[error(
        "environment variable {name} is not set (referenced at line {line}, column {column} of scenario YAML)"
    )]
    UnsetVariable {
        name: String,
        line: usize,
        column: usize,
    },

    #[error(
        "unterminated `${{...}}` reference (started at line {line}, column {column} of scenario YAML)"
    )]
    Unterminated { line: usize, column: usize },

    #[error(
        "invalid environment variable name {name:?} (at line {line}, column {column} of scenario YAML): names must match [A-Z_][A-Z0-9_]*"
    )]
    InvalidName {
        name: String,
        line: usize,
        column: usize,
    },
}

/// Expand `${VAR}` / `${VAR:-default}` references against the process environment.
pub fn interpolate(input: &str) -> Result<String, InterpolateError> {
    interpolate_with(input, |name| env::var(name).ok())
}

/// Variant of [`interpolate`] with a caller-supplied lookup. Test seam.
pub fn interpolate_with<F>(input: &str, mut lookup: F) -> Result<String, InterpolateError>
where
    F: FnMut(&str) -> Option<String>,
{
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut line = 1usize;
    let mut column = 1usize;

    while i < bytes.len() {
        let b = bytes[i];

        if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            out.push('$');
            i += 2;
            column += 2;
            continue;
        }

        if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let ref_line = line;
            let ref_column = column;
            let close = match bytes[i + 2..].iter().position(|&c| c == b'}') {
                Some(off) => i + 2 + off,
                None => {
                    return Err(InterpolateError::Unterminated {
                        line: ref_line,
                        column: ref_column,
                    });
                }
            };
            let body = &input[i + 2..close];
            let (name, default) = match body.find(":-") {
                Some(sep) => (&body[..sep], Some(&body[sep + 2..])),
                None => (body, None),
            };
            validate_name(name, ref_line, ref_column)?;
            match lookup(name) {
                Some(value) => out.push_str(&value),
                None => match default {
                    Some(d) => out.push_str(d),
                    None => {
                        return Err(InterpolateError::UnsetVariable {
                            name: name.to_string(),
                            line: ref_line,
                            column: ref_column,
                        });
                    }
                },
            }
            advance_position(&bytes[i..=close], &mut line, &mut column);
            i = close + 1;
            continue;
        }

        // Step over the source char (1-4 bytes for UTF-8) without splitting.
        let char_len = utf8_char_len(b);
        let end = (i + char_len).min(bytes.len());
        out.push_str(&input[i..end]);
        if b == b'\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
        i = end;
    }

    Ok(out)
}

#[inline]
fn utf8_char_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn advance_position(consumed: &[u8], line: &mut usize, column: &mut usize) {
    for &c in consumed {
        if c == b'\n' {
            *line += 1;
            *column = 1;
        } else {
            *column += 1;
        }
    }
}

fn validate_name(name: &str, line: usize, column: usize) -> Result<(), InterpolateError> {
    let mut chars = name.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => {
            return Err(InterpolateError::InvalidName {
                name: name.to_string(),
                line,
                column,
            });
        }
    };
    if !(first.is_ascii_uppercase() || first == '_') {
        return Err(InterpolateError::InvalidName {
            name: name.to_string(),
            line,
            column,
        });
    }
    for c in chars {
        if !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
            return Err(InterpolateError::InvalidName {
                name: name.to_string(),
                line,
                column,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_from(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn interp(input: &str, pairs: &[(&str, &str)]) -> Result<String, InterpolateError> {
        let map = lookup_from(pairs);
        interpolate_with(input, |name| map.get(name).cloned())
    }

    #[test]
    fn empty_input_returns_empty_output() {
        let out = interp("", &[]).unwrap();
        assert_eq!(out, "");
    }

    #[test]
    fn input_without_references_is_unchanged() {
        let yaml = "version: 2\nscenarios: []\n";
        let out = interp(yaml, &[]).unwrap();
        assert_eq!(out, yaml);
    }

    #[test]
    fn required_var_set_is_substituted() {
        let out = interp("url: ${HOST}", &[("HOST", "example.com")]).unwrap();
        assert_eq!(out, "url: example.com");
    }

    #[test]
    fn required_var_unset_returns_error_with_position() {
        let err = interp("a: b\nurl: ${HOST}", &[]).unwrap_err();
        assert_eq!(
            err,
            InterpolateError::UnsetVariable {
                name: "HOST".into(),
                line: 2,
                column: 6,
            }
        );
    }

    #[test]
    fn optional_var_set_uses_value_not_default() {
        let out = interp(
            "url: ${HOST:-fallback.com}",
            &[("HOST", "real.example.com")],
        )
        .unwrap();
        assert_eq!(out, "url: real.example.com");
    }

    #[test]
    fn optional_var_unset_uses_default() {
        let out = interp("url: ${HOST:-fallback.com}", &[]).unwrap();
        assert_eq!(out, "url: fallback.com");
    }

    #[test]
    fn empty_default_is_allowed() {
        let out = interp("prefix${VAR:-}suffix", &[]).unwrap();
        assert_eq!(out, "prefixsuffix");
    }

    #[test]
    fn default_may_contain_colons_slashes_and_query_strings() {
        let out = interp(
            "url: ${URL:-http://localhost:8428/api/v1/import?foo=bar&baz=qux}",
            &[],
        )
        .unwrap();
        assert_eq!(
            out,
            "url: http://localhost:8428/api/v1/import?foo=bar&baz=qux"
        );
    }

    #[test]
    fn multiple_vars_in_one_string_all_substituted() {
        let out = interp(
            "${SCHEME}://${HOST}:${PORT}/path",
            &[
                ("SCHEME", "https"),
                ("HOST", "api.example.com"),
                ("PORT", "443"),
            ],
        )
        .unwrap();
        assert_eq!(out, "https://api.example.com:443/path");
    }

    #[test]
    fn dollar_dollar_escape_produces_literal_dollar() {
        let out = interp("price: $$5.00", &[]).unwrap();
        assert_eq!(out, "price: $5.00");
    }

    #[test]
    fn dollar_dollar_does_not_consume_following_brace() {
        let out = interp("$${VAR}", &[("VAR", "ignored")]).unwrap();
        assert_eq!(out, "${VAR}");
    }

    #[test]
    fn lone_dollar_at_eof_passes_through() {
        let out = interp("trailing $", &[]).unwrap();
        assert_eq!(out, "trailing $");
    }

    #[test]
    fn dollar_followed_by_non_special_char_passes_through() {
        let out = interp("price: $5", &[]).unwrap();
        assert_eq!(out, "price: $5");
    }

    #[test]
    fn malformed_unterminated_reference_returns_error() {
        let err = interp("url: ${HOST", &[]).unwrap_err();
        assert_eq!(err, InterpolateError::Unterminated { line: 1, column: 6 });
    }

    #[test]
    fn malformed_empty_name_returns_error() {
        let err = interp("url: ${}", &[]).unwrap_err();
        assert_eq!(
            err,
            InterpolateError::InvalidName {
                name: "".into(),
                line: 1,
                column: 6,
            }
        );
    }

    #[test]
    fn lowercase_name_returns_invalid_name_error() {
        let err = interp("url: ${host}", &[]).unwrap_err();
        assert_eq!(
            err,
            InterpolateError::InvalidName {
                name: "host".into(),
                line: 1,
                column: 6,
            }
        );
    }

    #[test]
    fn name_with_invalid_chars_returns_error() {
        let err = interp("url: ${HOST-NAME}", &[]).unwrap_err();
        assert_eq!(
            err,
            InterpolateError::InvalidName {
                name: "HOST-NAME".into(),
                line: 1,
                column: 6,
            }
        );
    }

    #[test]
    fn name_starting_with_digit_returns_error() {
        let err = interp("url: ${1HOST}", &[]).unwrap_err();
        assert_eq!(
            err,
            InterpolateError::InvalidName {
                name: "1HOST".into(),
                line: 1,
                column: 6,
            }
        );
    }

    #[test]
    fn name_starting_with_underscore_is_valid() {
        let out = interp("v: ${_PRIVATE}", &[("_PRIVATE", "ok")]).unwrap();
        assert_eq!(out, "v: ok");
    }

    #[test]
    fn name_with_digits_after_first_char_is_valid() {
        let out = interp("v: ${HOST1}", &[("HOST1", "h1")]).unwrap();
        assert_eq!(out, "v: h1");
    }

    #[test]
    fn multi_line_input_reports_correct_line_for_unset_var() {
        let yaml = "version: 2\ndefaults:\n  sink:\n    url: ${MISSING}\n";
        let err = interp(yaml, &[]).unwrap_err();
        match err {
            InterpolateError::UnsetVariable { name, line, column } => {
                assert_eq!(name, "MISSING");
                assert_eq!(line, 4);
                assert_eq!(column, 10);
            }
            other => panic!("expected UnsetVariable, got {other:?}"),
        }
    }

    #[test]
    fn substitution_is_single_pass_not_recursive() {
        let out = interp(
            "a: ${OUTER}",
            &[("OUTER", "${INNER}"), ("INNER", "should-not-appear")],
        )
        .unwrap();
        assert_eq!(out, "a: ${INNER}");
    }

    #[test]
    fn position_tracks_source_columns_past_a_substitution() {
        let err = interp("${A}${B}", &[("A", "value-of-a")]).unwrap_err();
        match err {
            InterpolateError::UnsetVariable { name, line, column } => {
                assert_eq!(name, "B");
                assert_eq!(line, 1);
                assert_eq!(column, 5);
            }
            other => panic!("expected UnsetVariable for B, got {other:?}"),
        }
    }

    #[test]
    fn long_default_with_special_chars() {
        let out = interp(
            "${KAFKA:-broker-1.example.com:9094,broker-2.example.com:9094}",
            &[],
        )
        .unwrap();
        assert_eq!(out, "broker-1.example.com:9094,broker-2.example.com:9094");
    }

    #[test]
    fn nested_dollar_brace_inside_default_terminates_at_first_close() {
        // `${A:-${B}` — default scans to the first `}`, yielding `${B`.
        let out = interp("${A:-${B}", &[]).unwrap();
        assert_eq!(out, "${B");
    }

    #[test]
    fn non_ascii_input_passes_through_unchanged() {
        let yaml = "name: caf\u{00e9}-\u{1f600}-\u{4e2d}\nurl: ${HOST}";
        let out = interp(yaml, &[("HOST", "x")]).unwrap();
        assert_eq!(out, "name: caf\u{00e9}-\u{1f600}-\u{4e2d}\nurl: x");
    }

    #[test]
    fn error_type_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InterpolateError>();
    }

    #[test]
    fn interpolate_uses_process_env() {
        let var = "SONDA_INTERPOLATE_TEST_VAR_2026";
        // SAFETY: unique-to-this-test var name; mutates process-global state.
        unsafe {
            env::set_var(var, "from-env");
        }
        let out = interpolate(&format!("v: ${{{var}}}")).unwrap();
        assert_eq!(out, "v: from-env");
        // SAFETY: see above.
        unsafe {
            env::remove_var(var);
        }
    }
}
