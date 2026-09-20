use std::collections::HashSet;
use std::sync::LazyLock;

use chrono::{
    DateTime, Datelike, Days, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone,
};
use chrono_tz::Tz;
use dec_from_char::DecimalExtended;
use regex::Regex;

use crate::{ParseIssue, ParseIssueCode, SourceShift, SpsInterval, SpsParseResult, TimeInterval};

/// Identity used for instructions in the event description, which has no TeamUp id.
pub const NOTES_SOURCE_ID: &str = "notes";

static UNI_IN_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\buni\b").expect("valid uni search regex"));
static UNI_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)^\s*uni(?:\s+(?P<day>\d{4}-\d{2}-\d{2}))?\s+(?P<body>.+?)(?:\s+(?P<weekday>mandag|man|tirsdag|tirs|tir|onsdag|ons|torsdag|tors|tor|fredag|fre|lørdag|lordag|lør|lor|søndag|sondag|søn|son)\.?)?\s*$",
    )
    .expect("valid uni instruction regex")
});
static INTERVAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<start_hour>[01]?\d|2[0-3])(?::(?P<start_minute>[0-5]\d))?\s*[-–]\s*(?P<end_hour>[01]?\d|2[0-3])(?::(?P<end_minute>[0-5]\d))?$",
    )
    .expect("valid interval regex")
});

pub fn parse_sps_instructions(shift: &SourceShift, timezone: Tz) -> SpsParseResult {
    let mut result = SpsParseResult::default();
    let local_start = shift.starts_at.with_timezone(&timezone);
    let local_end_inclusive = (shift.ends_at - Duration::microseconds(1)).with_timezone(&timezone);
    let days = ShiftDays {
        start: local_start.date_naive(),
        end: local_end_inclusive.date_naive(),
        implicit: (local_start.date_naive() == local_end_inclusive.date_naive())
            .then_some(local_start.date_naive()),
    };

    parse_block(
        NOTES_SOURCE_ID,
        &shift.notes,
        shift,
        timezone,
        days,
        &mut result,
    );
    for comment in &shift.comments {
        parse_block(
            &comment.id,
            &comment.text,
            shift,
            timezone,
            days,
            &mut result,
        );
    }

    result.intervals.sort_by_key(|item| item.interval.starts_at);
    let mut accepted: Vec<SpsInterval> = Vec::with_capacity(result.intervals.len());
    for current in result.intervals.drain(..) {
        if let Some(previous) = accepted.last() {
            if current.interval == previous.interval {
                result.issues.push(issue(
                    ParseIssueCode::DuplicateUniInterval,
                    "The same SPS interval appears more than once; it will be planned once",
                    &current.source_id,
                ));
                continue;
            }
            if current.interval.starts_at < previous.interval.ends_at {
                result.issues.push(issue(
                    ParseIssueCode::OverlappingUniIntervals,
                    "SPS intervals overlap and require review",
                    &current.source_id,
                ));
                continue;
            }
        }
        accepted.push(current);
    }
    result.intervals = accepted;

    let mut seen = HashSet::new();
    result
        .relevant_source_ids
        .retain(|source_id| seen.insert(source_id.clone()));
    result
}

#[derive(Clone, Copy)]
struct ShiftDays {
    start: NaiveDate,
    end: NaiveDate,
    implicit: Option<NaiveDate>,
}

