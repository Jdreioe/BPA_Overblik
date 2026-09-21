//! SQLite storage compatible with the Python application's existing state files.
//!
//! These are blocking disk operations; callers should keep them off the UI thread.
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use chrono::{DateTime, FixedOffset, SecondsFormat};
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::SourceShift;

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("Could not access synchronization state")]
    Io(#[from] std::io::Error),
    #[error("Could not read or write synchronization database")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Stored synchronization payload is invalid")]
    Payload(#[from] serde_json::Error),
    #[error("Another apply run is using this state file")]
    ApplyInProgress,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepRecord {
    pub source_key: String,
    pub step_key: String,
    /// Preserve legacy statuses verbatim; only reconciliation may decide recovery.
    pub status: String,
    pub destination_id: Option<String>,
    pub source_hash: String,
    pub synced_payload: Map<String, Value>,
    pub error: Option<String>,
}

pub struct SyncState {
    connection: Connection,
    path: PathBuf,
}

/// Retain this guard across submission and read-back, not just the SQLite write.
/// Closing its private file releases the OS lock, including during unwinding.
#[must_use = "keep the guard alive throughout the apply run"]
pub struct ApplyGuard {
    _file: Option<File>,
}

impl SyncState {
    /// Open or initialize state without changing any existing progress records.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StateError> {
        let path = path.as_ref().to_path_buf();
        if path != Path::new(":memory:") {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut connection = Connection::open(&path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(include_str!("state_schema.sql"))?;
        for column in ["recurrence_start", "source_version"] {
            let exists = transaction
                .prepare("PRAGMA table_info(source_occurrences)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|name| name == column);
            if !exists {
                transaction.execute_batch(&format!(
                    "ALTER TABLE source_occurrences ADD COLUMN {column} TEXT"
                ))?;
            }
        }
        transaction.commit()?;
        Ok(Self { connection, path })
    }

    pub fn exclusive_apply(&self) -> Result<ApplyGuard, StateError> {
        if self.path == Path::new(":memory:") {
            return Ok(ApplyGuard { _file: None });
        }
        let mut path = self.path.as_os_str().to_os_string();
        path.push(".lock");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)?;
        FileExt::try_lock_exclusive(&file).map_err(|error| {
            if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                StateError::ApplyInProgress
            } else {
                StateError::Io(error)
            }
        })?;
        Ok(ApplyGuard { _file: Some(file) })
    }

    pub fn get_step(
        &self,
        source_key: &str,
        step_key: &str,
    ) -> Result<Option<StepRecord>, StateError> {
        self.connection
            .query_row(
                "SELECT * FROM sync_steps WHERE source_key = ? AND step_key = ?",
                params![source_key, step_key],
                read_step,
            )
            .optional()?
            .transpose()
    }

    pub fn steps_for_source(&self, source_key: &str) -> Result<Vec<StepRecord>, StateError> {
        let mut statement = self
            .connection
            .prepare("SELECT * FROM sync_steps WHERE source_key = ? ORDER BY step_key")?;
        let rows = statement.query_map([source_key], read_step)?;
        rows.map(|row| row.map_err(StateError::from).and_then(|record| record))
            .collect()
    }

    /// Clear local progress only. Future reconciliation must still check duplicates.
    pub fn forget_steps(&mut self, source_key: &str) -> Result<Vec<String>, StateError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let keys = transaction
            .prepare("SELECT step_key FROM sync_steps WHERE source_key = ? ORDER BY step_key")?
            .query_map([source_key], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        transaction.execute("DELETE FROM sync_steps WHERE source_key = ?", [source_key])?;
        transaction.commit()?;
        Ok(keys)
    }

    /// Commit before returning, so an uncertain marker survives a process crash.
    pub fn record_step(
        &self,
        record: &StepRecord,
        updated_at: DateTime<FixedOffset>,
    ) -> Result<(), StateError> {
        let payload = serde_json::to_string(&record.synced_payload)?;
        self.connection.execute(
            "INSERT INTO sync_steps (source_key, step_key, status, destination_id, source_hash,
                 synced_payload_json, error, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_key, step_key) DO UPDATE SET status = excluded.status,
                 destination_id = excluded.destination_id, source_hash = excluded.source_hash,
                 synced_payload_json = excluded.synced_payload_json, error = excluded.error,
                 updated_at = excluded.updated_at",
            params![
                record.source_key,
                record.step_key,
                record.status,
                record.destination_id,
                record.source_hash,
                payload,
                record.error,
                isoformat(updated_at)
            ],
        )?;
        Ok(())
    }

    /// Record a successful reconciliation. Preview callers must not call this.
    /// Occurrence and comment changes commit together or are rolled back together.
    pub fn record_source_snapshot(
        &mut self,
        shift: &SourceShift,
        observed_at: DateTime<FixedOffset>,
    ) -> Result<(), StateError> {
        let source_key = shift.key();
        let observed_at = isoformat(observed_at);
        let recurrence_start = shift.recurrence_start.map(isoformat);
        let hash = snapshot_hash(BTreeMap::from([
            ("title", Some(shift.title.clone())),
            ("helper_key", Some(shift.helper_key.clone())),
            ("notes", Some(shift.notes.clone())),
            ("starts_at", Some(isoformat(shift.starts_at))),
            ("ends_at", Some(isoformat(shift.ends_at))),
            ("recurrence_start", recurrence_start.clone()),
            ("source_version", shift.source_version.clone()),
        ]));
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO source_occurrences (source_key, calendar_id, event_id, occurrence_id,
                 recurrence_start, source_version, snapshot_hash, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_key) DO UPDATE SET calendar_id = excluded.calendar_id,
                 event_id = excluded.event_id, occurrence_id = excluded.occurrence_id,
                 recurrence_start = excluded.recurrence_start, source_version = excluded.source_version,
                 snapshot_hash = excluded.snapshot_hash, last_seen_at = excluded.last_seen_at",
            params![source_key, shift.calendar_id, shift.event_id, shift.occurrence_id,
                recurrence_start, shift.source_version, hash, observed_at],
        )?;
        for comment in &shift.comments {
            let updated_at = comment.updated_at.map(isoformat);
            // Python hashes str(datetime), whose date/time separator is a space,
            // and str(None), while storing ISO timestamps / SQL NULL separately.
            let hash = snapshot_hash(BTreeMap::from([
                ("text", Some(comment.text.clone())),
                (
                    "updated_at",
                    Some(
                        updated_at
                            .as_ref()
                            .map(|s| s.replacen('T', " ", 1))
                            .unwrap_or_else(|| "None".into()),
                    ),
                ),
            ]));
            transaction.execute(
                "INSERT INTO source_comments (source_key, comment_id, snapshot_hash, text, updated_at, last_seen_at)
                 VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(source_key, comment_id) DO UPDATE SET
                 snapshot_hash = excluded.snapshot_hash, text = excluded.text,
                 updated_at = excluded.updated_at, last_seen_at = excluded.last_seen_at",
                params![source_key, comment.id, hash, comment.text, updated_at, observed_at],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// The newest successful full-batch verification time, if any.
    ///
    /// Source snapshots are only recorded after the final read-back, so this
    /// is the last time a transfer was fully verified, not the last attempt.
    pub fn last_source_snapshot_at(&self) -> Result<Option<DateTime<FixedOffset>>, StateError> {
        let latest: Option<String> = self.connection.query_row(
            "SELECT max(last_seen_at) FROM source_occurrences",
            [],
            |row| row.get(0),
        )?;
        latest
            .map(|value| {
                DateTime::parse_from_rfc3339(&value).map_err(|_| {
                    StateError::Sqlite(rusqlite::Error::InvalidColumnType(
                        0,
                        "last_seen_at".into(),
                        rusqlite::types::Type::Text,
                    ))
                })
            })
            .transpose()
    }
}

fn read_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<StepRecord, StateError>> {
    let payload: String = row.get("synced_payload_json")?;
    let record = StepRecord {
        source_key: row.get("source_key")?,
        step_key: row.get("step_key")?,
        status: row.get("status")?,
        destination_id: row.get("destination_id")?,
        source_hash: row.get("source_hash")?,
        synced_payload: match serde_json::from_str(&payload) {
            Ok(payload) => payload,
            Err(error) => return Ok(Err(error.into())),
        },
        error: row.get("error")?,
    };
    Ok(Ok(record))
}

pub(crate) fn isoformat(value: DateTime<FixedOffset>) -> String {
    let precision = if value.timestamp_subsec_micros() == 0 {
        SecondsFormat::Secs
    } else {
        SecondsFormat::Micros
    };
    value.to_rfc3339_opts(precision, false)
}

/// Match Python's sorted, ASCII-escaped JSON for the string/null snapshot maps.
/// This is deliberately not a general JSON/approval digest serializer.
fn snapshot_hash(payload: BTreeMap<&str, Option<String>>) -> String {
    fn string(value: &str) -> String {
        let json = serde_json::to_string(value).expect("serializing a string cannot fail");
        let mut ascii = String::new();
        for c in json.chars() {
            if c.is_ascii() && c != '\u{7f}' {
                ascii.push(c);
            } else {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    ascii.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
        ascii
    }
    let entries = payload
        .into_iter()
        .map(|(key, value)| {
            format!(
                "{}: {}",
                string(key),
                value
                    .as_deref()
                    .map(string)
                    .unwrap_or_else(|| "null".into())
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{:x}", Sha256::digest(format!("{{{entries}}}")))
}
