//! Every message of the mbox fixture: `>From ` unquoting and a final message
//! with no trailing newline.

use rapid_parser::read_mbox;

#[test]
fn mbox_records() {
    let mut records = Vec::new();
    read_mbox("tests/fixtures/mbox/sample.mbox", |r| {
        records.push(serde_json::to_value(&r)?);
        Ok(())
    })
    .unwrap();
    insta::assert_json_snapshot!(records);
}
