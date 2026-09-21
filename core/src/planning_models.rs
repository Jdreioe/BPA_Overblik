use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::TimeInterval;

/// Only the confirmed configuration needed to plan transfers. Credentials and
/// browser/setup settings belong to the adapters, not to planning.
#[derive(Clone, Debug, Deserialize)]
pub struct PlanningConfig {
    pub timezone: Tz,
    pub default_helper_count: i64,
    pub duos_arrangement_id: String,
    pub duos_registration_type: String,
    pub helpers: BTreeMap<String, HelperMapping>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HelperMapping {
    pub mithf_name: String,
    pub duos_employee_number: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MitHfShift {
    pub id: String,
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    pub helper_count: i64,
    pub helper_name: Option<String>,
    #[serde(default)]
    pub sps_intervals: Vec<TimeInterval>,
    #[serde(default)]
    pub meeting_intervals: Vec<TimeInterval>,
    #[serde(default)]
    pub sps_record_ids: Vec<String>,
    #[serde(default)]
    pub meeting_record_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DuosRegistration {
    pub id: String,
    pub arrangement_id: String,
    pub employee_number: String,
    pub registration_type: String,
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    #[serde(default)]
    pub status_id: i64,
}

/// A successful, complete destination read. Adapters must return an error for
/// incomplete reads instead of passing partial data to the planner.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DestinationSnapshot {
    #[serde(default)]
    pub mithf_shifts: Vec<MitHfShift>,
    #[serde(default)]
    pub duos_registrations: Vec<DuosRegistration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    WouldCreate,
    WouldUpdate,
    AlreadyMatched,
    Conflicted,
    Failed,
    Review,
    Excluded,
    PendingIntegration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanSystem {
    Source,
    Mapping,
    Mithf,
    Duos,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanItem {
    pub source_key: String,
    pub system: PlanSystem,
    pub step_key: String,
    pub outcome: Outcome,
    pub summary: String,
    pub payload: Map<String, Value>,
    pub destination_id: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncPlan {
    pub starts_at: DateTime<FixedOffset>,
    pub ends_at: DateTime<FixedOffset>,
    pub generated_at: DateTime<FixedOffset>,
    pub items: Vec<PlanItem>,
}

/// Segment zero preserves the legacy key when a shift gains SPS intervals.
pub fn segment_step(base: &str, index: usize) -> String {
    if index == 0 {
        base.into()
    } else {
        format!("{base}#{index}")
    }
}
