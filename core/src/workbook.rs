//! Spreadsheet files as the text a person sees in each cell.
//!
//! The sheet template is learned from cells a person copies, so every source
//! must produce the same text they see: `02-11-2026` and `mandag`, not the
//! serial numbers a workbook stores. A CSV already is text. An `.ods` file
//! stores each cell's displayed text next to its value, so that text is used
//! as it is. An `.xlsx` file stores only values and number formats, so its
//! values come from `calamine` and are shown through their format code the way
//! Danish Excel and LibreOffice show them. `.xls` and `.xlsb` are refused:
//! their format codes are not available, and a date cell that shows only a
//! weekday or a month would then be read as a date.
//!
//! Coordinates are the sheet's own, so row 1 is always the grid's first row
//! and the parser's A1 names match what the person sees. A merged range keeps
//! its value in the top-left cell only, like Google's CSV export.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use calamine::{Data, Reader, Xlsx};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Timelike};
use quick_xml::events::{BytesStart, Event};

/// A workbook never needs more cells than this to hold a shift plan. It
/// bounds memory for a file that repeats empty rows or columns.
const MAX_CELLS: usize = 1_000_000;

/// What a downloaded or chosen file turned out to be, judged by its content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Csv,
    Xlsx,
    Ods,
}

/// Recognise a spreadsheet by its first bytes, never by its name.
pub fn kind(bytes: &[u8]) -> Result<Kind, &'static str> {
    if bytes.starts_with(b"PK\x03\x04") {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| NOT_A_SHEET)?;
        if archive.by_name("xl/workbook.xml").is_ok() {
            return Ok(Kind::Xlsx);
        }
        if archive.by_name("xl/workbook.bin").is_ok() {
            return Err(SAVE_AS_XLSX);
        }
        let mut mimetype = String::new();
        if let Ok(entry) = archive.by_name("mimetype") {
            entry
                .take(100)
                .read_to_string(&mut mimetype)
                .map_err(|_| NOT_A_SHEET)?;
        }
        if mimetype.trim() == "application/vnd.oasis.opendocument.spreadsheet" {
            return Ok(Kind::Ods);
        }
        return Err(NOT_A_SHEET);
    }
    if bytes.starts_with(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1") {
        return Err(SAVE_AS_XLSX);
    }
    let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let start = text.iter().position(|b| !b.is_ascii_whitespace());
    if start.is_some_and(|at| text[at] == b'<') {
        return Err(WEB_PAGE);
    }
    std::str::from_utf8(text).map_err(|_| NOT_A_SHEET)?;
    Ok(Kind::Csv)
}

pub const NOT_A_SHEET: &str = "Filen er ikke et regneark. Brug en .xlsx-, .ods- eller .csv-fil.";
pub const SAVE_AS_XLSX: &str =
    "Regnearket er gemt i et ældre format. Gem det som .xlsx, og prøv igen.";
pub const WEB_PAGE: &str = "Linket gav en webside i stedet for regnearket. Del det, så alle med linket kan se det uden at logge ind.";

/// The tab names of a workbook, in order. A CSV has none.
pub fn tabs(bytes: &[u8]) -> Result<Vec<String>, &'static str> {
    match kind(bytes)? {
        Kind::Csv => Ok(Vec::new()),
        Kind::Xlsx => Ok(Xlsx::new(Cursor::new(bytes))
            .map_err(|_| UNREADABLE)?
            .sheet_names()),
        Kind::Ods => Ok(ods(bytes)?.into_iter().map(|(name, _)| name).collect()),
    }
}

const UNREADABLE: &str = "Regnearket kunne ikke læses. Gem det igen, og prøv igen.";

/// The displayed text of one tab, or of the CSV. A workbook needs `tab`; a
/// tab that is gone says so instead of reading another one.
pub fn cells(bytes: &[u8], tab: Option<&str>) -> Result<Vec<Vec<String>>, &'static str> {
    let kind = kind(bytes)?;
    if kind == Kind::Csv {
        return crate::sheets::csv_cells(bytes);
    }
    let tab = tab.ok_or("Vælg fanen med vagtplanen.")?;
    let missing = "Den valgte fane findes ikke længere i regnearket. Vælg fanen igen under Indstillinger → Udbydere.";
    match kind {
        Kind::Xlsx => xlsx(bytes, tab)?.ok_or(missing),
        _ => ods(bytes)?
            .into_iter()
            .find(|(name, _)| name == tab)
            .map(|(_, grid)| grid)
            .ok_or(missing),
    }
}

