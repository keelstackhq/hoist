//! Redaction of `hoist secrets exec` output (spec §15.7.1 step 7): every value and its URL-encoded
//! and JSON-escaped forms become `***`, line by line; GitHub gets `::add-mask::` lines.

use hoist::secrets::redact::Redactor;

const V: &str = "hoist-test-value-1/+é?";

fn red(values: &[&str], text: &str) -> String {
    let r = Redactor::new(values.iter().map(|v| v.as_bytes()));
    String::from_utf8(r.line(text.as_bytes())).unwrap()
}

/// `text` redacted line by line, as `secrets exec` reads the child's output.
fn lines(values: &[&str], text: &str) -> String {
    text.split_inclusive('\n').map(|l| red(values, l)).collect()
}

#[test]
fn value_and_its_encoded_forms_become_stars() {
    let forms = [
        V,
        "hoist-test-value-1%2F%2B%C3%A9%3F",
        "hoist-test-value-1%2f%2b%c3%a9%3f",
        "hoist-test-value-1/%2B%C3%A9%3F", // Python `quote`
        "hoist-test-value-1%2F+%c3%A9?",   // mixed spellings
        &["hoist-test-value-1", r"\/+", r"\", "u00e9?"].concat(),
    ];
    for form in forms {
        let line = format!("a {form} b {form}\n");
        assert_eq!(red(&[V], &line), "a *** b ***\n", "{form}");
    }
    assert_eq!(red(&["a b&c"], "q=a+b%26c&x=1"), "q=***&x=1");
    // A value that starts with an escaped character, and one outside the BMP (a surrogate pair).
    let starts = [
        ("/abc-secret", "%2Fabc-secret"),
        (" abc-secret", "+abc-secret"),
        ("\"abc-secret", r#"\"abc-secret"#),
        ("éabc-secret", r"\u00E9abc-secret"),
        ("key-\u{1F511}-1", r"key-\ud83d\udd11-1"),
        ("\u{8}abcd", r"\babcd"),
    ];
    for (value, form) in starts {
        assert_eq!(red(&[value], &format!("x={form}")), "x=***", "{form}");
    }
    // Both readings of `%25` and `\\` stay possible: escaped, or written as is.
    for v in [r"p%25w\\x", r"ab%25cd\ef"] {
        assert_eq!(red(&[v], &format!("<{v}>")), "<***>", "{v}");
    }
    assert_eq!(red(&["a\u{8}b\u{c}c"], r"x=a\bb\fc"), "x=***");
    assert_eq!(red(&["abcd%"], "abcd%25 x"), "*** x");
    let json = r#"{"K":"pa\"ss\\w0rd\n2nd"}"#;
    assert_eq!(red(&["pa\"ss\\w0rd\n2nd"], json), r#"{"K":"***"}"#);
    let plain = "nothing here 100% + more\n";
    assert_eq!(red(&[V], plain), plain);
}

/// A 64 KiB PEM value against a log of `-` lines stays fast: a part that stops matching is
/// dropped at once (milliseconds; walking the whole value at each `-` took seconds).
#[test]
fn a_mismatch_stops_the_search_at_once() {
    let body: String = (0..1000u32).map(|i| format!("MII{i:061x}\n")).collect();
    let pem = format!("-----BEGIN KEY-----\n{body}-----END KEY-----\n"); // 64 KiB
    let r = Redactor::new([pem.as_bytes()]);
    let log = format!("{}\n", "-".repeat(80)).repeat(100);
    let started = std::time::Instant::now();
    assert_eq!(r.line(log.as_bytes()), log.as_bytes());
    assert!(started.elapsed().as_secs() < 3, "{:?}", started.elapsed());
}

#[test]
fn multi_line_values_are_redacted_line_by_line() {
    let pem = "-----BEGIN KEY-----\nMIIBhoistline2\nab\n-----END KEY-----\n";
    // `ab` is shorter than 4 bytes: too short to redact without wrecking the log.
    assert_eq!(lines(&[pem], pem), "***\n***\nab\n***\n");
    // A value ending in a line break (a file) never swallows the output's line break.
    assert_eq!(red(&["line-one\n"], "line-one\nx"), "***\nx");
    assert_eq!(red(&["line-one\r\nline-two"], "line-one\n"), "***\n");
    assert_eq!(red(&["abcd\r"], "abcd\n"), "***\n");
}

#[test]
fn overlapping_and_short_values() {
    assert_eq!(red(&["abcd", "cdef"], "xabcdefx"), "x***x");
    assert_eq!(red(&["abcd"], "abcdabcd"), "***");
    assert_eq!(red(&["abc", ""], "abc"), "abc");
    let r = Redactor::new([b"\xffsecret".as_slice()]);
    assert_eq!(r.line(b"a\xffsecret\xfe"), b"a***\xfe");
}

#[test]
fn github_masks_one_line_per_value_line_escaped() {
    let values = [
        b"a%bc\r\ncdef\nxy\ngh ij".as_slice(),
        b"ab",
        b"\xffsecret",
        V.as_bytes(),
    ];
    let masks: Vec<String> = Redactor::masks(values)
        .iter()
        .map(|m| m.to_string())
        .collect();
    let want = ["a%25bc", "cdef", "gh ij", V].map(|m| format!("::add-mask::{m}"));
    assert_eq!(masks, want);
}

#[test]
fn debug_never_shows_a_value() {
    let shown = format!("{:?}", Redactor::new([V.as_bytes()]));
    assert!(!shown.contains("test-value"), "{shown}");
}
