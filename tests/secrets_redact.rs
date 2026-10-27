//! Redaction of `hoist secrets exec` output (spec §15.7.1 step 7): every value, its base64,
//! URL-encoded and JSON-escaped forms become `***`, line by line; GitHub gets `::add-mask::` lines.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
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
        &STANDARD.encode(V),
        &URL_SAFE_NO_PAD.encode(V),
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
    // A value whose base64 has `/` and `+`, which the URL-safe alphabet spells `_` and `-`.
    let w = "?>?>~~~";
    for form in [STANDARD.encode(w), URL_SAFE.encode(w)] {
        assert_eq!(red(&[w], &form), "***", "{form}");
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
        ("\u{1F511}-secret-1", r"\ud83d\udd11-secret-1"),
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

/// Short random values (seeded) and fixed ones, at offsets 0 to 2 in standard and URL-safe base64.
fn values() -> Vec<String> {
    let mut seed = 7u64;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as u8
    };
    let random = (0..300).map(|i| {
        (0..4 + i % 5)
            .map(|_| char::from(b'!' + next() % 94))
            .collect()
    });
    let fixed = [V, "abcd", "?>?>~~~", "~lA>"].map(String::from);
    fixed.into_iter().chain(random).collect()
}

#[test]
fn base64_inside_a_longer_encoding_is_caught_at_every_offset() {
    for v in values() {
        for prefix in ["", "u", "us", "x-access-token:"] {
            for suffix in ["", "!", "!!"] {
                // `Basic base64(user:token)`, and a JWT segment (URL-safe, no padding).
                for engine in [STANDARD, URL_SAFE_NO_PAD] {
                    let text = engine.encode(format!("{prefix}{v}{suffix}"));
                    let out = red(&[&v], &text);
                    // Kept: only characters that hold a bit of the prefix or the suffix.
                    let (p, n) = (prefix.len(), prefix.len() + v.len());
                    let (head, tail) = ((8 * p).div_ceil(6), text.len() - 8 * n / 6);
                    let (a, b) = out
                        .split_once("***")
                        .unwrap_or_else(|| panic!("{v}: {out}"));
                    let kept = text.starts_with(a) && text.ends_with(b) && !b.contains('*');
                    let ok = kept && a.len() <= head && b.len() <= tail;
                    assert!(ok, "{v}|{prefix}|{suffix}: {out}");
                }
            }
        }
    }
}

