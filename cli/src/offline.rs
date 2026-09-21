//! Fixture input for the native CLI. This path cannot construct a live writer.
use crate::{Dates, Result};
use chrono::{DateTime, FixedOffset};
use chrono_tz::Tz;
use serde::Deserialize;
use std::{collections::BTreeMap, path::PathBuf};
use teamup_shift_sync_core::{
    build_plan, DestinationSnapshot, HelperMapping, PlanRequest, PlanningConfig, SourceShift,
    SyncPlan, SyncState,
};

#[derive(Deserialize)]
struct Config {
    #[serde(default = "timezone")]
    timezone: Tz,
    #[serde(default = "one")]
    default_helper_count: i64,
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

#[derive(Deserialize)]
struct Fixture {
    // Require all three lists: a missing destination list is not proof of emptiness.
    source_shifts: Vec<SourceShift>,
    mithf_shifts: Vec<teamup_shift_sync_core::MitHfShift>,
    duos_registrations: Vec<teamup_shift_sync_core::DuosRegistration>,
}

pub(crate) fn preview(
    config_path: PathBuf,
    fixture_path: PathBuf,
    state_path: PathBuf,
    dates: Dates,
    now: DateTime<FixedOffset>,
) -> Result<SyncPlan> {
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
        helpers,
    };
    let range = dates.resolve(config.timezone, now)?;
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
            range_start: range.start,
            range_end: range.end,
            now,
            live: false,
        },
        &state,
    )?)
}
