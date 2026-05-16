pub mod nginx;
pub mod plain;

use super::LogFormatParser;

pub fn all_parsers() -> Vec<Box<dyn LogFormatParser>> {
    vec![
        Box::new(plain::PlainParser),
        Box::new(nginx::NginxParser::default()),
    ]
}
