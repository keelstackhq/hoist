//! Streaming redaction of `hoist secrets exec` output (spec §15.7.1 step 7): the child's output is
//! cut into lines at `\n` or `\r`, and each whole line is redacted and written at once.

use hoist::secrets::redact::Redactor;
use hoist::secrets::stream::copy;
use std::io::{BufReader, Read};

const V: &str = "hoist-test-value-1";

fn run(values: &[&str], input: &[u8]) -> Vec<u8> {
    let r = Redactor::new(values.iter().map(|v| v.as_bytes()));
    let mut out = Vec::new();
    copy(&r, input, &mut out).unwrap();
    out
}

#[test]
fn every_line_is_redacted_and_its_break_kept() {
    let input = format!("one\r\n{V}\r\n50% {V}\rlast {V}");
    assert_eq!(
        run(&[V], input.as_bytes()),
        b"one\r\n***\r\n50% ***\rlast ***"
    );
    assert_eq!(run(&[V], b""), b"");
}

/// Fails after its text (once interrupted, which is retried), so only what was written before
/// that reaches the output.
struct ThenFail(&'static [u8], bool);

impl Read for ThenFail {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.1 {
            self.1 = true;
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        match self.0.read(buf)? {
            0 => Err(std::io::Error::other("closed")),
            n => Ok(n),
        }
    }
}

/// Keeps only what was flushed.
#[derive(Default)]
struct Flushed(Vec<u8>, Vec<u8>);

impl std::io::Write for Flushed {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.1.append(&mut self.0);
        Ok(())
    }
}

#[test]
fn each_line_is_flushed_before_more_output_arrives() {
    let r = Redactor::new([V.as_bytes()]);
    let mut out = Flushed::default();
    // A tiny buffer: a line arrives in many reads and is still redacted whole.
    let from = BufReader::with_capacity(3, ThenFail(b"10%\rhoist-test-value-1\r20", false));
    assert!(copy(&r, from, &mut out).is_err());
    assert_eq!(out.1, b"10%\r***\r");
}

/// A line over 1 MiB is never held: it becomes `***`, the rest up to its break is dropped, and
/// the next line is redacted as usual.
#[test]
fn an_overlong_line_is_written_as_stars() {
    let input = format!("{}{V}{}\nnext {V}\n", "x".repeat(2 << 20), "y".repeat(10));
    let r = Redactor::new([V.as_bytes()]);
    let mut out = Vec::new();
    copy(&r, BufReader::new(input.as_bytes()), &mut out).unwrap();
    assert_eq!(out, b"***\nnext ***\n");
    let last = format!("ok\n{}{V}", "x".repeat(2 << 20)); // the last line, with no break
    assert_eq!(run(&[V], last.as_bytes()), b"ok\n***");
    let fits = format!("{}\n", "x".repeat(1 << 20));
    assert_eq!(run(&[V], fits.as_bytes()), fits.as_bytes());
}

/// Lines at the 1 MiB edge: a body of 1 MiB is redacted as usual, one byte more is `***`, with the
/// break kept (`\n` or `\r`) or none at the end of the output.
#[test]
fn lines_at_the_size_edge() {
    let mib = 1 << 20;
    let at = |n: usize| "x".repeat(n);
    let cases = [
        (format!("{}\n", at(mib)), format!("{}\n", at(mib))),
        (
            format!("{}\rnext {V}\n", at(mib + 1)),
            "***\rnext ***\n".into(),
        ),
        (at(mib), at(mib)),
        (at(mib + 1), "***".into()),
    ];
    for (input, want) in cases {
        let out = run(&[V], input.as_bytes());
        assert!(
            out == want.as_bytes(),
            "{} bytes in, {} out",
            input.len(),
            out.len()
        );
    }
}

/// Many short lines stay fast: only the bytes of each line are wiped, not the whole buffer.
#[test]
fn many_short_lines_stay_fast() {
    let input = "a\n".repeat(100_000);
    let started = std::time::Instant::now();
    assert_eq!(run(&[V], input.as_bytes()), input.as_bytes());
    let limit = if cfg!(debug_assertions) { 10 } else { 1 };
    assert!(
        started.elapsed().as_secs() < limit,
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_write_error_is_returned() {
    struct Closed;
    impl std::io::Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let r = Redactor::new([V.as_bytes()]);
    let err = copy(&r, b"line\n".as_slice(), Closed).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
}
