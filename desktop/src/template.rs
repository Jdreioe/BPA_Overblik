//! Learn a sheet layout from one pasted example shift.
//!
//! The person copies the cells of one shift in their spreadsheet, pastes them
//! here and clicks the date, helper, time and so on in turn. The cells stay in
//! memory only; the learned `SheetLayout` holds positions, not contents.

use std::collections::BTreeMap;

use iced::widget::{button, column, row, text};
use iced::{Element, Length};
use teamup_shift_sync_core::sheets::{tsv_cells, SheetLayout, TemplateCells};

/// Larger pastes are almost certainly more than one shift.
const MAX_CELLS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    Date,
    Helper,
    Time,
    End,
    Sps,
    Title,
}

impl Field {
    const ORDER: [Field; 6] = [
        Field::Date,
        Field::Helper,
        Field::Time,
        Field::End,
        Field::Sps,
        Field::Title,
    ];

    fn label(self) -> &'static str {
        match self {
            Field::Date => "Dato",
            Field::Helper => "Hjælper",
            Field::Time => "Tid",
            Field::End => "Sluttid",
            Field::Sps => "SPS",
            Field::Title => "Titel",
        }
    }

    /// The question for this field. Each is answered by clicking a cell.
    fn prompt(self) -> &'static str {
        match self {
            Field::Date => "Hvor står datoen?",
            Field::Helper => "Hvor står hjælperen?",
            Field::Time => "Hvor står tiden?",
            Field::End => "Hvor står sluttiden?",
            Field::Sps => "Hvor står SPS-timerne?",
            Field::Title => "Hvor står titlen, fx P-MØDE?",
        }
    }

    /// The button for an optional field the shift does not have.
    fn skip(self) -> &'static str {
        match self {
            Field::Sps => "Ingen SPS",
            _ => "Ingen titel",
        }
    }

    fn optional(self) -> bool {
        matches!(self, Field::Sps | Field::Title)
    }
}

// `Pasted` holds shift contents. Do not derive Debug.
#[derive(Clone)]
pub enum Message {
    Paste,
    Pasted(Option<String>),
    Pick(usize, usize),
    Skip,
    Reset,
}

#[derive(Default)]
pub struct Template {
    cells: Vec<Vec<String>>,
    /// `None` marks an optional field the person skipped.
    picks: BTreeMap<Field, Option<(usize, usize)>>,
    error: Option<&'static str>,
}

impl Template {
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn update(&mut self, message: Message) {
        match message {
            // The clipboard is read by the caller.
            Message::Paste => {}
            Message::Pasted(clipboard) => self.paste(clipboard.as_deref().unwrap_or("")),
            Message::Pick(row, col) => self.pick((row, col)),
            Message::Skip => {
                if let Some(field) = self.current().filter(|field| field.optional()) {
                    self.picks.insert(field, None);
                }
            }
            // Back to pasting: a new copy is the easiest way to fix a wrong one.
            Message::Reset => {
                self.picks.clear();
                self.cells.clear();
                self.error = None;
            }
        }
    }

    fn paste(&mut self, clipboard: &str) {
        self.picks.clear();
        self.cells.clear();
        self.error = None;
        match tsv_cells(clipboard.trim_end().as_bytes()) {
            Ok(cells)
                if cells
                    .iter()
                    .any(|row| row.iter().any(|c| !c.trim().is_empty())) =>
            {
                if cells.len() > MAX_CELLS || cells.iter().any(|row| row.len() > MAX_CELLS) {
                    self.error = Some("Kopiér kun cellerne for én vagt.");
                } else {
                    self.cells = cells;
                }
            }
            _ => self.error = Some("Kopiér cellerne for én vagt i regnearket først."),
        }
    }

    /// Clicking a chosen cell again frees it, so a slip is easy to undo.
    fn pick(&mut self, cell: (usize, usize)) {
        if let Some(field) = self.field_at(cell) {
            self.picks.remove(&field);
        } else if let Some(field) = self.current() {
            self.picks.insert(field, Some(cell));
        }
    }

    fn field_at(&self, cell: (usize, usize)) -> Option<Field> {
        self.picks
            .iter()
            .find(|(_, picked)| **picked == Some(cell))
            .map(|(field, _)| *field)
    }

