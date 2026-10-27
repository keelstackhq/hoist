//! Redaction for `hoist secrets exec` (spec §15.7.1 step 7). Every value of at least 4 bytes and
//! each line of a multi-line value become `***` in each line of the child's output:
//! as written, URL-encoded or JSON-escaped in any spelling. Shorter values are left alone: they
//! would wreck the log.

use zeroize::Zeroizing;

/// Values shorter than this are not redacted or masked.
const MIN: usize = 4;
/// At most this many readings of a value are followed at once (see `spelled_len`).
const READINGS: usize = 32;
/// Matching steps allowed per byte of a line; a line that needs more becomes `***`.
const BUDGET: usize = 64;
/// A longer line becomes `***` without being checked.
const MAX_LINE: usize = 1 << 20;

/// What to replace; zeroized on drop.
pub struct Redactor {
    /// Values and their lines, sorted, matched in any spelling (`spelled_len`).
    parts: Vec<Zeroizing<Vec<u8>>>,
    /// Bytes each part shares with the next one, to skip parts that would fail alike.
    shared: Vec<usize>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Redactor({} parts)", self.parts.len())
    }
}

/// The pieces of `value` between line breaks (`\n` or `\r`) and, when it has any, `value` itself;
/// each at least `MIN` bytes.
fn parts(value: &[u8]) -> Vec<&[u8]> {
    let pieces: Vec<&[u8]> = value.split(|b| matches!(b, b'\n' | b'\r')).collect();
    let whole = (pieces.len() > 1).then_some(value);
    pieces
        .into_iter()
        .chain(whole)
        .filter(|p| p.len() >= MIN)
        .collect()
}

fn hex(h: u8, l: u8) -> Option<u32> {
    let digit = |c: u8| char::from(c).to_digit(16);
    Some(digit(h)? * 16 + digit(l)?)
}

/// Length of `unit` (one byte of a value) URL-encoded at the start of `text`: `%hh` in any case
/// or `+` for a space.
fn url_len(text: &[u8], unit: &[u8]) -> Option<usize> {
    match (text, unit) {
        ([b'+', ..], b" ") => Some(1),
        ([b'%', h, l, ..], [b]) => (hex(*h, *l)? == u32::from(*b)).then_some(3),
        _ => None,
    }
}

/// Length of `unit` (one character of a value) JSON-escaped at the start of `text`: `\"`, `\\`,
/// `\/`, `\n`, `\r`, `\t`, `\b`, `\f` or `\uXXXX` (a surrogate pair outside the BMP).
fn json_len(text: &[u8], unit: &[u8]) -> Option<usize> {
    let c = std::str::from_utf8(unit).ok()?.chars().next()?;
    let short = match c {
        '"' | '\\' | '/' => Some(c as u8),
        '\n' => Some(b'n'),
        '\r' => Some(b'r'),
        '\t' => Some(b't'),
        '\u{8}' => Some(b'b'),
        '\u{c}' => Some(b'f'),
        _ => None,
    };
    if short.is_some() && text.get(..2) == Some(&[b'\\', short?]) {
        return Some(2);
    }
    let mut utf16 = [0u16; 2];
    c.encode_utf16(&mut utf16).iter().try_fold(0, |at, &u| {
        let [b'\\', b'u', a, b, c, d] = *text.get(at..at + 6)? else {
            return None;
        };
        (hex(a, b)? * 256 + hex(c, d)? == u32::from(u)).then_some(at + 6)
    })
}

/// Length of `part` at the start of `text`, unit by unit (a byte when `url`, else a character):
/// each URL-encoded (`url`) or JSON-escaped, or else as itself (never a line break). One encoding
/// per pass keeps the reading unambiguous (`\\` in JSON is one backslash), except where a unit
/// can be read two ways (`%25`, `\\`): then every reading is followed (`spelled_set`). Each step
/// costs one unit of `budget`; `None` when it runs out.
fn spelled_len(text: &[u8], part: &[u8], url: bool, budget: &mut usize) -> Result<usize, usize> {
    // `seen`: part bytes the outcome so far depends on, for `Err` (see `covered`).
    let (mut at, mut i, mut seen) = (0, 0, 0);
    while i < part.len() {
        *budget = budget.checked_sub(1).ok_or(0usize)?;
        let n = unit_len(&part[i..], url);
        seen = seen.max(looked(&part[i..], i, n, url));
        let ends = text.get(at..).map(|t| ends(t, &part[i..i + n], url));
        match ends.unwrap_or_default() {
            [Some(a), Some(b)] if a != b => {
                let first = [at + a, at + b];
                return spelled_set(text, first, part, (i + n, seen), url, budget);
            }
            [Some(e), _] | [None, Some(e)] => at += e,
            [None, None] => return Err(seen),
        }
        i += n;
    }
    Ok(at)
}

