//! Runs over every `.eml` fixture. 1.1 replaces the check with a JSON snapshot of each record.

#[test]
fn fixtures_have_headers_and_body() {
    insta::glob!("fixtures/eml/*.eml", |path| {
        let raw = std::fs::read(path).unwrap();
        let split =
            raw.windows(4).any(|w| w == b"\r\n\r\n") || raw.windows(2).any(|w| w == b"\n\n");
        assert!(split, "no blank line between headers and body");
    });
}
