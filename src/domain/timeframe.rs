//! Reference-compatible timeframe parsing and timestamp formatting.
//!
//! Mirrors `basic_memory.schemas.base` (`validate_timeframe` / `parse_timeframe`)
//! and the layout the reference writes into the SQLite `entity.created_at` /
//! `entity.updated_at` columns (SQLAlchemy `DATETIME` on SQLite:
//! `%Y-%m-%d %H:%M:%S.%f`, local wall clock, no offset).
//!
//! The upstream expression parser is `dateparser`; this module covers the forms
//! the CLI/MCP surfaces document (`7d`, `24h`, `2 weeks`, `1 month ago`,
//! `today`, `yesterday`, `last week`, ISO dates) plus the CLI default `7d`.

use chrono::{
    DateTime, Duration, FixedOffset, Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone,
};

use crate::error::{Error, Result};

/// Timezone-aware instant carried through the domain and index layers.
pub type Instant = DateTime<FixedOffset>;

/// SQLite `DATETIME` layout the reference uses for stored timestamps.
const STORAGE_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.6f";

/// Normalized timeframe floor: the reference never looks back less than a day.
const MINIMUM_LOOKBACK_SECONDS: i64 = 86_400;

/// Largest accepted lookback (`validate_timeframe` rejects more than a year).
const MAXIMUM_LOOKBACK_DAYS: i64 = 365;

/// Normalize a human timeframe the way `validate_timeframe` does.
///
/// Returns `"today"` for that literal (it keeps its special meaning upstream) and
/// otherwise `"<days>d"`, rounded to whole days and capped at one year.
pub fn normalize_timeframe(input: &str) -> Result<String> {
    let value = input.trim();
    if value.is_empty() {
        return Err(invalid(value, "timeframe must not be empty"));
    }
    if value.eq_ignore_ascii_case("today") {
        return Ok("today".to_owned());
    }

    let parsed = resolve(value)?;
    let now = now_local();
    if parsed > now {
        return Err(invalid(value, "timeframe cannot be in the future"));
    }

    let days = round_half_even((now - parsed).num_seconds() as f64 / 86_400.0);
    if days > MAXIMUM_LOOKBACK_DAYS {
        return Err(invalid(value, "timeframe should be <= 1 year"));
    }
    Ok(format!("{days}d"))
}

/// Resolve a timeframe into the `since` instant used by context/search queries.
///
/// Mirrors `parse_timeframe`: `today` and every sub-day expression resolve to at
/// least one day back, so timezone drift between writer and reader cannot hide
/// recent rows.
pub fn parse_timeframe(input: &str) -> Result<DateTime<FixedOffset>> {
    let value = input.trim();
    if value.is_empty() {
        return Err(invalid(value, "timeframe must not be empty"));
    }

    let now = now_local();
    let resolved = if value.eq_ignore_ascii_case("today") {
        now - Duration::seconds(MINIMUM_LOOKBACK_SECONDS)
    } else {
        resolve(value)?
    };

    let floor = now - Duration::seconds(MINIMUM_LOOKBACK_SECONDS);
    Ok(if resolved > floor { floor } else { resolved })
}

/// Current local instant captured with its UTC offset.
pub fn now_local() -> DateTime<FixedOffset> {
    Local::now().fixed_offset()
}

/// Build an instant from a Unix timestamp in seconds (file metadata fallback).
pub fn from_unix_seconds(seconds: i64) -> DateTime<FixedOffset> {
    DateTime::from_timestamp(seconds, 0)
        .map_or_else(now_local, |utc| utc.with_timezone(&Local).fixed_offset())
}

/// Build an instant from a Unix timestamp in seconds and microseconds.
///
/// The reference derives `created`/`updated` from `st_ctime`/`st_mtime` through
/// `datetime.fromtimestamp`, which keeps microseconds. Truncating to whole seconds
/// would collapse every file copied in the same second onto one timestamp and make
/// `updated_*` directory ordering fall back to name order.
pub fn from_unix_seconds_micros(seconds: i64, micros: u32) -> DateTime<FixedOffset> {
    DateTime::from_timestamp(seconds, micros.saturating_mul(1_000))
        .map_or_else(now_local, |utc| utc.with_timezone(&Local).fixed_offset())
}

/// Render an instant the way the reference stores `created_at` / `updated_at`.
pub fn storage_timestamp(instant: DateTime<FixedOffset>) -> String {
    instant.naive_local().format(STORAGE_FORMAT).to_string()
}

/// Current local time in the reference storage layout.
pub fn now_storage_timestamp() -> String {
    storage_timestamp(now_local())
}

/// Render the `since` bound compared against stored timestamps.
///
/// The reference passes `datetime.isoformat()` into SQLite, where SQLAlchemy
/// compares it against the stored strings; the comparison is lexicographic, so
/// the bound keeps the same `%Y-%m-%d` prefix and the explicit offset.
pub fn since_bound(instant: &DateTime<FixedOffset>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Micros, false)
}

/// Current UTC time as an ISO 8601 string (reference `datetime.now(timezone.utc)`).
pub fn utc_now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}

