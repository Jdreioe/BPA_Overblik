use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceComment {
    pub id: String,
    pub text: String,
    pub updated_at: Option<DateTime<FixedOffset>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceShift {
    pub calendar_id: String,
    pub event_id: String,
    pub occurrence_id: String,
    pub title: String,
    pub helper_key: String,
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    /// TeamUp event description, where SPS instructions are normally written.
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub comments: Vec<SourceComment>,
    pub recurrence_start: Option<DateTime<FixedOffset>>,
    pub source_version: Option<String>,
    /// The source supplied no hours, so setup supplied this interval.
    #[serde(default, skip_serializing_if = "is_false")]
    pub standard_time: bool,
    /// The other helpers the source names on this shift, as in `Anna / Bo`.
    /// The source cannot say whose hours, SPS or absence are whose, so the
    /// shift waits until each part is checked in the app.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_with: Vec<String>,
    /// Checked in the app since the source last changed it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirmed: bool,
}

/// A calendar note on a helper's calendar, such as a day-off wish. It is shown
/// in the week but never planned or transferred, and its text is never read
/// for hours.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceMarker {
    pub helper_key: String,
    pub title: String,
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    pub all_day: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl SourceShift {
    pub fn key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.calendar_id, self.event_id, self.occurrence_id
        )
    }

    /// One shift per helper key. The first keeps this shift's identity, so
    /// naming a second helper beside the first leaves the first's transfer in
    /// place; the others are told apart by their helper.
    pub fn for_helpers(self, keys: &[String]) -> Vec<SourceShift> {
        keys.iter()
            .enumerate()
            .map(|(index, key)| {
                let mut shift = self.clone();
                if index > 0 {
                    shift.occurrence_id = format!("{}+{key}", self.occurrence_id);
                }
                shift.helper_key = key.clone();
                shift.shared_with = keys.iter().filter(|other| *other != key).cloned().collect();
                shift
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimeInterval {
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
}

impl TimeInterval {
    pub fn hours(&self) -> f64 {
        (self.ends_at.timestamp_micros() - self.starts_at.timestamp_micros()) as f64
            / 3_600_000_000.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SpsInterval {
    pub key: String,
    /// `notes` for the event description, otherwise the TeamUp comment id.
    pub source_id: String,
    pub ordinal: usize,
    pub interval: TimeInterval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseIssueCode {
    UnsupportedUniSyntax,
    InvalidUniDate,
    UniWeekdayOutsideShift,
    AmbiguousUniWeekday,
    AmbiguousUniDate,
    ConflictingUniDate,
    InvalidUniInterval,
    NonexistentLocalTime,
    AmbiguousLocalTime,
    UniOutsideShift,
    DuplicateUniInterval,
    OverlappingUniIntervals,
    UnreadableAbsence,
    AmbiguousAbsenceDate,
    AbsenceOutsideShift,
    OverlappingAbsences,
    UnknownAbsenceHelper,
    AmbiguousAbsenceHelper,
    AbsenceHelperIsPlanned,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseIssue {
    pub code: ParseIssueCode,
    pub message: String,
    pub source_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SpsParseResult {
    pub intervals: Vec<SpsInterval>,
    pub issues: Vec<ParseIssue>,
    pub relevant_source_ids: Vec<String>,
}