/// A structured value (indented JSON) gives many base64 forms that share a first 4 bytes; they are
/// scanned like a sorted dictionary, so an unrelated base64 line full of indentation is not
/// failed closed. A periodic value printed in base64 costs its bytes, not bytes per start.
#[test]
fn base64_forms_sharing_a_prefix_stay_cheap() {
    let json: String = (0..2000)
        .map(|i| format!("        \"key{i}\": \"v{i}\",\n"))
        .collect();
    let r = Redactor::new([json.as_bytes()]);
    let unrelated = STANDARD.encode(format!("{}spec:\n", " ".repeat(8)).repeat(500));
    let out = String::from_utf8(r.line(unrelated.as_bytes())).unwrap();
    assert!(
        out.len() > unrelated.len() / 2,
        "{}",
        &out[..out.len().min(80)]
    );
    let periodic = "A".repeat(65_536);
    let r = Redactor::new([periodic.as_bytes()]);
    let size = if cfg!(debug_assertions) {
        100_000
    } else {
        1_000_000
    };
    let text = STANDARD.encode("A".repeat(size * 3 / 4));
    let started = std::time::Instant::now();
    assert_eq!(r.line(text.as_bytes()), b"***");
    assert!(
        started.elapsed().as_millis() < limit(),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn wrapped_base64_of_a_long_value_is_caught_on_every_line() {
    let long: String = (0..200)
        .map(|i| char::from(b'!' + (i * 7 % 90) as u8))
        .collect();
    for prefix in ["", "a", "ab"] {
        let b64 = STANDARD.encode(format!("{prefix}{long}"));
        for width in [76, 64, 60, 20, 16] {
            let wrapped: Vec<&str> = b64
                .as_bytes()
                .chunks(width)
                .map(|c| std::str::from_utf8(c).unwrap())
                .collect();
            let out = lines(&[&long], &(wrapped.join("\n") + "\n"));
            for (i, line) in out.lines().enumerate() {
                let kept = line.split("***").map(str::len).max().unwrap();
                assert!(
                    line.contains("***") && kept <= 4,
                    "{prefix}|{width}|{i}: {line}"
                );
            }
        }
    }
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

/// 100 values, some starting with `é` or `\\`, the shapes a `\\u` escape can begin.
fn many_values() -> Vec<String> {
    let starts = ["a", "é", "\\", "\u{1F511}"];
    (0..100)
        .map(|i| format!("{}secret-{i:03}", starts[i % 4]))
        .collect()
}

/// Time limit in ms: tight in a release build, loose in a debug build, which CI runs next to other
/// tests (a debug build is about 10 times slower).
fn limit() -> u128 {
    if cfg!(debug_assertions) {
        10_000
    } else {
        1_000
    }
}

/// 1 MB of an escape in under 1 s in a release build (100 kB in a debug build). Values that
/// share a prefix fail alike, so 25 values starting with `\\` cost little against a `\\` flood.
#[test]
fn escape_floods_stay_fast() {
    let values = many_values();
    let r = Redactor::new(values.iter().map(|v| v.as_bytes()));
    let size = if cfg!(debug_assertions) {
        100_000
    } else {
        1_000_000
    };
    for unit in [r"\u", r"\u00e9", r"\ud83d", "%2", r"\\"] {
        let text = unit.repeat(size / unit.len());
        let started = std::time::Instant::now();
        let out = r.line(text.as_bytes());
        let elapsed = started.elapsed();
        assert!(
            out == text.as_bytes() && elapsed.as_millis() < limit(),
            "{unit}: {elapsed:?}"
        );
    }
}

/// A PEM bundle (10 certificates, as one value each) leaves separator lines alone: the values
/// share their `-----BEGIN` prefix, so they cost one try, not ten.
#[test]
fn shared_prefixes_are_tried_once() {
    let pems: Vec<String> = (0..10)
        .map(|i| format!("-----BEGIN CERTIFICATE-----\nMII{i:061}\n-----END CERTIFICATE-----\n"))
        .collect();
    let r = Redactor::new(pems.iter().map(|p| p.as_bytes()));
    for line in [
        format!("{}\n", "-".repeat(2000)),
        "---- Captured stdout call ----\n".into(),
    ] {
        assert_eq!(r.line(line.as_bytes()), line.as_bytes());
    }
    assert_eq!(r.line(pems[3].lines().nth(1).unwrap().as_bytes()), b"***");
}

/// A value is skipped only when a neighbour failed within bytes they share, counting the bytes
/// read to split a character: an invalid byte must not hide a neighbour's valid one.
#[test]
fn a_skipped_value_could_not_have_matched() {
    // Each value JSON-escaped, so only the JSON pass, which splits characters, can match it.
    let cases: [(&[&[u8]], &str); 3] = [
        (&[b"\xc3\x28ab", "éxy".as_bytes()], r"k=\u00e9xy!"),
        (&[b"\xe2\x82Aqq", "€zzz".as_bytes()], r"k=\u20aczzz!"),
        (
            &[b"\"\\\\\xe2\x82/b", "\"\\\\€b".as_bytes()],
            r#"k=\"\\\\\u20acb!"#,
        ),
    ];
    for (values, text) in cases {
        assert_eq!(
            Redactor::new(values.iter().copied()).line(text.as_bytes()),
            b"k=***!",
            "{values:?}"
        );
    }
}

/// Too much matching work in one line (a run of `\\` in a value against a run in the output), or
/// a line over 1 MiB, fails closed: the whole line becomes `***`, its line break kept.
#[test]
fn a_line_too_costly_to_check_becomes_stars() {
    let value = "\\".repeat(256);
    let r = Redactor::new([value.as_bytes()]);
    // Costly from the start, and costly after a long cheap stretch (saved credit is capped).
    let tail = "\\".repeat(65_536);
    for line in [
        format!("{tail}visible\n"),
        format!("{}{tail}\n", "x".repeat(900_000)),
    ] {
        let started = std::time::Instant::now();
        assert_eq!(r.line(line.as_bytes()), b"***\n");
        assert!(
            started.elapsed().as_millis() < limit(),
            "{:?}",
            started.elapsed()
        );
    }
    let long = format!("{}\r\n", "x".repeat((1 << 20) + 1));
    assert_eq!(r.line(long.as_bytes()), b"***\r\n");
    let fine = format!("{}\n", "x".repeat(1 << 20));
    assert_eq!(r.line(fine.as_bytes()), fine.as_bytes());
}

#[test]
fn multi_line_values_are_redacted_line_by_line() {
    let pem = "-----BEGIN KEY-----\nMIIBhoistline2\nab\n-----END KEY-----\n";
    // `ab` is shorter than 4 bytes: too short to redact without wrecking the log.
    assert_eq!(lines(&[pem], pem), "***\n***\nab\n***\n");
    assert_eq!(red(&[pem], &STANDARD.encode(pem)), "***");
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
