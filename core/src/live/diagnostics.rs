//! User-initiated, redacted diagnostics.
//!
//! What a maintainer needs in order to explain a failure: versions, how far
//! setup reached, how much local history exists, and whether the browser and
//! its profiles are in place. Everything else is deliberately left out. In
//! particular this file never contains credentials, cookies, the TeamUp
//! calendar link, service identifiers, helper or customer names, shift text,
//! or raw responses from MitHF and DUOS.

use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde_json::{json, Value};

use super::{browser::browser_executable, LiveError};
use crate::{state::isoformat, SyncState};

/// Counts and flags only, ready to write next to the application data.
pub fn redacted_report(data_dir: &Path, now: DateTime<FixedOffset>) -> Result<Value, LiveError> {
    Ok(json!({
        "version": 1,
        "recorded_at": isoformat(now),
        "app": {
            "version": app_version(),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "setup": setup_summary(data_dir),
        "accounts": account_summaries(data_dir),
        "browser": {
            "executable_found": browser_executable(data_dir).is_ok(),
            "profiles": profiles(data_dir),
        },
    }))
}

/// The dated version of the installed package when the build had one, and the
/// crate version for a plain `cargo build`.
fn app_version() -> &'static str {
    option_env!("TEAMUP_SHIFT_SYNC_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// How far setup got, as sizes and yes/no answers. No chosen id or name.
fn setup_summary(data_dir: &Path) -> Value {
    let Some(setup) = std::fs::read_to_string(data_dir.join("setup.json"))
        .ok()
        .and_then(|document| serde_json::from_str::<Value>(&document).ok())
    else {
        return json!({"saved": false});
    };
    let mappings = setup["mappings"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let excluded = mappings.iter().filter(|row| row["excluded"] == json!(true)).count();
    json!({
        "saved": true,
        "version": setup["version"],
        "stage": setup["stage"].as_str().unwrap_or("ukendt"),
        "has_credentials": setup["credential"].as_str().is_some_and(|handle| !handle.is_empty()),
        "timezone": setup["timezone"].as_str().unwrap_or("Europe/Copenhagen"),
        "lookback_days": setup["lookback_days"].as_i64().unwrap_or(7),
        "calendars": count(&setup["calendars"]),
        "mappings": mappings.len(),
        "excluded_calendars": excluded,
        "included_calendars": mappings.len() - excluded,
        "mithf_helpers_offered": count(&setup["mithf"]),
        "duos_helpers_offered": count(&setup["duos"]),
        "arrangement_chosen": chosen(&setup["arrangement"]),
        "registration_type_chosen": chosen(&setup["registration_type"]),
    })
}

/// One entry per local synchronization database. The scope is the file name
/// the app already uses: a digest, not the account it was derived from.
fn account_summaries(data_dir: &Path) -> Value {
    let mut accounts = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(data_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("sync-") && name.ends_with(".sqlite3"))
        .collect();
    names.sort();
    for name in names {
        let scope = name.trim_start_matches("sync-").trim_end_matches(".sqlite3").to_owned();
        let Ok(state) = SyncState::open(data_dir.join(&name)) else {
            accounts.push(json!({"scope": scope, "readable": false}));
            continue;
        };
        accounts.push(json!({
            "scope": scope,
            "readable": true,
            "steps_by_status": state.step_status_counts().unwrap_or_default(),
            "last_verified_at": state.last_source_snapshot_at().ok().flatten().map(isoformat),
        }));
    }
    Value::Array(accounts)
}

/// Which service profiles the app has saved a browser session directory for.
fn profiles(data_dir: &Path) -> Vec<&'static str> {
    ["mithf", "duos"]
        .into_iter()
        .filter(|service| data_dir.join("rust-preview/profiles").join(service).is_dir())
        .collect()
}

fn count(value: &Value) -> usize {
    value.as_array().map_or(0, Vec::len)
}
fn chosen(value: &Value) -> bool {
    value.as_str().is_some_and(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever else changes, no secret, identifier, name or text may appear.
    #[test]
    fn the_report_holds_counts_and_flags_but_no_account_content() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("setup.json"),
            serde_json::json!({
                "version": 1, "stage": "ready", "credential": "a-keyring-handle",
                "calendars": [{"id": "ks-sub-1", "name": "Ida Å"}],
                "mithf": [{"id": "m1", "name": "Ida Å"}], "duos": [{"id": "d1", "name": "Ida Å"}],
                "mappings": [{"source": "ks-sub-1", "mithf": "m1", "duos": "d1", "excluded": false}],
                "arrangement": "portfolio-7", "registration_type": "type-3",
                "account": "MitHF: Bo Borger · Bevilling 12", "notice": "",
            })
            .to_string(),
        )
        .expect("write setup");
        let now = DateTime::parse_from_rfc3339("2026-09-21T12:00:00+02:00").unwrap();

        let report = redacted_report(dir.path(), now).expect("report");

        assert_eq!(report["setup"]["stage"], "ready");
        assert_eq!(report["setup"]["has_credentials"], true);
        assert_eq!(report["setup"]["included_calendars"], 1);
        assert_eq!(report["setup"]["arrangement_chosen"], true);
        let document = report.to_string();
        for secret in [
            "a-keyring-handle", "ks-sub-1", "Ida", "m1", "d1", "portfolio-7", "type-3",
            "Bo Borger", "Bevilling",
        ] {
            assert!(!document.contains(secret), "{secret} must not be reported");
        }
    }

    #[test]
    fn a_missing_setup_reports_its_absence_rather_than_failing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let now = DateTime::parse_from_rfc3339("2026-09-21T12:00:00+02:00").unwrap();
        let report = redacted_report(dir.path(), now).expect("report");
        assert_eq!(report["setup"]["saved"], false);
        assert_eq!(report["accounts"], serde_json::json!([]));
    }

    #[test]
    fn every_local_account_database_is_summarized_by_counts() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = SyncState::open(dir.path().join("sync-abc123.sqlite3")).expect("state");
        state
            .record_step(
                &crate::StepRecord {
                    source_key: "calendar:event:2026-09-14T07:30:00+02:00".into(),
                    step_key: "mithf.create_shift".into(),
                    status: "created".into(),
                    destination_id: Some("mithf-1".into()),
                    source_hash: "hash".into(),
                    synced_payload: Default::default(),
                    error: None,
                },
                DateTime::parse_from_rfc3339("2026-09-20T20:00:00+02:00").unwrap(),
            )
            .expect("record");
        let now = DateTime::parse_from_rfc3339("2026-09-21T12:00:00+02:00").unwrap();

        let report = redacted_report(dir.path(), now).expect("report");

        assert_eq!(report["accounts"][0]["scope"], "abc123");
        assert_eq!(report["accounts"][0]["steps_by_status"]["created"], 1);
        // The shift key and destination id stay in the database.
        assert!(!report.to_string().contains("mithf-1"));
        assert!(!report.to_string().contains("2026-09-14"));
    }
}