fn xlsx(bytes: &[u8], tab: &str) -> Result<Option<Vec<Vec<String>>>, &'static str> {
    let mut workbook = Xlsx::new(Cursor::new(bytes)).map_err(|_| UNREADABLE)?;
    if !workbook.sheet_names().iter().any(|name| name == tab) {
        return Ok(None);
    }
    let is_1904 = workbook.has_1904_epoch();
    let range = workbook.worksheet_range(tab).map_err(|_| UNREADABLE)?;
    let formats = XlsxFormats::read(bytes, tab)?;
    let Some((top, left)) = range.start() else {
        return Ok(Some(Vec::new()));
    };
    let (height, width) = range.get_size();
    let rows = top as usize + height;
    let columns = left as usize + width;
    if rows.saturating_mul(columns) > MAX_CELLS {
        return Err(TOO_LARGE);
    }
    let mut grid = vec![vec![String::new(); columns]; rows];
    for (row, column, value) in range.used_cells() {
        let (row, column) = (top as usize + row, left as usize + column);
        let code = formats.code(row as u32, column as u32);
        grid[row][column] = shown(value, code, is_1904);
    }
    trim(&mut grid);
    Ok(Some(grid))
}

const TOO_LARGE: &str = "Regnearket er for stort til at blive læst sikkert.";

/// Drop empty trailing columns and rows, so a formatted but empty region
/// does not reach the parser.
fn trim(grid: &mut Vec<Vec<String>>) {
    for row in grid.iter_mut() {
        while row.last().is_some_and(String::is_empty) {
            row.pop();
        }
    }
    while grid.last().is_some_and(Vec::is_empty) {
        grid.pop();
    }
}

/// A cell's value shown through its number format code.
fn shown(value: &Data, code: &str, is_1904: bool) -> String {
    match value {
        Data::Empty => String::new(),
        Data::String(text) => text.clone(),
        Data::Bool(true) => "SAND".into(),
        Data::Bool(false) => "FALSK".into(),
        Data::Error(error) => error.to_string(),
        Data::Int(number) => number_text(*number as f64, code),
        Data::Float(number) => number_text(*number, code),
        Data::DateTime(serial) => serial_moment(serial.as_f64(), is_1904)
            .map(|moment| date_text(moment, serial.as_f64(), code))
            .unwrap_or_default(),
        Data::DateTimeIso(iso) => NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%S")
            .or_else(|_| {
                NaiveDate::parse_from_str(iso, "%Y-%m-%d").map(|d| d.and_time(Default::default()))
            })
            .map(|moment| date_text(moment, 0.0, code))
            .unwrap_or_else(|_| iso.clone()),
        Data::DurationIso(iso) => iso.clone(),
    }
}

/// An Excel serial day as a local date and time, rounded to the second.
/// The 1900 system counts the fictional 29 February 1900, so serials from 61
/// on count from 30 December 1899.
fn serial_moment(serial: f64, is_1904: bool) -> Option<NaiveDateTime> {
    if !serial.is_finite() || serial < 0.0 || serial > 2_958_466.0 {
        return None;
    }
    let seconds = (serial * 86_400.0).round() as i64;
    let base = if is_1904 {
        NaiveDate::from_ymd_opt(1904, 1, 1)?
    } else if serial < 61.0 {
        NaiveDate::from_ymd_opt(1899, 12, 31)?
    } else {
        NaiveDate::from_ymd_opt(1899, 12, 30)?
    };
    base.and_hms_opt(0, 0, 0)?
        .checked_add_signed(Duration::seconds(seconds))
}

