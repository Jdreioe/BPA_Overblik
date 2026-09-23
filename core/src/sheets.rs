//! Convert sheet cells into source shifts using a one-shift template.
//!
//! The user pastes the cells of one example shift and points out which cell
//! holds the date, helper, time and so on. The layout stores where each field
//! sits relative to the date cell. Reading a sheet, every cell that is a date
//! in the template's format anchors one shift, so weekly grids and
//! one-shift-per-row tables need no further configuration.
//!
//! A shift's identity is its date (`20261102`). Editing the cells in place
//! keeps the identity; moving a shift to another date is a new shift, exactly
//! like deleting and recreating a TeamUp event.

use std::collections::BTreeSet;

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::SourceShift;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CellOffset {
    /// Offset from the date cell. Positive values move down or right.
    pub row: i32,
    pub column: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimeCells {
    /// One cell such as `8-24`.
    Range {
        cell: CellOffset,
    },
    Separate {
        start: CellOffset,
        end: CellOffset,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SheetLayout {
    pub date_format: String,
    /// Word before SPS intervals, such as `Sps` in `Sps 8-16`. Empty when the
    /// cell holds only the intervals.
    pub sps_label: String,
    pub helper: CellOffset,
    pub time: TimeCells,
    pub sps: Option<CellOffset>,
    pub title: Option<CellOffset>,
}

/// Date formats a sheet may display, tried in order. Two-digit years first:
/// `%Y` would also read `26` as the year 26. The year may be left out only
/// with `/`: `23.9` and `23-9` look like the times `8.30` and `8-12`.
const DATE_FORMATS: [&str; 8] = [
    "%d/%m/%y", "%d.%m.%y", "%d-%m-%y", "%d/%m/%Y", "%d.%m.%Y", "%d-%m-%Y", "%Y-%m-%d", "%d/%m",
];

/// Read a date in `format`. A date without a year, such as `23/9`, takes the
/// year that puts it nearest `reference`, so a week read for preview and
/// again for transfer resolves every date the same way.
fn parse_date(value: &str, format: &str, reference: NaiveDate) -> Option<NaiveDate> {
    let value = value.trim();
    if format.contains(['y', 'Y']) {
        return NaiveDate::parse_from_str(value, format).ok();
    }
    let with_year = format!("{format}/%Y");
    (reference.year() - 1..=reference.year() + 1)
        .filter_map(|year| NaiveDate::parse_from_str(&format!("{value}/{year}"), &with_year).ok())
        .min_by_key(|date| (*date - reference).num_days().abs())
}

/// The template cells the user pointed out, as (row, column) in the pasted
/// grid, counted from zero.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TemplateCells {
    pub date: (usize, usize),
    pub helper: (usize, usize),
    pub time: (usize, usize),
    pub end: Option<(usize, usize)>,
    pub sps: Option<(usize, usize)>,
    pub title: Option<(usize, usize)>,
}

impl SheetLayout {
    /// Learn the layout from one pasted example shift. The example must read
    /// back as exactly one shift, so a wrong pick is caught here, not later.
    pub fn from_example(
        cells: &[Vec<String>],
        picked: TemplateCells,
        zone: Tz,
    ) -> Result<Self, &'static str> {
        let at = |(row, col): (usize, usize)| {
            cells
                .get(row)
                .and_then(|r| r.get(col))
                .map_or("", |value| value.trim())
        };
        let today = Utc::now().with_timezone(&zone).date_naive();
        let date = at(picked.date);
        let date_format = DATE_FORMATS
            .into_iter()
            .find(|format| parse_date(date, format, today).is_some())
            .ok_or("Datoen kan ikke læses. Skriv den som 23/9, 23/09/26 eller 2026-09-23.")?;
        let offset = |(row, col): (usize, usize)| CellOffset {
            row: row as i32 - picked.date.0 as i32,
            column: col as i32 - picked.date.1 as i32,
        };
        let sps_label = picked.sps.map_or(String::new(), |cell| {
            at(cell)
                .split(|c: char| c.is_ascii_digit())
                .next()
                .unwrap_or("")
                .trim()
                .to_owned()
        });
        let layout = Self {
            date_format: date_format.into(),
            sps_label,
            helper: offset(picked.helper),
            time: match picked.end {
                Some(end) => TimeCells::Separate {
                    start: offset(picked.time),
                    end: offset(end),
                },
                None => TimeCells::Range {
                    cell: offset(picked.time),
                },
            },
            sps: picked.sps.map(offset),
            title: picked.title.map(offset),
        };
        let parsed = parse_cells(cells, &layout, "template", zone, today)?;
        if let Some(issue) = parsed.issues.first() {
            return Err(issue.kind.reason());
        }
        if parsed.shifts.len() != 1 {
            return Err("Kopiér kun cellerne for én vagt.");
        }
        Ok(layout)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !DATE_FORMATS.contains(&self.date_format.as_str()) {
            return Err("Regnearkets datoformat er ugyldigt.");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IssueKind {
    MissingHelper,
    MissingTime,
    UnreadableTime,
    UnreadableSps,
    ImpossibleDate,
    RepeatedDate,
}

impl IssueKind {
    /// A short reason without names, for checking a pasted template.
    pub fn reason(self) -> &'static str {
        match self {
            IssueKind::MissingHelper => "Hjælperen mangler.",
            IssueKind::MissingTime => "Tiden mangler.",
            IssueKind::UnreadableTime => "Tiden skal være som 8-24 eller 08:30-16:00.",
            IssueKind::UnreadableSps => "SPS-feltet følger ikke skabelonens skrivemåde.",
            IssueKind::ImpossibleDate => "Datoen findes ikke.",
            IssueKind::RepeatedDate => "Datoen står ved flere vagter i regnearket.",
        }
    }
}

/// A shift the sheet does not describe clearly enough to transfer. Cell
/// contents other than the helper's name are never kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SheetIssue {
    pub cell: String,
    pub kind: IssueKind,
    /// The shift's date, when it could be read. `None` means the issue could
    /// belong to any week.
    pub date: Option<NaiveDate>,
    pub helper: Option<String>,
}

impl SheetIssue {
    /// Name the helper and date, so the shift can be found without looking
    /// up a cell. Only an impossible date, with neither, points at its cell.
    /// Shown on screen only; it names a helper, so never put it in a report.
    pub fn message(&self, sps_label: &str) -> String {
        let Some(date) = self.date else {
            return format!("Datoen i celle {} findes ikke.", self.cell);
        };
        let day = format!("d. {}/{}", date.day(), date.month());
        let shift = match &self.helper {
            Some(helper) if helper.ends_with(['s', 'x', 'z', 'S', 'X', 'Z']) => {
                format!("{helper}' vagt {day}")
            }
            Some(helper) => format!("{helper}s vagt {day}"),
            None => format!("Vagten {day}"),
        };
        match self.kind {
            IssueKind::MissingHelper => format!("Vagten {day} mangler en hjælper."),
            IssueKind::MissingTime => format!("{shift} mangler tid."),
            IssueKind::UnreadableTime => format!(
                "{shift} har en tid, der ikke kan læses. Skriv den som 8-24 eller 08:30-16:00."
            ),
            IssueKind::UnreadableSps => {
                let label = if sps_label.is_empty() { "" } else { " " };
                format!(
                    "{shift} har SPS-timer, der ikke kan læses. Skriv dem som »{sps_label}{label}8-10«."
                )
            }
            IssueKind::RepeatedDate => {
                format!("Der står flere vagter {day}. Hver dato må kun have én vagt.")
            }
            IssueKind::ImpossibleDate => format!("Datoen i celle {} findes ikke.", self.cell),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ParsedSheet {
    pub shifts: Vec<SourceShift>,
    pub issues: Vec<SheetIssue>,
    /// Every helper named on a shift, including shifts that still need a
    /// fix, so setup can map a helper before their times are corrected.
    pub helpers: BTreeSet<String>,
}

/// Cells copied from a spreadsheet arrive tab separated.
pub fn tsv_cells(data: &[u8]) -> Result<Vec<Vec<String>>, &'static str> {
    grid(data, b'\t')
}

pub fn csv_cells(data: &[u8]) -> Result<Vec<Vec<String>>, &'static str> {
    grid(data, b',')
}

fn grid(data: &[u8], delimiter: u8) -> Result<Vec<Vec<String>>, &'static str> {
    // The csv crate skips empty physical lines. In a sheet export they still
    // occupy a row, so keep them before resolving A1 coordinates.
    let mut with_empty_rows = Vec::with_capacity(data.len());
    let mut quoted = false;
    let mut line_has_content = false;
    let mut index = 0;
    while index < data.len() {
        let byte = data[index];
        if !quoted && (byte == b'\n' || byte == b'\r') {
            if !line_has_content {
                with_empty_rows.extend_from_slice(b"\"\"");
            }
            with_empty_rows.push(byte);
            if byte == b'\r' && data.get(index + 1) == Some(&b'\n') {
                with_empty_rows.push(b'\n');
                index += 1;
            }
            line_has_content = false;
        } else {
            if byte == b'"' {
                quoted = !quoted;
            }
            line_has_content = true;
            with_empty_rows.push(byte);
        }
        index += 1;
    }
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_reader(with_empty_rows.as_slice());
    reader
        .records()
        .map(|record| {
            record
                .map(|row| row.iter().map(str::to_owned).collect())
                .map_err(|_| "Cellerne kunne ikke læses.")
        })
        .collect()
}

/// Parse every shift in the sheet, including dates outside the viewed week.
/// The caller must reject any issue that can touch the planned week before
/// planning or transferring anything. `reference`, normally the first day of
/// the week being read, gives dates written without a year their year.
pub fn parse_cells(
    cells: &[Vec<String>],
    layout: &SheetLayout,
    source_id: &str,
    zone: Tz,
    reference: NaiveDate,
) -> Result<ParsedSheet, &'static str> {
    layout.validate()?;
    let mut parsed = ParsedSheet::default();
    let mut readings = Vec::new();
    for (row_index, row) in cells.iter().enumerate() {
        for (col_index, value) in row.iter().enumerate() {
            let (row, col) = (row_index + 1, col_index + 1);
            // The template's format first; a sheet may show other dates
            // differently, such as `21/9` next to `22/09/2026`.
            let date = std::iter::once(layout.date_format.as_str())
                .chain(DATE_FORMATS)
                .find_map(|format| parse_date(value, format, reference));
            match date {
                Some(date) => {
                    readings.extend(read_shift(cells, layout, source_id, zone, date, row, col))
                }
                None if looks_like_date(value) => parsed.issues.push(SheetIssue {
                    cell: a1(row, col),
                    kind: IssueKind::ImpossibleDate,
                    date: None,
                    helper: None,
                }),
                None => {}
            }
        }
    }
    // A heading date just above a block reads that block's cells as its own.
    // Such a failed reading is not a shift when a valid shift owns the cells.
    let owned: BTreeSet<_> = readings
        .iter()
        .filter(|reading| reading.result.is_ok())
        .flat_map(|reading| reading.cells.iter().copied())
        .collect();
    let mut positions = BTreeSet::new();
    for reading in readings {
        let stray =
            reading.result.is_err() && reading.cells.iter().any(|cell| owned.contains(cell));
        if !stray && !reading.helper.is_empty() {
            parsed.helpers.insert(reading.helper.clone());
        }
        match reading.result {
            Ok(shift) => {
                // The date is the shift's identity, so a repeated date would
                // make two shifts indistinguishable.
                if positions.insert(shift.event_id.clone()) {
                    parsed.shifts.push(shift);
                } else {
                    parsed.issues.push(SheetIssue {
                        cell: reading.date_cell,
                        kind: IssueKind::RepeatedDate,
                        date: Some(reading.date),
                        helper: None,
                    });
                }
            }
            Err(issue) if !stray => parsed.issues.push(issue),
            Err(_) => {}
        }
    }
    Ok(parsed)
}

/// What one date cell's template reads as, and which cells it read.
struct Reading {
    result: Result<SourceShift, SheetIssue>,
    helper: String,
    cells: Vec<(usize, usize)>,
    date: NaiveDate,
    date_cell: String,
}

/// `None` for a date with nothing around it, such as a heading.
fn read_shift(
    cells: &[Vec<String>],
    layout: &SheetLayout,
    source_id: &str,
    zone: Tz,
    date: NaiveDate,
    date_row: usize,
    date_col: usize,
) -> Option<Reading> {
    let offsets = [
        Some(layout.helper),
        Some(match layout.time {
            TimeCells::Range { cell } => cell,
            TimeCells::Separate { start, .. } => start,
        }),
        match layout.time {
            TimeCells::Range { .. } => None,
            TimeCells::Separate { end, .. } => Some(end),
        },
        layout.sps,
        layout.title,
    ];
    let value = |offset: Option<CellOffset>| {
        offset.map_or("", |offset| {
            relative(cells, date_row, date_col, offset).trim()
        })
    };
    let [helper, time, end, sps, title] = offsets.map(value);
    if [helper, time, end, sps, title].iter().all(|v| v.is_empty()) {
        return None;
    }
    let (start_text, end_text) = match layout.time {
        TimeCells::Range { .. } => split_range(time),
        TimeCells::Separate { .. } => (time, end),
    };
    let issue = |offset: Option<CellOffset>, kind| SheetIssue {
        cell: offset.map_or_else(String::new, |o| position_of(date_row, date_col, o)),
        kind,
        date: Some(date),
        helper: (!helper.is_empty()).then(|| helper.to_owned()),
    };
    let result = if helper.is_empty() {
        Err(issue(offsets[0], IssueKind::MissingHelper))
    } else if let Some((starts_at, ends_at)) = interval(date, start_text, end_text, zone) {
        match sps_notes(sps, &layout.sps_label) {
            Some(notes) => {
                let id = date.format("%Y%m%d").to_string();
                Ok(SourceShift {
                    calendar_id: source_id.into(),
                    event_id: id.clone(),
                    occurrence_id: id,
                    title: if title.is_empty() { "Vagt" } else { title }.into(),
                    helper_key: helper.into(),
                    starts_at,
                    ends_at,
                    notes,
                    comments: vec![],
                    recurrence_start: None,
                    source_version: None,
                })
            }
            None => Err(issue(layout.sps, IssueKind::UnreadableSps)),
        }
    } else if start_text.is_empty() || end_text.is_empty() {
        Err(issue(offsets[1], IssueKind::MissingTime))
    } else {
        Err(issue(offsets[1], IssueKind::UnreadableTime))
    };
    Some(Reading {
        result,
        helper: helper.to_owned(),
        cells: offsets
            .into_iter()
            .flatten()
            .filter_map(|offset| coordinates(date_row, date_col, offset))
            .collect(),
        date,
        date_cell: a1(date_row, date_col),
    })
}

/// A mistyped date, such as `31/11/26`, must not silently drop its shift:
/// numbers in the shape of a supported format mark it as a date attempt.
fn looks_like_date(value: &str) -> bool {
    DATE_FORMATS.iter().any(|format| {
        let separator = format.chars().nth(2).unwrap_or('/');
        let parts: Vec<_> = value.trim().split(separator).collect();
        parts.len() == format.split(separator).count()
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn cell(cells: &[Vec<String>], row: usize, col: usize) -> &str {
    cells
        .get(row.saturating_sub(1))
        .and_then(|r| r.get(col.saturating_sub(1)))
        .map_or("", String::as_str)
}

fn relative(cells: &[Vec<String>], row: usize, col: usize, offset: CellOffset) -> &str {
    match coordinates(row, col, offset) {
        Some((row, col)) => cell(cells, row, col),
        None => "",
    }
}

fn coordinates(row: usize, col: usize, offset: CellOffset) -> Option<(usize, usize)> {
    let row = i64::try_from(row)
        .ok()?
        .checked_add(i64::from(offset.row))?;
    let col = i64::try_from(col)
        .ok()?
        .checked_add(i64::from(offset.column))?;
    if row < 1 || col < 1 {
        return None;
    }
    Some((usize::try_from(row).ok()?, usize::try_from(col).ok()?))
}

fn position_of(row: usize, col: usize, offset: CellOffset) -> String {
    coordinates(row, col, offset).map_or_else(String::new, |(r, c)| a1(r, c))
}

fn a1(row: usize, mut col: usize) -> String {
    let mut letters = String::new();
    while col > 0 {
        let digit = (col - 1) % 26;
        letters.insert(0, (b'A' + digit as u8) as char);
        col = (col - 1) / 26;
    }
    format!("{letters}{row}")
}

fn split_range(value: &str) -> (&str, &str) {
    value
        .split_once(['-', '–', '—'])
        .map_or((value, ""), |(start, end)| (start.trim(), end.trim()))
}

fn clock(value: &str, end: bool) -> Option<(u32, u32)> {
    let value = value.trim().replace('.', ":");
    let (hour, minute) = value.split_once(':').unwrap_or((&value, "0"));
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    if (hour < 24 && minute < 60) || (end && hour == 24 && minute == 0) {
        Some((hour, minute))
    } else {
        None
    }
}

fn interval(
    date: NaiveDate,
    start: &str,
    end: &str,
    zone: Tz,
) -> Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
    let (start_hour, start_minute) = clock(start, false)?;
    let (end_hour, end_minute) = clock(end, true)?;
    let local_start = date.and_time(NaiveTime::from_hms_opt(start_hour, start_minute, 0)?);
    let start = zone
        .from_local_datetime(&local_start)
        .single()?
        .fixed_offset();
    let end_date = if end_hour == 24
        || end_hour < start_hour
        || (end_hour == start_hour && end_minute <= start_minute)
    {
        date.checked_add_signed(Duration::days(1))?
    } else {
        date
    };
    let end_hour = if end_hour == 24 { 0 } else { end_hour };
    let local_end = end_date.and_time(NaiveTime::from_hms_opt(end_hour, end_minute, 0)?);
    let end = zone
        .from_local_datetime(&local_end)
        .single()?
        .fixed_offset();
    (end > start).then_some((start, end))
}

fn sps_notes(value: &str, label: &str) -> Option<String> {
    let starts_with = |line: &str, prefix: &str| {
        line.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    };
    let mut lines = Vec::new();
    for line in value.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let intervals = if starts_with(line, "uni") {
            &line[3..]
        } else if starts_with(line, label) {
            &line[label.len()..]
        } else {
            return None;
        };
        if intervals.trim().is_empty() {
            return None;
        }
        lines.push(format!("uni {}", intervals.trim()));
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TZ: Tz = chrono_tz::Europe::Copenhagen;

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    /// A 1x5 column: date, weekday, helper, time, SPS.
    fn column_template() -> SheetLayout {
        let cells = tsv_cells(b"02/11/26\nMandag\nAlex\n8-24\nSps 8-16\n").unwrap();
        SheetLayout::from_example(
            &cells,
            TemplateCells {
                date: (0, 0),
                helper: (2, 0),
                time: (3, 0),
                sps: Some((4, 0)),
                ..Default::default()
            },
            TZ,
        )
        .unwrap()
    }

    #[test]
    fn a_column_template_finds_every_day_of_a_week_grid() {
        let layout = column_template();
        assert_eq!(layout.date_format, "%d/%m/%y");
        assert_eq!(layout.sps_label, "Sps");
        // Row labels in column A and a heading date with nothing below it
        // are not shifts.
        let sheet = csv_cells(
            b"Opdateret,01/11/26\n\
              Dato,02/11/26,03/11/26\n\
              Dag,Mandag,Tirsdag\n\
              Navn,Alex,Joe\n\
              Tid,8-24,20-8\n\
              SPS,Sps 8-16,\n",
        )
        .unwrap();
        let parsed = parse_cells(&sheet, &layout, "sheet", TZ, day(2026, 11, 2)).unwrap();
        assert!(parsed.issues.is_empty());
        assert_eq!(parsed.shifts.len(), 2);
        assert_eq!(parsed.shifts[0].event_id, "20261102");
        assert_eq!(parsed.shifts[0].notes, "uni 8-16");
        assert_eq!(parsed.shifts[1].helper_key, "Joe");
        assert_eq!(
            parsed.shifts[1].ends_at.to_rfc3339(),
            "2026-11-04T08:00:00+01:00"
        );
    }

    #[test]
    fn a_helper_is_found_even_when_their_shift_needs_a_fix() {
        let layout = column_template();
        // Merged cells export as one value and empty neighbours.
        let sheet = csv_cells(
            b"21/9/26,,,22/9/26\n\
              Mandag,,,Tirsdag\n\
              Zain,,,Ninke\n\
              7:30-12,,,\n",
        )
        .unwrap();
        let parsed = parse_cells(&sheet, &layout, "sheet", TZ, day(2026, 9, 21)).unwrap();
        assert_eq!(parsed.shifts.len(), 1);
        assert_eq!(
            parsed.issues[0].message("SPS"),
            "Ninkes vagt d. 22/9 mangler tid."
        );
        assert_eq!(
            parsed
                .helpers
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["Ninke", "Zain"]
        );
    }

    #[test]
    fn a_sheet_may_show_its_dates_in_another_supported_format() {
        let cells = tsv_cells(b"21/9\nMandag\nZain\n7:30-12\n").unwrap();
        let layout = SheetLayout::from_example(
            &cells,
            TemplateCells {
                date: (0, 0),
                helper: (2, 0),
                time: (3, 0),
                ..Default::default()
            },
            TZ,
        )
        .unwrap();
        let sheet =
            csv_cells(b"21/9,22/09/2026\nMandag,Tirsdag\nZain,Ninke\n7:30-12,16-20\n").unwrap();
        let parsed = parse_cells(&sheet, &layout, "sheet", TZ, day(2026, 9, 21)).unwrap();
        assert!(parsed.issues.is_empty());
        assert_eq!(parsed.shifts.len(), 2);
        assert_eq!(parsed.shifts[1].event_id, "20260922");
    }

    #[test]
    fn issues_name_the_helper_and_date_instead_of_a_cell() {
        let issue = |kind, helper: Option<&str>, date| SheetIssue {
            cell: "Q4".into(),
            kind,
            date,
            helper: helper.map(str::to_owned),
        };
        let sunday = Some(day(2026, 9, 27));
        assert_eq!(
            issue(IssueKind::MissingTime, Some("Zain"), sunday).message("SPS"),
            "Zains vagt d. 27/9 mangler tid."
        );
        assert_eq!(
            issue(IssueKind::UnreadableSps, Some("Jonas"), sunday).message("SPS"),
            "Jonas' vagt d. 27/9 har SPS-timer, der ikke kan læses. Skriv dem som »SPS 8-10«."
        );
        assert_eq!(
            issue(IssueKind::MissingHelper, None, sunday).message("SPS"),
            "Vagten d. 27/9 mangler en hjælper."
        );
        // Without a date there is nothing else to point at.
        assert_eq!(
            issue(IssueKind::ImpossibleDate, None, None).message("SPS"),
            "Datoen i celle Q4 findes ikke."
        );
    }

    #[test]
    fn a_row_template_with_separate_times_reads_a_table() {
        let cells = tsv_cells(b"2026-11-02\tAlex\t08:00\t12:00\n").unwrap();
        let layout = SheetLayout::from_example(
            &cells,
            TemplateCells {
                date: (0, 0),
                helper: (0, 1),
                time: (0, 2),
                end: Some((0, 3)),
                ..Default::default()
            },
            TZ,
        )
        .unwrap();
        let sheet =
            csv_cells(b"2026-11-02,Alex,08:00,12:00\n2026-11-03,John,09:00,16:00\n").unwrap();
        let parsed = parse_cells(&sheet, &layout, "sheet", TZ, day(2026, 11, 2)).unwrap();
        assert!(parsed.issues.is_empty());
        assert_eq!(parsed.shifts.len(), 2);
        assert_eq!(
            parsed.shifts[1].starts_at.to_rfc3339(),
            "2026-11-03T09:00:00+01:00"
        );
    }

    #[test]
    fn a_wrong_pick_is_rejected_while_setting_up() {
        let cells = tsv_cells(b"02/11/26\nMandag\nAlex\n8-24\n").unwrap();
        // The weekday cell picked as the time.
        let wrong = TemplateCells {
            date: (0, 0),
            helper: (2, 0),
            time: (1, 0),
            ..Default::default()
        };
        assert!(SheetLayout::from_example(&cells, wrong, TZ).is_err());
    }

    #[test]
    fn bad_cells_block_without_disclosing_their_contents() {
        let layout = column_template();
        let sheet =
            csv_cells(b"02/11/26,31/11/26\nMandag,Tirsdag\nAlex,Joe\nsecret time,8-24\n").unwrap();
        let parsed = parse_cells(&sheet, &layout, "sheet", TZ, day(2026, 11, 2)).unwrap();
        let cells: Vec<_> = parsed.issues.iter().map(|i| i.cell.as_str()).collect();
        // The impossible date is reported rather than silently dropped.
        assert_eq!(cells, ["B1", "A4"]);
        assert_eq!(parsed.issues[0].date, None);
        assert!(!format!("{:?}", parsed.issues).contains("secret"));
    }

    #[test]
    fn identity_is_the_date_and_a_repeated_date_is_rejected() {
        let layout = column_template();
        let before = csv_cells(b"02/11/26\nMandag\nAlex\n8-24\n").unwrap();
        let edited = csv_cells(b"02/11/26\nMandag\nJoe\n9-16\n").unwrap();
        let key = |cells| {
            parse_cells(cells, &layout, "sheet", TZ, day(2026, 11, 2))
                .unwrap()
                .shifts[0]
                .key()
        };
        assert_eq!(key(&before), key(&edited));

        let repeated =
            csv_cells(b"02/11/26,02/11/26\nMandag,Mandag\nAlex,Joe\n8-24,8-24\n").unwrap();
        let parsed = parse_cells(&repeated, &layout, "sheet", TZ, day(2026, 11, 2)).unwrap();
        assert_eq!(parsed.shifts.len(), 1);
        assert_eq!(parsed.issues[0].cell, "B1");
    }

    #[test]
    fn a_date_without_a_year_takes_the_year_nearest_the_week_read() {
        assert_eq!(
            parse_date("23/9", "%d/%m", day(2026, 9, 21)),
            Some(day(2026, 9, 23))
        );
        // Across the new year, each day lands in the week being read.
        let monday = day(2026, 12, 28);
        assert_eq!(
            parse_date("28/12", "%d/%m", monday),
            Some(day(2026, 12, 28))
        );
        assert_eq!(parse_date("3/1", "%d/%m", monday), Some(day(2027, 1, 3)));
        assert_eq!(parse_date("30/2", "%d/%m", monday), None);
        assert!(looks_like_date("30/2"));
        // Times are not date attempts.
        assert!(!looks_like_date("8-12") && !looks_like_date("8.30"));
    }

    #[test]
    fn blank_rows_keep_sheet_coordinates() {
        let cells = csv_cells(b"first\r\n\r\n\"two\nlines\",third\r\n").unwrap();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[1], vec![String::new()]);
        assert_eq!(cells[2][0], "two\nlines");
    }
}
