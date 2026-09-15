from __future__ import annotations

import fcntl
import hashlib
import json
import sqlite3
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any, Self

from .models import SourceShift


@dataclass(frozen=True)
class StepRecord:
    source_key: str
    step_key: str
    status: str
    destination_id: str | None
    source_hash: str
    synced_payload: dict[str, Any]
    error: str | None


class SyncState:
    def __init__(self, path: Path | str):
        self.path = str(path)
        if self.path != ":memory:":
            Path(self.path).parent.mkdir(parents=True, exist_ok=True)
        self.connection = sqlite3.connect(self.path)
        self.connection.row_factory = sqlite3.Row

    def __enter__(self) -> Self:
        self.initialize()
        return self

    def __exit__(self, *_: object) -> None:
        self.connection.close()

    def initialize(self) -> None:
        self.connection.executescript(
            """
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS source_occurrences (
                source_key TEXT PRIMARY KEY,
                calendar_id TEXT NOT NULL,
                event_id TEXT NOT NULL,
                occurrence_id TEXT NOT NULL,
                recurrence_start TEXT,
                source_version TEXT,
                snapshot_hash TEXT NOT NULL,
                last_seen_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS source_comments (
                source_key TEXT NOT NULL,
                comment_id TEXT NOT NULL,
                snapshot_hash TEXT NOT NULL,
                text TEXT NOT NULL,
                updated_at TEXT,
                last_seen_at TEXT NOT NULL,
                PRIMARY KEY (source_key, comment_id),
                FOREIGN KEY (source_key) REFERENCES source_occurrences(source_key)
            );
            CREATE TABLE IF NOT EXISTS sync_steps (
                source_key TEXT NOT NULL,
                step_key TEXT NOT NULL,
                status TEXT NOT NULL,
                destination_id TEXT,
                source_hash TEXT NOT NULL,
                synced_payload_json TEXT NOT NULL,
                error TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (source_key, step_key)
            );
            """
        )
        self._ensure_column("source_occurrences", "recurrence_start", "TEXT")
        self._ensure_column("source_occurrences", "source_version", "TEXT")
        self.connection.commit()

    @contextmanager
    def exclusive_apply(self):
        """One writer per state file, including the network/read-back interval."""
        if self.path == ":memory:":
            yield
            return
        with open(self.path + ".lock", "a") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise ValueError("Another apply run is using this state file") from None
            try:
                yield
            finally:
                fcntl.flock(lock, fcntl.LOCK_UN)

    def _ensure_column(self, table: str, column: str, definition: str) -> None:
        existing = {
            row["name"]
            for row in self.connection.execute(f"PRAGMA table_info({table})")
        }
        if column not in existing:
            self.connection.execute(
                f"ALTER TABLE {table} ADD COLUMN {column} {definition}"
            )

    def get_step(self, source_key: str, step_key: str) -> StepRecord | None:
        row = self.connection.execute(
            "SELECT * FROM sync_steps WHERE source_key = ? AND step_key = ?",
            (source_key, step_key),
        ).fetchone()
        if row is None:
            return None
        return StepRecord(
            source_key=row["source_key"],
            step_key=row["step_key"],
            status=row["status"],
            destination_id=row["destination_id"],
            source_hash=row["source_hash"],
            synced_payload=json.loads(row["synced_payload_json"]),
            error=row["error"],
        )

    def steps_for_source(self, source_key: str) -> tuple[StepRecord, ...]:
        rows = self.connection.execute(
            "SELECT * FROM sync_steps WHERE source_key = ? ORDER BY step_key",
            (source_key,),
        ).fetchall()
        return tuple(
            StepRecord(
                source_key=row["source_key"],
                step_key=row["step_key"],
                status=row["status"],
                destination_id=row["destination_id"],
                source_hash=row["source_hash"],
                synced_payload=json.loads(row["synced_payload_json"]),
                error=row["error"],
            )
            for row in rows
        )

    def record_step(
        self,
        *,
        source_key: str,
        step_key: str,
        status: str,
        destination_id: str | None,
        source_hash: str,
        synced_payload: dict[str, Any],
        updated_at: datetime,
        error: str | None = None,
    ) -> None:
        self.connection.execute(
            """
            INSERT INTO sync_steps (
                source_key, step_key, status, destination_id, source_hash,
                synced_payload_json, error, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(source_key, step_key) DO UPDATE SET
                status = excluded.status,
                destination_id = excluded.destination_id,
                source_hash = excluded.source_hash,
                synced_payload_json = excluded.synced_payload_json,
                error = excluded.error,
                updated_at = excluded.updated_at
            """,
            (
                source_key,
                step_key,
                status,
                destination_id,
                source_hash,
                json.dumps(synced_payload, sort_keys=True),
                error,
                updated_at.isoformat(),
            ),
        )
        self.connection.commit()

    def record_source_snapshot(self, shift: SourceShift, observed_at: datetime) -> None:
        """Persist a source snapshot after a successful apply/reconciliation run.

        Dry-run intentionally does not call this method. Keeping comment hashes
        separate makes comment-only edits visible even when event times do not
        change.
        """
        occurrence_payload = {
            "title": shift.title,
            "helper_key": shift.helper_key,
            "notes": shift.notes,
            "starts_at": shift.starts_at.isoformat(),
            "ends_at": shift.ends_at.isoformat(),
            "recurrence_start": (
                shift.recurrence_start.isoformat() if shift.recurrence_start else None
            ),
            "source_version": shift.source_version,
        }
        self.connection.execute(
            """
            INSERT INTO source_occurrences (
                source_key, calendar_id, event_id, occurrence_id,
                recurrence_start, source_version, snapshot_hash, last_seen_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(source_key) DO UPDATE SET
                calendar_id = excluded.calendar_id,
                event_id = excluded.event_id,
                occurrence_id = excluded.occurrence_id,
                recurrence_start = excluded.recurrence_start,
                source_version = excluded.source_version,
                snapshot_hash = excluded.snapshot_hash,
                last_seen_at = excluded.last_seen_at
            """,
            (
                shift.key,
                shift.calendar_id,
                shift.event_id,
                shift.occurrence_id,
                shift.recurrence_start.isoformat() if shift.recurrence_start else None,
                shift.source_version,
                _hash(occurrence_payload),
                observed_at.isoformat(),
            ),
        )
        for comment in shift.comments:
            self.connection.execute(
                """
                INSERT INTO source_comments (
                    source_key, comment_id, snapshot_hash, text,
                    updated_at, last_seen_at
                ) VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(source_key, comment_id) DO UPDATE SET
                    snapshot_hash = excluded.snapshot_hash,
                    text = excluded.text,
                    updated_at = excluded.updated_at,
                    last_seen_at = excluded.last_seen_at
                """,
                (
                    shift.key,
                    comment.id,
                    _hash(
                        {"text": comment.text, "updated_at": str(comment.updated_at)}
                    ),
                    comment.text,
                    comment.updated_at.isoformat() if comment.updated_at else None,
                    observed_at.isoformat(),
                ),
            )
        self.connection.commit()


def _hash(payload: dict[str, Any]) -> str:
    return hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()
