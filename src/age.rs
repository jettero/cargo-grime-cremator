//! Parse user-supplied `--max-age` strings into an absolute cutoff
//! `SystemTime`. Anything in the project's `target/` whose
//! `invoked.timestamp` mtime is *older than* the cutoff is considered stale.
//!
//! Accepted formats (modeled loosely on journalctl `--since` / `--until`):
//!
//! * **Relative durations**, with optional trailing ` ago`:
//!   `"1 month"`, `"4 days ago"`, `"2 hours 30 minutes"`, `"36h"`,
//!   `"1d12h"`, `"90 minutes"`. Parsed by `humantime`. The "ago" suffix
//!   is purely a no-op marker for human friendliness.
//! * **Absolute UTC datetimes**:
//!   `"2025-02-21"` (whole day, midnight UTC),
//!   `"2025-02-21 13:00:00"`,
//!   `"2025-02-21T13:00:00"`.
//! * **Disabled**: the literal strings `"never"`, `"off"`, and `"0"` mean
//!   "disable the age check entirely". The CLI surface returns `None`
//!   for those instead of calling this function.

use anyhow::{anyhow, bail, Context, Result};
use std::time::{Duration, SystemTime};

/// True if `s` is one of the documented "disable" sentinels.
pub fn is_disable_sentinel(s: &str) -> bool {
    matches!(s.trim(), "never" | "off" | "0" | "")
}

/// Parse the user input and return an absolute cutoff. Anything strictly
/// older than the returned `SystemTime` should be considered stale by the
/// caller.
pub fn parse_cutoff(input: &str) -> Result<SystemTime> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("empty --max-age value");
    }
    let now = SystemTime::now();

    // Try absolute first — only matches "YYYY-MM-DD..." style strings, so
    // it can't accidentally swallow durations.
    if let Some(absolute) = try_parse_absolute(trimmed)? {
        if absolute > now {
            bail!("--max-age {:?} is in the future", input);
        }
        return Ok(absolute);
    }

    // Relative duration. Strip the cosmetic " ago" suffix if present.
    let body = trimmed
        .strip_suffix(" ago")
        .map(str::trim)
        .unwrap_or(trimmed);
    let dur = humantime::parse_duration(body)
        .with_context(|| format!("could not parse --max-age {:?} as a duration", input))?;
    now.checked_sub(dur)
        .ok_or_else(|| anyhow!("--max-age {:?} is too far in the past", input))
}

/// Try to parse `s` as an absolute UTC date or datetime. Returns `Ok(None)`
/// if it doesn't look like an absolute timestamp at all (so the caller can
/// fall through to the duration parser).
fn try_parse_absolute(s: &str) -> Result<Option<SystemTime>> {
    // Quick prefilter: must start with a 4-digit year and a `-`.
    let bytes = s.as_bytes();
    if bytes.len() < 10 || !bytes[0..4].iter().all(|c| c.is_ascii_digit()) || bytes[4] != b'-' {
        return Ok(None);
    }
    // Split date / time on either ' ' or 'T'.
    let split_at = s.find([' ', 'T']);
    let (date_str, time_str) = match split_at {
        Some(idx) => (&s[..idx], Some(s[idx + 1..].trim())),
        None => (s, None),
    };
    let date_parts: Vec<&str> = date_str.split('-').collect();
    if date_parts.len() != 3 {
        return Ok(None);
    }
    let year: i32 = date_parts[0]
        .parse()
        .with_context(|| format!("invalid year: {}", date_parts[0]))?;
    let month: u32 = date_parts[1]
        .parse()
        .with_context(|| format!("invalid month: {}", date_parts[1]))?;
    let day: u32 = date_parts[2]
        .parse()
        .with_context(|| format!("invalid day: {}", date_parts[2]))?;
    let (h, m, sec) = if let Some(time) = time_str {
        let tcs: Vec<&str> = time.split(':').collect();
        if tcs.len() < 2 || tcs.len() > 3 {
            bail!("invalid time component {:?}", time);
        }
        let h: u32 = tcs[0].parse().context("hour")?;
        let m: u32 = tcs[1].parse().context("minute")?;
        let s: u32 = tcs
            .get(2)
            .copied()
            .unwrap_or("0")
            .parse()
            .context("second")?;
        (h, m, s)
    } else {
        (0, 0, 0)
    };
    Ok(Some(civil_to_systemtime(year, month, day, h, m, sec)?))
}

