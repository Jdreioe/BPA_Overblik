//! `kompensation.json` and the copied bilag in the app data directory.
//!
//! The log is separate from `setup.json` and from any account's sync history,
//! so changing the MitHF or DUOS account leaves it alone. Every change reads
//! the file, applies one `Change` and writes it back under one lock, so two
//! quick edits can never overwrite each other. A file that cannot be read is
//! reported and never replaced by an empty log.

use super::{Bilag, CompensationError, Entry, Expense};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const UNREADABLE: CompensationError = CompensationError(
    "Udgiftslisten kunne ikke læses. Den er ikke ændret. Del hvad der gik galt under Support.",
);
const UNSAVED: CompensationError =
    CompensationError("Udgiftslisten kunne ikke gemmes. Kontrollér diskplads og rettigheder.");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Log {
    version: u32,
    pub expenses: Vec<Expense>,
    /// Ask for a week's expenses once its transfer has completed.
    pub remind: bool,
    /// Mondays of transferred weeks whose reminder is not answered yet.
    pub reminders: BTreeSet<NaiveDate>,
    next_id: u64,
}

impl Default for Log {
    fn default() -> Self {
        Self {
            version: 1,
            expenses: Vec::new(),
            remind: false,
            reminders: BTreeSet::new(),
            next_id: 0,
        }
    }
}

/// What happens to an edited expense's bilag.
#[derive(Clone)]
pub enum BilagChange {
    Keep,
    Remove,
    /// Copy this file in, replacing any bilag the expense had.
    Replace(PathBuf),
}

/// One edit of the log.
#[derive(Clone)]
pub enum Change {
    /// Add an expense (`id: None`) or replace an existing one's fields.
    Save {
        id: Option<u64>,
        entry: Entry,
        bilag: BilagChange,
    },
    Delete(u64),
    /// Turn the reminder on or off. Turning it off drops waiting reminders.
    Remind(bool),
    /// A week's transfer completed. Queues its reminder when reminders are on.
    Transferred(NaiveDate),
    /// Either answer to a reminder.
    AnswerReminder(NaiveDate),
}

#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl Store {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.to_path_buf(),
            lock: Arc::new(Mutex::new(())),
        }
    }

    /// Where a bilag's copy lives.
    pub fn bilag_path(&self, bilag: &Bilag) -> PathBuf {
        self.dir.join("bilag").join(&bilag.file)
    }

    /// The saved log, or an empty one before anything is saved. Blocking.
    pub fn load(&self) -> Result<Log, CompensationError> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.read()
    }

    /// Apply one change and return the saved log. Blocking: it may copy a file.
    pub fn apply(&self, change: Change) -> Result<Log, CompensationError> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut log = self.read()?;
        // A copy made for this change, removed again if the change fails, and
        // the bilag the change made obsolete, removed once it is saved.
        let mut copied = None;
        let mut obsolete = None;
        match change {
            Change::Save { id, entry, bilag } => {
                let new_bilag = match &bilag {
                    BilagChange::Replace(source) => {
                        let copy = self.copy_bilag(source)?;
                        copied = Some(copy.clone());
                        Some(copy)
                    }
                    BilagChange::Keep | BilagChange::Remove => None,
                };
                match id {
                    None => {
                        let id = log
                            .expenses
                            .iter()
                            .map(|e| e.id)
                            .max()
                            .unwrap_or(0)
                            .max(log.next_id)
                            + 1;
                        log.next_id = id;
                        log.expenses.push(Expense {
                            id,
                            entry,
                            bilag: new_bilag,
                        });
                    }
                    Some(id) => {
                        let Some(expense) = log.expenses.iter_mut().find(|e| e.id == id) else {
                            self.remove(copied.as_ref());
                            return Err(CompensationError("Udgiften findes ikke længere."));
                        };
                        expense.entry = entry;
                        match bilag {
                            BilagChange::Keep => {}
                            BilagChange::Remove => obsolete = expense.bilag.take(),
                            BilagChange::Replace(_) => {
                                obsolete = std::mem::replace(&mut expense.bilag, new_bilag)
                            }
                        }
                    }
                }
            }
            Change::Delete(id) => {
                let Some(index) = log.expenses.iter().position(|e| e.id == id) else {
                    return Ok(log);
                };
                obsolete = log.expenses.remove(index).bilag;
            }
            Change::Remind(on) => {
                log.remind = on;
                if !on {
                    log.reminders.clear();
                }
            }
            Change::Transferred(monday) => {
                if !log.remind || !log.reminders.insert(monday) {
                    return Ok(log);
                }
            }
            Change::AnswerReminder(monday) => {
                if !log.reminders.remove(&monday) {
                    return Ok(log);
                }
            }
        }
        if let Err(error) = self.write(&log) {
            self.remove(copied.as_ref());
            return Err(error);
        }
        self.remove(obsolete.as_ref());
        Ok(log)
    }

    fn read(&self) -> Result<Log, CompensationError> {
        let path = self.dir.join("kompensation.json");
        if !path.exists() {
            return Ok(Log::default());
        }
        let log: Log = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .ok_or(UNREADABLE)?;
        if log.version != 1 {
            return Err(UNREADABLE);
        }
        Ok(log)
    }

    fn write(&self, log: &Log) -> Result<(), CompensationError> {
        private_dir(&self.dir).map_err(|_| UNSAVED)?;
        let text = serde_json::to_vec_pretty(log).map_err(|_| UNSAVED)?;
        write_atomically(&self.dir.join("kompensation.json"), |file| {
            file.write_all(&text)
        })
        .map_err(|_| UNSAVED)
    }

    fn copy_bilag(&self, source: &Path) -> Result<Bilag, CompensationError> {
        const UNCOPIED: CompensationError = CompensationError(
            "Bilaget kunne ikke kopieres. Kontrollér, at filen stadig findes, og prøv igen.",
        );
        let name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or(UNCOPIED)?;
        let folder = self.dir.join("bilag");
        private_dir(&self.dir).map_err(|_| UNCOPIED)?;
        private_dir(&folder).map_err(|_| UNCOPIED)?;
        let file = match extension(&name) {
            Some(extension) => format!("{}.{extension}", uuid::Uuid::new_v4()),
            None => uuid::Uuid::new_v4().to_string(),
        };
        let bilag = Bilag { file, name };
        let target = self.bilag_path(&bilag);
        if !std::fs::metadata(source).is_ok_and(|metadata| metadata.is_file()) {
            return Err(UNCOPIED);
        }
        write_atomically(&target, |file| {
            std::io::copy(&mut std::fs::File::open(source)?, file).map(|_| ())
        })
        .map_err(|_| UNCOPIED)?;
        Ok(bilag)
    }

    fn remove(&self, bilag: Option<&Bilag>) {
        if let Some(bilag) = bilag {
            let _ = std::fs::remove_file(self.bilag_path(bilag));
        }
    }
}

