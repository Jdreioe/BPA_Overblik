//! Ret vagt: change a shift in the app instead of in the shift source: its
//! day and time, its helper, added SPS and an absence.
//!
//! The core saves the change with what the source said at that moment and
//! keeps each part only while the source still says it, so a later change in
//! the source wins. See `teamup_shift_sync_core::apply_shift_edits`.

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveTime, TimeZone, Timelike};
use chrono_tz::Tz;
use iced::widget::{button, column, pick_list, row, text, Column};
use iced::{Element, Length};
use teamup_shift_sync_core::{AbsenceEdit, AbsenceReason, ShiftEdit, SourceSnapshot};

use crate::clock::{Dial, DialMessage, Outcome};

/// A shift in the shown week as it is now, what its source says, and its
/// changes made here.
#[derive(Clone, Debug, PartialEq)]
pub struct ShiftTimes {
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    pub helper_key: String,
    pub source: SourceSnapshot,
    pub edit: Option<ShiftEdit>,
    /// The other helpers the source names on the shift, by name.
    pub shared_with: Vec<String>,
}

/// A day to move the shift to, named like the week's columns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Day(pub NaiveDate);
impl std::fmt::Display for Day {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&teamup_shift_sync_gui::preview::date_label(self.0))
    }
}

/// A helper set up under Hjælpere.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Helper {
    pub key: String,
    pub name: String,
}
impl std::fmt::Display for Helper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// An absence reason, or none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Absence(pub Option<AbsenceReason>);
impl std::fmt::Display for Absence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.map_or("Intet fravær", AbsenceReason::label))
    }
}

/// What the form can offer: the helpers, and the absence reasons that have
/// a word under Fravær to be written with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Choices {
    pub helpers: Vec<Helper>,
    pub reasons: Vec<AbsenceReason>,
}

/// One time picker: the shift's start or end, or an SPS row's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Slot {
    Shift { end: bool },
    Sps { row: usize, end: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Open,
    Day(Day),
    OpenTime(Slot),
    Dial(DialMessage),
    Helper(Helper),
    AddSps,
    RemoveSps(usize),
    Absence(Absence),
    Substitute(Helper),
    Save,
    /// Go back to the source's values.
    Revert,
    Cancel,
}

/// What saving does to the stored edits.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Save(Box<ShiftEdit>),
    Forget(String),
}

type Clock = (u32, u32);

/// The open form for one shift.
#[derive(Clone, Debug, PartialEq)]
pub struct Editor {
    source_key: String,
    source: SourceSnapshot,
    edited: bool,
    shared_with: Vec<String>,
    choices: Choices,
    days: Vec<Day>,
    day: Day,
    start: Clock,
    end: Clock,
    helper: Option<Helper>,
    sps: Vec<(Clock, Clock)>,
    absence: Absence,
    substitute: Option<Helper>,
    dial: Option<(Slot, Dial)>,
    error: Option<&'static str>,
}

fn clock(time: NaiveTime) -> Clock {
    (time.hour(), time.minute())
}

fn time((hour, minute): Clock) -> NaiveTime {
    NaiveTime::from_hms_opt(hour, minute, 0).unwrap_or_default()
}

impl Editor {
    /// A form starting from the shift as it is now. It can move the shift to
    /// any day of the week from `monday`, where it stays in view.
    pub fn open(
        source_key: &str,
        shift: &ShiftTimes,
        monday: NaiveDate,
        zone: Tz,
        choices: &Choices,
    ) -> Self {
        let starts_at = shift.starts_at.with_timezone(&zone);
        let ends_at = shift.ends_at.with_timezone(&zone);
        let day = Day(starts_at.date_naive());
        let mut days: Vec<Day> = (0..7).map(|n| Day(monday + Duration::days(n))).collect();
        if !days.contains(&day) {
            days.insert(0, day);
        }
        let helper = |key: &str| choices.helpers.iter().find(|h| h.key == key).cloned();
        let edit = shift.edit.as_ref();
        let absence = edit.and_then(|edit| edit.absence.as_ref());
        Editor {
            source_key: source_key.into(),
            source: shift.source.clone(),
            edited: edit.is_some(),
            shared_with: shift.shared_with.clone(),
            choices: choices.clone(),
            days,
            day,
            start: clock(starts_at.time()),
            end: clock(ends_at.time()),
            helper: helper(&shift.helper_key),
            sps: edit
                .map(|edit| {
                    edit.sps
                        .iter()
                        .map(|(a, b)| (clock(*a), clock(*b)))
                        .collect()
                })
                .unwrap_or_default(),
            absence: Absence(absence.map(|absence| absence.reason)),
            substitute: absence.and_then(|absence| helper(&absence.substitute)),
            dial: None,
            error: None,
        }
    }

