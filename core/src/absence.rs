//! Absence lines such as `SYG: Anna` and the setup that names them.
//!
//! A line in a shift's notes or comments that starts with a configured word
//! says the planned helper was absent and names who worked instead. Without a
//! time it covers the whole shift; `SYG 8-12: Anna` covers only 8–12. The
//! planner turns each part into a sick shift for the planned helper and a
//! shift for the substitute.
use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::{Duration, NaiveDate};
use chrono_tz::Tz;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::parser::{local_interval, IntervalProblem, NOTES_SOURCE_ID};
use crate::{HelperMapping, ParseIssue, ParseIssueCode, SourceShift, TimeInterval};

/// MitHF's own sickness reasons, as its `sygemeld` action names them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbsenceReason {
    OwnIllness,
    ChildIllness,
    WorkInjury,
    OtherAbsence,
}

impl AbsenceReason {
    pub const ALL: [Self; 4] = [
        Self::OwnIllness,
        Self::ChildIllness,
        Self::WorkInjury,
        Self::OtherAbsence,
    ];

    /// The `aarsag` value MitHF's `sygemeld` expects.
    pub fn mithf_code(self) -> &'static str {
        match self {
            Self::OwnIllness => "1",
            Self::ChildIllness => "2",
            Self::WorkInjury => "3",
            Self::OtherAbsence => "5",
        }
    }

    /// MitHF's own label for the reason.
    pub fn label(self) -> &'static str {
        match self {
            Self::OwnIllness => "Egen sygdom",
            Self::ChildIllness => "Barns sygdom",
            Self::WorkInjury => "Arbejdsulykke",
            Self::OtherAbsence => "Andet fravær",
        }
    }

    fn default_words(self) -> &'static [&'static str] {
        match self {
            Self::OwnIllness => &["syg", "sygdom", "egen sygdom"],
            Self::ChildIllness => &["barn syg", "barns sygdom", "syg barn"],
            Self::WorkInjury => &["arbejdsskade", "arbejdsulykke"],
            Self::OtherAbsence => &["fravær", "andet fravær"],
        }
    }
}

/// How the absent helper's SPS hours in DUOS are registered. DUOS types
/// differ per arrangement, so there is no default: `Unset` asks the user to
/// choose once SPS hours fall in an absence.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuosAbsence {
    #[default]
    Unset,
    /// The absent helper's hours are not registered in DUOS.
    Skip,
    /// A registration type id of the configured DUOS arrangement.
    Type(String),
}

/// One absence reason and the words that start its lines.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AbsenceMarking {
    pub reason: AbsenceReason,
    /// No words turns the marking off.
    pub words: Vec<String>,
    #[serde(default)]
    pub duos: DuosAbsence,
}

/// Markings for a setup that has not chosen any, including one saved before
/// absences existed.
pub fn default_absences() -> Vec<AbsenceMarking> {
    AbsenceReason::ALL
        .map(|reason| AbsenceMarking {
            reason,
            words: reason.default_words().iter().map(|&w| w.into()).collect(),
            duos: DuosAbsence::Unset,
        })
        .to_vec()
}

/// Clean the words and require exactly one marking per reason. A word used by
/// two reasons would make a line mean either, so it is refused.
pub fn absence_markings(markings: &[AbsenceMarking]) -> Result<Vec<AbsenceMarking>, &'static str> {
    let mut cleaned = Vec::with_capacity(AbsenceReason::ALL.len());
    let mut seen: BTreeMap<String, AbsenceReason> = BTreeMap::new();
    for reason in AbsenceReason::ALL {
        let mut found = markings.iter().filter(|m| m.reason == reason);
        let (Some(marking), None) = (found.next(), found.next()) else {
            return Err("Hver fraværsårsag skal stå præcis én gang.");
        };
        let mut words = Vec::new();
        for word in &marking.words {
            let word = word.split_whitespace().collect::<Vec<_>>().join(" ");
            if word.is_empty() {
                continue;
            }
            if word.chars().count() > 40 {
                return Err("Et fraværsord må højst være 40 tegn.");
            }
            if !word.chars().next().is_some_and(char::is_alphabetic) {
                return Err("Et fraværsord skal begynde med et bogstav.");
            }
            match seen.insert(fold(&word), reason) {
                Some(other) if other != reason => {
                    return Err("Det samme ord kan ikke bruges til to fraværsårsager.");
                }
                Some(_) => continue,
                None => words.push(word),
            }
        }
        if let DuosAbsence::Type(id) = &marking.duos {
            if id.trim().is_empty() {
                return Err("Vælg en DUOS-type for fraværet.");
            }
        }
        cleaned.push(AbsenceMarking {
            reason,
            words,
            duos: marking.duos.clone(),
        });
    }
    Ok(cleaned)
}

