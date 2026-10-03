//! Shift changes made in the app instead of in the shift source: its times,
//! its helper, added SPS and an absence.
//!
//! An edit remembers what the source said when it was made. Each part
//! replaces the source only while the source still says that: once the
//! source itself changes that part, the source is newer than the edit, so the
//! source wins and the part is dropped.
//!
//! SPS and absences reach the planner as the same lines a coordinator writes
//! in the source, in a comment of their own, so they are read, checked and
//! shown exactly like the source's.

use chrono::{DateTime, Duration, FixedOffset, NaiveTime};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{AbsenceReason, PlanningConfig, SourceComment, SourceShift};

/// The comment holding the SPS and absence lines added in the app.
pub const APP_COMMENT_ID: &str = "app";

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

    /// Whether the source still has these times. A shift without a time
    /// only has a date in the source, so a change to Vagter uden tid is not
    /// a change in the source.
    fn unchanged(&self, shift: &SourceShift) -> bool {
        if self.standard_time && shift.standard_time {
            self.starts_at.date_naive() == shift.starts_at.date_naive()
        } else {
            *self == Self::of(shift)
        }
    }
}

/// What the source said about a shift when it was edited.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    pub times: SourceTimes,
    pub helper_key: String,
    /// A digest of the shift's notes and comments, where SPS and absences
    /// are written.
    pub text: String,
}

impl SourceSnapshot {
    pub fn of(shift: &SourceShift) -> Self {
        let mut text = Sha256::new();
        text.update(shift.notes.as_bytes());
        for comment in &shift.comments {
            text.update([0]);
            text.update(comment.id.as_bytes());
            text.update([0]);
            text.update(comment.text.as_bytes());
        }
        Self {
            times: SourceTimes::of(shift),
            helper_key: shift.helper_key.clone(),
            text: format!("{:x}", text.finalize()),
        }
    }
}

/// The planned helper was absent, and `substitute` worked the whole shift.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AbsenceEdit {
    pub reason: AbsenceReason,
    pub substitute: String,
}

/// One shift's changes made in the app. Parts left out keep the source's.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShiftEdit {
    pub source_key: String,
    pub source: SourceSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub times: Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper_key: Option<String>,
    /// SPS added to the source's, as clock times within the shift.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sps: Vec<(NaiveTime, NaiveTime)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absence: Option<AbsenceEdit>,
}

impl ShiftEdit {
    pub fn is_empty(&self) -> bool {
        self.times.is_none()
            && self.helper_key.is_none()
            && self.sps.is_empty()
            && self.absence.is_none()
    }

    /// Drop the parts the source has changed since.
    fn current(mut self, shift: &SourceShift) -> Self {
        let now = SourceSnapshot::of(shift);
        if !self.source.times.unchanged(shift) {
            self.times = None;
        }
        if self.source.helper_key != now.helper_key {
            self.helper_key = None;
        }
        if self.source.text != now.text {
            self.sps.clear();
            self.absence = None;
        }
        self
    }

    /// The lines a coordinator would write for the added SPS and absence.
    /// SPS before the shift's start time is on the next day, as in a shift
    /// over midnight. An absence reason without words cannot be written.
    fn lines(&self, shift: &SourceShift, config: &PlanningConfig) -> Vec<String> {
        let zone: Tz = config.timezone;
        let start = shift.starts_at.with_timezone(&zone);
        let mut lines: Vec<String> = self
            .sps
            .iter()
            .map(|(from, to)| {
                let day = if *from >= start.time() {
                    start.date_naive()
                } else {
                    start.date_naive() + Duration::days(1)
                };
                format!(
                    "uni {} {}-{}",
                    day.format("%Y-%m-%d"),
                    from.format("%H:%M"),
                    to.format("%H:%M")
                )
            })
            .collect();
        if let Some(absence) = &self.absence {
            let word = config
                .absences
                .iter()
                .find(|marking| marking.reason == absence.reason)
                .and_then(|marking| marking.words.first());
            let name = config
                .helpers
                .get(&absence.substitute)
                .map(|helper| helper.mithf_name.as_str());
            if let (Some(word), Some(name)) = (word, name) {
                lines.push(format!("{word}: {name}"));
            }
        }
        lines
    }
}

