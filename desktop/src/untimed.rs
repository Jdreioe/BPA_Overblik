//! Vagter uden tid: the hours a shift gets when the shift plan gives it
//! none, such as a TeamUp or iCal all-day event or an empty time cell in a
//! sheet.
//!
//! The core calls these standard times and stores each as a `HH:MM-HH:MM`
//! range. Here every range is picked with a start and an end time picker, and
//! each weekday follows the common time, has its own, or has none. The picks
//! become the same text the core always read, so the saved setup is unchanged.

use iced::widget::{column, container, pick_list, row, space, text, toggler, Column};
use iced::{Element, Length};

use crate::setup::{Message, SetupUi};

/// What a shift without a time is called everywhere the user can see it.
pub const NAME: &str = "Vagter uden tid";

/// What the setting does, in the user's own terms.
pub const INTRO: &str =
    "Tiden en vagt får, når den står uden klokkeslæt, fx som heldagsbegivenhed.";

/// The range a newly switched-on time starts from.
const FIRST_RANGE: Range = Range {
    start: (8, 0),
    end: (16, 0),
};

/// The width before the pickers: a weekday's name and its choice, so the
/// common time's pickers line up with the weekdays'.
const LABEL_WIDTH: f32 = 250.0;

const WEEKDAYS: [&str; 7] = [
    "Mandag", "Tirsdag", "Onsdag", "Torsdag", "Fredag", "Lørdag", "Søndag",
];

/// One time picker: the common time's or a weekday's (0 is Monday), start or
/// end.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Slot {
    pub day: Option<usize>,
    pub end: bool,
}

/// How one weekday treats a shift without a time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DayMode {
    Common,
    Own,
    Off,
}
impl DayMode {
    const ALL: [DayMode; 3] = [DayMode::Common, DayMode::Own, DayMode::Off];

    /// The weekday's saved text: empty follows the common time, `ingen` has
    /// none, anything else is its own range.
    fn of(value: &str) -> Self {
        match value.trim() {
            "" => DayMode::Common,
            value if value.eq_ignore_ascii_case("ingen") => DayMode::Off,
            _ => DayMode::Own,
        }
    }
}
impl std::fmt::Display for DayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DayMode::Common => "Som alle dage",
            DayMode::Own => "Egen tid",
            DayMode::Off => "Ingen tid",
        })
    }
}

/// A start and end clock time. An end at or before the start is the next day.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Range {
    start: (u32, u32),
    end: (u32, u32),
}
impl Range {
    /// Read the text the core reads: `6-22`, `08:30-16:00` or `22.15-8`.
    /// `24` as an end is midnight.
    fn parse(value: &str) -> Option<Self> {
        let clock = |value: &str| -> Option<(u32, u32)> {
            let value = value.trim().replace('.', ":");
            let (hour, minute) = value.split_once(':').unwrap_or((&value, "0"));
            let (hour, minute): (u32, u32) = (hour.parse().ok()?, minute.parse().ok()?);
            match (hour, minute) {
                (24, 0) => Some((0, 0)),
                (hour, minute) if hour < 24 && minute < 60 => Some((hour, minute)),
                _ => None,
            }
        };
        let (start, end) = value.split_once('-')?;
        Some(Range {
            start: clock(start)?,
            end: clock(end)?,
        })
    }
    fn text(self) -> String {
        format!("{}-{}", clock(self.start), clock(self.end))
    }
    fn label(self) -> String {
        format!("{}–{}", clock(self.start), clock(self.end))
    }
    /// How long the shift is, and when it ends on the next day.
    fn length(self) -> String {
        let minutes = |(hour, minute): (u32, u32)| hour * 60 + minute;
        let (start, end) = (minutes(self.start), minutes(self.end));
        let length = if end > start {
            end - start
        } else {
            end + 24 * 60 - start
        };
        let mut label = match (length / 60, length % 60) {
            (0, minutes) => format!("{minutes} min."),
            (1, 0) => "1 time".to_owned(),
            (hours, 0) => format!("{hours} timer"),
            (hours, minutes) => format!("{hours} t. {minutes} min."),
        };
        if end <= start && end > 0 {
            label.push_str(", slutter næste dag");
        }
        label
    }
}

