use std::collections::BTreeMap;

use crate::rawlog::{LogFormatParser, ParsedLogRow};

pub struct PlainParser;

impl LogFormatParser for PlainParser {
    fn name(&self) -> &'static str {
        "plain"
    }

    fn parse_line(&self, line: &str) -> Option<ParsedLogRow> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(ParsedLogRow {
            timestamp: None,
            severity: None,
            message: trimmed.to_string(),
            fields: BTreeMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_row_with_message_equal_to_line() {
        let row = PlainParser.parse_line("hello world").unwrap();
        assert_eq!(row.message, "hello world");
        assert!(row.timestamp.is_none());
        assert!(row.severity.is_none());
        assert!(row.fields.is_empty());
    }

    #[test]
    fn returns_none_for_empty_line() {
        assert!(PlainParser.parse_line("").is_none());
    }

    #[test]
    fn returns_none_for_whitespace_only_line() {
        assert!(PlainParser.parse_line("   \t  ").is_none());
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let row = PlainParser.parse_line("  body  ").unwrap();
        assert_eq!(row.message, "body");
    }

    #[test]
    fn preserves_internal_whitespace_and_punctuation() {
        let row = PlainParser
            .parse_line(r#"2025-01-01T12:00:00Z INFO something happened, with detail"#)
            .unwrap();
        assert_eq!(
            row.message,
            r#"2025-01-01T12:00:00Z INFO something happened, with detail"#
        );
    }

    #[test]
    fn name_is_plain() {
        assert_eq!(PlainParser.name(), "plain");
    }
}
