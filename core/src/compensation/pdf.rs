//! The two export documents as plain A4 tables.
//!
//! They use the PDF standard fonts Helvetica and Helvetica-Bold, which every
//! reader has, so no font file is embedded. Text is WinAnsi-encoded, which
//! covers Danish; anything else prints as `?`.

use super::{format_date, format_kr, per_month, Documentation, ExportRow, Period, Summary};
use chrono::NaiveDate;
use pdf_writer::{Content, Name, Pdf, Rect, Ref, Str, TextStr};

const PAGE_WIDTH: f32 = 595.0;
const PAGE_HEIGHT: f32 = 842.0;
const MARGIN: f32 = 42.0;
const TEXT_WIDTH: f32 = PAGE_WIDTH - 2.0 * MARGIN;
const SIZE: f32 = 9.0;
const LEADING: f32 = 12.0;
const REGULAR: Name = Name(b"F1");
const BOLD: Name = Name(b"F2");

/// Expenses, then totals, for `Udgiftsoversigt.pdf`.
pub(super) fn overview(rows: &[ExportRow], summary: &Summary, created: NaiveDate) -> Vec<u8> {
    let mut document = Document::new("Udgiftsoversigt – kompensationsydelse");
    document.paragraph(&format!(
        "Periode: {}. Udarbejdet {}.",
        summary.period.label(),
        format_date(created)
    ));
    document.paragraph(
        "Bilag henviser til nummeret i Bilagsliste.pdf og i mappen Bilag. Udgifter uden bilag er sandsynliggjort.",
    );
    if rows.is_empty() {
        document.paragraph("Der er ingen udgifter i perioden.");
    } else {
        let columns = [
            Column::left("Dato", 62.0),
            Column::left("Kategori", 118.0),
            Column::right("Beløb", 66.0),
            Column::left("Bilag", 70.0),
            Column::left("Note", TEXT_WIDTH - 316.0),
        ];
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|row| {
                let entry = &row.expense.entry;
                vec![
                    format_date(entry.date),
                    entry.category.label().to_owned(),
                    format_kr(entry.amount.into()),
                    match &row.documentation {
                        Documentation::Bilag(bilag) => format!("Nr. {:02}", bilag.number),
                        Documentation::Missing => "Bilag mangler".into(),
                        Documentation::None => "Uden bilag".into(),
                    },
                    entry.note.clone(),
                ]
            })
            .collect();
        document.table(&columns, &cells);
    }
    document.heading("Samlet");
    let mut totals: Vec<Vec<String>> = summary
        .totals
        .iter()
        .map(|(category, total)| vec![category.label().to_owned(), format_kr(*total)])
        .collect();
    let months = summary.period.months();
    totals.extend([
        vec!["I alt".into(), format_kr(summary.total)],
        vec![
            format!("Gennemsnit pr. måned ({months} måneder)"),
            format_kr(summary.average()),
        ],
        vec![
            "heraf dokumenteret".into(),
            format_kr(summary.documented_average()),
        ],
        vec![
            "heraf sandsynliggjort".into(),
            format_kr(per_month(summary.total - summary.documented, months)),
        ],
    ]);
    document.table(
        &[Column::left("", 250.0), Column::right("Beløb", 100.0)],
        &totals,
    );
    document.finish()
}

/// One row per bilag, for `Bilagsliste.pdf`. A missing bilag is listed
/// without a number, so the person can see what to find.
pub(super) fn bilagsliste(rows: &[ExportRow], period: Period, created: NaiveDate) -> Vec<u8> {
    let mut document = Document::new("Bilagsliste");
    document.paragraph(&format!(
        "Periode: {}. Udarbejdet {}.",
        period.label(),
        format_date(created)
    ));
    let cells: Vec<Vec<String>> = rows
        .iter()
        .filter_map(|row| {
            let (number, file) = match &row.documentation {
                Documentation::Bilag(bilag) => {
                    (format!("{:02}", bilag.number), bilag.file_name.clone())
                }
                Documentation::Missing => ("–".into(), "Bilag mangler".into()),
                Documentation::None => return None,
            };
            let entry = &row.expense.entry;
            Some(vec![
                number,
                format_date(entry.date),
                entry.category.label().to_owned(),
                format_kr(entry.amount.into()),
                file,
            ])
        })
        .collect();
    if cells.is_empty() {
        document.paragraph("Der er ingen bilag i perioden.");
    } else {
        document.table(
            &[
                Column::left("Nr.", 34.0),
                Column::left("Dato", 62.0),
                Column::left("Kategori", 118.0),
                Column::right("Beløb", 66.0),
                Column::left("Fil i mappen Bilag", TEXT_WIDTH - 280.0),
            ],
            &cells,
        );
    }
    document.finish()
}