/// Numbers the way Danish Excel shows them: a decimal comma, and the number
/// of decimals the format asks for. `General` shows up to ten significant
/// digits without trailing zeros.
fn number_text(number: f64, code: &str) -> String {
    let section = first_section(code);
    let percent = section.contains('%');
    let value = if percent { number * 100.0 } else { number };
    let digits = section.split_once('.').map(|(_, decimals)| {
        decimals
            .chars()
            .take_while(|c| *c == '0' || *c == '#')
            .count()
    });
    let text = match digits {
        Some(decimals) if section.contains('0') => format!("{value:.decimals$}"),
        _ if section.contains('0') && !section.eq_ignore_ascii_case("general") => {
            format!("{value:.0}")
        }
        _ => general(value),
    };
    let text = text.replace('.', ",");
    if percent {
        format!("{text}%")
    } else {
        text
    }
}

fn general(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{value:.0}");
    }
    let text = format!("{value:.10}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn first_section(code: &str) -> &str {
    let mut quoted = false;
    for (at, c) in code.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => return &code[..at],
            _ => {}
        }
    }
    code
}

const MONTHS: [&str; 12] = [
    "januar",
    "februar",
    "marts",
    "april",
    "maj",
    "juni",
    "juli",
    "august",
    "september",
    "oktober",
    "november",
    "december",
];
const DAYS: [&str; 7] = [
    "mandag", "tirsdag", "onsdag", "torsdag", "fredag", "lørdag", "søndag",
];
const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS_EN: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// A date or time through its format code, with Danish names and separators
/// unless the code names an English locale. `/` is the locale's date
/// separator, which is `-` in Danish, as in Excel and LibreOffice.
fn date_text(moment: NaiveDateTime, serial: f64, code: &str) -> String {
    let section = first_section(code);
    let english = section.contains("[$-409]") || section.contains("[$-809]");
    let section = match section {
        s if s.contains("[$-F800]") => "d. mmmm yyyy",
        s if s.contains("[$-F400]") => "hh:mm:ss",
        s => s,
    };
    let (months, days) = if english {
        (MONTHS_EN, DAYS_EN)
    } else {
        (MONTHS, DAYS)
    };
    let twelve = section.contains("AM/PM") || section.contains("A/P");
    let chars: Vec<char> = section.chars().collect();
    let mut out = String::new();
    let mut last_was_hour = false;
    let mut after_seconds = false;
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        let run = chars[at..]
            .iter()
            .take_while(|&&x| x.eq_ignore_ascii_case(&c))
            .count();
        match c.to_ascii_lowercase() {
            '"' => {
                let end = chars[at + 1..]
                    .iter()
                    .position(|&x| x == '"')
                    .map_or(chars.len(), |p| at + 1 + p);
                out.extend(&chars[at + 1..end.min(chars.len())]);
                at = end + 1;
                continue;
            }
            '\\' => {
                if let Some(next) = chars.get(at + 1) {
                    out.push(*next);
                }
                at += 2;
                continue;
            }
            '_' => {
                out.push(' ');
                at += 2;
                continue;
            }
            '*' => {
                at += 2;
                continue;
            }
            '[' => {
                let end = chars[at..]
                    .iter()
                    .position(|&x| x == ']')
                    .map_or(chars.len(), |p| at + p);
                let inner: String = chars[at + 1..end]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase();
                let total = serial.max(0.0) * 24.0;
                match inner.as_str() {
                    "h" | "hh" => out.push_str(&format!(
                        "{:0width$}",
                        total.floor() as i64,
                        width = inner.len()
                    )),
                    "m" | "mm" => out.push_str(&format!(
                        "{:0width$}",
                        (total * 60.0).floor() as i64,
                        width = inner.len()
                    )),
                    "s" | "ss" => out.push_str(&format!(
                        "{:0width$}",
                        (total * 3600.0).round() as i64,
                        width = inner.len()
                    )),
                    _ => {}
                }
                last_was_hour = inner.starts_with('h');
                at = end + 1;
                continue;
            }
            'y' => {
                if run <= 2 {
                    out.push_str(&format!("{:02}", moment.year() % 100));
                } else {
                    out.push_str(&format!("{:04}", moment.year()));
                }
            }
            'm' => {
                let minutes = last_was_hour || next_is_seconds(&chars[at + run..]);
                if minutes && run <= 2 {
                    out.push_str(&pad(moment.minute(), run));
                } else {
                    let month = moment.month0() as usize;
                    match run {
                        1 | 2 => out.push_str(&pad(moment.month(), run)),
                        3 => out.push_str(&months[month][..3]),
                        4 => out.push_str(months[month]),
                        _ => out.extend(months[month].chars().take(1)),
                    }
                }
                last_was_hour = false;
            }
            'd' => {
                let day = moment.weekday().num_days_from_monday() as usize;
                match run {
                    1 | 2 => out.push_str(&pad(moment.day(), run)),
                    3 => out.extend(days[day].chars().take(3)),
                    _ => out.push_str(days[day]),
                }
            }
            'h' => {
                let hour = if twelve {
                    (moment.hour() + 11) % 12 + 1
                } else {
                    moment.hour()
                };
                out.push_str(&pad(hour, run));
                last_was_hour = true;
            }
            's' => {
                out.push_str(&pad(moment.second(), run));
                after_seconds = true;
                at += run;
                continue;
            }
            // Tenths after seconds: the stored value is rounded to seconds.
            '.' if after_seconds => {
                at += 1 + chars[at + 1..].iter().take_while(|c| **c == '0').count();
                continue;
            }
            'a' if chars[at..]
                .iter()
                .collect::<String>()
                .to_ascii_uppercase()
                .starts_with("AM/PM") =>
            {
                out.push_str(if moment.hour() < 12 { "AM" } else { "PM" });
                at += 5;
                continue;
            }
            'a' if chars[at..]
                .iter()
                .collect::<String>()
                .to_ascii_uppercase()
                .starts_with("A/P") =>
            {
                out.push(if moment.hour() < 12 { 'A' } else { 'P' });
                at += 3;
                continue;
            }
            '/' if !english => out.push('-'),
            _ => out.extend(std::iter::repeat_n(c, run)),
        }
        after_seconds = false;
        at += run;
    }
    out
}