fn clock((hour, minute): (u32, u32)) -> String {
    format!("{hour:02}:{minute:02}")
}

impl SetupUi {
    /// The settings page: heading, what it is for, and the fields.
    pub fn untimed_view(&self) -> Element<'_, Message> {
        column![
            text(NAME).size(20),
            text(INTRO).size(13),
            self.untimed_fields()
        ]
        .spacing(12)
        .into()
    }

    /// The common time, each weekday, and an example of what a shift
    /// without a time becomes. The guide shows these under its own heading.
    pub fn untimed_fields(&self) -> Element<'_, Message> {
        let common = Range::parse(&self.standard_default);
        let mut content = column![row![toggler(common.is_some())
            .label("Giv vagter uden tid en tid")
            .on_toggle(Message::UntimedOn),]]
        .spacing(14);
        if let Some(range) = common {
            content = content.push(self.range_row(text("Alle dage").size(14), None, range));
        }
        let own_days = self
            .standard_days
            .iter()
            .any(|value| DayMode::of(value) != DayMode::Common);
        if common.is_some() || own_days || self.untimed_days_open {
            content = content.push(self.weekdays(common));
        } else {
            content = content.push(row![crate::setup::quiet_button(
                "Vælg en tid for enkelte ugedage",
                Message::ShowUntimedDays,
            )]);
        }
        content = content.push(crate::widgets::card(text(self.example()).size(13)));
        if let Some(error) = &self.error {
            content = content.push(text(error).size(13));
        }
        content.into()
    }

    fn weekdays(&self, common: Option<Range>) -> Column<'_, Message> {
        let mut days = column![text("Enkelte ugedage").size(14)].spacing(8);
        for (index, name) in WEEKDAYS.into_iter().enumerate() {
            let value = &self.standard_days[index];
            let mode = DayMode::of(value);
            let choice = pick_list(DayMode::ALL, Some(mode), move |mode| {
                Message::UntimedDay(index, mode)
            })
            .padding([6, 10])
            .width(Length::Fixed(160.0));
            let label = row![text(name).width(Length::Fixed(80.0)), choice]
                .spacing(10)
                .align_y(iced::alignment::Vertical::Center);
            days = days.push(match (mode, Range::parse(value), common) {
                (DayMode::Own, Some(range), _) => self.range_row(label, Some(index), range),
                (DayMode::Common, _, Some(range)) => muted_row(label, range.label()),
                _ => muted_row(label, "ingen tid".to_owned()),
            });
        }
        days
    }

    /// A label, then »fra« and »til« pickers and how long that makes the
    /// shift. A picker opens as a dialog over the page; see
    /// [`SetupUi::time_dialog`].
    fn range_row<'a>(
        &'a self,
        label: impl Into<Element<'a, Message>>,
        day: Option<usize>,
        range: Range,
    ) -> Element<'a, Message> {
        let field = |end: bool| {
            let slot = Slot { day, end };
            crate::widgets::time_field(
                if end { range.end } else { range.start },
                Message::OpenTime(Some(slot)),
            )
        };
        row![
            container(label.into()).width(Length::Fixed(LABEL_WIDTH)),
            text("fra"),
            field(false),
            text("til"),
            field(true),
            text(range.length()).size(13),
        ]
        .spacing(10)
        .align_y(iced::alignment::Vertical::Center)
        .into()
    }

    /// The time a picker starts from: the slot's saved start or end.
    fn clock(&self, slot: Slot) -> (u32, u32) {
        let current = match slot.day {
            None => &self.standard_default,
            Some(index) => &self.standard_days[index],
        };
        let range = Range::parse(current).unwrap_or(FIRST_RANGE);
        if slot.end {
            range.end
        } else {
            range.start
        }
    }

    /// The open picker's dialog, which the app shows over the whole page.
    pub fn time_dialog(&self) -> Option<Element<'_, Message>> {
        let (_, dial) = self.open_time.as_ref()?;
        Some(dial.view().map(Message::Dial))
    }

    /// What a shift without a time becomes on the first weekday that gives
    /// it one, or what happens when none does.
    fn example(&self) -> String {
        let common = Range::parse(&self.standard_default);
        let found = WEEKDAYS.into_iter().enumerate().find_map(|(index, name)| {
            let value = &self.standard_days[index];
            let range = match DayMode::of(value) {
                DayMode::Common => common,
                DayMode::Own => Range::parse(value),
                DayMode::Off => None,
            }?;
            Some((name.to_lowercase(), range))
        });
        match found {
            Some((day, range)) => format!(
                "Eksempel: »Anna« uden tid en {day} bliver Anna {}.",
                range.label()
            ),
            None => "Slået fra: en vagt uden tid vises som en advarsel.".into(),
        }
    }

    /// Turn a picker or a choice into the change it makes: the common time's
    /// or a weekday's new text, which then saves like a typed time did. Only
    /// opening, closing or expanding returns nothing.
    pub fn update_untimed(&mut self, message: &Message) -> Option<Message> {
        match *message {
            Message::UntimedOn(on) => {
                self.open_time = None;
                Some(Message::StandardDefault(if on {
                    FIRST_RANGE.text()
                } else {
                    String::new()
                }))
            }
            Message::UntimedDay(index, mode) => {
                self.open_time = None;
                let value = match mode {
                    DayMode::Common => String::new(),
                    DayMode::Off => "ingen".into(),
                    DayMode::Own => Range::parse(&self.standard_days[index])
                        .or_else(|| Range::parse(&self.standard_default))
                        .unwrap_or(FIRST_RANGE)
                        .text(),
                };
                Some(Message::StandardDay(index, value))
            }
            Message::ShowUntimedDays => {
                self.untimed_days_open = true;
                None
            }
            Message::OpenTime(slot) => {
                self.open_time = slot.map(|slot| (slot, crate::clock::Dial::new(self.clock(slot))));
                None
            }
            Message::Dial(dial_message) => {
                let (slot, dial) = self.open_time.as_mut()?;
                let slot = *slot;
                let clock = match dial.update(dial_message)? {
                    crate::clock::Outcome::Cancel => {
                        self.open_time = None;
                        return None;
                    }
                    crate::clock::Outcome::Pick(clock) => clock,
                };
                self.open_time = None;
                let current = match slot.day {
                    None => &self.standard_default,
                    Some(index) => &self.standard_days[index],
                };
                let mut range = Range::parse(current).unwrap_or(FIRST_RANGE);
                if slot.end {
                    range.end = clock;
                } else {
                    range.start = clock;
                }
                Some(match slot.day {
                    None => Message::StandardDefault(range.text()),
                    Some(index) => Message::StandardDay(index, range.text()),
                })
            }
            _ => None,
        }
    }
}

