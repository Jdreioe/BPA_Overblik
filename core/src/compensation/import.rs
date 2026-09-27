//! One-time import of a report zip from kompensationsydelsesapp
//! (`udgifter.csv` and `bilag/`), for moving from that app to this one.
//!
//! The app's CSV has no bilag column. Its bilag are named after the
//! expense's beskrivelse (`Bilag: Taxa.pdf`, `Bilag: Taxa (2).pdf`), so a
//! bilag is linked only when exactly one expense and one file share that
//! name. Every other bilag is reported by file name, to attach by hand.

use super::{CompensationError, Entry, ExpenseType, Log, Route, Store};
use chrono::NaiveDate;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::Path;

const NOT_A_REPORT: CompensationError =
    CompensationError("Filen er ikke en rapport fra kompensationsydelsesapp.");

/// What an import did, for the person to check.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Imported {
    pub expenses: usize,
    pub bilag: usize,
    /// Already in the log, for example from an earlier import.
    pub skipped: usize,
    /// CSV rows without a readable date or price.
    pub unreadable: usize,
    /// Bilag that could not be linked to exactly one expense.
    pub unmatched: Vec<String>,
}

impl Store {
    /// Add the expenses and bilag in the report zip at `path`. Expenses
    /// already in the log are skipped, so importing twice adds nothing.
    /// Blocking.
    pub fn import_rapport(&self, path: &Path) -> Result<(Log, Imported), CompensationError> {
        let file = std::fs::File::open(path).map_err(|_| NOT_A_REPORT)?;
        let mut archive = zip::ZipArchive::new(file).map_err(|_| NOT_A_REPORT)?;
        // The report may have been unpacked and zipped again inside a folder,
        // so its bilag are read from beside its CSV. macOS adds `__MACOSX/`
        // copies of every file, which are not the report.
        let names: Vec<String> = archive
            .file_names()
            .filter(|name| !name.starts_with("__MACOSX/"))
            .map(str::to_owned)
            .collect();
        let csv_name = names
            .iter()
            .find(|name| name.rsplit('/').next() == Some("udgifter.csv"))
            .ok_or(NOT_A_REPORT)?
            .clone();
        let folder = format!(
            "{}bilag/",
            &csv_name[..csv_name.len() - "udgifter.csv".len()]
        );
        let mut read = |name: &str| -> Result<Vec<u8>, CompensationError> {
            let mut bytes = Vec::new();
            archive
                .by_name(name)
                .map_err(|_| NOT_A_REPORT)?
                .read_to_end(&mut bytes)
                .map_err(|_| NOT_A_REPORT)?;
            Ok(bytes)
        };
        let csv = read(&csv_name)?;
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        // The app writes a bilag twice when it is both in its folder and
        // referenced, the second time with a ` (2)` name. Keep the first. A
        // different expense's identical receipt has another name and stays.
        let mut seen = BTreeSet::new();
        for name in &names {
            let Some(file_name) = name.strip_prefix(&folder) else {
                continue;
            };
            if file_name.is_empty() || file_name.contains('/') {
                continue;
            }
            let bytes = read(name)?;
            let base = bilag_base(file_name).unwrap_or_else(|| file_name.to_owned());
            if seen.insert((base, Sha256::digest(&bytes))) {
                files.push((file_name.to_owned(), bytes));
            }
        }
        let (rows, unreadable) = parse_csv(&csv)?;
        let (links, unmatched) = match_bilag(&rows, &files);

        let _guard = self.locked();
        let mut log = self.read()?;
        let mut imported = Imported {
            unreadable,
            unmatched,
            ..Imported::default()
        };
        let mut created = Vec::new();
        // Each expense already in the log accounts for one identical row, so
        // two identical rows are both imported the first time and both
        // skipped the next.
        let mut existing: Vec<Option<Entry>> =
            log.expenses.iter().map(|e| Some(e.entry.clone())).collect();
        for (entry, link) in rows.into_iter().zip(links) {
            if let Some(slot) = existing
                .iter_mut()
                .find(|slot| slot.as_ref() == Some(&entry))
            {
                *slot = None;
                imported.skipped += 1;
                continue;
            }
            let bilag = match link {
                Some(index) => {
                    let (name, bytes) = &files[index];
                    // `Bilag: Taxa.pdf` is shown as `Taxa.pdf` after »Bilag:«.
                    let shown = name.strip_prefix("Bilag: ").unwrap_or(name).to_owned();
                    match self.new_bilag(shown, |file| file.write_all(bytes)) {
                        Ok(bilag) => {
                            created.push(bilag.clone());
                            imported.bilag += 1;
                            Some(bilag)
                        }
                        Err(_) => {
                            for bilag in &created {
                                self.remove(Some(bilag));
                            }
                            return Err(CompensationError(
                                "Bilagene kunne ikke gemmes. Kontrollér diskplads og rettigheder.",
                            ));
                        }
                    }
                }
                None => None,
            };
            log.push(entry, bilag);
            imported.expenses += 1;
        }
        if imported.expenses > 0 {
            log.revision += 1;
            if let Err(error) = self.write(&log) {
                for bilag in &created {
                    self.remove(Some(bilag));
                }
                return Err(error);
            }
        }
        Ok((log, imported))
    }
}