/// Bytes of `rest` from which a unit split starting `i` bytes into the part was decided: an
/// invalid UTF-8 byte (a one-byte unit in JSON mode) was decided by up to 3 bytes after it.
fn looked(rest: &[u8], i: usize, n: usize, url: bool) -> usize {
    match !url && rest[0] >= 0x80 && n == 1 {
        true => i + 4,
        false => i + n,
    }
}

/// Bytes in the next unit of `rest`: one when `url` or ASCII, else a whole UTF-8 character.
fn unit_len(rest: &[u8], url: bool) -> usize {
    match rest[0] {
        b if url || b < 0x80 => 1,
        _ => (2..=4.min(rest.len()))
            .find(|&n| std::str::from_utf8(&rest[..n]).is_ok())
            .unwrap_or(1),
    }
}

/// Where `unit` can end at the start of `text`: escaped, and as itself (never a line break).
fn ends(text: &[u8], unit: &[u8], url: bool) -> [Option<usize>; 2] {
    let escaped = if url {
        url_len(text, unit)
    } else {
        json_len(text, unit)
    };
    let raw = !matches!(unit, b"\n" | b"\r") && text.starts_with(unit);
    [escaped, raw.then_some(unit.len())]
}

/// `spelled_len` from several readings at once: every position a reading can reach, at most
/// `READINGS` of them (a run of `\\` in a value doubles them).
fn spelled_set(
    text: &[u8],
    first: [usize; 2],
    part: &[u8],
    (mut i, mut seen): (usize, usize),
    url: bool,
    budget: &mut usize,
) -> Result<usize, usize> {
    // Two buffers on the stack, swapped each unit: this runs inside the per-byte loop.
    let mut bufs = [[0usize; READINGS]; 2];
    let (mut cur, mut len) = (0, 2);
    bufs[0][..2].copy_from_slice(&first);
    while i < part.len() {
        let n = unit_len(&part[i..], url);
        seen = seen.max(looked(&part[i..], i, n, url));
        let (ats, next) = match &mut bufs {
            [a, b] if cur == 0 => (&*a, b),
            [a, b] => (&*b, a),
        };
        let mut count = 0;
        for &at in &ats[..len] {
            *budget = budget.checked_sub(1).ok_or(0usize)?;
            let Some(text) = text.get(at..) else { continue };
            for end in ends(text, &part[i..i + n], url).into_iter().flatten() {
                if count < READINGS && !next[..count].contains(&(at + end)) {
                    (next[count], count) = (at + end, count + 1);
                }
            }
        }
        if count == 0 {
            return Err(seen);
        }
        (cur, len, i) = (1 - cur, count, i + n);
    }
    Ok(bufs[cur][..len].iter().copied().max().unwrap_or(0))
}

/// The first byte of what a URL or JSON escape at the start of `text` decodes to, if any. A
/// `\uXXXX` high surrogate fixes the first UTF-8 byte of its character on its own.
fn decoded_first(text: &[u8]) -> Option<u8> {
    match text {
        [b'%', h, l, ..] => hex(*h, *l).map(|b| b as u8),
        [b'+', ..] => Some(b' '),
        [b'\\', b'u', a, b, c, d, ..] => {
            let unit = hex(*a, *b)? * 256 + hex(*c, *d)?;
            let code = match unit {
                0xD800..=0xDBFF => 0x10000 + ((unit - 0xD800) << 10),
                _ => unit,
            };
            char::from_u32(code).map(|c| c.encode_utf8(&mut [0; 4]).as_bytes()[0])
        }
        [b'\\', b'n', ..] => Some(b'\n'),
        [b'\\', b'r', ..] => Some(b'\r'),
        [b'\\', b't', ..] => Some(b'\t'),
        [b'\\', b'b', ..] => Some(8),
        [b'\\', b'f', ..] => Some(12),
        [b'\\', c, ..] => Some(*c),
        _ => None,
    }
}

