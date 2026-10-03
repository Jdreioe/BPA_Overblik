//! Shift times changed in the app instead of in the shift source.
//!
//! An edit remembers the times the source had when it was made. It replaces
//! them only while the source still has those times: once the source itself
//! changes them, the source is newer than the edit, so the source wins and
//! the edit is dropped.

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::SourceShift;

/// A shift's times as the source gave them, before any edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceTimes {
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    /// The source gave no hours, so they came from Vagter uden tid.
    pub standard_time: bool,
}

impl SourceTimes {
    pub fn of(shift: &SourceShift) -> Self {
        Self {
            starts_at: shift.starts_at,
            ends_at: shift.ends_at,
            standard_time: shift.standard_time,
        }
    }

    /// Whether the source still says what it said when the edit was made. A
    /// shift without a time only has a date in the source, so a change to
    /// Vagter uden tid is not a change in the source.
    fn unchanged(&self, shift: &SourceShift) -> bool {
        if self.standard_time && shift.standard_time {
            self.starts_at.date_naive() == shift.starts_at.date_naive()
        } else {
            *self == Self::of(shift)
        }
    }
}

/// New times for one source shift, chosen in the app.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShiftEdit {
    pub source_key: String,
    /// What the source said when the edit was made.
    pub source: SourceTimes,
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
}

/// What [`apply_shift_edits`] did with the saved edits.
#[derive(Debug, Default, PartialEq)]
pub struct AppliedEdits {
    /// Edits now in the shifts' times.
    pub applied: Vec<ShiftEdit>,
    /// Keys of edits whose source has changed since. Forget them, so an
    /// edit never comes back if the source later returns to the old times.
    pub outdated: Vec<String>,
}

/// Give each shift its edited times while its source is unchanged. Edits for
/// shifts outside `shifts` are left alone: they belong to another week.
pub fn apply_shift_edits(shifts: &mut [SourceShift], edits: Vec<ShiftEdit>) -> AppliedEdits {
    let mut result = AppliedEdits::default();
    for edit in edits {
        let Some(shift) = shifts.iter_mut().find(|s| s.key() == edit.source_key) else {
            continue;
        };
        if !edit.source.unchanged(shift) {
            result.outdated.push(edit.source_key);
            continue;
        }
        shift.starts_at = edit.starts_at;
        shift.ends_at = edit.ends_at;
        shift.standard_time = false;
        result.applied.push(edit);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).unwrap()
    }

    fn shift(starts_at: &str, ends_at: &str, standard_time: bool) -> SourceShift {
        SourceShift {
            calendar_id: "cal".into(),
            event_id: "event".into(),
            occurrence_id: "1".into(),
            title: "Vagt".into(),
            helper_key: "anna".into(),
            starts_at: at(starts_at),
            ends_at: at(ends_at),
            notes: String::new(),
            comments: vec![],
            recurrence_start: None,
            source_version: None,
            standard_time,
        }
    }

    fn edit(source: &SourceShift) -> ShiftEdit {
        ShiftEdit {
            source_key: source.key(),
            source: SourceTimes::of(source),
            starts_at: at("2026-09-15T09:00:00+02:00"),
            ends_at: at("2026-09-15T17:00:00+02:00"),
        }
    }

    #[test]
    fn an_edit_replaces_the_times_while_the_source_is_unchanged() {
        let read = shift(
            "2026-09-14T08:00:00+02:00",
            "2026-09-14T16:00:00+02:00",
            false,
        );
        let mut shifts = vec![read.clone()];
        let applied = apply_shift_edits(&mut shifts, vec![edit(&read)]);
        assert_eq!(applied.applied, [edit(&read)]);
        assert!(applied.outdated.is_empty());
        assert_eq!(shifts[0].starts_at, at("2026-09-15T09:00:00+02:00"));
        assert_eq!(shifts[0].ends_at, at("2026-09-15T17:00:00+02:00"));
    }

    #[test]
    fn a_source_changed_after_the_edit_wins() {
        let before = shift(
            "2026-09-14T08:00:00+02:00",
            "2026-09-14T16:00:00+02:00",
            false,
        );
        let after = shift(
            "2026-09-14T08:00:00+02:00",
            "2026-09-14T18:00:00+02:00",
            false,
        );
        let mut shifts = vec![after.clone()];
        let applied = apply_shift_edits(&mut shifts, vec![edit(&before)]);
        assert!(applied.applied.is_empty());
        assert_eq!(applied.outdated, [before.key()]);
        assert_eq!(shifts, [after]);
    }

    #[test]
    fn a_shift_without_a_time_keeps_its_edit_when_vagter_uden_tid_changes() {
        let before = shift(
            "2026-09-14T08:00:00+02:00",
            "2026-09-14T16:00:00+02:00",
            true,
        );
        let mut shifts = vec![shift(
            "2026-09-14T06:00:00+02:00",
            "2026-09-14T22:00:00+02:00",
            true,
        )];
        let applied = apply_shift_edits(&mut shifts, vec![edit(&before)]);
        assert_eq!(applied.applied.len(), 1);
        assert!(!shifts[0].standard_time);
        // Given hours of its own, the source has changed.
        let mut shifts = vec![shift(
            "2026-09-14T08:00:00+02:00",
            "2026-09-14T16:00:00+02:00",
            false,
        )];
        let applied = apply_shift_edits(&mut shifts, vec![edit(&before)]);
        assert_eq!(applied.outdated, [before.key()]);
    }

    #[test]
    fn edits_for_other_weeks_are_left_alone() {
        let read = shift(
            "2026-09-14T08:00:00+02:00",
            "2026-09-14T16:00:00+02:00",
            false,
        );
        let applied = apply_shift_edits(&mut [], vec![edit(&read)]);
        assert_eq!(applied, AppliedEdits::default());
    }
}
