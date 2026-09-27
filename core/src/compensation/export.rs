//! The zip the person attaches to an application: `Udgiftsoversigt.pdf`,
//! `Bilagsliste.pdf` and the numbered bilag in `Bilag/`.

use super::store::{extension, write_atomically};
use super::{pdf, Bilag, CompensationError, Expense, Log, Period, Store, Summary};
use std::io::Write;
use std::path::Path;

/// A bilag's number and its file name in the zip's `Bilag` folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BilagNumber {
    pub number: usize,
    pub file_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Documentation {
    Bilag(BilagNumber),
    /// The expense had a bilag, but its file is gone. It counts as
    /// sandsynliggjort and gets no number.
    Missing,
    /// No bilag: sandsynliggjort.
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportRow {
    pub expense: Expense,
    pub documentation: Documentation,
}

/// The period's expenses in date order, with bilag numbered in that order so
/// both documents and the folder agree. `present` says whether a bilag's
/// file can still be read.
pub fn export_rows(
    expenses: &[Expense],
    period: Period,
    present: impl Fn(&Bilag) -> bool,
) -> Vec<ExportRow> {
    let mut chosen: Vec<&Expense> = expenses
        .iter()
        .filter(|e| period.contains(e.entry.date))
        .collect();
    chosen.sort_by_key(|e| (e.entry.date, e.id));
    let count = chosen
        .iter()
        .filter(|e| e.bilag.as_ref().is_some_and(&present))
        .count();
    let width = count.to_string().len().max(2);
    let mut number = 0;
    chosen
        .into_iter()
        .map(|expense| {
            let documentation = match &expense.bilag {
                None => Documentation::None,
                Some(bilag) if present(bilag) => {
                    number += 1;
                    let suffix = extension(&bilag.file)
                        .map(|extension| format!(".{extension}"))
                        .unwrap_or_default();
                    Documentation::Bilag(BilagNumber {
                        number,
                        file_name: format!(
                            "Bilag {number:0width$} - {} - {}{suffix}",
                            expense.entry.date,
                            expense.entry.category.label()
                        ),
                    })
                }
                Some(_) => Documentation::Missing,
            };
            ExportRow {
                expense: expense.clone(),
                documentation,
            }
        })
        .collect()
}

/// Write the period's export to `target`. Blocking. The zip is written
/// beside `target` first, so a failure never leaves half a zip.
pub fn write_export(
    store: &Store,
    log: &Log,
    period: Period,
    created: chrono::NaiveDate,
    target: &Path,
) -> Result<(), CompensationError> {
    let present = |bilag: &Bilag| store.bilag_path(bilag).is_file();
    let rows = export_rows(&log.expenses, period, present);
    let summary = Summary::new(&log.expenses, period, |expense| {
        expense.bilag.as_ref().is_some_and(present)
    });
    let overview = pdf::overview(&rows, &summary, created);
    let list = pdf::bilagsliste(&rows, period, created);
    write_atomically(target, |file| {
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("Udgiftsoversigt.pdf", options)?;
        zip.write_all(&overview)?;
        zip.start_file("Bilagsliste.pdf", options)?;
        zip.write_all(&list)?;
        for row in &rows {
            if let (Documentation::Bilag(number), Some(bilag)) =
                (&row.documentation, &row.expense.bilag)
            {
                zip.start_file(format!("Bilag/{}", number.file_name), options)?;
                std::io::copy(&mut std::fs::File::open(store.bilag_path(bilag))?, &mut zip)?;
            }
        }
        zip.finish()?;
        Ok(())
    })
    .map_err(|_| {
        CompensationError("Eksporten kunne ikke gemmes. Vælg en anden mappe, og prøv igen.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compensation::{BilagChange, Category, Change, Entry};

    fn save(store: &Store, date: &str, category: Category, bilag: Option<&Path>) -> Log {
        store
            .apply(Change::Save {
                id: None,
                entry: Entry {
                    date: date.parse().unwrap(),
                    category,
                    amount: 250,
                    note: "Taxa til træning".into(),
                },
                bilag: bilag.map_or(BilagChange::Keep, |path| BilagChange::Replace(path.into())),
            })
            .unwrap()
    }

    #[test]
    fn bilag_are_numbered_in_date_order_and_a_missing_one_gets_no_number() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = dir.path().join("kvittering.pdf");
        std::fs::write(&receipt, b"%PDF receipt").unwrap();
        let store = Store::new(&dir.path().join("data"));
        save(&store, "2026-09-20", Category::Medicine, Some(&receipt));
        save(&store, "2026-09-02", Category::Transport, Some(&receipt));
        save(&store, "2026-09-10", Category::Diet, None);
        let log = save(&store, "2026-09-15", Category::Leisure, Some(&receipt));
        let gone = log.expenses[3].bilag.clone().unwrap();
        std::fs::remove_file(store.bilag_path(&gone)).unwrap();

        let rows = export_rows(&log.expenses, Period::year(2026), |b| {
            store.bilag_path(b).is_file()
        });
        let documentation: Vec<_> = rows.iter().map(|r| r.documentation.clone()).collect();
        assert_eq!(
            documentation,
            [
                Documentation::Bilag(BilagNumber {
                    number: 1,
                    file_name: "Bilag 01 - 2026-09-02 - Befordring.pdf".into()
                }),
                Documentation::None,
                Documentation::Missing,
                Documentation::Bilag(BilagNumber {
                    number: 2,
                    file_name: "Bilag 02 - 2026-09-20 - Medicin.pdf".into()
                }),
            ]
        );
    }

    #[test]
    fn the_zip_holds_both_documents_and_each_existing_bilag() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = dir.path().join("kvittering.png");
        std::fs::write(&receipt, b"png bytes").unwrap();
        let store = Store::new(&dir.path().join("data"));
        save(&store, "2026-03-04", Category::Utilities, Some(&receipt));
        save(&store, "2025-03-04", Category::Utilities, Some(&receipt));
        let log = save(&store, "2026-05-06", Category::Clothing, None);
        let target = dir.path().join("eksport.zip");

        write_export(
            &store,
            &log,
            Period::year(2026),
            "2026-09-27".parse().unwrap(),
            &target,
        )
        .unwrap();

        let mut zip = zip::ZipArchive::new(std::fs::File::open(&target).unwrap()).unwrap();
        let mut names: Vec<String> = zip.file_names().map(str::to_owned).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "Bilag/Bilag 01 - 2026-03-04 - El, vand og varme.png",
                "Bilagsliste.pdf",
                "Udgiftsoversigt.pdf",
            ]
        );
        let mut bilag = Vec::new();
        std::io::Read::read_to_end(
            &mut zip
                .by_name("Bilag/Bilag 01 - 2026-03-04 - El, vand og varme.png")
                .unwrap(),
            &mut bilag,
        )
        .unwrap();
        assert_eq!(bilag, b"png bytes");
        let mut overview = Vec::new();
        std::io::Read::read_to_end(
            &mut zip.by_name("Udgiftsoversigt.pdf").unwrap(),
            &mut overview,
        )
        .unwrap();
        assert!(overview.starts_with(b"%PDF-"));
        // Only the file names the zip itself uses, never a path from the disk.
        let text = String::from_utf8_lossy(&overview);
        assert!(!text.contains(&*dir.path().to_string_lossy()));
    }
}