struct Column {
    title: &'static str,
    width: f32,
    right: bool,
}

impl Column {
    fn left(title: &'static str, width: f32) -> Self {
        Self {
            title,
            width,
            right: false,
        }
    }
    fn right(title: &'static str, width: f32) -> Self {
        Self {
            title,
            width,
            right: true,
        }
    }
}

/// Pages laid out top to bottom. Each page's footer is added at the end,
/// once the page count is known.
struct Document {
    title: String,
    pages: Vec<Content>,
    y: f32,
}

impl Document {
    fn new(title: &str) -> Self {
        let mut document = Self {
            title: title.to_owned(),
            pages: Vec::new(),
            y: 0.0,
        };
        document.new_page();
        document.y -= 16.0;
        document.show(MARGIN, document.y, BOLD, 16.0, title);
        document.y -= 22.0;
        document
    }

    fn new_page(&mut self) {
        self.pages.push(Content::new());
        self.y = PAGE_HEIGHT - MARGIN;
    }

    /// Start a new page unless `height` still fits above the footer.
    fn reserve(&mut self, height: f32) -> bool {
        if self.y - height < MARGIN + 16.0 {
            self.new_page();
            return true;
        }
        false
    }

    fn show(&mut self, x: f32, y: f32, font: Name, size: f32, text: &str) {
        let page = self.pages.last_mut().expect("a page");
        page.begin_text()
            .set_font(font, size)
            .next_line(x, y)
            .show(Str(&winansi(text)))
            .end_text();
    }

    fn rule(&mut self, y: f32) {
        let page = self.pages.last_mut().expect("a page");
        page.set_line_width(0.5)
            .move_to(MARGIN, y)
            .line_to(PAGE_WIDTH - MARGIN, y)
            .stroke();
    }

    fn heading(&mut self, text: &str) {
        self.reserve(40.0);
        self.y -= 14.0;
        self.show(MARGIN, self.y, BOLD, 12.0, text);
        self.y -= 8.0;
    }

    fn paragraph(&mut self, text: &str) {
        for line in wrap(text, SIZE + 1.0, TEXT_WIDTH) {
            self.reserve(LEADING + 2.0);
            self.y -= LEADING + 2.0;
            self.show(MARGIN, self.y, REGULAR, SIZE + 1.0, &line);
        }
        self.y -= 6.0;
    }

    /// Rows of cells under a bold header. Left-aligned cells wrap; the
    /// header repeats on each new page.
    fn table(&mut self, columns: &[Column], rows: &[Vec<String>]) {
        self.reserve(3.0 * LEADING);
        self.table_header(columns);
        for row in rows {
            let lines: Vec<Vec<String>> = columns
                .iter()
                .zip(row)
                .map(|(column, cell)| {
                    if column.right {
                        vec![cell.clone()]
                    } else {
                        wrap(cell, SIZE, column.width - 6.0)
                    }
                })
                .collect();
            let count = lines.iter().map(Vec::len).max().unwrap_or(1);
            // A short row stays on one page. A taller one continues on the
            // next, so a long note never runs past the footer.
            if self.reserve(count.min(4) as f32 * LEADING + 4.0) {
                self.table_header(columns);
            }
            for index in 0..count {
                if self.reserve(LEADING) {
                    self.table_header(columns);
                }
                self.y -= LEADING;
                let mut x = MARGIN;
                for (column, cell) in columns.iter().zip(&lines) {
                    if let Some(line) = cell.get(index) {
                        let left = if column.right {
                            x + column.width - 6.0 - text_width(line, SIZE)
                        } else {
                            x
                        };
                        self.show(left, self.y, REGULAR, SIZE, line);
                    }
                    x += column.width;
                }
            }
            self.y -= 4.0;
        }
        self.y -= 8.0;
    }

    fn table_header(&mut self, columns: &[Column]) {
        self.y -= LEADING;
        let mut x = MARGIN;
        for column in columns {
            // Bold is about a tenth wider than the regular widths measured.
            let left = if column.right {
                x + column.width - 6.0 - 1.1 * text_width(column.title, SIZE)
            } else {
                x
            };
            self.show(left, self.y, BOLD, SIZE, column.title);
            x += column.width;
        }
        self.y -= 4.0;
        self.rule(self.y);
        self.y -= 2.0;
    }