    pub fn source_key(&self) -> &str {
        &self.source_key
    }

    fn clock(&self, slot: Slot) -> Option<Clock> {
        match slot {
            Slot::Shift { end } => Some(if end { self.end } else { self.start }),
            Slot::Sps { row, end } => self.sps.get(row).map(|(a, b)| if end { *b } else { *a }),
        }
    }

    /// Apply a message. Save and Revert return the change to store; Open and
    /// Cancel are the caller's.
    pub fn update(&mut self, message: Message, zone: Tz) -> Option<Change> {
        self.error = None;
        match message {
            Message::Open | Message::Cancel => {}
            Message::Day(day) => self.day = day,
            Message::Helper(helper) => self.helper = Some(helper),
            Message::OpenTime(slot) => {
                self.dial = self.clock(slot).map(|clock| (slot, Dial::new(clock)));
            }
            Message::Dial(message) => {
                let (slot, dial) = self.dial.as_mut()?;
                let slot = *slot;
                let outcome = dial.update(message)?;
                self.dial = None;
                if let Outcome::Pick(clock) = outcome {
                    match slot {
                        Slot::Shift { end: false } => self.start = clock,
                        Slot::Shift { end: true } => self.end = clock,
                        Slot::Sps { row, end } => {
                            if let Some(sps) = self.sps.get_mut(row) {
                                *if end { &mut sps.1 } else { &mut sps.0 } = clock;
                            }
                        }
                    }
                }
            }
            Message::AddSps => {
                // A new row starts at the shift's start and lasts an hour.
                let (hour, minute) = self.start;
                self.sps.push((self.start, ((hour + 1) % 24, minute)));
            }
            Message::RemoveSps(row) => {
                if row < self.sps.len() {
                    self.sps.remove(row);
                }
            }
            Message::Absence(absence) => self.absence = absence,
            Message::Substitute(helper) => self.substitute = Some(helper),
            Message::Save => match self.change(zone) {
                Ok(change) => return Some(change),
                Err(error) => self.error = Some(error),
            },
            Message::Revert => return Some(Change::Forget(self.source_key.clone())),
        }
        None
    }