/// Convert a UTC civil date/time to `SystemTime`.
///
/// Uses Howard Hinnant's days-from-civil algorithm
/// (<https://howardhinnant.github.io/date_algorithms.html>), which is
/// branch-free and valid for any proleptic Gregorian date — well beyond
/// the range we care about.
fn civil_to_systemtime(y: i32, mth: u32, d: u32, h: u32, mi: u32, s: u32) -> Result<SystemTime> {
    if !(1..=12).contains(&mth) || !(1..=31).contains(&d) || h >= 24 || mi >= 60 || s >= 60 {
        bail!(
            "date out of range: {}-{:02}-{:02} {:02}:{:02}:{:02}",
            y,
            mth,
            d,
            h,
            mi,
            s
        );
    }
    let yy: i32 = if mth <= 2 { y - 1 } else { y };
    let era: i32 = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe: i32 = yy - era * 400;
    let m = mth as i32;
    let doy: i32 = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i32 - 1;
    let doe: i32 = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch: i64 = era as i64 * 146097 + doe as i64 - 719468;
    let secs: i64 = days_since_epoch * 86400 + h as i64 * 3600 + mi as i64 * 60 + s as i64;
    if secs < 0 {
        bail!("--max-age date predates 1970");
    }
    Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relative_month() {
        let cutoff = parse_cutoff("1 month").unwrap();
        let age = SystemTime::now().duration_since(cutoff).unwrap();
        assert!(age.as_secs() > 28 * 86400);
        assert!(age.as_secs() < 32 * 86400);
    }

    #[test]
    fn parses_relative_days_with_ago() {
        let a = parse_cutoff("4 days").unwrap();
        let b = parse_cutoff("4 days ago").unwrap();
        // The two SystemTime::now() calls happen microseconds apart, so the
        // results should be within a couple of seconds.
        let diff = match b.duration_since(a) {
            Ok(d) => d,
            Err(e) => e.duration(),
        };
        assert!(diff.as_secs() < 5);
    }

    #[test]
    fn parses_relative_compound() {
        let cutoff = parse_cutoff("2 hours 30 minutes").unwrap();
        let age = SystemTime::now().duration_since(cutoff).unwrap();
        // 2h30m = 9000s, with a tiny tolerance for the now() drift.
        assert!(age.as_secs() >= 9000);
        assert!(age.as_secs() < 9000 + 5);
    }

    #[test]
    fn parses_absolute_utc_date() {
        let cutoff = parse_cutoff("2025-02-21").unwrap();
        let secs = cutoff
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // `date -u -d "2025-02-21 00:00:00 UTC" +%s` = 1740096000
        assert_eq!(secs, 1_740_096_000);
    }

    #[test]
    fn parses_absolute_utc_datetime_space() {
        let cutoff = parse_cutoff("2025-02-21 13:00:00").unwrap();
        let secs = cutoff
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(secs, 1_740_096_000 + 13 * 3600);
    }

    #[test]
    fn parses_absolute_utc_datetime_iso_t() {
        let cutoff = parse_cutoff("2025-02-21T13:00:00").unwrap();
        let secs = cutoff
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(secs, 1_740_096_000 + 13 * 3600);
    }

    #[test]
    fn rejects_future_absolute() {
        assert!(parse_cutoff("2999-01-01").is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_cutoff("nonsense input").is_err());
        assert!(parse_cutoff("").is_err());
    }

    #[test]
    fn disable_sentinels() {
        assert!(is_disable_sentinel("never"));
        assert!(is_disable_sentinel("off"));
        assert!(is_disable_sentinel("0"));
        assert!(is_disable_sentinel(""));
        assert!(is_disable_sentinel("  off  "));
        assert!(!is_disable_sentinel("1 month"));
        assert!(!is_disable_sentinel("forever"));
    }
}
