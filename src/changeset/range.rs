//! Version ranges of internal dependencies (changesets spec §5.2): npm semver and Cargo
//! requirements, the forms workspaces use. `None` means unparseable: no bump, no rewrite.

use crate::release::semver::Version;

const OPS: [&str; 4] = [">=", "^", "~", "="];

/// Does npm range `range` (`^ ~ >= =`, exact, `x`/`*`, `workspace:`) admit `v`?
pub fn npm_satisfies(range: &str, v: &Version) -> Option<bool> {
    let r = range.trim();
    if let Some(rest) = r.strip_prefix("workspace:") {
        return match rest {
            "*" | "^" | "~" => Some(true),
            _ => npm_satisfies(rest, v),
        };
    }
    if r.is_empty() {
        return Some(true);
    }
    satisfies(r, v, "=")
}

/// Does Cargo requirement `req` (bare = caret, `^ ~ = >=`, `*`) admit `v`?
pub fn cargo_satisfies(req: &str, v: &Version) -> Option<bool> {
    satisfies(req.trim(), v, "^")
}

/// One comparator; `bare` is the operator of a version without one.
fn satisfies(r: &str, v: &Version, bare: &str) -> Option<bool> {
    let (op, rest) = match OPS.iter().find(|o| r.starts_with(**o)) {
        Some(o) => (*o, r[o.len()..].trim()),
        None => (bare, r),
    };
    if matches!(rest, "*" | "x" | "X") {
        return Some(true);
    }
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
    let hi = match (op, minor, patch) {
        (">=", ..) => return Some(*v >= lo),
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

/// `old` with `new` in place of its version, keeping the operator; `workspace:*|^|~` stay.
pub fn rewrite(old: &str, new: &Version) -> String {
    if let Some(rest) = old.strip_prefix("workspace:") {
        return match rest {
            "*" | "^" | "~" => old.to_string(),
            _ => format!("workspace:{}", rewrite(rest, new)),
        };
    }
    let op = OPS.iter().find(|o| old.starts_with(**o)).unwrap_or(&"");
    format!("{op}{new}")
}