    fn finish(mut self) -> Vec<u8> {
        let count = self.pages.len();
        for (index, page) in self.pages.iter_mut().enumerate() {
            let footer = format!("{} · side {} af {count}", self.title, index + 1);
            page.begin_text()
                .set_font(REGULAR, 8.0)
                .next_line(MARGIN, MARGIN - 18.0)
                .show(Str(&winansi(&footer)))
                .end_text();
        }
        let catalog = Ref::new(1);
        let tree = Ref::new(2);
        let regular = Ref::new(3);
        let bold = Ref::new(4);
        let info = Ref::new(5);
        let page_ids: Vec<(Ref, Ref)> = (0..count as i32)
            .map(|index| (Ref::new(6 + 2 * index), Ref::new(7 + 2 * index)))
            .collect();
        let mut pdf = Pdf::new();
        pdf.catalog(catalog).pages(tree);
        pdf.pages(tree)
            .kids(page_ids.iter().map(|(page, _)| *page))
            .count(count as i32);
        for ((page_id, content_id), content) in page_ids.into_iter().zip(self.pages) {
            let mut page = pdf.page(page_id);
            page.media_box(Rect::new(0.0, 0.0, PAGE_WIDTH, PAGE_HEIGHT))
                .parent(tree)
                .contents(content_id);
            page.resources()
                .fonts()
                .pair(REGULAR, regular)
                .pair(BOLD, bold);
            drop(page);
            pdf.stream(content_id, &content.finish());
        }
        for (id, name) in [(regular, "Helvetica"), (bold, "Helvetica-Bold")] {
            pdf.type1_font(id)
                .base_font(Name(name.as_bytes()))
                .encoding_predefined(Name(b"WinAnsiEncoding"));
        }
        pdf.document_info(info)
            .title(TextStr(&self.title))
            .producer(TextStr("BPA Overblik"));
        pdf.finish()
    }
}

/// Break `text` into lines no wider than `max`. A word wider than the
/// column is broken between characters.
fn wrap(text: &str, size: f32, max: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if text_width(&candidate, size) <= max {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        for c in word.chars() {
            line.push(c);
            if text_width(&line, size) > max && line.chars().count() > 1 {
                line.pop();
                lines.push(std::mem::replace(&mut line, c.to_string()));
            }
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

fn text_width(text: &str, size: f32) -> f32 {
    text.chars().map(glyph_width).sum::<u32>() as f32 * size / 1000.0
}

/// Helvetica's advance widths (per 1000 units of font size) for printable
/// ASCII, from its standard font metrics.
const ASCII_WIDTHS: [u32; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278,
    278, // space–/
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, // 0–?
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, // @–O
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, // P–_
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, // `–o
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, // p–~
];

fn glyph_width(c: char) -> u32 {
    match c {
        ' '..='~' => ASCII_WIDTHS[c as usize - 32],
        'æ' => 889,
        'Æ' | '—' | '…' => 1000,
        'ø' => 611,
        'Ø' => 778,
        'Å' => 667,
        // Most other Latin-1 letters, `–`, `»`, `«` and the `?` that
        // replaces anything WinAnsi lacks.
        _ => 556,
    }
}

/// Encode for WinAnsiEncoding: Latin-1 plus a few typographic marks.
fn winansi(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| match c {
            c if c.is_whitespace() => b' ',
            ' '..='~' => c as u8,
            '\u{a0}'..='\u{ff}' => c as u32 as u8,
            '€' => 0x80,
            '…' => 0x85,
            '‘' => 0x91,
            '’' => 0x92,
            '“' => 0x93,
            '”' => 0x94,
            '•' => 0x95,
            '–' => 0x96,
            '—' => 0x97,
            _ => b'?',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_notes_wrap_within_their_column() {
        let lines = wrap(
            "Taxa fra hjemmet til genoptræning og retur med hjælper",
            SIZE,
            80.0,
        );
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|line| text_width(line, SIZE) <= 80.0));
        let broken = wrap(&"x".repeat(60), SIZE, 50.0);
        assert!(broken.iter().all(|line| text_width(line, SIZE) <= 50.0));
        assert_eq!(broken.concat(), "x".repeat(60));
    }

    #[test]
    fn a_row_taller_than_a_page_continues_on_the_next() {
        let mut document = Document::new("Test");
        let columns = [Column::left("Note", 60.0)];
        let note = "ord ".repeat(400);
        document.table(&columns, &[vec![note], vec!["næste".into()]]);
        assert!(document.pages.len() > 1);
        assert!(document.y > MARGIN);
    }

    #[test]
    fn danish_letters_encode_and_the_rest_is_replaced() {
        assert_eq!(winansi("Bløde æbler – Å"), b"Bl\xf8de \xe6bler \x96 \xc5");
        assert_eq!(winansi("🙂\tok"), b"? ok");
    }
}