fn muted_row<'a>(label: impl Into<Element<'a, Message>>, value: String) -> Element<'a, Message> {
    row![
        label.into(),
        text(value)
            .size(13)
            .style(|theme: &iced::Theme| text::Style {
                color: Some(theme.extended_palette().background.strong.text),
            }),
        space::horizontal(),
    ]
    .spacing(10)
    .align_y(iced::alignment::Vertical::Center)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::DialMessage;

    fn value(message: Option<Message>) -> String {
        match message {
            Some(Message::StandardDefault(value) | Message::StandardDay(_, value)) => value,
            _ => panic!("expected a time change"),
        }
    }

    #[test]
    fn saved_times_read_back_in_every_written_form() {
        for (saved, label) in [
            ("6-22", "06:00–22:00"),
            ("08:30-16:00", "08:30–16:00"),
            ("22.15-8", "22:15–08:00"),
            ("8-24", "08:00–00:00"),
        ] {
            assert_eq!(
                Range::parse(saved).map(Range::label).as_deref(),
                Some(label)
            );
        }
        assert!(Range::parse("").is_none());
        assert!(Range::parse("ingen").is_none());
        assert!(Range::parse("25-8").is_none());
    }

    #[test]
    fn the_length_says_when_a_shift_ends_the_next_day() {
        let length = |value| Range::parse(value).unwrap().length();
        assert_eq!(length("6-22"), "16 timer");
        assert_eq!(length("22-8"), "10 timer, slutter næste dag");
        assert_eq!(length("8-8"), "24 timer, slutter næste dag");
        assert_eq!(length("8-24"), "16 timer");
        assert_eq!(length("08:30-09:15"), "45 min.");
    }

    #[test]
    fn picking_writes_the_range_the_core_reads_and_closes_the_picker() {
        let mut ui = SetupUi::default();
        assert_eq!(
            value(ui.update_untimed(&Message::UntimedOn(true))),
            "08:00-16:00"
        );
        ui.standard_default = "6-22".into();
        let start = Slot {
            day: None,
            end: false,
        };
        // The picker starts from the saved start time.
        ui.update_untimed(&Message::OpenTime(Some(start)));
        assert_eq!(ui.open_time, Some((start, crate::clock::Dial::new((6, 0)))));
        let dial = |message| Message::Dial(message);
        assert!(ui
            .update_untimed(&dial(DialMessage::Hour {
                hour: 7,
                next: true
            }))
            .is_none());
        ui.update_untimed(&dial(DialMessage::Minute(30)));
        assert_eq!(
            value(ui.update_untimed(&dial(DialMessage::Confirm))),
            "07:30-22:00"
        );
        assert_eq!(ui.open_time, None);
        // Annuller keeps the saved time.
        ui.update_untimed(&Message::OpenTime(Some(start)));
        ui.update_untimed(&dial(DialMessage::Minute(45)));
        assert!(ui.update_untimed(&dial(DialMessage::Cancel)).is_none());
        assert_eq!(ui.open_time, None);
        assert_eq!(value(ui.update_untimed(&Message::UntimedOn(false))), "");
    }

    #[test]
    fn a_weekday_follows_the_common_time_has_its_own_or_none() {
        let mut ui = SetupUi {
            standard_default: "6-22".into(),
            ..SetupUi::default()
        };
        // Its own time starts from the common one.
        assert_eq!(
            value(ui.update_untimed(&Message::UntimedDay(5, DayMode::Own))),
            "06:00-22:00"
        );
        assert_eq!(
            value(ui.update_untimed(&Message::UntimedDay(6, DayMode::Off))),
            "ingen"
        );
        assert_eq!(
            value(ui.update_untimed(&Message::UntimedDay(6, DayMode::Common))),
            ""
        );
        assert_eq!(DayMode::of("ingen"), DayMode::Off);
        assert_eq!(DayMode::of(" "), DayMode::Common);
        assert_eq!(DayMode::of("8-20"), DayMode::Own);
    }

    #[test]
    fn the_example_names_the_first_day_with_a_time_or_says_there_is_none() {
        let mut ui = SetupUi::default();
        assert!(ui.example().starts_with("Slået fra"));
        ui.standard_days[5] = "8-20".into();
        assert!(ui.example().contains("en lørdag bliver Anna 08:00–20:00"));
        ui.standard_default = "6-22".into();
        ui.standard_days[0] = "ingen".into();
        assert!(ui.example().contains("en tirsdag"));
        // Every combination renders, an open picker included.
        ui.update_untimed(&Message::OpenTime(Some(Slot {
            day: Some(5),
            end: true,
        })));
        let _ = ui.untimed_view();
        assert!(ui.time_dialog().is_some());
        ui.standard_default.clear();
        ui.standard_days = Default::default();
        let _ = ui.untimed_view();
        ui.untimed_days_open = true;
        let _ = ui.untimed_view();
    }
}