/// The app's `Date,TYPE,BESKRIVELSE,FRA,TIL,Km,PRIS,ANDET` rows, and how
/// many could not be read.
fn parse_csv(bytes: &[u8]) -> Result<(Vec<Entry>, usize), CompensationError> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(bytes);
    let headers = reader.headers().map_err(|_| NOT_A_REPORT)?;
    if headers.get(2) != Some("BESKRIVELSE") || headers.get(6) != Some("PRIS") {
        return Err(NOT_A_REPORT);
    }
    let mut rows = Vec::new();
    let mut unreadable = 0;
    for record in reader.records() {
        match record.ok().as_ref().and_then(parse_row) {
            Some(entry) => rows.push(entry),
            None => unreadable += 1,
        }
    }
    Ok((rows, unreadable))
}

fn parse_row(record: &csv::StringRecord) -> Option<Entry> {
    let field = |index| record.get(index).unwrap_or("").trim();
    let date = NaiveDate::parse_from_str(field(0), "%Y-%m-%d").ok()?;
    let kind = ExpenseType::from_label(field(1));
    let route = (kind == ExpenseType::Driving).then(|| Route {
        fra: field(3).to_owned(),
        til: field(4).to_owned(),
        km: field(5)
            .parse::<f64>()
            .ok()
            .filter(|km| km.is_finite() && *km >= 0.0),
    });
    Some(Entry {
        date,
        kind,
        beskrivelse: field(2).to_owned(),
        route,
        pris: field(6).parse().ok()?,
        andet: field(7).to_owned(),
    })
}

/// For each row, the index of its bilag in `files`, and the names of the
/// files left over.
fn match_bilag(rows: &[Entry], files: &[(String, Vec<u8>)]) -> (Vec<Option<usize>>, Vec<String>) {
    let mut files_by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, (name, _)) in files.iter().enumerate() {
        if let Some(base) = bilag_base(name) {
            files_by_name.entry(base).or_default().push(index);
        }
    }
    let mut rows_by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, entry) in rows.iter().enumerate() {
        rows_by_name
            .entry(app_file_name(&entry.beskrivelse))
            .or_default()
            .push(index);
    }
    let mut links = vec![None; rows.len()];
    let mut linked = vec![false; files.len()];
    for (name, row_indices) in &rows_by_name {
        if let ([row], Some([file])) = (
            row_indices.as_slice(),
            files_by_name.get(name).map(Vec::as_slice),
        ) {
            links[*row] = Some(*file);
            linked[*file] = true;
        }
    }
    let unmatched = files
        .iter()
        .zip(&linked)
        .filter(|(_, linked)| !**linked)
        .map(|((name, _), _)| name.clone())
        .collect();
    (links, unmatched)
}

/// The beskrivelse as the app puts it in a bilag's name.
fn app_file_name(beskrivelse: &str) -> String {
    let cleaned: String = beskrivelse
        .trim()
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    if cleaned.trim().is_empty() {
        "Uden beskrivelse".into()
    } else {
        cleaned
    }
}