impl Redactor {
    pub fn new<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Redactor {
        let mut parts: Vec<_> = values.into_iter().flat_map(parts).collect();
        parts.sort();
        parts.dedup(); // a PEM bundle repeats its BEGIN and END lines
        let common = |(a, b): (&[u8], &[u8])| a.iter().zip(b).take_while(|(x, y)| x == y).count();
        let shared = parts.windows(2).map(|w| common((w[0], w[1]))).collect();
        let parts = parts
            .into_iter()
            .map(|p| Zeroizing::new(p.to_vec()))
            .collect();
        Redactor { parts, shared }
    }

    /// Which bytes of `text` belong to a value in some form; `None` when the work runs ahead of
    /// `BUDGET` steps per byte scanned (plus 4096 to start, with at most 64 KiB worth saved), so a
    /// costly stretch anywhere stops within a few million steps.
    fn covered(&self, text: &[u8]) -> Option<Vec<bool>> {
        let mut covered = vec![false; text.len()];
        let mut budget = 4096;
        // The parts that start with byte `b`, as a range of `self.parts`.
        let with = |b: u8| {
            let lo = self.parts.partition_point(|p| p[0] < b);
            lo..self.parts.partition_point(|p| p[0] <= b)
        };
        for (start, key) in text.windows(MIN).enumerate() {
            budget = (budget + BUDGET).min(BUDGET * 65_536);
            // Only parts that start with this byte or with what an escape here decodes to.
            let decoded = decoded_first(&text[start..]).filter(|d| *d != key[0]);
            for range in [Some(with(key[0])), decoded.map(with)]
                .into_iter()
                .flatten()
            {
                for url in [true, false] {
                    // A part that fails after reading `need` bytes (with the bytes that decided
                    // how a character splits) fails the same way for every next part sharing
                    // them: skip those.
                    let (mut need, mut run) = (usize::MAX, usize::MAX);
                    for g in range.clone() {
                        run = run.min(g.checked_sub(1).map_or(0, |p| self.shared[p]));
                        if need != usize::MAX && run >= need {
                            continue;
                        }
                        match spelled_len(&text[start..], &self.parts[g], url, &mut budget) {
                            Ok(len) => {
                                covered[start..start + len].fill(true);
                                need = usize::MAX;
                            }
                            Err(_) if budget == 0 => return None,
                            Err(k) => (need, run) = (k, usize::MAX),
                        }
                    }
                }
            }
        }
        Some(covered)
    }

    /// `line` with every run of covered bytes replaced by one `***`. A line over `MAX_LINE`
    /// bytes, or too costly to check, fails closed: all of it but its line break becomes `***`.
    pub fn line(&self, line: &[u8]) -> Vec<u8> {
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        let body = body.strip_suffix(b"\r").unwrap_or(body);
        let covered = if body.len() <= MAX_LINE {
            self.covered(line)
        } else {
            None
        };
        match covered {
            Some(covered) => render(line, &covered),
            None => [b"***", &line[body.len()..]].concat(),
        }
    }

    /// One GitHub `::add-mask::` command per single-line value and per line of a multi-line one,
    /// with `%` escaped. A value that is not UTF-8 is redacted but cannot be masked.
    pub fn masks<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Vec<Zeroizing<String>> {
        let lines = values.into_iter().flat_map(parts);
        let lines = lines.filter(|p| !p.iter().any(|b| matches!(b, b'\n' | b'\r')));
        let text = lines.filter_map(|p| std::str::from_utf8(p).ok());
        let mask = |t: &str| {
            let mut line = Zeroizing::new(String::with_capacity(12 + 3 * t.len()));
            line.push_str("::add-mask::");
            t.chars().for_each(|c| match c {
                '%' => line.push_str("%25"),
                c => line.push(c),
            });
            line
        };
        text.map(mask).collect()
    }
}

/// `text` with each run of covered bytes replaced by `***`.
fn render(text: &[u8], covered: &[bool]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for (i, &b) in text.iter().enumerate() {
        match (covered[i], i > 0 && covered[i - 1]) {
            (false, _) => out.push(b),
            (true, false) => out.extend_from_slice(b"***"),
            (true, true) => {}
        }
    }
    out
}
