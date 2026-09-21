//! Native, read-only integrations for the desktop preview during migration.
mod browser;
mod capture;
mod config;
mod destinations;
mod setup;
mod teamup;

pub use browser::{BrowserSessions, Service, Visibility};
pub use capture::read_shapes;
pub use setup::Setup;
pub use config::{load_saved_setup, LiveConfig};
pub use destinations::{read_destinations, LiveDestinations};
pub use teamup::read_teamup;

use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use serde_json::Value;

#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct LiveError(pub &'static str);

const INVALID: LiveError = LiveError("Tjenesten returnerede ufuldstændige eller ugyldige data. Prøv igen; intet er overført.");

fn rows(value: &Value) -> Result<&[Value], LiveError> {
    value.as_array().map(Vec::as_slice).ok_or(INVALID)
}
fn text(value: &Value) -> Result<&str, LiveError> { value.as_str().ok_or(INVALID) }
/// A `{id, name}` option as the setup document records it. Whitespace in the
/// name is collapsed so a service's formatting cannot change a stored choice.
pub(crate) fn choice(identifier: &Value, name: &Value) -> Result<Value, LiveError> {
    let name = text(name)?.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() { return Err(INVALID); }
    Ok(serde_json::json!({"id": id(identifier)?, "name": name}))
}
/// The same, for a row whose display name is under one of several keys.
pub(crate) fn named(value: &Value) -> Result<Value, LiveError> {
    let name = ["name", "navn", "title", "description", "displayName"].iter()
        .find_map(|key| value[*key].as_str().filter(|s| !s.is_empty())).ok_or(INVALID)?;
    choice(&value["id"], &serde_json::json!(name))
}
/// Collapse repeated ids, rejecting rows that disagree about the same id.
pub(crate) fn unique(values: Vec<Value>) -> Result<Value, LiveError> {
    let mut result: Vec<Value> = Vec::new();
    for value in values {
        if let Some(existing) = result.iter().find(|v| v["id"] == value["id"]) {
            if *existing != value { return Err(INVALID); }
        } else { result.push(value); }
    }
    Ok(Value::Array(result))
}

/// Flag test matching the Python implementation's truthiness.
///
/// MitHF's PHP endpoints return flags as `1`/`0` or `"1"`/`"0"` as readily as
/// JSON booleans, so a strict `as_bool()` silently drops valid rows.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(rows) => !rows.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}
fn id(value: &Value) -> Result<String, LiveError> {
    match value {
        Value::String(s) if !s.is_empty() => Ok(s.clone()),
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(n.to_string()),
        _ => Err(INVALID),
    }
}
fn timestamp(value: &Value, zone: Tz) -> Result<DateTime<FixedOffset>, LiveError> {
    if let Some(number) = value.as_f64() {
        if !number.is_finite() || number.abs() > 8_000_000_000_000.0 { return Err(INVALID); }
        return DateTime::from_timestamp_micros((number * 1_000_000.0).round() as i64)
            .map(|v| v.with_timezone(&zone).fixed_offset()).ok_or(INVALID);
    }
    let value = text(value)?;
    if let Ok(date) = DateTime::parse_from_rfc3339(value) { return Ok(date); }
    let local = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M"))
        .map_err(|_| INVALID)?;
    zone.from_local_datetime(&local).single().map(|v| v.fixed_offset())
        .ok_or(LiveError("Tjenesten returnerede et tvetydigt eller ugyldigt lokalt tidspunkt omkring sommertid."))
}