/// Whether the next date part after a month-or-minute `m` is seconds.
fn next_is_seconds(rest: &[char]) -> bool {
    rest.iter()
        .find(|c| c.is_ascii_alphabetic())
        .is_some_and(|c| c.eq_ignore_ascii_case(&'s'))
}

fn pad(value: u32, width: usize) -> String {
    if width >= 2 {
        format!("{value:02}")
    } else {
        value.to_string()
    }
}

/// The number format code of every styled cell on one `.xlsx` tab. calamine
/// reads the values; it does not say how they are shown.
struct XlsxFormats {
    by_cell: HashMap<(u32, u32), String>,
}

impl XlsxFormats {
    fn code(&self, row: u32, column: u32) -> &str {
        self.by_cell
            .get(&(row, column))
            .map_or("General", String::as_str)
    }

    fn read(bytes: &[u8], tab: &str) -> Result<Self, &'static str> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| UNREADABLE)?;
        let styles = entry(&mut archive, "xl/styles.xml").unwrap_or_default();
        let codes = cell_format_codes(&styles);
        let workbook = entry(&mut archive, "xl/workbook.xml").ok_or(UNREADABLE)?;
        let relations = entry(&mut archive, "xl/_rels/workbook.xml.rels").ok_or(UNREADABLE)?;
        let path = sheet_path(&workbook, &relations, tab).ok_or(UNREADABLE)?;
        let sheet = entry(&mut archive, &path).ok_or(UNREADABLE)?;
        let mut by_cell = HashMap::new();
        let mut reader = quick_xml::Reader::from_str(&sheet);
        loop {
            match reader.read_event() {
                Ok(Event::Start(element) | Event::Empty(element))
                    if element.local_name().as_ref() == b"c" =>
                {
                    let (Some(position), Some(style)) =
                        (attribute(&element, b"r"), attribute(&element, b"s"))
                    else {
                        continue;
                    };
                    let (Some(cell), Ok(style)) = (a1(&position), style.parse::<usize>()) else {
                        continue;
                    };
                    if let Some(code) = codes.get(style) {
                        by_cell.insert(cell, code.clone());
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => return Err(UNREADABLE),
                _ => {}
            }
        }
        Ok(Self { by_cell })
    }
}

fn entry(archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut text = String::new();
    archive
        .by_name(name)
        .ok()?
        .take(50_000_000)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

fn attribute(element: &BytesStart, name: &[u8]) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| {
            a.decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, element.decoder())
                .ok()
                .map(|v| v.into_owned())
        })
}

