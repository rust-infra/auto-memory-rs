//! Valid time: when an observation's claim holds.
//!
//! Upstream 0.24.0 (#1426, SPEC-82) lets an observation say *when* it is true, so a search
//! can ask "what did we believe in June?" instead of only "what mentions this?".
//!
//! The authored form is a qualifier at the start of the observation body:
//!
//! ```text
//! - [decision] @effective:2026-06-10 The cache layer will use Redis.
//! - [decision] @effective[2026-06-10,2026-07-27) The cache layer will use Redis.
//! - [fact] @occurred:"June 10, 2026" The launch slipped.
//! - [task] @due:2026-07-01 Ship the migration.
//! - [note] @mentioned:2026-06-01 Someone brought this up.
//! ```
//!
//! The kinds are `effective`, `valid` (the default when a bare date is given), `occurred`,
//! `due` and `mentioned`. A qualifier that does not parse stays in the observation text
//! verbatim — the reference does the same, so an author's typo is visible rather than
//! silently dropping the claim.

use serde::{Deserialize, Serialize};

/// What kind of time a qualifier states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeKind {
    /// The claim became true at this point.
    Effective,
    /// The claim holds over a range (the default for a bare date).
    Valid,
    /// The event happened at this point.
    Occurred,
    /// A deadline.
    Due,
    /// The claim was mentioned at this point.
    Mentioned,
}

impl TimeKind {
    /// The wire name, as `time_kind` accepts it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Effective => "effective",
            Self::Valid => "valid",
            Self::Occurred => "occurred",
            Self::Due => "due",
            Self::Mentioned => "mentioned",
        }
    }

    /// Parse a `time_kind` filter value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "effective" => Some(Self::Effective),
            "valid" => Some(Self::Valid),
            "occurred" => Some(Self::Occurred),
            "due" => Some(Self::Due),
            "mentioned" => Some(Self::Mentioned),
            _ => None,
        }
    }
}

/// A parsed valid-time qualifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidTime {
    /// Which kind of time this is.
    pub kind: TimeKind,
    /// Inclusive start of the range, `YYYY-MM-DD`.
    pub start: String,
    /// Exclusive end of the range; equal to `start` for an instant.
    pub end: String,
}

impl ValidTime {
    /// Whether `instant` (`YYYY-MM-DD`) falls inside the half-open range.
    #[must_use]
    pub fn covers(&self, instant: &str) -> bool {
        // Both sides are `YYYY-MM-DD`, so lexicographic order is chronological order.
        instant >= self.start.as_str() && instant < self.end.as_str()
    }

    /// Whether this range overlaps `other`, half-open on both sides.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// Whether the range is a single instant rather than a span.
    #[must_use]
    pub fn is_instant(&self) -> bool {
        self.start == self.end
    }
}

/// One observation's valid-time qualifier and the text left after removing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQualifier {
    /// The qualifier, when one parsed.
    pub time: Option<ValidTime>,
    /// The observation text with the qualifier removed; the original text when it did not
    /// parse, so a typo stays visible.
    pub body: String,
}

/// The kinds, longest first so `@effective` is not consumed as `@e`.
const KINDS: [(&str, TimeKind); 5] = [
    ("effective", TimeKind::Effective),
    ("mentioned", TimeKind::Mentioned),
    ("occurred", TimeKind::Occurred),
    ("valid", TimeKind::Valid),
    ("due", TimeKind::Due),
];