/// `Taxa` from `Bilag: Taxa.pdf` or `Bilag: Taxa (2).pdf`.
fn bilag_base(file_name: &str) -> Option<String> {
    let rest = file_name.strip_prefix("Bilag: ")?;
    let stem = rest
        .len()
        .checked_sub(4)
        .filter(|&end| rest.is_char_boundary(end) && rest[end..].eq_ignore_ascii_case(".pdf"))
        .map(|end| &rest[..end])?;
    let base = match stem.rsplit_once(" (") {
        Some((base, number))
            if number
                .strip_suffix(')')
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) =>
        {
            base
        }
        _ => stem,
    };
    Some(base.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(dir: &Path, csv: &str, bilag: &[(&str, &[u8])]) -> std::path::PathBuf {
        let path = dir.join("Rapport.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("udgifter.csv", options).unwrap();
        zip.write_all(csv.as_bytes()).unwrap();
        for (name, bytes) in bilag {
            zip.start_file(format!("bilag/{name}"), options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    const CSV: &str = "Date,TYPE,BESKRIVELSE,FRA,TIL,Km,PRIS,ANDET\n\
        \"2026-03-02\",\"Kørsel\",\"Træning\",\"Hjemvej 1\",\"Hallen 2\",\"12.4\",\"47\",\"\"\n\
        \"2026-03-01\",\"Kosrt\",\"Glutenfri, uge 9\",\"\",\"\",\"\",\"120\",\"Netto\"\n\
        \"2026-02-20\",\"Medicin\",\"Taxa\",\"\",\"\",\"\",\"80\",\"\"\n\
        \"2026-02-10\",\"Medicin\",\"Taxa\",\"\",\"\",\"\",\"80\",\"\"\n\
        \"ikke en dato\",\"Andet\",\"x\",\"\",\"\",\"\",\"1\",\"\"\n";

    #[test]
    fn an_app_report_imports_once_with_its_unambiguous_bilag() {
        let dir = tempfile::tempdir().unwrap();
        let zip = report(
            dir.path(),
            CSV,
            &[
                ("Bilag: Glutenfri, uge 9.pdf", b"kost"),
                // The same file again under the app's duplicate name.
                ("Bilag: Glutenfri, uge 9 (2).pdf", b"kost"),
                ("Bilag: Taxa.pdf", b"taxa 1"),
                ("Bilag: Taxa (2).pdf", b"taxa 2"),
            ],
        );
        let store = Store::new(&dir.path().join("data"));

        let (log, imported) = store.import_rapport(&zip).unwrap();
        assert_eq!(imported.expenses, 4);
        assert_eq!(imported.bilag, 1);
        assert_eq!(imported.unreadable, 1);
        assert_eq!(
            imported.unmatched,
            ["Bilag: Taxa.pdf", "Bilag: Taxa (2).pdf"]
        );

        let trip = log
            .expenses
            .iter()
            .find(|e| e.entry.kind == ExpenseType::Driving)
            .unwrap();
        assert_eq!(
            trip.entry.route,
            Some(Route {
                fra: "Hjemvej 1".into(),
                til: "Hallen 2".into(),
                km: Some(12.4)
            })
        );
        let diet = log
            .expenses
            .iter()
            .find(|e| e.entry.kind == ExpenseType::Diet)
            .unwrap();
        assert_eq!(diet.entry.andet, "Netto");
        let bilag = diet.bilag.as_ref().unwrap();
        assert_eq!(bilag.name, "Glutenfri, uge 9.pdf");
        assert_eq!(std::fs::read(store.bilag_path(bilag)).unwrap(), b"kost");

        let (again, imported) = store.import_rapport(&zip).unwrap();
        assert_eq!(imported.expenses, 0);
        assert_eq!(imported.skipped, 4);
        assert_eq!(again.expenses.len(), 4);
    }

    #[test]
    fn identical_rows_and_receipts_all_import() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Rapport.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        // Unpacked and zipped again on a Mac.
        zip.start_file("__MACOSX/Rapport/._udgifter.csv", options)
            .unwrap();
        zip.write_all(b"junk").unwrap();
        zip.start_file("Rapport/udgifter.csv", options).unwrap();
        zip.write_all(
            "Date,TYPE,BESKRIVELSE,FRA,TIL,Km,PRIS,ANDET\n\
             \"2026-03-02\",\"Andet\",\"Parkering\",\"\",\"\",\"\",\"20\",\"\"\n\
             \"2026-03-02\",\"Andet\",\"Parkering\",\"\",\"\",\"\",\"20\",\"\"\n\
             \"2026-03-03\",\"Andet\",\"Taxa A\",\"\",\"\",\"\",\"90\",\"\"\n\
             \"2026-03-04\",\"Andet\",\"Taxa B\",\"\",\"\",\"\",\"90\",\"\"\n"
                .as_bytes(),
        )
        .unwrap();
        for name in ["Bilag: Taxa A.pdf", "Bilag: Taxa B.pdf"] {
            zip.start_file(format!("Rapport/bilag/{name}"), options)
                .unwrap();
            zip.write_all(b"same receipt").unwrap();
        }
        zip.finish().unwrap();
        let store = Store::new(&dir.path().join("data"));

        let (_, imported) = store.import_rapport(&path).unwrap();
        assert_eq!((imported.expenses, imported.bilag), (4, 2));
        let (_, again) = store.import_rapport(&path).unwrap();
        assert_eq!((again.expenses, again.skipped), (0, 4));
    }

    #[test]
    fn a_zip_without_the_apps_csv_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let zip = report(dir.path(), "a,b\n1,2\n", &[]);
        let store = Store::new(dir.path());
        assert_eq!(store.import_rapport(&zip).unwrap_err(), NOT_A_REPORT);
        assert!(store.load().unwrap().expenses.is_empty());
    }
}