/// Zero-based row and column of an A1 reference.
fn a1(reference: &str) -> Option<(u32, u32)> {
    let letters = reference
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .count();
    let column = reference[..letters].bytes().try_fold(0u32, |acc, b| {
        acc.checked_mul(26)?
            .checked_add((b.to_ascii_uppercase() - b'A' + 1) as u32)
    })?;
    let row: u32 = reference[letters..].parse().ok()?;
    Some((row.checked_sub(1)?, column.checked_sub(1)?))
}

/// The format code of each `cellXfs` entry, in order, so a cell's `s`
/// attribute indexes it. Built-in ids use their Danish codes.
fn cell_format_codes(styles: &str) -> Vec<String> {
    let mut custom = HashMap::new();
    let mut codes = Vec::new();
    let mut in_cell_xfs = false;
    let mut reader = quick_xml::Reader::from_str(styles);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) => match e.local_name().as_ref() {
                b"numFmt" => {
                    if let (Some(id), Some(code)) =
                        (attribute(&e, b"numFmtId"), attribute(&e, b"formatCode"))
                    {
                        custom.insert(id, code);
                    }
                }
                b"cellXfs" => in_cell_xfs = true,
                b"xf" if in_cell_xfs => {
                    let id = attribute(&e, b"numFmtId").unwrap_or_else(|| "0".into());
                    let code = custom
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| builtin(&id).into());
                    codes.push(code);
                }
                _ => {}
            },
            Ok(Event::End(e)) if e.local_name().as_ref() == b"cellXfs" => in_cell_xfs = false,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    codes
}

/// Built-in number formats as Danish Excel shows them. Id 14 is the system's
/// short date, which is `dd-mm-yyyy` in Danish.
fn builtin(id: &str) -> &'static str {
    match id {
        "1" => "0",
        "2" => "0.00",
        "3" => "#,##0",
        "4" => "#,##0.00",
        "9" => "0%",
        "10" => "0.00%",
        "14" => "dd-mm-yyyy",
        "15" => "dd-mmm-yy",
        "16" => "dd-mmm",
        "17" => "mmm-yy",
        "18" => "h:mm AM/PM",
        "19" => "h:mm:ss AM/PM",
        "20" => "hh:mm",
        "21" => "hh:mm:ss",
        "22" => "dd-mm-yyyy hh:mm",
        "45" => "mm:ss",
        "46" => "[h]:mm:ss",
        "47" => "mm:ss",
        _ => "General",
    }
}