/// A short, plain file extension from `name`, lowercased, if it has one.
pub(super) fn extension(name: &str) -> Option<String> {
    let (_, extension) = name.rsplit_once('.')?;
    (!extension.is_empty()
        && extension.len() <= 5
        && extension.chars().all(|c| c.is_ascii_alphanumeric()))
    .then(|| extension.to_ascii_lowercase())
}

fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write `target` through a temporary file beside it, so a failure never
/// leaves a half-written file. Only the person can read the result.
pub(super) fn write_atomically(
    target: &Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let parent = target.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| std::io::ErrorKind::Other)?;
    let temporary = parent.join(format!(
        ".bpa-overblik-{}-{}.part",
        std::process::id(),
        stamp.as_nanos()
    ));
    let outcome = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        write(&mut file)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, target)
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compensation::Category;

    fn entry(amount: u32) -> Entry {
        Entry {
            date: "2026-09-22".parse().unwrap(),
            category: Category::Transport,
            amount,
            note: "Taxa".into(),
        }
    }

    #[test]
    fn a_replaced_or_deleted_bilag_leaves_no_copy_behind() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = dir.path().join("kvittering.JPG");
        std::fs::write(&receipt, b"receipt").unwrap();
        let store = Store::new(&dir.path().join("data"));
        let log = store
            .apply(Change::Save {
                id: None,
                entry: entry(350),
                bilag: BilagChange::Replace(receipt.clone()),
            })
            .unwrap();
        let first = log.expenses[0].bilag.clone().unwrap();
        assert_eq!(first.name, "kvittering.JPG");
        assert!(first.file.ends_with(".jpg"));
        assert_eq!(std::fs::read(store.bilag_path(&first)).unwrap(), b"receipt");

        let log = store
            .apply(Change::Save {
                id: Some(log.expenses[0].id),
                entry: entry(400),
                bilag: BilagChange::Replace(receipt),
            })
            .unwrap();
        let second = log.expenses[0].bilag.clone().unwrap();
        assert_eq!(log.expenses[0].entry.amount, 400);
        assert!(!store.bilag_path(&first).exists());

        let log = store.apply(Change::Delete(log.expenses[0].id)).unwrap();
        assert!(log.expenses.is_empty());
        assert!(!store.bilag_path(&second).exists());
        assert_eq!(store.load().unwrap(), log);
    }

    #[test]
    fn a_failed_copy_saves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let result = store.apply(Change::Save {
            id: None,
            entry: entry(350),
            bilag: BilagChange::Replace(dir.path().join("gone.pdf")),
        });
        assert!(result.is_err());
        assert!(store.load().unwrap().expenses.is_empty());
    }

    #[test]
    fn an_unreadable_log_is_reported_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kompensation.json"), "{not json").unwrap();
        let store = Store::new(dir.path());
        assert_eq!(store.load(), Err(UNREADABLE));
        assert!(store.apply(Change::Remind(true)).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("kompensation.json")).unwrap(),
            "{not json"
        );
    }

    #[test]
    fn reminders_queue_only_while_turned_on() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let monday: NaiveDate = "2026-09-21".parse().unwrap();
        assert!(store
            .apply(Change::Transferred(monday))
            .unwrap()
            .reminders
            .is_empty());
        store.apply(Change::Remind(true)).unwrap();
        let log = store.apply(Change::Transferred(monday)).unwrap();
        assert_eq!(log.reminders.iter().copied().collect::<Vec<_>>(), [monday]);
        assert!(store
            .apply(Change::AnswerReminder(monday))
            .unwrap()
            .reminders
            .is_empty());
        store.apply(Change::Transferred(monday)).unwrap();
        assert!(store
            .apply(Change::Remind(false))
            .unwrap()
            .reminders
            .is_empty());
    }
}