fn parse_block(
    source_id: &str,
    text: &str,
    shift: &SourceShift,
    timezone: Tz,
    days: ShiftDays,
    result: &mut SpsParseResult,
) {
    let lines: Vec<_> = text
        .lines()
        .filter(|line| UNI_IN_LINE.is_match(line))
        .collect();
    if lines.is_empty() {
        return;
    }

    result.relevant_source_ids.push(source_id.to_owned());
    let mut block_ordinal = 0;
    for (line_index, line) in lines.into_iter().enumerate() {
        let Some(captures) = UNI_LINE.captures(line) else {
            result.issues.push(issue(
                ParseIssueCode::UnsupportedUniSyntax,
                format!(
                    "{source_id} line {} contains 'uni' but does not match the supported syntax",
                    line_index + 1
                ),
                source_id,
            ));
            continue;
        };

        let explicit_day = captures.name("day").map(|value| value.as_str());
        let weekday = captures.name("weekday").map(|value| value.as_str());
        let interval_day = if let Some(value) = explicit_day {
            match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
                Ok(day) => day,
                Err(_) => {
                    result.issues.push(issue(
                        ParseIssueCode::InvalidUniDate,
                        format!("Invalid SPS date: {value}"),
                        source_id,
                    ));
                    continue;
                }
            }
        } else if let Some(value) = weekday {
            let target = weekday_index(value).expect("weekday capture has a known spelling");
            let current = days.start.weekday().num_days_from_monday();
            let offset = (target + 7 - current) % 7;
            let day = days
                .start
                .checked_add_days(Days::new(offset.into()))
                .expect("a weekday offset is representable");
            if day > days.end {
                result.issues.push(issue(
                    ParseIssueCode::UniWeekdayOutsideShift,
                    "The named weekday is outside the source shift",
                    source_id,
                ));
                continue;
            }
            if day
                .checked_add_days(Days::new(7))
                .is_some_and(|next| next <= days.end)
            {
                result.issues.push(issue(
                    ParseIssueCode::AmbiguousUniWeekday,
                    "The shift contains this weekday more than once; use an ISO date",
                    source_id,
                ));
                continue;
            }
            day
        } else if let Some(day) = days.implicit {
            day
        } else {
            result.issues.push(issue(
                ParseIssueCode::AmbiguousUniDate,
                "An undated SPS instruction belongs to a shift spanning more than one local date",
                source_id,
            ));
            continue;
        };

        if let Some(value) = weekday {
            let target = weekday_index(value).expect("weekday capture has a known spelling");
            if interval_day.weekday().num_days_from_monday() != target {
                result.issues.push(issue(
                    ParseIssueCode::ConflictingUniDate,
                    "The explicit date and weekday disagree",
                    source_id,
                ));
                continue;
            }
        }

        for token in captures["body"].split('&') {
            let ordinal = block_ordinal;
            block_ordinal += 1;
            let token = token.trim();
            let Some(interval) = parse_interval(token, interval_day, timezone) else {
                result.issues.push(issue(
                    ParseIssueCode::InvalidUniInterval,
                    format!("Unsupported SPS interval: {token:?}"),
                    source_id,
                ));
                continue;
            };

            let (start, end) = match interval {
                LocalizedInterval::Valid(start, end) => (start, end),
                LocalizedInterval::Invalid(problem) => {
                    let (code, label) = match problem {
                        LocalTimeProblem::Nonexistent => {
                            (ParseIssueCode::NonexistentLocalTime, "nonexistent")
                        }
                        LocalTimeProblem::Ambiguous => {
                            (ParseIssueCode::AmbiguousLocalTime, "ambiguous")
                        }
                    };
                    result.issues.push(issue(
                        code,
                        format!(
                            "SPS interval {token:?} contains an {label} local time in {timezone}"
                        ),
                        source_id,
                    ));
                    continue;
                }
            };

            if start < shift.starts_at || end > shift.ends_at {
                result.issues.push(issue(
                    ParseIssueCode::UniOutsideShift,
                    format!("SPS interval {token:?} is outside the source shift"),
                    source_id,
                ));
                continue;
            }
            result.intervals.push(SpsInterval {
                key: format!("{source_id}:{ordinal}"),
                source_id: source_id.to_owned(),
                ordinal,
                interval: TimeInterval {
                    starts_at: start,
                    ends_at: end,
                },
            });
        }
    }
}

enum LocalizedInterval {
    Valid(DateTime<chrono::FixedOffset>, DateTime<chrono::FixedOffset>),
    Invalid(LocalTimeProblem),
}

#[derive(Clone, Copy)]
enum LocalTimeProblem {
    Nonexistent,
    Ambiguous,
}

fn parse_interval(token: &str, day: NaiveDate, timezone: Tz) -> Option<LocalizedInterval> {
    let captures = INTERVAL.captures(token)?;
    let start_clock = clock(&captures, "start_hour", "start_minute")?;
    let end_clock = clock(&captures, "end_hour", "end_minute")?;
    let end_day = if end_clock <= start_clock {
        day.checked_add_days(Days::new(1))?
    } else {
        day
    };

    let start = match localize(day.and_time(start_clock), timezone) {
        Ok(value) => value,
        Err(problem) => return Some(LocalizedInterval::Invalid(problem)),
    };
    let end = match localize(end_day.and_time(end_clock), timezone) {
        Ok(value) => value,
        Err(problem) => return Some(LocalizedInterval::Invalid(problem)),
    };
    Some(LocalizedInterval::Valid(start, end))
}

fn clock(captures: &regex::Captures<'_>, hour: &str, minute: &str) -> Option<NaiveTime> {
    // Python's int() accepts the Unicode decimal digits matched by \d.
    // Convert only after matching so the existing clock grammar stays unchanged.
    let decimal = |value: &str| {
        value.chars().try_fold(0_u32, |number, digit| {
            Some(number * 10 + u32::from(digit.to_decimal_utf8()?))
        })
    };
    let hour = decimal(&captures[hour])?;
    let minute = match captures.name(minute) {
        Some(value) => decimal(value.as_str())?,
        None => 0,
    };
    NaiveTime::from_hms_opt(hour, minute, 0)
}

fn localize(
    local: NaiveDateTime,
    timezone: Tz,
) -> Result<DateTime<chrono::FixedOffset>, LocalTimeProblem> {
    match timezone.from_local_datetime(&local) {
        LocalResult::None => Err(LocalTimeProblem::Nonexistent),
        LocalResult::Ambiguous(_, _) => Err(LocalTimeProblem::Ambiguous),
        LocalResult::Single(value) => Ok(value.fixed_offset()),
    }
}

fn weekday_index(value: &str) -> Option<u32> {
    match value.trim_end_matches('.').to_lowercase().as_str() {
        "mandag" | "man" => Some(0),
        "tirsdag" | "tirs" | "tir" => Some(1),
        "onsdag" | "ons" => Some(2),
        "torsdag" | "tors" | "tor" => Some(3),
        "fredag" | "fre" => Some(4),
        "lørdag" | "lordag" | "lør" | "lor" => Some(5),
        "søndag" | "sondag" | "søn" | "son" => Some(6),
        _ => None,
    }
}

fn issue(code: ParseIssueCode, message: impl Into<String>, source_id: &str) -> ParseIssue {
    ParseIssue {
        code,
        message: message.into(),
        source_id: source_id.to_owned(),
    }
}