/// The part name of a tab's sheet, through the workbook's relationships.
fn sheet_path(workbook: &str, relations: &str, tab: &str) -> Option<String> {
    let mut relation = None;
    let mut reader = quick_xml::Reader::from_str(workbook);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if e.local_name().as_ref() == b"sheet" => {
                if attribute(&e, b"name").as_deref() == Some(tab) {
                    relation = attribute(&e, b"id");
                    break;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    let relation = relation?;
    let mut reader = quick_xml::Reader::from_str(relations);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if e.local_name().as_ref() == b"Relationship" => {
                if attribute(&e, b"Id").as_deref() == Some(relation.as_str()) {
                    let target = attribute(&e, b"Target")?;
                    return Some(match target.strip_prefix('/') {
                        Some(absolute) => absolute.to_owned(),
                        None => format!("xl/{target}"),
                    });
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

/// Every tab of an `.ods` file with each cell's displayed text. Repeated
/// empty rows and cells are only filled in when something follows them.
/// Each tab's name and its grid of displayed text.
type Tabs = Vec<(String, Vec<Vec<String>>)>;

fn ods(bytes: &[u8]) -> Result<Tabs, &'static str> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| UNREADABLE)?;
    let content = entry(&mut archive, "content.xml").ok_or(UNREADABLE)?;
    let mut reader = quick_xml::Reader::from_str(&content);
    let mut tabs = Vec::new();
    let mut grid: Vec<Vec<String>> = Vec::new();
    let mut name = String::new();
    let (mut row, mut row_repeat, mut empty_rows) = (Vec::<String>::new(), 1usize, 0usize);
    let (mut cell, mut cell_repeat, mut empty_cells) = (String::new(), 1usize, 0usize);
    let (mut in_cell, mut paragraphs, mut cells) = (false, 0usize, 0usize);
    let repeat = |e: &BytesStart, key: &[u8]| {
        attribute(e, key)
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1)
    };
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"table" => {
                    name = attribute(&e, b"name").unwrap_or_default();
                    grid = Vec::new();
                    empty_rows = 0;
                }
                b"table-row" => {
                    row = Vec::new();
                    row_repeat = repeat(&e, b"number-rows-repeated");
                    empty_cells = 0;
                }
                b"table-cell" | b"covered-table-cell" => {
                    cell = String::new();
                    cell_repeat = repeat(&e, b"number-columns-repeated");
                    in_cell = true;
                    paragraphs = 0;
                }
                b"p" if in_cell => {
                    if paragraphs > 0 {
                        cell.push('\n');
                    }
                    paragraphs += 1;
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"table-cell" | b"covered-table-cell" => {
                    empty_cells += repeat(&e, b"number-columns-repeated");
                }
                b"s" if in_cell => {
                    cell.extend(std::iter::repeat_n(' ', repeat(&e, b"c")));
                }
                b"tab" if in_cell => cell.push('\t'),
                b"line-break" if in_cell => cell.push('\n'),
                b"table-row" => empty_rows += repeat(&e, b"number-rows-repeated"),
                _ => {}
            },
            Ok(Event::Text(text)) if in_cell && paragraphs > 0 => {
                cell.push_str(&text.decode().map_err(|_| UNREADABLE)?);
            }
            Ok(Event::GeneralRef(reference)) if in_cell && paragraphs > 0 => {
                let resolved = match reference.decode().map_err(|_| UNREADABLE)?.as_ref() {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" => "'",
                    _ => "",
                };
                cell.push_str(resolved);
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"table-cell" | b"covered-table-cell" => {
                    in_cell = false;
                    if cell.is_empty() {
                        empty_cells += cell_repeat;
                    } else {
                        cells += empty_cells + cell_repeat;
                        if cells > MAX_CELLS {
                            return Err(TOO_LARGE);
                        }
                        row.extend(std::iter::repeat_n(String::new(), empty_cells));
                        row.extend(std::iter::repeat_n(cell.clone(), cell_repeat));
                        empty_cells = 0;
                    }
                }
                b"table-row" => {
                    if row.is_empty() {
                        empty_rows += row_repeat;
                    } else {
                        cells += (empty_rows + row_repeat) * row.len();
                        if cells > MAX_CELLS {
                            return Err(TOO_LARGE);
                        }
                        grid.extend(std::iter::repeat_n(Vec::new(), empty_rows));
                        grid.extend(std::iter::repeat_n(row.clone(), row_repeat));
                        empty_rows = 0;
                    }
                }
                b"table" => tabs.push((std::mem::take(&mut name), std::mem::take(&mut grid))),
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => return Err(UNREADABLE),
            _ => {}
        }
    }
    Ok(tabs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheets::{tsv_cells, SheetLayout, TemplateCells};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/sheets/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn grid(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|row| row.iter().map(|cell| cell.to_string()).collect())
            .collect()
    }

    /// The fixtures were saved by LibreOffice with Danish formats: a date,
    /// the weekday below it as a formula shown as `NNNN`, a merged month
    /// heading that is a date shown as `MMMM`, and time cells. The expected
    /// text is LibreOffice's own "as shown" CSV export of the same files.
    #[test]
    fn workbook_cells_read_as_libreoffice_shows_them() {
        let week = grid(&[
            &["november"],
            &["02-11-26", "03-11-26"],
            &["mandag", "tirsdag"],
            &["Alex", "Joe"],
            &["8-24", "22-8"],
        ]);
        for name in ["libreoffice.xlsx", "libreoffice.ods"] {
            let bytes = fixture(name);
            assert_eq!(tabs(&bytes).unwrap(), ["Uge", "Tider"], "{name}");
            assert_eq!(cells(&bytes, Some("Uge")).unwrap(), week, "{name}");
            let times = &cells(&bytes, Some("Tider")).unwrap()[0];
            assert_eq!(
                times[..5],
                ["02-11-26", "Alex", "08:00", "16:30", "8"],
                "{name}"
            );
        }
    }

    /// A template pasted from the file reads the file: the weekday and the
    /// month heading are text, not two more dates.
    #[test]
    fn a_template_pasted_from_the_workbook_reads_it() {
        let pasted = tsv_cells("02-11-26\nmandag\nAlex\n8-24\n".as_bytes()).unwrap();
        let picked = TemplateCells {
            date: (0, 0),
            helper: (2, 0),
            time: (3, 0),
            ..Default::default()
        };
        let layout =
            SheetLayout::from_example(&pasted, picked, chrono_tz::Europe::Copenhagen).unwrap();
        let sheet = cells(&fixture("libreoffice.xlsx"), Some("Uge")).unwrap();
        let parsed = crate::sheets::parse_cells(
            &sheet,
            &layout,
            "sheet",
            chrono_tz::Europe::Copenhagen,
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
        )
        .unwrap();
        assert!(parsed.issues.is_empty());
        let helpers: Vec<_> = parsed
            .shifts
            .iter()
            .map(|s| s.helper_key.as_str())
            .collect();
        assert_eq!(helpers, ["Alex", "Joe"]);
    }

    #[test]
    fn excel_formats_show_danish_text() {
        let serial = 46328.0 + 8.5 / 24.0; // Monday 2 November 2026 08:30
        let moment = serial_moment(serial, false).unwrap();
        let shown = |code: &str| date_text(moment, serial, code);
        assert_eq!(shown(builtin("14")), "02-11-2026");
        assert_eq!(shown("d. mmmm yyyy"), "2. november 2026");
        assert_eq!(shown("ddd d/m"), "man 2-11");
        assert_eq!(shown("[$-409]dddd d/m"), "Monday 2/11");
        assert_eq!(shown("h:mm"), "8:30");
        assert_eq!(shown("hh:mm:ss.0"), "08:30:00");
        assert_eq!(shown("mm:ss"), "30:00");
        assert_eq!(shown("h:mm AM/PM"), "8:30 AM");
        assert_eq!(
            date_text(serial_moment(1.25, false).unwrap(), 1.25, "[h]:mm"),
            "30:00"
        );
        assert_eq!(number_text(7.5, "General"), "7,5");
        assert_eq!(number_text(8.0, "0.00"), "8,00");
        assert_eq!(number_text(0.25, "0%"), "25%");
    }

    #[test]
    fn a_danish_excel_csv_uses_its_semicolons() {
        let rows = cells(&fixture("semicolon-bom.csv"), None).unwrap();
        assert_eq!(rows[0][0], "Dato");
        assert_eq!(rows[1], ["02-11-2026", "Alex; vikar", "8-24"]);
    }

    #[test]
    fn a_file_is_judged_by_its_content() {
        assert_eq!(
            kind(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1rest"),
            Err(SAVE_AS_XLSX)
        );
        assert_eq!(
            kind(b"  <!DOCTYPE html><title>Log ind</title>"),
            Err(WEB_PAGE)
        );
        assert_eq!(kind(b"\xFF\xFEnot utf-8"), Err(NOT_A_SHEET));
        assert_eq!(kind(&fixture("libreoffice.ods")), Ok(Kind::Ods));
        let missing = cells(&fixture("libreoffice.xlsx"), Some("Omdøbt")).unwrap_err();
        assert!(missing.contains("findes ikke længere"));
    }
}
