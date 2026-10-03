//! Learn a sheet layout from one example shift.
//!
//! The sheet chosen in step 1 is shown as a grid, and the person clicks the
//! date, helper, time and so on in turn. Pasting copied cells is the fallback
//! when the sheet cannot be read or the shift is hard to find in it. The
//! cells stay in memory only; the learned `SheetLayout` holds positions, not
//! contents.

use std::collections::BTreeMap;

use iced::widget::{button, column, container, pick_list, row, scrollable, text};
use iced::{Element, Length};
use teamup_shift_sync_core::live::SheetPreview;
use teamup_shift_sync_core::sheets::{tsv_cells, SheetLayout, TemplateCells};

/// The picked cells must fit in this many rows and columns, as a paste of
/// one shift does. Larger pastes are almost certainly more than one shift.
const MAX_CELLS: usize = 12;

/// The part of the sheet shown. The first shifts are enough to pick one, and
/// every cell is a widget.
const SHOWN_ROWS: usize = 100;
const SHOWN_COLUMNS: usize = 26;

const CELL_WIDTH: f32 = 100.0;
const CELL_HEIGHT: f32 = 40.0;
const HEADER_WIDTH: f32 = 36.0;
const GRID_HEIGHT: f32 = 360.0;

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
            Field::Date => "Klik på en dato.",
            Field::Helper => "Hvor står hjælperen?",
            Field::Time => "Hvor står tiden?",
            Field::End => "Hvor står sluttiden?",
            Field::Sps => "Hvor står SPS-timerne?",
            Field::Title => "Hvor står titlen, fx P-MØDE?",
        }
    }

    /// The button for an optional field the shift does not have. A shift
    /// without a time takes the time chosen under Vagter uden tid.
    fn skip(self) -> &'static str {
        match self {
            Field::Time => "Arket har ingen tider",
            Field::Sps => "Ingen SPS",
            _ => "Ingen titel",
        }
    }

    fn optional(self) -> bool {
        matches!(self, Field::Time | Field::Sps | Field::Title)
    }
}

/// Reading the sheet from step 1.
#[derive(Default)]
enum Sheet {
    #[default]
    Unread,
    Reading,
    Failed(String),
    Read {
        tabs: Vec<String>,
        tab: Option<String>,
    },
}

// `Pasted` holds shift contents. Do not derive Debug.
#[derive(Clone)]
pub enum Message {
    Paste,
    Pasted(Option<String>),
    Pick(usize, usize),
    Skip,
    Reset,
    /// Pick a new shift instead of the saved one.
    Replace,
    /// Read another workbook tab. The caller reads it.
    Tab(String),
}

#[derive(Default)]
pub struct Template {
    sheet: Sheet,
    /// The tab read from the sheet.
    sheet_cells: Vec<Vec<String>>,
    /// Pasted cells, used instead of the sheet until **Start forfra**.
    pasted: Option<Vec<Vec<String>>>,
    /// `None` marks an optional field the person skipped.
    picks: BTreeMap<Field, Option<(usize, usize)>>,
    error: Option<&'static str>,
    replacing: bool,
}

impl Template {
    /// Nothing is picked, so a saved layout still applies.
    pub fn untouched(&self) -> bool {
        self.picks.is_empty()
    }

    /// A new sheet is being read. What was picked in the old one is gone.
    pub fn reading(&mut self) {
        *self = Self {
            sheet: Sheet::Reading,
            replacing: self.replacing,
            ..Self::default()
        };
    }

    pub fn read(&mut self, result: Result<SheetPreview, String>) {
        match result {
            Ok(preview) => {
                self.sheet = Sheet::Read {
                    tabs: preview.tabs,
                    tab: preview.tab,
                };
                self.sheet_cells = preview.cells;
            }
            Err(error) => self.sheet = Sheet::Failed(error),
        }
    }

    pub fn update(&mut self, message: Message) {
        match message {
            // The clipboard and the tab are read by the caller.
            Message::Paste | Message::Tab(_) => {}
            Message::Pasted(clipboard) => self.paste(clipboard.as_deref().unwrap_or("")),
            Message::Pick(row, col) => self.pick((row, col)),
            Message::Skip => {
                if let Some(field) = self.current().filter(|field| field.optional()) {
                    self.picks.insert(field, None);
                }
            }
            // Back to the sheet, which is the easiest way to fix a wrong pick.
            Message::Reset => {
                self.picks.clear();
                self.pasted = None;
                self.error = None;
            }
            Message::Replace => self.replacing = true,
        }
    }