    /// The form as an edit, holding only what differs from the source. An
    /// end at or before the start is the next day, for the shift and SPS.
    /// A shift naming several helpers is saved as checked even unchanged.
    fn change(&self, zone: Tz) -> Result<Change, &'static str> {
        const CLOCK_CHANGE: &str = "Tiden findes ikke den dag, fordi uret stilles om.";
        let at = |date: NaiveDate, clock: Clock| {
            zone.from_local_datetime(&date.and_time(time(clock)))
                .single()
                .map(|time| time.fixed_offset())
                .ok_or(CLOCK_CHANGE)
        };
        let next = |date: NaiveDate| date.succ_opt().ok_or(CLOCK_CHANGE);
        let starts_at = at(self.day.0, self.start)?;
        let ends_at = at(
            if self.end <= self.start {
                next(self.day.0)?
            } else {
                self.day.0
            },
            self.end,
        )?;
        let source = (self.source.times.starts_at, self.source.times.ends_at);
        // SPS read like the core writes it: before the shift's start time
        // is the next day.
        for (from, to) in &self.sps {
            let day = if *from >= self.start {
                self.day.0
            } else {
                next(self.day.0)?
            };
            let sps_start = at(day, *from)?;
            let sps_end = at(if to <= from { next(day)? } else { day }, *to)?;
            if sps_start < starts_at || sps_end > ends_at {
                return Err("SPS skal ligge inden for vagten.");
            }
        }
        let helper_key = self
            .helper
            .as_ref()
            .map(|helper| helper.key.clone())
            .filter(|key| *key != self.source.helper_key);
        let planned = helper_key.as_deref().unwrap_or(&self.source.helper_key);
        let absence = match self.absence.0 {
            None => None,
            Some(reason) => {
                let substitute = self
                    .substitute
                    .as_ref()
                    .ok_or("Vælg, hvem der tog vagten.")?;
                if substitute.key == planned {
                    return Err("Vælg en anden end vagtens hjælper.");
                }
                Some(AbsenceEdit {
                    reason,
                    substitute: substitute.key.clone(),
                })
            }
        };
        let edit = ShiftEdit {
            source_key: self.source_key.clone(),
            source: self.source.clone(),
            times: ((starts_at, ends_at) != source).then_some((starts_at, ends_at)),
            helper_key,
            sps: self
                .sps
                .iter()
                .map(|(from, to)| (time(*from), time(*to)))
                .collect(),
            absence,
            confirmed: !self.shared_with.is_empty(),
        };
        Ok(if edit.is_empty() {
            Change::Forget(self.source_key.clone())
        } else {
            Change::Save(Box::new(edit))
        })
    }

    /// Day and time, helper, SPS and absence, then Gem and Annuller, and
    /// Brug kildens for a shift already changed here.
    pub fn view(&self, enabled: bool) -> Element<'_, Message> {
        let field = |slot: Slot| {
            crate::widgets::time_field(
                self.clock(slot).unwrap_or_default(),
                Message::OpenTime(slot),
            )
        };
        let label = |value: &'static str| text(value).size(13);
        let mut form = column![].spacing(8);
        if !self.shared_with.is_empty() {
            form = form.push(
                text(format!(
                    "Deles med {} – kildens SPS og fravær bruges ikke.",
                    self.shared_with.join(" og ")
                ))
                .size(13),
            );
        }
        form = form.push(
            column![
                pick_list(self.days.clone(), Some(self.day), Message::Day)
                    .padding([6, 10])
                    .width(Length::Fill),
                row![
                    text("fra"),
                    field(Slot::Shift { end: false }),
                    text("til"),
                    field(Slot::Shift { end: true })
                ]
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center),
            ]
            .spacing(8),
        );
        if self.end <= self.start {
            form = form.push(label("Slutter næste dag."));
        }
        form = form.push(label("Hjælper")).push(
            pick_list(
                self.choices.helpers.clone(),
                self.helper.clone(),
                Message::Helper,
            )
            .placeholder("Vælg hjælper")
            .padding([6, 10])
            .width(Length::Fill),
        );
        form = form.push(label("SPS"));
        for row_index in 0..self.sps.len() {
            form = form.push(
                row![
                    field(Slot::Sps {
                        row: row_index,
                        end: false
                    }),
                    text("–"),
                    field(Slot::Sps {
                        row: row_index,
                        end: true
                    }),
                    button(text("✕"))
                        .style(button::text)
                        .on_press(Message::RemoveSps(row_index)),
                ]
                .spacing(6)
                .align_y(iced::alignment::Vertical::Center),
            );
        }
        form = form.push(
            button(text("+ SPS").size(14))
                .style(crate::widgets::outlined)
                .padding([5, 10])
                .on_press(Message::AddSps),
        );
        form = form.push(self.absence_fields());
        if let Some(error) = self.error {
            form = form.push(label(error));
        }
        let save = button(text("Gem"))
            .style(button::primary)
            .padding([7, 14])
            .on_press_maybe(enabled.then_some(Message::Save));
        let cancel = button(text("Annuller"))
            .style(crate::widgets::outlined)
            .padding([7, 14])
            .on_press(Message::Cancel);
        form = form.push(row![save, cancel].spacing(8));
        if self.edited {
            form = form.push(
                button(text("Brug kildens").size(14))
                    .style(crate::widgets::outlined)
                    .padding([7, 12])
                    .on_press_maybe(enabled.then_some(Message::Revert)),
            );
        }
        form.into()
    }

    /// The reason, and who took the shift once there is one.
    fn absence_fields(&self) -> Column<'_, Message> {
        let mut reasons = vec![Absence(None)];
        reasons.extend(self.choices.reasons.iter().map(|r| Absence(Some(*r))));
        let mut fields = column![
            text("Fravær").size(13),
            pick_list(reasons, Some(self.absence), Message::Absence)
                .padding([6, 10])
                .width(Length::Fill),
        ]
        .spacing(8);
        if self.absence.0.is_some() {
            fields = fields.push(
                pick_list(
                    self.choices.helpers.clone(),
                    self.substitute.clone(),
                    Message::Substitute,
                )
                .placeholder("Hvem tog vagten?")
                .padding([6, 10])
                .width(Length::Fill),
            );
        }
        fields
    }

    /// The open picker's dialog, which the app shows over the whole page.
    pub fn time_dialog(&self) -> Option<Element<'_, Message>> {
        let (_, dial) = self.dial.as_ref()?;
        Some(dial.view().map(Message::Dial))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use teamup_shift_sync_core::SourceTimes;

    const ZONE: Tz = chrono_tz::Europe::Copenhagen;

    fn at(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).unwrap()
    }

    fn helper(key: &str, name: &str) -> Helper {
        Helper {
            key: key.into(),
            name: name.into(),
        }
    }

    fn choices() -> Choices {
        Choices {
            helpers: vec![helper("anna", "Anna Hansen"), helper("bo", "Bo Jensen")],
            reasons: vec![AbsenceReason::OwnIllness],
        }
    }

    fn shift() -> ShiftTimes {
        let source = SourceSnapshot {
            times: SourceTimes {
                starts_at: at("2026-09-14T08:00:00+02:00"),
                ends_at: at("2026-09-14T16:00:00+02:00"),
                standard_time: false,
            },
            helper_key: "anna".into(),
            text: "digest".into(),
            shared_with: vec![],
        };
        ShiftTimes {
            starts_at: source.times.starts_at,
            ends_at: source.times.ends_at,
            helper_key: "anna".into(),
            source,
            edit: None,
            shared_with: vec![],
        }
    }

    fn monday() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 14).unwrap()
    }

    fn open(shift: &ShiftTimes) -> Editor {
        Editor::open("cal:event:1", shift, monday(), ZONE, &choices())
    }

    fn pick(editor: &mut Editor, slot: Slot, hour: u32) {
        editor.update(Message::OpenTime(slot), ZONE);
        assert!(editor.dial.is_some());
        editor.update(Message::Dial(DialMessage::Hour { hour, next: true }), ZONE);
        editor.update(Message::Dial(DialMessage::Minute(0)), ZONE);
        editor.update(Message::Dial(DialMessage::Confirm), ZONE);
        assert!(editor.dial.is_none());
    }

    fn saved(editor: &mut Editor) -> ShiftEdit {
        match editor.update(Message::Save, ZONE) {
            Some(Change::Save(edit)) => *edit,
            other => panic!("expected an edit, got {other:?} ({:?})", editor.error),
        }
    }

    const START: Slot = Slot::Shift { end: false };
    const END: Slot = Slot::Shift { end: true };

    #[test]
    fn saving_keeps_only_what_differs_from_the_source() {
        let mut editor = open(&shift());
        editor.update(Message::Day(Day(monday().succ_opt().unwrap())), ZONE);
        pick(&mut editor, START, 9);
        pick(&mut editor, END, 17);
        let edit = saved(&mut editor);
        assert_eq!(edit.source, shift().source);
        assert_eq!(
            edit.times,
            Some((
                at("2026-09-15T09:00:00+02:00"),
                at("2026-09-15T17:00:00+02:00")
            ))
        );
        assert_eq!(edit.helper_key, None);
        assert!(edit.sps.is_empty() && edit.absence.is_none());
    }

    #[test]
    fn an_end_before_the_start_is_the_next_day() {
        let mut editor = open(&shift());
        pick(&mut editor, START, 22);
        pick(&mut editor, END, 7);
        assert_eq!(
            saved(&mut editor).times.unwrap().1,
            at("2026-09-15T07:00:00+02:00")
        );
        let _ = editor.view(true);
    }

    #[test]
    fn a_helper_sps_and_an_absence_are_saved_and_checked() {
        let mut editor = open(&shift());
        editor.update(Message::Helper(helper("bo", "Bo Jensen")), ZONE);
        editor.update(Message::AddSps, ZONE);
        assert_eq!(editor.sps, [((8, 0), (9, 0))]);
        pick(&mut editor, Slot::Sps { row: 0, end: true }, 17);
        assert_eq!(editor.update(Message::Save, ZONE), None);
        assert_eq!(editor.error, Some("SPS skal ligge inden for vagten."));
        pick(&mut editor, Slot::Sps { row: 0, end: true }, 10);
        editor.update(
            Message::Absence(Absence(Some(AbsenceReason::OwnIllness))),
            ZONE,
        );
        assert_eq!(editor.update(Message::Save, ZONE), None);
        assert_eq!(editor.error, Some("Vælg, hvem der tog vagten."));
        editor.update(Message::Substitute(helper("bo", "Bo Jensen")), ZONE);
        assert_eq!(editor.update(Message::Save, ZONE), None);
        assert_eq!(editor.error, Some("Vælg en anden end vagtens hjælper."));
        editor.update(Message::Substitute(helper("anna", "Anna Hansen")), ZONE);
        let _ = editor.view(true);
        let edit = saved(&mut editor);
        assert_eq!(edit.times, None);
        assert_eq!(edit.helper_key.as_deref(), Some("bo"));
        assert_eq!(edit.sps, [(time((8, 0)), time((10, 0)))]);
        assert_eq!(
            edit.absence,
            Some(AbsenceEdit {
                reason: AbsenceReason::OwnIllness,
                substitute: "anna".into()
            })
        );
        // Reopened, the form shows the saved changes.
        let mut changed = shift();
        changed.helper_key = "bo".into();
        changed.edit = Some(edit.clone());
        let mut again = open(&changed);
        assert_eq!(saved(&mut again), edit);
        again.update(Message::RemoveSps(0), ZONE);
        assert!(saved(&mut again).sps.is_empty());
    }

    #[test]
    fn a_shift_naming_several_helpers_is_saved_as_checked_even_unchanged() {
        let mut shared = shift();
        shared.shared_with = vec!["Bo Jensen".into()];
        let mut editor = open(&shared);
        let Some(Change::Save(edit)) = editor.update(Message::Save, ZONE) else {
            panic!("a check is saved");
        };
        assert!(edit.confirmed);
        assert!(edit.times.is_none() && edit.helper_key.is_none());
        // Brug kildens still forgets it, so the shift waits again.
        assert_eq!(
            editor.update(Message::Revert, ZONE),
            Some(Change::Forget("cal:event:1".into()))
        );
    }

    #[test]
    fn the_source_values_or_revert_forget_the_edit() {
        let mut edited = shift();
        edited.starts_at = at("2026-09-15T09:00:00+02:00");
        edited.ends_at = at("2026-09-15T17:00:00+02:00");
        edited.edit = Some(ShiftEdit {
            source_key: "cal:event:1".into(),
            source: shift().source,
            times: Some((edited.starts_at, edited.ends_at)),
            helper_key: None,
            sps: vec![],
            absence: None,
            confirmed: false,
        });
        let mut editor = open(&edited);
        let forget = Some(Change::Forget("cal:event:1".into()));
        assert_eq!(editor.clone().update(Message::Revert, ZONE), forget);
        editor.update(Message::Day(Day(monday())), ZONE);
        pick(&mut editor, START, 8);
        pick(&mut editor, END, 16);
        assert_eq!(editor.update(Message::Save, ZONE), forget);
    }

    #[test]
    fn a_time_the_clock_change_skips_is_refused() {
        let mut editor = open(&shift());
        editor
            .days
            .push(Day(NaiveDate::from_ymd_opt(2027, 3, 28).unwrap()));
        editor.update(Message::Day(*editor.days.last().unwrap()), ZONE);
        editor.update(Message::OpenTime(START), ZONE);
        editor.update(
            Message::Dial(DialMessage::Hour {
                hour: 2,
                next: false,
            }),
            ZONE,
        );
        editor.update(Message::Dial(DialMessage::Minute(30)), ZONE);
        editor.update(Message::Dial(DialMessage::Confirm), ZONE);
        assert_eq!(editor.update(Message::Save, ZONE), None);
        assert!(editor.error.is_some());
        // Annuller in the picker keeps the time.
        editor.update(Message::OpenTime(END), ZONE);
        editor.update(Message::Dial(DialMessage::Minute(45)), ZONE);
        editor.update(Message::Dial(DialMessage::Cancel), ZONE);
        assert_eq!(editor.end, (16, 0));
    }
}
