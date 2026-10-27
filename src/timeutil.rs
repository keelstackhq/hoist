//! Minimal RFC 3339 / civil-date arithmetic (no date crate). `HOIST_NOW` (RFC 3339)
//! overrides "now"; it exists for tests only.

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of `days_from_civil`: (year, month, day).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (days_from_civil(ny, nm, 1) - days_from_civil(y, m, 1)) as u32
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date: ASCII digits only, a real calendar day.
pub fn parse_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let shape = b.len() == 10 && b[4] == b'-' && b[7] == b'-';
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    if !shape || !digits(0..4) || !digits(5..7) || !digits(8..10) {
        return None;
    }
    let (y, m, d) = (num(&s[..4])?, num(&s[5..7])? as u32, num(&s[8..])? as u32);
    let valid = (1..=12).contains(&m) && (1..=days_in_month(y, m)).contains(&d);
    valid.then(|| days_from_civil(y, m, d))
}

/// `days` plus `months` calendar months, the day clamped to the month end
/// (2026-01-31 + 1 month = 2026-02-28).
pub fn add_months(days: i64, months: i64) -> i64 {
    let (y, m, d) = civil_from_days(days);
    let total = y * 12 + i64::from(m - 1) + months;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    days_from_civil(y, m, d.min(days_in_month(y, m)))
}

fn num(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Unix seconds of an RFC 3339 timestamp (`Z` or `±hh:mm`, optional fraction), else `None`.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', 't', ' '])?;
    let mut dp = date.split('-');
    let (y, m, d) = (num(dp.next()?)?, num(dp.next()?)?, num(dp.next()?)?);
    if dp.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let split = rest.find(['Z', 'z', '+', '-']);
    let (time, zone) = match split {
        Some(i) => rest.split_at(i),
        None => (rest, "Z"),
    };
    let mut tp = time.split(':');
    let (h, mi) = (num(tp.next()?)?, num(tp.next()?)?);
    let sec_part = tp.next().unwrap_or("0");
    let sec = num(sec_part.split('.').next()?)?;
    if tp.next().is_some() || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let offset = match zone {
        "Z" | "z" => 0,
        z => {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let (oh, om) = z[1..].split_once(':')?;
            sign * (num(oh)? * 3600 + num(om)? * 60)
        }
    };
    Some(days_from_civil(y, m as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

/// `YYYY-MM-DDTHH:MM:SSZ` (UTC).
pub fn format_rfc3339(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        t / 3600,
        t % 3600 / 60,
        t % 60
    )
}

/// Current Unix time, or `HOIST_NOW` when it parses.
pub fn now_secs() -> i64 {
    if let Some(secs) = std::env::var("HOIST_NOW")
        .ok()
        .and_then(|v| parse_rfc3339(&v))
    {
        return secs;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_date_accepts_only_real_days() {
        assert_eq!(parse_date("2026-10-16"), Some(20_742));
        assert_eq!(parse_date("2024-02-29"), Some(days_from_civil(2024, 2, 29)));
        assert_eq!(parse_date("2000-02-29"), Some(days_from_civil(2000, 2, 29)));
        assert_eq!(parse_date("1969-12-31"), Some(-1));
        for bad in [
            "1900-02-29",
            "2100-02-29",
            "2023-02-29",
            "2027-02-30",
            "2027-13-01",
            "2027-00-10",
            "2027-01-00",
        ] {
            assert_eq!(parse_date(bad), None, "{bad}");
        }
        for bad in [
            "2027-6-1",
            "2027-+6-01",
            "+027-06-01",
            "-027-06-01",
            "2027-06-1é",
            "x",
        ] {
            assert_eq!(parse_date(bad), None, "{bad}");
        }
    }

    #[test]
    fn add_months_clamps_to_the_month_end() {
        let d = |s| parse_date(s).unwrap();
        assert_eq!(add_months(d("2026-10-27"), 13), d("2027-11-27"));
        assert_eq!(add_months(d("2026-01-31"), 13), d("2027-02-28"));
        assert_eq!(add_months(d("2027-01-31"), 13), d("2028-02-29"));
        assert_eq!(add_months(d("2026-12-15"), 13), d("2028-01-15"));
        assert_eq!(add_months(d("2026-12-31"), 2), d("2027-02-28"));
        assert_eq!(add_months(d("2026-03-31"), -1), d("2026-02-28"));
        assert_eq!(add_months(d("2026-01-15"), -1), d("2025-12-15"));
    }

    #[test]
    fn civil_days_known_values() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2026, 10, 16), 20_742);
        assert_eq!(civil_from_days(20_742), (2026, 10, 16));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn civil_roundtrip_across_leap_days() {
        for z in (-800_000..800_000).step_by(37) {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(
            civil_from_days(days_from_civil(2100, 3, 1) - 1),
            (2100, 2, 28)
        );
    }

    #[test]
    fn parse_and_format() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339("2026-10-16T12:30:45Z"),
            Some(20_742 * 86_400 + 45_045)
        );
        assert_eq!(
            parse_rfc3339("2026-10-16T12:30:45.123Z"),
            Some(20_742 * 86_400 + 45_045)
        );
        assert_eq!(
            parse_rfc3339("2026-10-16T14:30:45+02:00"),
            parse_rfc3339("2026-10-16T12:30:45Z")
        );
        assert_eq!(
            format_rfc3339(20_742 * 86_400 + 45_045),
            "2026-10-16T12:30:45Z"
        );
        assert_eq!(parse_rfc3339("garbage"), None);
        assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339("2026-10-16"), None);
    }
}