/// What [`apply_shift_edits`] did with the saved edits.
#[derive(Debug, Default, PartialEq)]
pub struct AppliedEdits {
    /// Edits now in the shifts.
    pub applied: Vec<ShiftEdit>,
    /// Edits that lost a part to a newer source. Save them again, so a part
    /// never comes back if the source later returns to what it said.
    pub reduced: Vec<ShiftEdit>,
    /// Keys of edits the source has replaced entirely. Forget them.
    pub outdated: Vec<String>,
}

/// Give each shift its edits while its source is unchanged. Edits for
/// shifts outside `shifts` are left alone: they belong to another week.
pub fn apply_shift_edits(
    shifts: &mut [SourceShift],
    edits: Vec<ShiftEdit>,
    config: &PlanningConfig,
) -> AppliedEdits {
    let mut result = AppliedEdits::default();
    for edit in edits {
        let Some(shift) = shifts.iter_mut().find(|s| s.key() == edit.source_key) else {
            continue;
        };
        let current = edit.clone().current(shift);
        if current.is_empty() {
            result.outdated.push(current.source_key);
            continue;
        }
        if current != edit {
            result.reduced.push(current.clone());
        }
        // What the source says now, for changing the edit again. The parts
        // left are those it has not changed, so this keeps their meaning.
        let mut current = current;
        current.source = SourceSnapshot::of(shift);
        if let Some((starts_at, ends_at)) = current.times {
            shift.starts_at = starts_at;
            shift.ends_at = ends_at;
            shift.standard_time = false;
        }
        if let Some(helper_key) = &current.helper_key {
            shift.helper_key = helper_key.clone();
        }
        let lines = current.lines(shift, config);
        if !lines.is_empty() {
            shift.comments.push(SourceComment {
                id: APP_COMMENT_ID.into(),
                text: lines.join("\n"),
                updated_at: None,
            });
        }
        result.applied.push(current);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{parse_absences, parse_sps_instructions, HelperMapping};

    fn at(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).unwrap()
    }

    fn clock(value: &str) -> NaiveTime {
        NaiveTime::parse_from_str(value, "%H:%M").unwrap()
    }

    fn config() -> PlanningConfig {
        let helper = |name: &str| HelperMapping {
            mithf_name: name.into(),
            duos_employee_number: "1".into(),
            source_name: String::new(),
        };
        PlanningConfig {
            timezone: chrono_tz::Europe::Copenhagen,
            default_helper_count: 1,
            duos_arrangement_id: "a".into(),
            duos_registration_type: "Almindelig".into(),
            duos_enabled: true,
            helpers: BTreeMap::from([
                ("anna".into(), helper("Anna Hansen")),
                ("bo".into(), helper("Bo Jensen")),
            ]),
            absences: crate::default_absences(),
        }
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

    fn day() -> SourceShift {
        shift(
            "2026-09-14T08:00:00+02:00",
            "2026-09-14T16:00:00+02:00",
            false,
        )
    }

    fn edit(source: &SourceShift) -> ShiftEdit {
        ShiftEdit {
            source_key: source.key(),
            source: SourceSnapshot::of(source),
            times: Some((
                at("2026-09-15T09:00:00+02:00"),
                at("2026-09-15T17:00:00+02:00"),
            )),
            helper_key: None,
            sps: vec![],
            absence: None,
        }
    }

    #[test]
    fn an_edit_replaces_the_times_while_the_source_is_unchanged() {
        let read = day();
        let mut shifts = vec![read.clone()];
        let applied = apply_shift_edits(&mut shifts, vec![edit(&read)], &config());
        assert_eq!(applied.applied, [edit(&read)]);
        assert!(applied.outdated.is_empty() && applied.reduced.is_empty());
        assert_eq!(shifts[0].starts_at, at("2026-09-15T09:00:00+02:00"));
        assert_eq!(shifts[0].ends_at, at("2026-09-15T17:00:00+02:00"));
    }

    #[test]
    fn a_source_changed_after_the_edit_wins() {
        let before = day();
        let after = shift(
            "2026-09-14T08:00:00+02:00",
            "2026-09-14T18:00:00+02:00",
            false,
        );
        let mut shifts = vec![after.clone()];
        let applied = apply_shift_edits(&mut shifts, vec![edit(&before)], &config());
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
        let applied = apply_shift_edits(&mut shifts, vec![edit(&before)], &config());
        assert_eq!(applied.applied.len(), 1);
        assert!(!shifts[0].standard_time);
        // Given hours of its own, the source has changed.
        let mut shifts = vec![day()];
        let applied = apply_shift_edits(&mut shifts, vec![edit(&before)], &config());
        assert_eq!(applied.outdated, [before.key()]);
    }

    #[test]
    fn added_sps_and_an_absence_read_like_the_sources_own_lines() {
        let config = config();
        let read = shift(
            "2026-09-14T22:00:00+02:00",
            "2026-09-15T08:00:00+02:00",
            false,
        );
        let mut changed = edit(&read);
        changed.times = None;
        changed.sps = vec![
            (clock("23:00"), clock("01:00")),
            (clock("06:00"), clock("07:30")),
        ];
        changed.absence = Some(AbsenceEdit {
            reason: AbsenceReason::OwnIllness,
            substitute: "bo".into(),
        });
        let mut shifts = vec![read];
        apply_shift_edits(&mut shifts, vec![changed], &config);
        assert_eq!(
            shifts[0].comments[0].text,
            "uni 2026-09-14 23:00-01:00\nuni 2026-09-15 06:00-07:30\nsyg: Bo Jensen"
        );
        let sps = parse_sps_instructions(&shifts[0], config.timezone);
        assert!(sps.issues.is_empty(), "{:?}", sps.issues);
        assert_eq!(sps.intervals.len(), 2);
        assert_eq!(
            sps.intervals[0].interval.ends_at,
            at("2026-09-15T01:00:00+02:00")
        );
        let absences = parse_absences(
            &shifts[0],
            &config.absences,
            &config.helpers,
            config.timezone,
        );
        assert!(absences.issues.is_empty(), "{:?}", absences.issues);
        assert_eq!(absences.parts[0].substitute, "bo");
    }

    #[test]
    fn each_part_gives_way_only_to_its_own_change_in_the_source() {
        let read = day();
        let mut changed = edit(&read);
        changed.helper_key = Some("bo".into());
        changed.sps = vec![(clock("10:00"), clock("12:00"))];
        // The source's notes change: the added SPS goes, the rest stays.
        let mut noted = read.clone();
        noted.notes = "uni 13-14".into();
        let mut shifts = vec![noted];
        let applied = apply_shift_edits(&mut shifts, vec![changed.clone()], &config());
        let mut kept = changed.clone();
        kept.sps.clear();
        assert_eq!(applied.reduced, [kept.clone()]);
        assert_eq!(applied.applied[0].helper_key, kept.helper_key);
        assert!(applied.applied[0].sps.is_empty());
        assert_eq!(shifts[0].helper_key, "bo");
        assert!(shifts[0].comments.is_empty());
        // The source's helper changes: that part goes too.
        let mut moved = read.clone();
        moved.helper_key = "bo".into();
        let mut shifts = vec![moved];
        let applied = apply_shift_edits(&mut shifts, vec![changed], &config());
        assert!(applied.reduced[0].helper_key.is_none());
        assert_eq!(applied.reduced[0].sps.len(), 1);
    }

    #[test]
    fn edits_for_other_weeks_are_left_alone() {
        let applied = apply_shift_edits(&mut [], vec![edit(&day())], &config());
        assert_eq!(applied, AppliedEdits::default());
    }
}
