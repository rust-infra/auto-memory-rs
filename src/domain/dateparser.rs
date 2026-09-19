//! The `dateparser.parse` subset the reference's search `after_date` accepts.
//!
//! `build_context` and `recent_activity` resolve a timeframe into an **aware** local
//! timestamp ([`crate::domain::timeframe::parse_timeframe`]), but `SearchQuery.after_date`
//! goes through `dateparser.parse` instead, and the two do not agree. `dateparser`
//! returns a **naive** timestamp in local wall-clock time, and the reference binds it
//! into `datetime(search_index.updated_at) > datetime(:after_date)`, so SQLite reads it
//! as UTC: the bound is effectively shifted by the local UTC offset.
//!
//! That quirk is observable — `after_date="1d"` excludes notes younger than a day *plus
//! the offset* — so it is reproduced here rather than normalized away. Only the forms
//! documented for the tool (and the ones a CLI user reaches for) are supported:
//! absolute dates, `<n><unit>` / `<n> <unit>` with an optional `ago`, and
//! `now`/`today`/`yesterday`. Anything else returns `None`, which the caller treats as
//! "no date filter", exactly as an unparsable `dateparser` input would.

use chrono::{Duration, Months, NaiveDate, NaiveDateTime, NaiveTime};

use crate::domain::timeframe;

/// Parse a search `after_date` expression into the naive local timestamp to bind.
pub fn parse_after_date(input: &str) -> Option<String> {
    parse_after_date_at(input, timeframe::now_local().naive_local())
}

/// [`parse_after_date`] against an explicit "now", so the relative forms are testable.
pub fn parse_after_date_at(input: &str, now: NaiveDateTime) -> Option<String> {
    let text = input.trim().to_lowercase();
    if text.is_empty() {
        return None;
    }

    if let Some(absolute) = parse_absolute(&text) {
        return Some(storage_timestamp(absolute));
    }
    match text.as_str() {
        "now" | "today" => return Some(storage_timestamp(now)),
        "yesterday" => return Some(storage_timestamp(now - Duration::days(1))),
        _ => {}
    }

    let relative = text.strip_suffix(" ago").unwrap_or(&text);
    let relative = relative.trim();
    let (amount, unit) = split_relative(relative)?;
    let unit = unit.trim_start_matches(' ');
    let shifted = match unit {
        "m" | "min" | "mins" | "minute" | "minutes" => now - Duration::minutes(amount),
        "h" | "hr" | "hrs" | "hour" | "hours" => now - Duration::hours(amount),
        "d" | "day" | "days" => now - Duration::days(amount),
        "w" | "week" | "weeks" => now - Duration::weeks(amount),
        "month" | "months" => now.checked_sub_months(Months::new(amount as u32))?,
        "y" | "year" | "years" => {
            now.checked_sub_months(Months::new((amount as u32).checked_mul(12)?))?
        }
        _ => return None,
    };
    Some(storage_timestamp(shifted))
}

/// Split `"1d"` or `"2 weeks"` into its amount and unit.
fn split_relative(text: &str) -> Option<(i64, &str)> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let amount = digits.parse::<i64>().ok()?;
    let unit = text[digits.len()..].trim();
    if unit.is_empty() {
        return None;
    }
    Some((amount, unit))
}

/// Absolute dates, with or without a time: `2026-09-01`, `2026-09-01 10:00:00`,
/// `2026-09-01T10:00`.
fn parse_absolute(text: &str) -> Option<NaiveDateTime> {
    let normalized = text.replace('t', " ");
    let (date, time) = match normalized.split_once(' ') {
        Some((date, time)) => (date, Some(time.trim())),
        None => (normalized.as_str(), None),
    };
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let time = match time {
        None | Some("") => NaiveTime::MIN,
        Some(time) => ["%H:%M:%S", "%H:%M"]
            .iter()
            .find_map(|format| NaiveTime::parse_from_str(time, format).ok())?,
    };
    Some(NaiveDateTime::new(date, time))
}

/// The reference binds a datetime; SQLite compares it as text through `datetime()`.
fn storage_timestamp(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

#[cfg(test)]
mod tests {
    use super::parse_after_date_at;
    use chrono::{Duration, Months, NaiveDate};

    /// A fixed instant, so every expectation below is deterministic.
    fn now() -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 11)
            .expect("date")
            .and_hms_micro_opt(8, 0, 0, 0)
            .expect("time")
    }

    fn expected(shift: impl FnOnce(chrono::NaiveDateTime) -> chrono::NaiveDateTime) -> String {
        shift(now()).format("%Y-%m-%d %H:%M:%S%.6f").to_string()
    }

    fn parse(value: &str) -> Option<String> {
        parse_after_date_at(value, now())
    }

    #[test]
    fn relative_units_subtract_from_local_wall_clock() {
        assert_eq!(parse("1d"), Some(expected(|now| now - Duration::days(1))));
        assert_eq!(parse("30d"), Some(expected(|now| now - Duration::days(30))));
        assert_eq!(parse("1w"), Some(expected(|now| now - Duration::weeks(1))));
        assert_eq!(
            parse("2 hours"),
            Some(expected(|now| now - Duration::hours(2)))
        );
        assert_eq!(
            parse("15m"),
            Some(expected(|now| now - Duration::minutes(15)))
        );
        assert_eq!(
            parse("3 weeks ago"),
            Some(expected(|now| now - Duration::weeks(3)))
        );
        assert_eq!(
            parse("1 month"),
            Some(expected(|now| now
                .checked_sub_months(Months::new(1))
                .expect("month")))
        );
    }

    #[test]
    fn keywords_and_absolute_dates_are_naive() {
        assert_eq!(parse("today"), Some(expected(|now| now)));
        assert_eq!(parse("now"), Some(expected(|now| now)));
        assert_eq!(
            parse("yesterday"),
            Some(expected(|now| now - Duration::days(1)))
        );
        assert_eq!(
            parse("2026-09-01").as_deref(),
            Some("2026-09-01 00:00:00.000000")
        );
        assert_eq!(
            parse("2026-09-01T10:30").as_deref(),
            Some("2026-09-01 10:30:00.000000")
        );
    }

    #[test]
    fn unparsable_input_means_no_filter() {
        for value in ["", "   ", "nonsense", "last tuesday", "d", "5"] {
            assert_eq!(parse(value), None, "{value}");
        }
    }
}
