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
