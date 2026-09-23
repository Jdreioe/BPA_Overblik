//! Fixture input for offline previews and bundle checks.
//!
//! This path reads the TOML fixture configuration and the JSON fixture week,
//! then plans against a scratch SQLite file. It cannot construct a live
//! writer: no credentials are read and no service is contacted.

use std::{collections::BTreeMap, path::Path};

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone};
use chrono_tz::Tz;
use serde::Deserialize;

use crate::{
    build_plan, DestinationSnapshot, HelperMapping, PlanRequest, PlanningConfig, PlanningError,
    SourceShift, SyncPlan, SyncState,
};

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Planning(#[from] PlanningError),
    #[error(transparent)]
    State(#[from] crate::StateError),
}

impl From<String> for FixtureError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for FixtureError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

#[derive(Deserialize)]
struct Config {
    #[serde(default = "timezone")]
    timezone: Tz,
    #[serde(default = "one")]
    default_helper_count: i64,
    #[serde(default = "enabled")]
    duos_enabled: bool,
    duos: Duos,
    helpers: Vec<Helper>,
}
#[derive(Deserialize)]
struct Duos {
    arrangement_id: String,
    registration_type: String,
}
#[derive(Deserialize)]
struct Helper {
    teamup_key: String,
    mithf_name: String,
    duos_employee_number: String,
}
fn timezone() -> Tz {
    chrono_tz::Europe::Copenhagen
}
fn one() -> i64 {
    1
}
fn enabled() -> bool {
    true
}

#[derive(Deserialize)]
struct Fixture {
    // Require all three lists: a missing destination list is not proof of emptiness.
    source_shifts: Vec<SourceShift>,
    mithf_shifts: Vec<crate::MitHfShift>,
    duos_registrations: Vec<crate::DuosRegistration>,
}

/// Plan the fixture week. `from`/`to` default to the current local week.
/// The state file is created when needed but never gains transfer records.
#[allow(clippy::too_many_arguments)]
pub fn preview(
    config_path: &Path,
    fixture_path: &Path,
    state_path: &Path,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    now: DateTime<FixedOffset>,
) -> Result<SyncPlan, FixtureError> {
    let config: Config = toml::from_str(
        &std::fs::read_to_string(config_path)
            .map_err(|_| "Could not read fixture configuration")?,
    )
    .map_err(|_| "Invalid fixture configuration")?;
    if config.default_helper_count < 1 {
        return Err("default_helper_count must be positive".into());
    }
    let mut helpers = BTreeMap::new();
    for helper in config.helpers {
        if helper.teamup_key.trim().is_empty()
            || helper.mithf_name.trim().is_empty()
            || helper.duos_employee_number.trim().is_empty()
        {
            return Err("Fixture helper mapping contains an empty identity".into());
        }
        if helpers
            .insert(
                helper.teamup_key,
                HelperMapping {
                    mithf_name: helper.mithf_name,
                    duos_employee_number: helper.duos_employee_number,
                },
            )
            .is_some()
        {
            return Err("Duplicate TeamUp helper key in fixture configuration".into());
        }
    }
    let config = PlanningConfig {
        timezone: config.timezone,
        default_helper_count: config.default_helper_count,
        duos_arrangement_id: config.duos.arrangement_id,
        duos_registration_type: config.duos.registration_type,
        duos_enabled: config.duos_enabled,
        helpers,
    };
    let (range_start, range_end) = resolve_range(config.timezone, from, to, now)?;
    let fixture: Fixture = serde_json::from_str(
        &std::fs::read_to_string(fixture_path).map_err(|_| "Could not read fixture")?,
    )
    .map_err(|_| "Invalid or incomplete fixture data")?;
    if fixture
        .source_shifts
        .iter()
        .any(|s| s.ends_at <= s.starts_at)
        || fixture.mithf_shifts.iter().any(|s| {
            s.ends_at <= s.starts_at
                || s.helper_count < 1
                || s.sps_intervals
                    .iter()
                    .chain(&s.meeting_intervals)
                    .any(|i| i.ends_at <= i.starts_at)
        })
        || fixture
            .duos_registrations
            .iter()
            .any(|s| s.ends_at <= s.starts_at)
    {
        return Err("Fixture contains an invalid interval or helper count".into());
    }
    let state = SyncState::open(state_path)?;
    Ok(build_plan(
        &PlanRequest {
            config: &config,
            shifts: &fixture.source_shifts,
            destination: &DestinationSnapshot {
                mithf_shifts: fixture.mithf_shifts,
                duos_registrations: fixture.duos_registrations,
            },
            range_start,
            range_end,
            now,
            live: false,
        },
        &state,
    )?)
}

fn resolve_range(
    zone: Tz,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    now: DateTime<FixedOffset>,
) -> Result<(DateTime<FixedOffset>, DateTime<FixedOffset>), FixtureError> {
    let today = now.with_timezone(&zone).date_naive();
    let from = from
        .or_else(|| {
            today.checked_sub_signed(Duration::days(
                today.weekday().num_days_from_monday().into(),
            ))
        })
        .ok_or("Start date is outside the supported range")?;
    let to = to
        .or_else(|| from.checked_add_signed(Duration::days(6)))
        .ok_or("End date is outside the supported range")?;
    if to < from {
        return Err("--to must be on or after --from".into());
    }
    let midnight = |date: NaiveDate| -> Result<DateTime<FixedOffset>, FixtureError> {
        zone.from_local_datetime(&date.and_hms_opt(0, 0, 0).ok_or("Invalid date")?)
            .single()
            .map(|v| v.fixed_offset())
            .ok_or_else(|| "Range boundary is an ambiguous or nonexistent local time".into())
    };
    let start = midnight(from)?;
    let end = midnight(
        to.succ_opt()
            .ok_or("End date is outside the supported range")?,
    )?;
    Ok((start, end))
}

/// Fixed evaluation time the bundle check and golden tests share, so the
/// representative digest is stable.
pub fn golden_now() -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339("2026-09-20T20:00:00+02:00").expect("fixed golden time")
}
