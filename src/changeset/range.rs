//! Version ranges of internal dependencies (changesets spec §5.2): npm semver and Cargo
//! requirements, the forms workspaces use. `None` means unparseable: no bump, no rewrite.

use crate::release::semver::Version;

const OPS: [&str; 7] = [">=", "<=", ">", "<", "^", "~", "="];

/// Does npm range `range` (`^ ~ >= > <= < =`, exact, `x`/`*`, space-separated lists, `||`,
/// `workspace:`) admit `v`? `old` is the
/// dependency's current version: `workspace:*|^|~` mean `old`, `^old`, `~old` (as changesets).
pub fn npm_satisfies(range: &str, old: &Version, v: &Version) -> Option<bool> {
    let r = range.trim();
    if let Some(rest) = r.strip_prefix("workspace:") {
        return match rest {
            "*" => Some(v == old),
            "^" | "~" => npm_satisfies(&format!("{rest}{old}"), old, v),
            _ => npm_satisfies(rest, old, v),
        };
    }
    if r.is_empty() {
        return Some(true);
    }
    let mut any = false;
    for alt in r.split("||") {
        // `>= 1.2` is one comparator: glue a lone operator to the next word.
        let (mut all, mut glued) = (Some(true), String::new());
        for w in alt.split_whitespace() {
            glued.push_str(w);
            if !OPS.contains(&glued.as_str()) {
                all = all.zip(satisfies(&glued, v, "=")).map(|(a, b)| a && b);
                glued.clear();
            }
        }
        if !glued.is_empty() || alt.trim().is_empty() {
            return None;
        }
        any |= all?;
    }
    Some(any)
}

/// Does Cargo requirement `req` (bare = caret, `^ ~ = >= > <= <`, `*`, comma lists) admit `v`?
pub fn cargo_satisfies(req: &str, v: &Version) -> Option<bool> {
    let mut all = true;
    for c in req.split(',') {
        all &= satisfies(c.trim(), v, "^")?;
    }
    Some(all)
}

/// One comparator; `bare` is the operator of a version without one.
fn satisfies(r: &str, v: &Version, bare: &str) -> Option<bool> {
    let (op, rest) = match OPS.iter().find(|o| r.starts_with(**o)) {
        Some(o) => (*o, r[o.len()..].trim()),
        // `1.2.*` is a wildcard range, not a caret one.
        None if r.contains(['*', 'x', 'X']) => ("=", r),
        None => (bare, r),
    };
    if matches!(rest, "*" | "x" | "X") {
        return Some(!matches!(op, "<" | ">"));
    }
    let rest = rest.strip_prefix('v').unwrap_or(rest);
    let mut parts = rest.split('.');
    let mut num = |required: bool| -> Option<Option<u64>> {
        match parts.next() {
            None | Some("x" | "X" | "*") if !required => Some(None),
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
                p.parse().ok().map(Some)
            }
            _ => None,
        }
    };
    let major = num(true)??;
    let (minor, patch) = (num(false)?, num(false)?);
    if parts.next().is_some() || (minor.is_none() && patch.is_some()) {
        return None;
    }
    let lo = Version {
        major,
        minor: minor.unwrap_or(0),
        patch: patch.unwrap_or(0),
    };
    let next_major = Version {
        major: major + 1,
        minor: 0,
        patch: 0,
    };
    let next_minor = Version {
        minor: lo.minor + 1,
        patch: 0,
        ..lo
    };
    // `lo` with its last given part plus one: `<=1.2` is `<1.3.0`, `>1` is `>=2.0.0`.
    let up = match (minor, patch) {
        (None, _) => next_major,
        (Some(_), None) => next_minor,
        (Some(_), Some(_)) => lo,
    };
    let full = patch.is_some();
    let hi = match (op, minor, patch) {
        (">=", ..) => return Some(*v >= lo),
        (">", ..) if full => return Some(*v > lo),
        (">", ..) => return Some(*v >= up),
        ("<", ..) => return Some(*v < lo),
        ("<=", ..) if full => return Some(*v <= lo),
        ("<=", ..) => return Some(*v < up),
        ("=", _, Some(_)) => return Some(*v == lo),
        (_, None, _) => next_major,
        ("=" | "~", Some(_), _) => next_minor,
        (_, Some(_), _) if major > 0 => next_major,
        (_, Some(m), p) if m > 0 || p.is_none() => next_minor,
        (_, Some(_), p) => Version {
            patch: p? + 1,
            ..lo
        },
    };
    Some(*v >= lo && *v < hi)
}

/// `old` with `new` in place of its version, keeping the operator; wildcards (`*`, `x`, empty)
/// and `workspace:*|^|~` stay.
pub fn rewrite(old: &str, new: &Version) -> String {
    let old = old.trim();
    if matches!(old.trim(), "" | "*" | "x" | "X") {
        return old.to_string();
    }
    if let Some(rest) = old.strip_prefix("workspace:") {
        return match rest {
            "*" | "^" | "~" => old.to_string(),
            _ => format!("workspace:{}", rewrite(rest, new)),
        };
    }
    let op = OPS.iter().find(|o| old.starts_with(**o)).unwrap_or(&"");
    format!("{op}{new}")
}