    fn cells(&self) -> &[Vec<String>] {
        self.pasted.as_deref().unwrap_or(&self.sheet_cells)
    }

    fn cell(&self, (row, col): (usize, usize)) -> &str {
        self.cells()
            .get(row)
            .and_then(|cells| cells.get(col))
            .map_or("", |value| value.trim())
    }

    fn paste(&mut self, clipboard: &str) {
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
                    self.picks.clear();
                    self.pasted = Some(cells);
                }
            }
            _ => self.error = Some("Kopiér cellerne for én vagt i regnearket først."),
        }
    }

    /// Clicking a chosen cell again frees it, so a slip is easy to undo.
    fn pick(&mut self, cell: (usize, usize)) {
        if let Some(field) = self.field_at(cell) {
            self.picks.remove(&field);
            self.error = None;
        } else if let Some(field) = self.current() {
            let spread = |cells: &mut dyn Iterator<Item = usize>| {
                let (min, max) =
                    cells.fold((usize::MAX, 0), |(lo, hi), at| (lo.min(at), hi.max(at)));
                max - min + 1
            };
            let picked: Vec<(usize, usize)> = self.picked().chain([cell]).collect();
            if spread(&mut picked.iter().map(|at| at.0)) > MAX_CELLS
                || spread(&mut picked.iter().map(|at| at.1)) > MAX_CELLS
            {
                self.error = Some("Vælg cellerne for én vagt.");
                return;
            }
            self.error = None;
            self.picks.insert(field, Some(cell));
        }
    }

    fn picked(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.picks.values().filter_map(|cell| *cell)
    }

    fn field_at(&self, cell: (usize, usize)) -> Option<Field> {
        self.picks
            .iter()
            .find(|(_, picked)| **picked == Some(cell))
            .map(|(field, _)| *field)
    }

    /// The next field to point out. The end time is only asked for when the
    /// time cell does not already hold a range such as `8-24`, and not when
    /// the shift has no time.
    fn current(&self) -> Option<Field> {
        let skip_end = match self.picks.get(&Field::Time) {
            Some(Some(cell)) => self.cell(*cell).contains(['-', '–', '—']),
            Some(None) => true,
            None => false,
        };
        Field::ORDER
            .into_iter()
            .find(|field| !self.picks.contains_key(field) && !(*field == Field::End && skip_end))
    }

    /// `None` until every field is answered. The layout is learned from the
    /// smallest block holding the picked cells: exactly what copying that
    /// block would have pasted.
    pub fn layout(&self) -> Option<Result<SheetLayout, &'static str>> {
        if self.cells().is_empty() || self.current().is_some() {
            return None;
        }
        let top = self.picked().map(|at| at.0).min()?;
        let left = self.picked().map(|at| at.1).min()?;
        let bottom = self.picked().map(|at| at.0).max()?;
        let right = self.picked().map(|at| at.1).max()?;
        let block: Vec<Vec<String>> = (top..=bottom)
            .map(|row| {
                (left..=right)
                    .map(|col| self.cell((row, col)).to_owned())
                    .collect()
            })
            .collect();
        let at = |field| {
            self.picks
                .get(&field)
                .copied()
                .flatten()
                .map(|(row, col)| (row - top, col - left))
        };
        let picked = TemplateCells {
            date: at(Field::Date)?,
            helper: at(Field::Helper)?,
            time: at(Field::Time),
            end: at(Field::End),
            sps: at(Field::Sps),
            title: at(Field::Title),
        };
        // The zone only checks that the example's times exist.
        Some(SheetLayout::from_example(
            &block,
            picked,
            chrono_tz::Europe::Copenhagen,
        ))
    }

    /// `saved` says a layout from an earlier connection exists, so the step
    /// offers to replace it rather than asking for a first example.
    pub fn view(&self, saved: bool) -> Element<'_, Message> {
        let quiet = |label| {
            button(text(label).size(14))
                .style(crate::widgets::outlined)
                .padding([7, 12])
        };
        let paste = quiet("Sæt kopierede celler ind").on_press(Message::Paste);
        let mut content = column![].spacing(8);
        if saved && !self.replacing && self.untouched() && self.pasted.is_none() {
            return content
                .push(text("Din vagt er gemt.").size(13))
                .push(quiet("Vælg en ny vagt").on_press(Message::Replace))
                .into();
        }
        if self.pasted.is_none() {
            match &self.sheet {
                Sheet::Unread => {}
                Sheet::Reading => return content.push(text("Henter arket …").size(13)).into(),
                Sheet::Failed(error) => {
                    content = content.push(text(error).size(13));
                    if let Some(error) = self.error {
                        content = content.push(text(error).size(13));
                    }
                    return content.push(paste).into();
                }
                Sheet::Read { tabs, tab } => {
                    if tabs.len() > 1 {
                        content = content.push(
                            row![
                                text("Fane").size(13),
                                pick_list(tabs.as_slice(), tab.as_ref(), Message::Tab),
                            ]
                            .spacing(8)
                            .align_y(iced::alignment::Vertical::Center),
                        );
                    }
                    if self.sheet_cells.is_empty() {
                        return content
                            .push(text("Fanen er tom.").size(13))
                            .push(paste)
                            .into();
                    }
                }
            }
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
        if let Some(error) = self.error {
            content = content.push(text(error).size(13));
        }
        content = content.push(self.grid());
        let mut actions = row![].spacing(8);
        if !self.untouched() || self.pasted.is_some() {
            actions = actions.push(quiet("Start forfra").on_press(Message::Reset));
        }
        content.push(actions.push(paste)).into()
    }

    /// The cells as in the spreadsheet, with column letters and row numbers
    /// for the sheet. Pasted cells have no place in the sheet, so no headers.
    fn grid(&self) -> Element<'_, Message> {
        let headers = self.pasted.is_none();
        let cells = self.cells();
        let columns = cells
            .iter()
            .take(SHOWN_ROWS)
            .map(Vec::len)
            .max()
            .unwrap_or(0)
            .min(SHOWN_COLUMNS);
        let header = |label: String, width: f32, height: f32| {
            container(text(label).size(11))
                .center_x(Length::Fixed(width))
                .center_y(Length::Fixed(height))
        };
        let mut grid = column![].spacing(2);
        if headers {
            let mut letters = row![header(String::new(), HEADER_WIDTH, 20.0)].spacing(2);
            for col in 0..columns {
                letters = letters.push(header(column_name(col), CELL_WIDTH, 20.0));
            }
            grid = grid.push(letters);
        }
        for (r, values) in cells.iter().take(SHOWN_ROWS).enumerate() {
            let mut line = row![].spacing(2);
            if headers {
                line = line.push(header((r + 1).to_string(), HEADER_WIDTH, CELL_HEIGHT));
            }
            for c in 0..columns {
                let value = values.get(c).map_or("", |value| value.trim());
                line = line.push(self.grid_cell(r, c, value));
            }
            grid = grid.push(line);
        }
        let rows = cells.len().min(SHOWN_ROWS) + usize::from(headers);
        scrollable(grid)
            .direction(scrollable::Direction::Both {
                vertical: scrollable::Scrollbar::default(),
                horizontal: scrollable::Scrollbar::default(),
            })
            .width(Length::Fill)
            .height(Length::Fixed(
                (rows as f32 * (CELL_HEIGHT + 2.0) + 14.0).min(GRID_HEIGHT),
            ))
            .into()
    }

    /// One cell. Empty cells hold nothing to pick, so they are not buttons.
    fn grid_cell(&self, r: usize, c: usize, value: &str) -> Element<'_, Message> {
        let field = self.field_at((r, c));
        let size = |content: Element<'static, Message>| {
            container(content)
                .width(Length::Fixed(CELL_WIDTH))
                .center_y(Length::Fixed(CELL_HEIGHT))
                .padding([0, 6])
                .clip(true)
        };
        if value.is_empty() && field.is_none() {
            return size(text("").into()).into();
        }
        let mut label = column![].spacing(1);
        if let Some(field) = field {
            label = label.push(text(field.label()).size(10));
        }
        let label = label.push(
            text(value.to_owned())
                .size(13)
                .wrapping(text::Wrapping::None),
        );
        button(size(label.into()))
            .padding(0)
            .style(if field.is_some() {
                button::primary
            } else {
                crate::widgets::outlined
            })
            .on_press(Message::Pick(r, c))
            .into()
    }
}

