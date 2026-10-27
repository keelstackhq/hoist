//! Redaction for `hoist secrets exec` (spec §15.7.1 step 7). Every value of at least 4 bytes and
//! each line of a multi-line value become `***` in each line of the child's output:
//! as written, URL-encoded or JSON-escaped in any spelling. Shorter values are left alone: they
//! would wreck the log.

use zeroize::Zeroizing;

/// Values shorter than this are not redacted or masked.
const MIN: usize = 4;
/// At most this many readings of a value are followed at once (see `spelled_len`).
const READINGS: usize = 32;

/// What to replace; zeroized on drop.
pub struct Redactor {
    /// Values and their lines, sorted, matched in any spelling (`spelled_len`).
    parts: Vec<Zeroizing<Vec<u8>>>,
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
/// per pass keeps the reading unambiguous (`\\` in JSON is one backslash).
fn spelled_len(text: &[u8], part: &[u8], url: bool) -> Option<usize> {
    // Every position a reading can reach: `%25` may be an escaped `%` or a literal one. Kept on
    // the stack: this runs for each part at many start positions.
    let (mut ats, mut len, mut rest) = ([0usize; READINGS], 1, part);
    while let Some(&first) = rest.first() {
        let n = match first {
            _ if url || first < 0x80 => 1,
            _ => (2..=4.min(rest.len()))
                .find(|&n| std::str::from_utf8(&rest[..n]).is_ok())
                .unwrap_or(1),
        };
        let unit = &rest[..n];
        let (mut next, mut count) = ([0usize; READINGS], 0);
        for &at in &ats[..len] {
            let Some(text) = text.get(at..) else { continue };
            let escaped = if url {
                url_len(text, unit)
            } else {
                json_len(text, unit)
            };
            let raw = !matches!(unit, b"\n" | b"\r") && text.starts_with(unit);
            for end in [escaped, raw.then_some(n)]
                .into_iter()
                .flatten()
                .map(|e| at + e)
            {
                // A run of `\` in a value doubles the readings: keep the first `READINGS`.
                if count < READINGS && !next[..count].contains(&end) {
                    (next[count], count) = (end, count + 1);
                }
            }
        }
        if count == 0 {
            return None;
        }
        (ats, len, rest) = (next, count, &rest[n..]);
    }
    ats[..len].iter().max().copied()
}

/// The byte a URL or JSON escape at the start of `text` decodes to, if any; `None` after `\u`,
/// which may begin any character.
fn decoded_first(text: &[u8]) -> Option<Option<u8>> {
    Some(match text {
        [b'%', h, l, ..] => hex(*h, *l).map(|b| b as u8),
        [b'+', ..] => Some(b' '),
        [b'\\', b'u', ..] => return None,
        [b'\\', b'n', ..] => Some(b'\n'),
        [b'\\', b'r', ..] => Some(b'\r'),
        [b'\\', b't', ..] => Some(b'\t'),
        [b'\\', b'b', ..] => Some(8),
        [b'\\', b'f', ..] => Some(12),
        [b'\\', c, ..] => Some(*c),
        _ => None,
    })
}

impl Redactor {
    pub fn new<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Redactor {
        let mut parts: Vec<_> = values.into_iter().flat_map(parts).collect();
        parts.sort();
        let parts = parts
            .into_iter()
            .map(|p| Zeroizing::new(p.to_vec()))
            .collect();
        Redactor { parts }
    }

    /// Which bytes of `text` belong to a value in some form.
    fn covered(&self, text: &[u8]) -> Vec<bool> {
        let mut covered = vec![false; text.len()];
        for (start, key) in text.windows(MIN).enumerate() {
            // Only parts that start with this byte or with what an escape here decodes to.
            let with = |b: u8| {
                let lo = self.parts.partition_point(|p| p[0] < b);
                &self.parts[lo..self.parts.partition_point(|p| p[0] <= b)]
            };
            let (parts, decoded) = match decoded_first(&text[start..]) {
                None => (&self.parts[..], &[][..]),
                Some(d) => (
                    with(key[0]),
                    d.filter(|d| *d != key[0]).map_or(&[][..], with),
                ),
            };
            let spelled = parts.iter().chain(decoded);
            let spelled = spelled.flat_map(|p| [(p, true), (p, false)]);
            let spelled = spelled.filter_map(|(p, url)| spelled_len(&text[start..], p, url));
            if let Some(len) = spelled.max() {
                covered[start..start + len].fill(true);
            }
        }
        covered
    }

    /// `line` with every run of covered bytes replaced by one `***`.
    pub fn line(&self, line: &[u8]) -> Vec<u8> {
        render(line, &self.covered(line))
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
