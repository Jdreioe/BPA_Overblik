//! Ret tid: change a shift's day and time in the app instead of in the
//! shift source.
//!
//! The core saves the change with the source's times at that moment and
//! keeps it only while the source still has them, so a later change in the
//! source wins. See `teamup_shift_sync_core::apply_shift_edits`.

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;
use iced::widget::{button, column, pick_list, row, text};
use iced::{Element, Length};
use teamup_shift_sync_core::{ShiftEdit, SourceTimes};

use crate::clock::{Dial, DialMessage, Outcome};

/// A shift's times in the shown week, and what its source says.
#[derive(Clone, Debug, PartialEq)]
pub struct ShiftTimes {
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    pub source: SourceTimes,
    pub edited: bool,
}

/// A day to move the shift to, named like the week's columns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Day(pub NaiveDate);
impl std::fmt::Display for Day {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&teamup_shift_sync_gui::preview::date_label(self.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Message {
    Open,
    Day(Day),
    /// Open the start's picker, or the end's.
    OpenTime(bool),
    Dial(DialMessage),
    Save,
    /// Go back to the source's time.
    Revert,
    Cancel,
}

/// What saving does to the stored edits.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Save(ShiftEdit),
    Forget(String),
}

/// The open form for one shift.
#[derive(Clone, Debug, PartialEq)]
pub struct Editor {
    source_key: String,
    source: SourceTimes,
    edited: bool,
    days: Vec<Day>,
    day: Day,
    start: (u32, u32),
    end: (u32, u32),
    /// The open picker: the end's, or the start's, and its dial.
    dial: Option<(bool, Dial)>,
    error: Option<&'static str>,
}

impl Editor {
    /// A form starting from the shift's current times. It can move the
    /// shift to any day of the week from `monday`, where it stays in view.
    pub fn open(source_key: &str, shift: &ShiftTimes, monday: NaiveDate, zone: Tz) -> Self {
        let starts_at = shift.starts_at.with_timezone(&zone);
        let ends_at = shift.ends_at.with_timezone(&zone);
        let day = Day(starts_at.date_naive());
        let mut days: Vec<Day> = (0..7).map(|n| Day(monday + Duration::days(n))).collect();
        if !days.contains(&day) {
            days.insert(0, day);
        }
        Editor {
            source_key: source_key.into(),
            source: shift.source,
            edited: shift.edited,
            days,
            day,
            start: (starts_at.hour(), starts_at.minute()),
            end: (ends_at.hour(), ends_at.minute()),
            dial: None,
            error: None,
        }
    }

    pub fn source_key(&self) -> &str {
        &self.source_key
    }

    /// Apply a message. Save and Revert return the change to store; Open and
    /// Cancel are the caller's.
    pub fn update(&mut self, message: Message, zone: Tz) -> Option<Change> {
        self.error = None;
        match message {
            Message::Open | Message::Cancel => {}
            Message::Day(day) => self.day = day,
            Message::OpenTime(end) => {
                let clock = if end { self.end } else { self.start };
                self.dial = Some((end, Dial::new(clock)));
            }
            Message::Dial(message) => {
                let (end, dial) = self.dial.as_mut()?;
                let end = *end;
                let outcome = dial.update(message)?;
                self.dial = None;
                if let Outcome::Pick(clock) = outcome {
                    if end {
                        self.end = clock;
                    } else {
                        self.start = clock;
                    }
                }
            }
            Message::Save => match self.change(zone) {
                Ok(change) => return Some(change),
                Err(error) => self.error = Some(error),
            },
            Message::Revert => return Some(Change::Forget(self.source_key.clone())),
        }
        None
    }

    /// The chosen times as an edit. An end at or before the start is the next
    /// day. Times equal to the source's need no edit.
    fn change(&self, zone: Tz) -> Result<Change, &'static str> {
        const CLOCK_CHANGE: &str = "Tiden findes ikke den dag, fordi uret stilles om.";
        let at = |date: NaiveDate, (hour, minute): (u32, u32)| {
            date.and_hms_opt(hour, minute, 0)
                .and_then(|local| zone.from_local_datetime(&local).single())
                .map(|time| time.fixed_offset())
                .ok_or(CLOCK_CHANGE)
        };
        let end_day = if self.end <= self.start {
            self.day.0.succ_opt().ok_or(CLOCK_CHANGE)?
        } else {
            self.day.0
        };
        let starts_at = at(self.day.0, self.start)?;
        let ends_at = at(end_day, self.end)?;
        if (starts_at, ends_at) == (self.source.starts_at, self.source.ends_at) {
            return Ok(Change::Forget(self.source_key.clone()));
        }
        Ok(Change::Save(ShiftEdit {
            source_key: self.source_key.clone(),
            source: self.source,
            starts_at,
            ends_at,
        }))
    }

