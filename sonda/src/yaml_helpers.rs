//! Shared YAML formatting and quoting utilities.
//!
//! These helpers are used by both `init/yaml_gen` and `import/yaml_gen` to
//! produce syntactically valid, human-readable YAML. Centralising them here
//! avoids duplication and ensures the two code paths apply the same quoting
//! rules.

/// A YAML parameter value that formats appropriately.
///
/// Used by both the `init` and `import` YAML generators to carry typed
/// parameter values through to the rendering stage.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    /// A floating-point number.
    Float(f64),
    /// A quoted string (e.g., a duration like `"10s"`).
    String(String),
}

/// Check if a YAML scalar value needs double-quoting to be parsed correctly.
///
/// Returns `true` for values that a YAML parser would interpret as something
/// other than a plain string: empty strings, numbers, boolean keywords
/// (including YAML 1.1 `on`/`off`), values with leading/trailing whitespace,
/// and values containing characters that are syntactically significant in
/// YAML (`:`  `#`  `{`  `}`  `[`  `]`  `"`  `'`  `\`  newlines).
pub fn needs_quoting(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    if value.parse::<f64>().is_ok() {
        return true;
    }
    // Leading/trailing whitespace changes semantics in YAML flow scalars.
    if value != value.trim() {
        return true;
    }
    let lower = value.to_lowercase();
    // Standard YAML booleans plus YAML 1.1 `on`/`off`.
    if lower == "true"
        || lower == "false"
        || lower == "null"
        || lower == "yes"
        || lower == "no"
        || lower == "on"
        || lower == "off"
    {
        return true;
    }
    if value.contains(':')
        || value.contains('#')
        || value.contains('{')
        || value.contains('}')
        || value.contains('[')
        || value.contains(']')
        || value.contains('"')
        || value.contains('\'')
        || value.contains('\\')
        || value.contains('\n')
    {
        return true;
    }
    false
}

/// Escape a string for use inside a double-quoted YAML scalar.
///
/// Replaces backslashes with `\\` and double quotes with `\"`, which are the
/// two characters that must be escaped inside YAML double-quoted scalars to
/// produce syntactically valid output.
pub fn escape_yaml_double_quoted(s: &str) -> String {
    // Backslash first, so we don't double-escape the backslashes we insert for quotes.
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Format a float nicely: avoid unnecessary trailing zeros.
///
/// Whole-number floats below 10^15 are rendered with a single decimal place
/// (e.g., `50.0`). Fractional values use Rust's default `Display`, which
/// preserves full precision.
pub fn format_float(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{:.1}", v) // e.g., 50.0
    } else {
        format!("{}", v) // full precision
    }
}

/// Format a rate value, using integer form for whole numbers.
///
/// Rates >= 1.0 with no fractional part are rendered without a decimal
/// (e.g., `1`, `10`). Fractional or sub-1 rates use Rust's default `Display`.
pub fn format_rate(rate: f64) -> String {
    if rate == rate.trunc() && rate >= 1.0 {
        format!("{}", rate as u64)
    } else {
        format!("{}", rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // needs_quoting
    // -----------------------------------------------------------------------

    #[test]
    fn needs_quoting_empty_string() {
        assert!(needs_quoting(""));
    }

    #[test]
    fn needs_quoting_numeric_string() {
        assert!(needs_quoting("42"));
        assert!(needs_quoting("3.14"));
    }

    #[test]
    fn needs_quoting_boolean_keywords() {
        assert!(needs_quoting("true"));
        assert!(needs_quoting("false"));
        assert!(needs_quoting("yes"));
        assert!(needs_quoting("no"));
        assert!(needs_quoting("null"));
    }

    #[test]
    fn needs_quoting_colon() {
        assert!(needs_quoting("http://example.com"));
    }

    #[test]
    fn needs_quoting_hash() {
        assert!(needs_quoting("value # comment"));
    }

    #[test]
    fn needs_quoting_braces() {
        assert!(needs_quoting("{key}"));
        assert!(needs_quoting("value}"));
    }

    #[test]
    fn needs_quoting_double_quote() {
        assert!(needs_quoting(r#"say "hello""#));
    }

    #[test]
    fn needs_quoting_backslash() {
        assert!(needs_quoting(r"C:\Users\admin"));
    }

    #[test]
    fn needs_quoting_square_brackets() {
        assert!(needs_quoting("[item]"));
        assert!(needs_quoting("value]"));
    }

    #[test]
    fn needs_quoting_single_quote() {
        assert!(needs_quoting("it's"));
    }

    #[test]
    fn needs_quoting_newline() {
        assert!(needs_quoting("line1\nline2"));
    }

    #[test]
    fn needs_quoting_leading_trailing_whitespace() {
        assert!(needs_quoting(" leading"));
        assert!(needs_quoting("trailing "));
        assert!(needs_quoting("  both  "));
    }

    #[test]
    fn needs_quoting_yaml_11_booleans() {
        assert!(needs_quoting("on"));
        assert!(needs_quoting("off"));
        assert!(needs_quoting("On"));
        assert!(needs_quoting("OFF"));
    }

    #[test]
    fn no_quoting_for_plain_identifiers() {
        assert!(!needs_quoting("web-01"));
        assert!(!needs_quoting("node_exporter"));
        assert!(!needs_quoting("eth0"));
    }

    // -----------------------------------------------------------------------
    // escape_yaml_double_quoted
    // -----------------------------------------------------------------------

    #[test]
    fn escape_no_special_chars() {
        assert_eq!(escape_yaml_double_quoted("hello world"), "hello world");
    }

    #[test]
    fn escape_double_quotes() {
        assert_eq!(
            escape_yaml_double_quoted(r#"say "hello""#),
            r#"say \"hello\""#
        );
    }

    #[test]
    fn escape_backslash() {
        assert_eq!(
            escape_yaml_double_quoted(r"path\to\file"),
            r"path\\to\\file"
        );
    }

    #[test]
    fn escape_both_backslash_and_quotes() {
        assert_eq!(escape_yaml_double_quoted(r#"a\"b"#), r#"a\\\"b"#);
    }

    // -----------------------------------------------------------------------
    // format_float
    // -----------------------------------------------------------------------

    #[test]
    fn format_float_integer_value() {
        assert_eq!(format_float(50.0), "50.0");
    }

    #[test]
    fn format_float_fractional_value() {
        assert_eq!(format_float(3.14159), "3.14159");
    }

    #[test]
    fn format_float_zero() {
        assert_eq!(format_float(0.0), "0.0");
    }

    // -----------------------------------------------------------------------
    // format_rate
    // -----------------------------------------------------------------------

    #[test]
    fn format_rate_whole_number() {
        assert_eq!(format_rate(1.0), "1");
        assert_eq!(format_rate(10.0), "10");
    }

    #[test]
    fn format_rate_fractional() {
        assert_eq!(format_rate(0.5), "0.5");
    }

    #[test]
    fn format_rate_sub_one() {
        assert_eq!(format_rate(0.1), "0.1");
    }
}
