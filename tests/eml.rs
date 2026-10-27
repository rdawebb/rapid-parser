//! A JSON snapshot of the record of every `.eml` fixture.

use mail_parser::MessageParser;
use rapid_parser::{record, write_ndjson};

#[test]
fn fixture_records() {
    insta::glob!("fixtures/eml/*.eml", |path| {
        let raw = std::fs::read(path).unwrap();
        let msg = MessageParser::default().parse(&raw).unwrap();
        insta::assert_json_snapshot!(record(&msg));
    });
}

#[test]
fn ndjson_is_one_object_per_line() {
    let raw = std::fs::read("tests/fixtures/eml/html-only.eml").unwrap();
    let msg = MessageParser::default().parse(&raw).unwrap();
    let mut out = Vec::new();
    write_ndjson([record(&msg), record(&msg)], &mut out).unwrap();
    let lines: Vec<_> = out.split(|&b| b == b'\n').collect();
    assert_eq!(lines.len(), 3, "two lines and a trailing newline");
    assert!(lines[2].is_empty());
    for line in &lines[..2] {
        serde_json::from_slice::<serde_json::Value>(line).unwrap();
    }
}