/// A part of a shift the planned helper missed, and who worked it instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AbsencePart {
    pub reason: AbsenceReason,
    /// The substitute's source helper key.
    pub substitute: String,
    pub interval: TimeInterval,
    pub source_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AbsenceParseResult {
    /// Sorted by start and never overlapping.
    pub parts: Vec<AbsencePart>,
    pub issues: Vec<ParseIssue>,
}

// After the marking word: an optional ISO date and times, an optional
// separator, the name, and optional filler such as "tog den".
static REST: LazyLock<Regex> = LazyLock::new(|| {
    let clock = r"\d{1,2}(?:[:.]\d{2})?";
    let interval = format!(r"{clock}\s*[-–]\s*{clock}");
    Regex::new(&format!(
        r"^(?:\s+(?P<day>\d{{4}}-\d{{2}}-\d{{2}}))?(?:\s*(?P<times>{interval}(?:\s*&\s*{interval})*))?\s*[:\-–]?\s*(?P<name>.*?)\s*$"
    ))
    .expect("valid absence regex")
});
static FILLER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\s+(?:tog|tager|dækker|daekker|overtog|overtager)(?:\s+(?:den|vagten|over))?)?[\s.!]*$")
        .expect("valid filler regex")
});

/// Read absence lines from the shift's notes and comments. Lines that start
/// with a marking word but cannot be read become issues, never silence.
pub fn parse_absences(
    shift: &SourceShift,
    markings: &[AbsenceMarking],
    helpers: &BTreeMap<String, HelperMapping>,
    timezone: Tz,
) -> AbsenceParseResult {
    let mut result = AbsenceParseResult::default();
    let mut words: Vec<(String, AbsenceReason)> = markings
        .iter()
        .flat_map(|m| m.words.iter().map(|w| (fold(w), m.reason)))
        .filter(|(w, _)| !w.is_empty())
        .collect();
    // The longest word wins, so "barn syg" is never read as "barn" + "syg".
    words.sort_by_key(|(w, _)| std::cmp::Reverse(w.chars().count()));
    let blocks = std::iter::once((NOTES_SOURCE_ID, shift.notes.as_str())).chain(
        shift
            .comments
            .iter()
            .map(|c| (c.id.as_str(), c.text.as_str())),
    );
    for (source_id, text) in blocks {
        for line in text.lines() {
            let line = fold(line);
            let Some((reason, rest)) = words.iter().find_map(|(word, reason)| {
                line.strip_prefix(word.as_str())
                    .filter(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
                    .map(|rest| (*reason, rest))
            }) else {
                continue;
            };
            let mut report = |code, message: String| {
                result.issues.push(ParseIssue {
                    code,
                    message,
                    source_id: source_id.into(),
                })
            };
            let Some(captures) = REST.captures(rest) else {
                report(
                    ParseIssueCode::UnreadableAbsence,
                    "An absence line does not match the supported syntax".into(),
                );
                continue;
            };
            let name = FILLER.replace(&captures["name"], "");
            if name.is_empty() {
                report(
                    ParseIssueCode::UnreadableAbsence,
                    "An absence line does not name who took the shift".into(),
                );
                continue;
            }
            let substitute = match find_helper(&name, helpers) {
                Ok(key) => key,
                Err(code) => {
                    report(
                        code,
                        "The helper named in an absence line is not unique or not set up".into(),
                    );
                    continue;
                }
            };
            if substitute == shift.helper_key {
                report(
                    ParseIssueCode::AbsenceHelperIsPlanned,
                    "An absence line names the shift's own helper".into(),
                );
                continue;
            }
            let Some(times) = captures.name("times") else {
                if captures.name("day").is_some() {
                    report(
                        ParseIssueCode::UnreadableAbsence,
                        "An absence line has a date but no time".into(),
                    );
                    continue;
                }
                result.parts.push(AbsencePart {
                    reason,
                    substitute,
                    interval: TimeInterval {
                        starts_at: shift.starts_at,
                        ends_at: shift.ends_at,
                    },
                    source_id: source_id.into(),
                });
                continue;
            };
            let day = match captures.name("day") {
                Some(day) => match NaiveDate::parse_from_str(day.as_str(), "%Y-%m-%d") {
                    Ok(day) => day,
                    Err(_) => {
                        report(
                            ParseIssueCode::UnreadableAbsence,
                            "Invalid absence date".into(),
                        );
                        continue;
                    }
                },
                None => {
                    let first = shift.starts_at.with_timezone(&timezone).date_naive();
                    let last = (shift.ends_at - Duration::microseconds(1))
                        .with_timezone(&timezone)
                        .date_naive();
                    if first != last {
                        report(
                            ParseIssueCode::AmbiguousAbsenceDate,
                            "An undated absence time belongs to a shift spanning more than one local date".into(),
                        );
                        continue;
                    }
                    first
                }
            };
            for token in times.as_str().split('&') {
                let token = token.trim().replace('.', ":");
                let interval = match local_interval(&token, day, timezone) {
                    Ok(interval) => interval,
                    Err(problem) => {
                        let code = match problem {
                            IntervalProblem::Unreadable => ParseIssueCode::UnreadableAbsence,
                            IntervalProblem::Nonexistent => ParseIssueCode::NonexistentLocalTime,
                            IntervalProblem::Ambiguous => ParseIssueCode::AmbiguousLocalTime,
                        };
                        report(
                            code,
                            format!("Absence time {token:?} cannot be read safely"),
                        );
                        continue;
                    }
                };
                if interval.starts_at < shift.starts_at || interval.ends_at > shift.ends_at {
                    report(
                        ParseIssueCode::AbsenceOutsideShift,
                        format!("Absence time {token:?} is outside the source shift"),
                    );
                    continue;
                }
                result.parts.push(AbsencePart {
                    reason,
                    substitute: substitute.clone(),
                    interval,
                    source_id: source_id.into(),
                });
            }
        }
    }
    result.parts.sort_by_key(|p| p.interval.starts_at);
    if result
        .parts
        .windows(2)
        .any(|pair| pair[1].interval.starts_at < pair[0].interval.ends_at)
    {
        result.issues.push(ParseIssue {
            code: ParseIssueCode::OverlappingAbsences,
            message: "Absence lines on this shift cover the same time".into(),
            source_id: NOTES_SOURCE_ID.into(),
        });
        result.parts.clear();
    }
    result
}

/// A full name wins over a first name; a first name shared by two helpers is
/// ambiguous rather than guessed.
fn find_helper(
    name: &str,
    helpers: &BTreeMap<String, HelperMapping>,
) -> Result<String, ParseIssueCode> {
    let names = |mapping: &HelperMapping| {
        [mapping.source_name.as_str(), mapping.mithf_name.as_str()]
            .into_iter()
            .filter(|n| !n.is_empty())
            .map(fold)
            .collect::<Vec<_>>()
    };
    let matching = |first_only: bool| {
        helpers
            .iter()
            .filter(|(_, mapping)| {
                names(mapping).iter().any(|full| {
                    if first_only {
                        full.split(' ').next() == Some(name)
                    } else {
                        full == name
                    }
                })
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>()
    };
    for first_only in [false, true] {
        match matching(first_only).as_slice() {
            [key] => return Ok(key.clone()),
            [] => {}
            _ => return Err(ParseIssueCode::AmbiguousAbsenceHelper),
        }
    }
    Err(ParseIssueCode::UnknownAbsenceHelper)
}

/// Lowercase, single-spaced and without accents, so `Barns  Sygdom` and
/// `José` match `barns sygdom` and `jose`. Danish æ, ø and å are letters,
/// not accents, and stay.
fn fold(text: &str) -> String {
    let folded: String = text
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            c => c,
        })
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use chrono_tz::Europe::Copenhagen;

    fn helpers() -> BTreeMap<String, HelperMapping> {
        [
            ("bo", "Bo Hansen"),
            ("anna", "Anna Holm"),
            ("anders1", "Anders Berg"),
            ("anders2", "Anders Kjær"),
        ]
        .into_iter()
        .map(|(key, name)| {
            (
                key.into(),
                HelperMapping {
                    mithf_name: name.into(),
                    duos_employee_number: String::new(),
                    source_name: name.into(),
                },
            )
        })
        .collect()
    }

    fn shift(notes: &str) -> SourceShift {
        SourceShift {
            calendar_id: "c".into(),
            event_id: "e".into(),
            occurrence_id: "o".into(),
            title: "Vagt".into(),
            helper_key: "bo".into(),
            starts_at: DateTime::parse_from_rfc3339("2026-09-28T08:00:00+02:00").unwrap(),
            ends_at: DateTime::parse_from_rfc3339("2026-09-28T16:00:00+02:00").unwrap(),
            notes: notes.into(),
            comments: vec![],
            recurrence_start: None,
            source_version: None,
            standard_time: false,
        }
    }

    fn parse(notes: &str) -> AbsenceParseResult {
        parse_absences(&shift(notes), &default_absences(), &helpers(), Copenhagen)
    }

    fn hours(part: &AbsencePart) -> (u32, u32) {
        use chrono::Timelike;
        let local = |t: DateTime<chrono::FixedOffset>| t.with_timezone(&Copenhagen).hour();
        (local(part.interval.starts_at), local(part.interval.ends_at))
    }

    #[test]
    fn loose_spellings_all_mean_the_same_whole_shift_absence() {
        for line in [
            "SYG: Anna",
            "syg anna",
            "Syg - Anna tog den",
            "SYG:Anna Holm",
            "  syg   ANNA  tager vagten.",
        ] {
            let parsed = parse(line);
            assert!(parsed.issues.is_empty(), "{line}: {:?}", parsed.issues);
            assert_eq!(parsed.parts.len(), 1, "{line}");
            assert_eq!(parsed.parts[0].substitute, "anna", "{line}");
            assert_eq!(parsed.parts[0].reason, AbsenceReason::OwnIllness, "{line}");
            assert_eq!(hours(&parsed.parts[0]), (8, 16), "{line}");
        }
    }

    #[test]
    fn the_longest_word_wins_and_a_time_limits_the_part() {
        let parsed = parse("Barn syg 8-12: Anna\nnoget andet");
        assert!(parsed.issues.is_empty());
        assert_eq!(parsed.parts[0].reason, AbsenceReason::ChildIllness);
        assert_eq!(hours(&parsed.parts[0]), (8, 12));

        let parsed = parse("syg 8-10 & 14-16 anna");
        assert_eq!(
            parsed.parts.iter().map(hours).collect::<Vec<_>>(),
            [(8, 10), (14, 16)]
        );
    }

    #[test]
    fn words_inside_other_words_and_other_lines_are_ignored() {
        assert_eq!(
            parse("Sygeplejerske kommer kl 10\nuni 9-10"),
            AbsenceParseResult::default()
        );
    }

    #[test]
    fn unreadable_lines_are_issues_not_silence() {
        let code = |notes: &str| parse(notes).issues.first().map(|i| i.code);
        assert_eq!(code("syg"), Some(ParseIssueCode::UnreadableAbsence));
        assert_eq!(
            code("syg: Carl"),
            Some(ParseIssueCode::UnknownAbsenceHelper)
        );
        assert_eq!(
            code("syg: Anders"),
            Some(ParseIssueCode::AmbiguousAbsenceHelper)
        );
        assert_eq!(
            code("syg: Bo"),
            Some(ParseIssueCode::AbsenceHelperIsPlanned)
        );
        assert_eq!(
            code("syg 6-9: Anna"),
            Some(ParseIssueCode::AbsenceOutsideShift)
        );
        assert_eq!(
            code("syg 25-26: Anna"),
            Some(ParseIssueCode::UnreadableAbsence)
        );
        assert_eq!(
            code("syg: Anna\nbarn syg 9-10: Anna"),
            Some(ParseIssueCode::OverlappingAbsences)
        );
        assert!(parse("syg: Anders Kjær").issues.is_empty());
    }

    #[test]
    fn a_word_cannot_mean_two_reasons() {
        let mut markings = default_absences();
        markings[1].words.push("Syg".into());
        assert!(absence_markings(&markings).is_err());
        markings[1].words.pop();
        markings[0].words.push("  SYG ".into());
        assert_eq!(
            absence_markings(&markings).unwrap()[0].words,
            ["syg", "sygdom", "egen sygdom"]
        );
    }
}