/// `A`, `B` … `Z`, `AA` as in a spreadsheet.
fn column_name(mut index: usize) -> String {
    let mut name = Vec::new();
    loop {
        name.push(b'A' + (index % 26) as u8);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    name.reverse();
    String::from_utf8(name).expect("ASCII letters")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(rows: &[&[&str]]) -> SheetPreview {
        SheetPreview {
            tabs: vec!["Uge".into(), "Tider".into()],
            tab: Some("Uge".into()),
            cells: rows
                .iter()
                .map(|row| row.iter().map(|cell| (*cell).to_owned()).collect())
                .collect(),
        }
    }

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

    /// Picking in the sheet learns the same layout as pasting that shift,
    /// wherever the shift is and whatever is around it.
    #[test]
    fn picking_in_the_sheet_learns_what_pasting_the_shift_would() {
        let mut pasted = Template::default();
        pasted.update(Message::Pasted(Some("02/11/26\nMandag\nAlex\n8-24".into())));
        for cell in [(0, 0), (2, 0), (3, 0)] {
            pasted.update(Message::Pick(cell.0, cell.1));
        }
        pasted.update(Message::Skip);
        pasted.update(Message::Skip);

        let mut picked = Template::default();
        picked.reading();
        picked.read(Ok(sheet(&[
            &["Vagtplan"],
            &["Uge 45"],
            &[],
            &["Dato", "02/11/26", "03/11/26"],
            &["Dag", "Mandag", "Tirsdag"],
            &["Hjælper", "Alex", "Sam"],
            &["Tid", "8-24", "8-16"],
        ])));
        let _ = picked.view(false);
        for cell in [(3, 1), (5, 1), (6, 1)] {
            picked.update(Message::Pick(cell.0, cell.1));
        }
        picked.update(Message::Skip);
        picked.update(Message::Skip);
        assert_eq!(
            picked.layout().unwrap().unwrap(),
            pasted.layout().unwrap().unwrap()
        );
        let _ = picked.view(false);
    }

    #[test]
    fn picks_far_apart_are_refused_and_start_over_keeps_the_sheet() {
        let mut template = Template::default();
        let mut rows = vec![vec!["02/11/26".to_owned()]];
        rows.extend((0..20).map(|_| vec![String::new()]));
        rows.push(vec!["Alex".to_owned()]);
        template.read(Ok(SheetPreview {
            tabs: vec![],
            tab: None,
            cells: rows,
        }));
        template.update(Message::Pick(0, 0));
        template.update(Message::Pick(21, 0));
        assert_eq!(template.error, Some("Vælg cellerne for én vagt."));
        assert_eq!(template.current(), Some(Field::Helper));
        template.update(Message::Reset);
        assert!(template.untouched());
        assert_eq!(template.sheet_cells.len(), 22);
    }

    #[test]
    fn a_paste_replaces_the_sheet_until_start_over() {
        let mut template = Template::default();
        template.read(Ok(sheet(&[&["02/11/26", "Alex", "8-24"]])));
        template.update(Message::Pick(0, 0));
        template.update(Message::Pasted(Some("03/11/26\tSam".into())));
        assert!(template.untouched());
        assert_eq!(template.cell((0, 1)), "Sam");
        template.update(Message::Reset);
        assert_eq!(template.cell((0, 1)), "Alex");
        // A failed read still offers pasting.
        template.read(Err("Regnearket kunne ikke hentes.".into()));
        let _ = template.view(false);
    }

    #[test]
    fn a_saved_shift_is_kept_until_a_new_one_is_chosen() {
        let mut template = Template::default();
        template.read(Ok(sheet(&[&["02/11/26", "Alex"]])));
        let _ = template.view(true);
        template.update(Message::Replace);
        assert!(template.replacing);
        template.reading();
        assert!(template.replacing, "reading another tab keeps choosing");
    }

    #[test]
    fn columns_are_named_as_in_a_spreadsheet() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(27), "AB");
    }
}