/// Parse a `created` / `modified` frontmatter value.
///
/// Mirrors `_parse_frontmatter_timestamp`: ISO 8601 only, date-only and naive
/// datetimes describe local wall-clock time, explicit offsets are preserved, and
/// anything else is an error.
pub fn parse_frontmatter_timestamp(value: &str) -> Result<DateTime<FixedOffset>> {
    let text = value.trim();
    if text.is_empty() {
        return Err(invalid(value, "timestamp must not be empty"));
    }

    if let Ok(naive) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return local_midnight(naive);
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(text) {
        return Ok(parsed);
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(parsed) = DateTime::parse_from_str(text, format) {
            return Ok(parsed);
        }
        if let Ok(parsed) = NaiveDateTime::parse_from_str(text, format) {
            return local_from_naive(parsed);
        }
    }

    Err(invalid(
        value,
        "expected an ISO 8601 date or datetime (for example 2026-01-02)",
    ))
}

/// Resolve one relative or absolute timeframe expression.
fn resolve(value: &str) -> Result<DateTime<FixedOffset>> {
    let text = value.trim();
    let lowered = text.to_ascii_lowercase();
    let now = now_local();

    if lowered == "now" {
        return Ok(now);
    }
    if matches!(lowered.as_str(), "yesterday" | "today") {
        return Ok(now - Duration::days(1));
    }
    if let Some(rest) = lowered.strip_prefix("last ") {
        if let Some(seconds) = unit_seconds(rest.trim()) {
            return Ok(now - Duration::seconds(seconds));
        }
    }

    // Absolute ISO values carry date separators; relative expressions do not.
    if text.contains('-') || text.contains(':') {
        return parse_frontmatter_timestamp(text);
    }

    let (amount, unit) = split_expression(&lowered)?;
    let seconds = unit_seconds(unit).ok_or_else(|| invalid(value, "unknown timeframe unit"))?;
    Ok(now - Duration::seconds(seconds * amount))
}

/// Split `"2 weeks ago"` / `"7d"` into the amount and the unit.
fn split_expression(value: &str) -> Result<(i64, &str)> {
    let mut text = value.trim();
    if let Some(stripped) = text.strip_suffix(" ago") {
        text = stripped.trim();
    }
    if text.starts_with('+') {
        text = text.trim_start_matches('+').trim();
    }

    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return Err(invalid(value, "timeframe must start with a number"));
    }
    let amount = digits
        .parse::<i64>()
        .map_err(|_| invalid(value, "timeframe amount is out of range"))?;
    let unit = text[digits.len()..].trim();
    if unit.is_empty() {
        return Err(invalid(value, "timeframe is missing a unit"));
    }
    Ok((amount, unit))
}

/// Seconds in one unit, accepting the singular, plural, and short spellings.
fn unit_seconds(unit: &str) -> Option<i64> {
    let seconds = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
        "d" | "day" | "days" => 86_400,
        "w" | "wk" | "wks" | "week" | "weeks" => 604_800,
        "mo" | "mon" | "month" | "months" => 2_592_000,
        "y" | "yr" | "yrs" | "year" | "years" => 31_536_000,
        _ => return None,
    };
    Some(seconds)
}

/// Local midnight for a date-only frontmatter value.
fn local_midnight(date: NaiveDate) -> Result<DateTime<FixedOffset>> {
    let naive = date.and_hms_opt(0, 0, 0).unwrap_or_default();
    local_from_naive(naive)
}

/// Interpret a naive datetime as local wall-clock time.
fn local_from_naive(naive: NaiveDateTime) -> Result<DateTime<FixedOffset>> {
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|value| value.fixed_offset())
        .ok_or_else(|| invalid(&naive.to_string(), "local time is out of range"))
}

/// Python's `round()` (half to even) for the day counts `validate_timeframe` uses.
fn round_half_even(value: f64) -> i64 {
    let floor = value.floor();
    let fraction = value - floor;
    if (fraction - 0.5).abs() < 1e-9 {
        let candidate = floor as i64;
        if candidate % 2 == 0 {
            candidate
        } else {
            candidate + 1
        }
    } else {
        value.round() as i64
    }
}

fn invalid(value: &str, message: &str) -> Error {
    Error::Timeframe {
        value: value.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_documented_forms() {
        assert_eq!(normalize_timeframe("7d").expect("7d"), "7d");
        assert_eq!(normalize_timeframe("1 week").expect("1 week"), "7d");
        assert_eq!(normalize_timeframe("2 weeks ago").expect("2 weeks"), "14d");
        assert_eq!(normalize_timeframe("today").expect("today"), "today");
        assert_eq!(normalize_timeframe("24h").expect("24h"), "1d");
    }

    #[test]
    fn rejects_out_of_range_timeframes() {
        assert!(normalize_timeframe("400d").is_err());
        assert!(normalize_timeframe("").is_err());
    }

    #[test]
    fn sub_day_expressions_keep_a_one_day_floor() {
        let now = now_local();
        let since = parse_timeframe("1h").expect("1h");
        // The floor is computed against its own `now`, so allow clock drift.
        assert!((now - since).num_seconds() >= MINIMUM_LOOKBACK_SECONDS - 5);
    }

    #[test]
    fn frontmatter_dates_parse_as_local_midnight() {
        let parsed = parse_frontmatter_timestamp("2026-01-02").expect("date");
        assert_eq!(storage_timestamp(parsed), "2026-01-02 00:00:00.000000");
        let datetime = parse_frontmatter_timestamp("2026-01-02T03:04:05+00:00").expect("datetime");
        // The reference stores the wall clock the value carried, offsets included.
        assert_eq!(storage_timestamp(datetime), "2026-01-02 03:04:05.000000");
        assert!(parse_frontmatter_timestamp("not a date").is_err());
    }
}