    /// The next field to point out. The end time is only asked for when the
    /// time cell does not already hold a range such as `8-24`.
    fn current(&self) -> Option<Field> {
        let time_is_range = self
            .picks
            .get(&Field::Time)
            .copied()
            .flatten()
            .is_some_and(|(row, col)| self.cells[row][col].contains(['-', '–', '—']));
        Field::ORDER.into_iter().find(|field| {
            !self.picks.contains_key(field) && !(*field == Field::End && time_is_range)
        })
    }

    /// `None` until every field is answered.
    pub fn layout(&self) -> Option<Result<SheetLayout, &'static str>> {
        if self.cells.is_empty() || self.current().is_some() {
            return None;
        }
        let at = |field| self.picks.get(&field).copied().flatten();
        let picked = TemplateCells {
            date: at(Field::Date)?,
            helper: at(Field::Helper)?,
            time: at(Field::Time)?,
            end: at(Field::End),
            sps: at(Field::Sps),
            title: at(Field::Title),
        };
        // The zone only checks that the example's times exist.
        Some(SheetLayout::from_example(
            &self.cells,
            picked,
            chrono_tz::Europe::Copenhagen,
        ))
    }

    /// `saved` says a layout from an earlier connection exists, so the empty
    /// step offers to replace it rather than asking for a first example.
    pub fn view(&self, saved: bool) -> Element<'_, Message> {
        let quiet = |label| {
            button(text(label).size(14))
                .style(crate::widgets::outlined)
                .padding([7, 12])
        };
        let mut content = column![].spacing(8);
        if self.cells.is_empty() {
            content = content
                .push(
                    text(if saved {
                        "Din vagt er gemt."
                    } else {
                        "Kopiér én vagt fra regnearket."
                    })
                    .size(13),
                )
                .push(
                    quiet(if saved {
                        "Indsæt en ny vagt"
                    } else {
                        "Indsæt vagt"
                    })
                    .on_press(Message::Paste),
                );
            if let Some(error) = self.error {
                content = content.push(text(error).size(13));
            }
            return content.into();
        }
        let question: Element<'_, Message> = match (self.current(), self.layout()) {
            (Some(field), _) => {
                let mut line = row![text(field.prompt()).size(15)]
                    .spacing(8)
                    .align_y(iced::alignment::Vertical::Center);
                if field.optional() {
                    line = line.push(quiet(field.skip()).on_press(Message::Skip));
                }
                line.into()
            }
            (None, Some(Err(error))) => text(error).size(15).into(),
            (None, _) => text("✓ Vagten er forstået.").size(15).into(),
        };
        content = content.push(question);
        for (r, cells) in self.cells.iter().enumerate() {
            let mut line = row![].spacing(4);
            for (c, value) in cells.iter().enumerate() {
                let field = self.field_at((r, c));
                let mut cell = column![].spacing(2);
                cell = cell.push(text(field.map_or("", Field::label)).size(11));
                cell = cell.push(text(value.trim()).size(13));
                line = line.push(
                    button(cell)
                        .width(Length::Fixed(110.0))
                        .padding(8)
                        .style(if field.is_some() {
                            button::primary
                        } else {
                            crate::widgets::outlined
                        })
                        .on_press(Message::Pick(r, c)),
                );
            }
            content = content.push(line);
        }
        content
            .push(quiet("Start forfra").on_press(Message::Reset))
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicking_the_cells_in_turn_learns_the_layout_and_a_click_undoes() {
        let mut template = Template::default();
        template.update(Message::Pasted(Some(
            "02/11/26\nMandag\nAlex\n8-24\nSps 8-16\n".into(),
        )));
        for (row, col) in [(0, 0), (2, 0), (3, 0)] {
            template.update(Message::Pick(row, col));
        }
        // The time is a range, so the end time is not asked for.
        assert_eq!(template.current(), Some(Field::Sps));
        template.update(Message::Pick(2, 0));
        assert_eq!(template.current(), Some(Field::Helper));
        template.update(Message::Pick(2, 0));
        template.update(Message::Pick(4, 0));
        template.update(Message::Skip);
        let layout = template.layout().unwrap().unwrap();
        assert_eq!(layout.helper.row, 2);
        assert_eq!(layout.sps_label, "Sps");
        assert_eq!(layout.title, None);
    }
}
