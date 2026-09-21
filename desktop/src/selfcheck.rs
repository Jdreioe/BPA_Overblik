//! Headless bundle check: build the representative fixture preview through
//! the core and print its approval digest. No window, no credentials, no
//! network, no destination writes.

use std::path::PathBuf;

use chrono::NaiveDate;
use teamup_shift_sync_core::{fixture, plan_digest};

pub fn run() -> iced::Result {
    match check() {
        Ok(digest) => {
            println!("Desktop bundle self-check passed: {digest}");
            Ok(())
        }
        Err(error) => {
            eprintln!("Desktop bundle self-check failed: {error}");
            std::process::exit(1);
        }
    }
}

fn check() -> Result<String, String> {
    let config = std::env::var("TEAMUP_SHIFT_SYNC_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("fixtures/offline-config.toml"));
    let fixture_path = std::env::var("TEAMUP_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("fixtures/representative-week.json"));
    let directory = tempfile::tempdir().map_err(|_| "self-check state failed")?;
    let plan = fixture::preview(
        &config,
        &fixture_path,
        &directory.path().join("self-check.sqlite3"),
        NaiveDate::from_ymd_opt(2026, 9, 14),
        NaiveDate::from_ymd_opt(2026, 9, 20),
        fixture::golden_now(),
    )
    .map_err(|error| error.to_string())?;
    plan_digest(&plan).map_err(|error| error.to_string())
}
