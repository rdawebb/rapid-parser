//! Every message of the Maildir++ fixture, with its folder and flags.

use rapid_parser::read_maildir;

#[test]
fn maildir_records() {
    let mut records = Vec::new();
    read_maildir("tests/fixtures/maildir", |r| {
        records.push(serde_json::to_value(&r)?);
        Ok(())
    })
    .unwrap();
    // Directory order is platform-dependent
    records.sort_by_key(|r| r["message_id"].to_string());
    insta::assert_json_snapshot!(records);
}
