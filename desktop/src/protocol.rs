//! Typed messages for the versioned JSON worker protocol (v1).
//!
//! Mirrors `src/teamup_shift_sync/worker.py`. The worker speaks
//! newline-delimited JSON on stdout only; diagnostics stay on stderr so they
//! can never corrupt this stream. Every response carries the opaque request
//! `id` back so the UI can match results to what it asked for.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

pub const PROTOCOL_VERSION: u32 = 1;

/// A request frame sent to the worker on stdin.
#[derive(Debug, Serialize)]
pub struct Request<P: Serialize> {
    pub protocol: u32,
    pub id: String,
    pub method: String,
    pub params: P,
}

impl<P: Serialize> Request<P> {
    pub fn new(id: impl Into<String>, method: &str, params: P) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            id: id.into(),
            method: method.to_string(),
            params,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EmptyParams {}

#[derive(Debug, Serialize)]
pub struct HomeStatusParams<'a> {
    pub config_path: &'a Path,
    pub state_path: &'a Path,
    pub now: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct FixturePreviewParams<'a> {
    pub config_path: &'a Path,
    pub fixture_path: &'a Path,
    pub state_path: &'a Path,
    pub from: &'a str,
    pub to: &'a str,
}

/// A response frame read from the worker on stdout.
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // `id` echoes requests; matching arrives with apply progress in #6.
pub struct Response {
    pub protocol: u32,
    pub id: Option<String>,
    pub event: String,
    pub payload: Value,
}

/// Progress event shape reserved by protocol v1 for long-running apply work.
#[derive(Debug, Clone, Deserialize)]
pub struct Progress {
    pub stage: String,
    pub message: String,
    pub current: Option<u64>,
    pub total: Option<u64>,
}

/// User-facing worker failure. `message` is Danish and safe to display;
/// `detail` is technical and belongs under Help, never in the normal flow.
#[derive(Debug, Clone)]
pub struct WorkerError {
    pub code: String,
    pub message: String,
    pub detail: String,
}

impl WorkerError {
    pub fn startup(detail: impl Into<String>) -> Self {
        Self {
            code: "worker_startup".to_string(),
            message: "Baggrundsarbejderen kunne ikke startes.".to_string(),
            detail: detail.into(),
        }
    }

    pub fn exited(detail: impl Into<String>) -> Self {
        Self {
            code: "worker_exit".to_string(),
            message: "Baggrundsarbejderen stoppede uventet.".to_string(),
            detail: detail.into(),
        }
    }

    pub fn from_payload(payload: &Value) -> Self {
        Self {
            code: payload
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("internal")
                .to_string(),
            message: payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Der opstod en uventet fejl.")
                .to_string(),
            detail: payload
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}]", self.message, self.code)
    }
}

impl std::error::Error for WorkerError {}

/// Data for the Danish home screen (`home_status` method).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // Full typed surface; remaining fields light up in #4–#7.
pub struct HomeStatus {
    pub timezone: String,
    pub week_start: String,
    pub week_end: String,
    #[serde(default)]
    pub config_issues: Vec<String>,
    pub last_verified_at: Option<String>,
    #[serde(default)]
    pub verified_steps: u64,
    #[serde(default = "unknown_login")]
    pub login_state: String,
    #[serde(default)]
    pub login_detail: String,
}

fn unknown_login() -> String {
    "unknown".to_string()
}

/// One row of a synchronization preview (`preview_fixture` method).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // Identity fields bind approval to the exact plan in #5.
pub struct PlanItem {
    pub source_key: String,
    pub system: String,
    pub step_key: String,
    pub outcome: String,
    pub summary: String,
    #[serde(default)]
    pub payload: Value,
    pub destination_id: Option<String>,
}

/// A fixture preview: the plan, its approval digest, and outcome counts.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // Range/digest fields bind approval to the exact plan in #5.
pub struct PlanPreview {
    pub starts_at: String,
    pub ends_at: String,
    pub generated_at: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub has_conflicts: bool,
    #[serde(default)]
    pub counts: BTreeMap<String, u64>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub items: Vec<PlanItem>,
}

/// App-owned data paths (`app_paths` method).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // Shown in the Help/diagnostics view in #7.
pub struct AppPaths {
    pub data_dir: String,
    pub state_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_payload_deserializes() {
        let raw = serde_json::json!({
            "starts_at": "2026-09-14T00:00:00+02:00",
            "ends_at": "2026-09-21T00:00:00+02:00",
            "generated_at": "2026-09-20T20:00:00+02:00",
            "digest": "abc",
            "has_conflicts": false,
            "counts": {"already_matched": 3},
            "blockers": [],
            "items": [{
                "source_key": "k", "system": "mithf", "step_key": "mithf.create_shift",
                "outcome": "would_create", "summary": "Would create",
                "payload": {}, "destination_id": serde_json::Value::Null,
            }],
        });
        let preview: PlanPreview = serde_json::from_value(raw).unwrap();
        assert_eq!(preview.digest, "abc");
        assert_eq!(preview.items.len(), 1);
        assert_eq!(preview.counts["already_matched"], 3);
    }

    #[test]
    fn error_payload_maps_to_danish_message() {
        let raw = serde_json::json!({
            "code": "fixture_error",
            "message": "Testugen kunne ikke indlæses.",
            "detail": "Fixture not found",
        });
        let error = WorkerError::from_payload(&raw);
        assert_eq!(error.code, "fixture_error");
        assert!(error.message.contains("Testugen"));
    }

    #[test]
    fn progress_payload_deserializes() {
        let progress: Progress = serde_json::from_value(serde_json::json!({
            "stage": "verify",
            "message": "Kontrollerer MitHF",
            "current": 2,
            "total": 4
        }))
        .unwrap();
        assert_eq!(progress.stage, "verify");
        assert_eq!(progress.current, Some(2));
    }
}

/// Account data stays inside the service browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    Mithf,
    Duos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginState {
    Connected,
    SignInRequired,
    Connecting,
    Unavailable,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionStatus {
    pub state: LoginState,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Sessions {
    pub mithf: SessionStatus,
    pub duos: SessionStatus,
}
