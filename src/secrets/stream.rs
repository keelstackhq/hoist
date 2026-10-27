//! Streaming redaction for `hoist secrets exec` (spec §15.7.1 step 7): the child's output is cut
//! into lines at `\n` or `\r` (a progress bar shows at once), and each whole line goes through
//! `Redactor::line`, then is written and flushed.

use super::redact::Redactor;
use std::io::{self, BufRead, Write};
use zeroize::{Zeroize, Zeroizing};

/// Zeroes the bytes of `line` and empties it, keeping its capacity. Only bytes up to the length
/// were ever written; `Zeroizing` wipes the whole buffer once, when it is dropped.
fn wipe(line: &mut Zeroizing<Vec<u8>>) {
    line.as_mut_slice().zeroize();
    line.clear();
}

/// The longest line held: 1 MiB and its break. `Redactor::line` fails a longer one closed, so it is
/// written as `***` without being held.
const CAP: usize = (1 << 20) + 1;

/// Copies `from` to `to` line by line, redacted. Output with no break (a `Password: ` prompt) is
/// held until a break or the end, so a child waiting for input looks silent; a stream with no
/// break at all is consumed in bounded memory and shows only `***` at its end. A read or write
/// error is returned and the held line dropped; the caller then kills the child, which would
/// otherwise block on a full pipe, so no output passes unredacted.
pub fn copy(redactor: &Redactor, mut from: impl BufRead, mut to: impl Write) -> io::Result<()> {
    let mut line = Zeroizing::new(Vec::with_capacity(CAP));
    // `over`: the line passed `CAP`; it is written as `***` and the rest of it dropped.
    let mut over = false;
    loop {
        let chunk = match from.fill_buf() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            chunk => chunk?,
        };
        if chunk.is_empty() {
            break;
        }
        let end = chunk.iter().position(|b| matches!(b, b'\n' | b'\r'));
        let n = end.map_or(chunk.len(), |i| i + 1);
        if !over && line.len() + n > CAP {
            wipe(&mut line);
            over = true;
        }
        if !over {
            line.extend_from_slice(&chunk[..n]);
        }
        let brk = end.map(|i| chunk[i]).filter(|_| over);
        let ended = end.is_some();
        from.consume(n);
        if ended {
            write_line(redactor, &mut line, over, brk, &mut to)?;
            over = false;
        }
    }
    if over || !line.is_empty() {
        write_line(redactor, &mut line, over, None, &mut to)?;
    }
    Ok(())
}

/// Writes `line` redacted, or `***` and its break `brk` for a line over `CAP`; then forgets it.
fn write_line(
    redactor: &Redactor,
    line: &mut Zeroizing<Vec<u8>>,
    over: bool,
    brk: Option<u8>,
    to: &mut impl Write,
) -> io::Result<()> {
    let out = match over {
        true => [b"***".as_slice(), brk.as_slice()].concat(),
        false => redactor.line(line),
    };
    wipe(line);
    to.write_all(&out)?;
    to.flush()
}