/// Parse a leading `@<kind>:<date>` or `@<kind>[<start>,<end>)` qualifier.
///
/// Returns the text unchanged when there is no qualifier or it does not parse, which is the
/// reference's behavior: a malformed qualifier is part of the observation, not a filter.
#[must_use]
pub fn parse_qualifier(body: &str) -> ParsedQualifier {
    let unchanged = |text: &str| ParsedQualifier {
        time: None,
        body: text.to_owned(),
    };
    let trimmed = body.trim_start();
    let Some(rest) = trimmed.strip_prefix('@') else {
        return unchanged(body);
    };

    for (name, kind) in KINDS {
        let Some(after) = rest.strip_prefix(name) else {
            continue;
        };
        // A range: `[start,end)` — the closing bracket is half-open by design.
        if let Some(range) = after.strip_prefix('[') {
            let Some((bounds, tail)) = range.split_once(')') else {
                return unchanged(body);
            };
            let Some((start, end)) = bounds.split_once(',') else {
                return unchanged(body);
            };
            let (Some(start), Some(end)) = (normalize_date(start), normalize_date(end)) else {
                return unchanged(body);
            };
            // An empty or inverted range is a mistake, not an empty window.
            if start >= end {
                return unchanged(body);
            }
            return ParsedQualifier {
                time: Some(ValidTime { kind, start, end }),
                body: tail.trim_start().to_owned(),
            };
        }
        // An instant: `:2026-06-10` or `:"June 10, 2026"`.
        let Some(after) = after.strip_prefix(':') else {
            return unchanged(body);
        };
        let (value, tail) = if let Some(quoted) = after.strip_prefix('"') {
            let Some((value, tail)) = quoted.split_once('"') else {
                return unchanged(body);
            };
            (value, tail)
        } else {
            // Unquoted: the value runs to the next whitespace.
            match after.find(char::is_whitespace) {
                Some(index) => (&after[..index], &after[index..]),
                None => (after, ""),
            }
        };
        let Some(date) = normalize_date(value) else {
            return unchanged(body);
        };
        return ParsedQualifier {
            // An instant covers the whole day it names, so its half-open range ends at the
            // next day. Storing `end == start` would make `start <= x < end` false for
            // every `x`, including the day itself.
            time: Some(ValidTime {
                kind,
                end: next_day(&date),
                start: date,
            }),
            body: tail.trim_start().to_owned(),
        };
    }
    unchanged(body)
}

/// Normalize a date expression to `YYYY-MM-DD`.
///
/// Accepts an ISO date, a quoted month-name date (`June 10, 2026`), and the compact
/// `YYYYMMDD`. Anything else is `None`, which leaves the qualifier in the observation text.
#[must_use]
pub fn normalize_date(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    // ISO `YYYY-MM-DD`.
    if let Some(date) = parse_iso(value) {
        return Some(date);
    }
    // Compact `YYYYMMDD`.
    if value.len() == 8 && value.chars().all(|c| c.is_ascii_digit()) {
        let candidate = format!("{}-{}-{}", &value[..4], &value[4..6], &value[6..8]);
        return parse_iso(&candidate);
    }
    // `Month D, YYYY` and `D Month YYYY`.
    parse_month_name(value)
}

/// The day after `date` (`YYYY-MM-DD`), used to close a single-day range.
///
/// Only the day needs arithmetic: month and year rollover follow from it.
fn next_day(date: &str) -> String {
    let (year, month, day) = (
        date[0..4].parse::<i32>().unwrap_or(1970),
        date[5..7].parse::<u32>().unwrap_or(1),
        date[8..10].parse::<u32>().unwrap_or(1),
    );
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    };
    if day < days_in_month {
        return format!("{year:04}-{month:02}-{:02}", day + 1);
    }
    if month < 12 {
        return format!("{year:04}-{:02}-01", month + 1);
    }
    format!("{:04}-01-01", year + 1)
}