    /// The day, »fra« and »til«, then Gem and Annuller, and Brug kildens tid
    /// for a shift already changed here.
    pub fn view(&self, enabled: bool) -> Element<'_, Message> {
        let field = |end: bool| {
            crate::widgets::time_field(
                if end { self.end } else { self.start },
                Message::OpenTime(end),
            )
        };
        let mut form = column![
            pick_list(self.days.clone(), Some(self.day), Message::Day)
                .padding([6, 10])
                .width(Length::Fill),
            row![text("fra"), field(false), text("til"), field(true)]
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center),
        ]
        .spacing(8);
        if self.end <= self.start {
            form = form.push(text("Slutter næste dag.").size(13));
        }
        if let Some(error) = self.error {
            form = form.push(text(error).size(13));
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
                button(text("Brug kildens tid").size(14))
                    .style(crate::widgets::outlined)
                    .padding([7, 12])
                    .on_press_maybe(enabled.then_some(Message::Revert)),
            );
        }
        form.into()
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

    const ZONE: Tz = chrono_tz::Europe::Copenhagen;

    fn at(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).unwrap()
    }

    fn shift() -> ShiftTimes {
        let source = SourceTimes {
            starts_at: at("2026-09-14T08:00:00+02:00"),
            ends_at: at("2026-09-14T16:00:00+02:00"),
            standard_time: false,
        };
        ShiftTimes {
            starts_at: source.starts_at,
            ends_at: source.ends_at,
            source,
            edited: false,
        }
    }

    fn monday() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 14).unwrap()
    }

    fn pick(editor: &mut Editor, end: bool, hour: u32) {
        editor.update(Message::OpenTime(end), ZONE);
        assert!(editor.dial.is_some());
        editor.update(Message::Dial(DialMessage::Hour { hour, next: true }), ZONE);
        editor.update(Message::Dial(DialMessage::Minute(0)), ZONE);
        editor.update(Message::Dial(DialMessage::Confirm), ZONE);
        assert!(editor.dial.is_none());
    }

    #[test]
    fn saving_keeps_the_source_times_the_change_was_made_from() {
        let mut editor = Editor::open("cal:event:1", &shift(), monday(), ZONE);
        editor.update(Message::Day(Day(monday().succ_opt().unwrap())), ZONE);
        pick(&mut editor, false, 9);
        pick(&mut editor, true, 17);
        assert_eq!(
            editor.update(Message::Save, ZONE),
            Some(Change::Save(ShiftEdit {
                source_key: "cal:event:1".into(),
                source: shift().source,
                starts_at: at("2026-09-15T09:00:00+02:00"),
                ends_at: at("2026-09-15T17:00:00+02:00"),
            }))
        );
    }

    #[test]
    fn an_end_before_the_start_is_the_next_day() {
        let mut editor = Editor::open("cal:event:1", &shift(), monday(), ZONE);
        pick(&mut editor, false, 22);
        pick(&mut editor, true, 7);
        let Some(Change::Save(edit)) = editor.update(Message::Save, ZONE) else {
            panic!("expected an edit");
        };
        assert_eq!(edit.ends_at, at("2026-09-15T07:00:00+02:00"));
        let _ = editor.view(true);
    }

    #[test]
    fn the_source_times_or_revert_forget_the_edit() {
        let mut edited = shift();
        edited.starts_at = at("2026-09-15T09:00:00+02:00");
        edited.ends_at = at("2026-09-15T17:00:00+02:00");
        edited.edited = true;
        let mut editor = Editor::open("cal:event:1", &edited, monday(), ZONE);
        let forget = Some(Change::Forget("cal:event:1".into()));
        assert_eq!(editor.clone().update(Message::Revert, ZONE), forget);
        editor.update(Message::Day(Day(monday())), ZONE);
        pick(&mut editor, false, 8);
        pick(&mut editor, true, 16);
        assert_eq!(editor.update(Message::Save, ZONE), forget);
    }

    #[test]
    fn a_time_the_clock_change_skips_is_refused() {
        let mut editor = Editor::open("cal:event:1", &shift(), monday(), ZONE);
        editor
            .days
            .push(Day(NaiveDate::from_ymd_opt(2027, 3, 28).unwrap()));
        editor.update(Message::Day(*editor.days.last().unwrap()), ZONE);
        editor.update(Message::OpenTime(false), ZONE);
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
        editor.update(Message::OpenTime(true), ZONE);
        editor.update(Message::Dial(DialMessage::Minute(45)), ZONE);
        editor.update(Message::Dial(DialMessage::Cancel), ZONE);
        assert_eq!(editor.end, (16, 0));
    }
}