/// Validate an ISO date and return it in canonical form.
fn parse_iso(value: &str) -> Option<String> {
    let mut parts = value.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    if !year.chars().all(|c| c.is_ascii_digit())
        || !month.chars().all(|c| c.is_ascii_digit())
        || !day.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let (month, day) = (month.parse::<u32>().ok()?, day.parse::<u32>().ok()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(value.to_owned())
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

fn parse_month_name(value: &str) -> Option<String> {
    let cleaned = value.replace(',', " ");
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let (month_word, day_word, year_word) = match words.as_slice() {
        [month, day, year] if month.parse::<u32>().is_err() => (*month, *day, *year),
        [day, month, year] if month.parse::<u32>().is_err() => (*month, *day, *year),
        _ => return None,
    };
    let month = MONTHS
        .iter()
        .position(|name| name.eq_ignore_ascii_case(month_word))? as u32
        + 1;
    let day: u32 = day_word.parse().ok()?;
    let year: u32 = year_word.parse().ok()?;
    if !(1..=31).contains(&day) || !(1000..=9999).contains(&year) {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_effective_instant_parses() {
        let parsed = parse_qualifier("@effective:2026-06-10 The cache layer will use Redis.");
        let time = parsed.time.expect("qualifier");
        assert_eq!(time.kind, TimeKind::Effective);
        assert_eq!(time.start, "2026-06-10");
        // An instant covers the whole day it names, so its range ends at the next day.
        assert_eq!(time.end, "2026-06-11");
        assert!(time.covers("2026-06-10"));
        assert!(!time.covers("2026-06-11"));
        // The qualifier is removed from the text the index stores.
        assert_eq!(parsed.body, "The cache layer will use Redis.");
    }

    #[test]
    fn an_effective_range_parses_as_half_open() {
        let parsed = parse_qualifier("@effective[2026-06-10,2026-07-27) The cache layer.");
        let time = parsed.time.expect("qualifier");
        assert_eq!(
            (time.start.as_str(), time.end.as_str()),
            ("2026-06-10", "2026-07-27")
        );
        // Half-open: the start is covered, the end is not.
        assert!(time.covers("2026-06-10"));
        assert!(time.covers("2026-07-26"));
        assert!(!time.covers("2026-07-27"));
        assert_eq!(parsed.body, "The cache layer.");
    }

    #[test]
    fn a_quoted_multi_word_date_parses() {
        let parsed = parse_qualifier("@occurred:\"June 10, 2026\" The launch slipped.");
        let time = parsed.time.expect("qualifier");
        assert_eq!(time.kind, TimeKind::Occurred);
        assert_eq!(time.start, "2026-06-10");
        assert_eq!(parsed.body, "The launch slipped.");
    }

    #[test]
    fn every_kind_parses() {
        for (text, kind) in [
            ("@effective:2026-06-10 x", TimeKind::Effective),
            ("@valid:2026-06-10 x", TimeKind::Valid),
            ("@occurred:2026-06-10 x", TimeKind::Occurred),
            ("@due:2026-06-10 x", TimeKind::Due),
            ("@mentioned:2026-06-10 x", TimeKind::Mentioned),
        ] {
            assert_eq!(parse_qualifier(text).time.expect(text).kind, kind, "{text}");
        }
    }

    #[test]
    fn a_malformed_qualifier_stays_in_the_text() {
        // The reference keeps an unparsable qualifier: a typo is visible, not silently
        // dropped along with the claim it was meant to qualify.
        for text in [
            "@effective:not-a-date The claim.",
            "@effective[2026-06-10,2026-06-01) inverted",
            "@effective[2026-06-10] wrong bracket",
            "@unknown:2026-06-10 The claim.",
            "@effective:2026-13-01 bad month",
        ] {
            let parsed = parse_qualifier(text);
            assert!(parsed.time.is_none(), "{text}");
            assert_eq!(parsed.body, text, "{text}");
        }
    }

    #[test]
    fn an_ordinary_observation_is_untouched() {
        let parsed = parse_qualifier("The cache layer will use Redis.");
        assert!(parsed.time.is_none());
        assert_eq!(parsed.body, "The cache layer will use Redis.");
    }

    #[test]
    fn ranges_overlap_only_when_they_intersect() {
        let june = parse_qualifier("@valid[2026-06-01,2026-07-01) x")
            .time
            .expect("range");
        let july = parse_qualifier("@valid[2026-07-01,2026-08-01) x")
            .time
            .expect("range");
        let late_june = parse_qualifier("@valid[2026-06-20,2026-06-25) x")
            .time
            .expect("range");
        // Adjacent half-open ranges do not overlap: July 1 belongs to `july` alone.
        assert!(!june.overlaps(&july));
        assert!(!july.overlaps(&june));
        assert!(june.overlaps(&late_june));
        assert!(late_june.overlaps(&june));
    }

    #[test]
    fn time_kind_round_trips_through_its_wire_name() {
        for kind in [
            TimeKind::Effective,
            TimeKind::Valid,
            TimeKind::Occurred,
            TimeKind::Due,
            TimeKind::Mentioned,
        ] {
            assert_eq!(TimeKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(TimeKind::parse("nonsense"), None);
    }

    #[test]
    fn compact_and_month_first_dates_normalize() {
        assert_eq!(normalize_date("20260610").as_deref(), Some("2026-06-10"));
        assert_eq!(
            normalize_date("10 June 2026").as_deref(),
            Some("2026-06-10")
        );
        assert_eq!(
            normalize_date("June 10, 2026").as_deref(),
            Some("2026-06-10")
        );
        assert_eq!(normalize_date("2026-06-10").as_deref(), Some("2026-06-10"));
        assert_eq!(normalize_date("nope"), None);
    }

    #[test]
    fn an_unquoted_instant_stops_at_whitespace() {
        let parsed = parse_qualifier("@due:2026-07-01 Ship the migration.");
        assert_eq!(parsed.time.expect("qualifier").start, "2026-07-01");
        assert_eq!(parsed.body, "Ship the migration.");
    }
}
