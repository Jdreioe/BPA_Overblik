//! Wire-shape capture for the browser-backed MitHF and DUOS reads.
//!
//! The native readers' failures come from assuming a leaf type the service does
//! not actually send, and no test sees real service JSON. This records the
//! structure of each read response — object keys and leaf types only, never a
//! value — so the recorded contract can be committed and checked against what
//! the readers assume. Reads only: it cannot reach a write action by type.

use chrono::NaiveDate;
use serde_json::{json, Map, Value};

use super::{BrowserSessions, LiveConfig, LiveError, Service};

/// Structural description of a JSON value.
///
/// Leaves become a type tag. Object keys that are not plain identifiers become
/// a tag too, because MitHF keys days by date and extras by shift id, so a key
/// can itself be account data.
fn shape(value: &Value) -> Value {
    match value {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("bool"),
        Value::Number(number) if number.is_i64() || number.is_u64() => json!("int"),
        Value::Number(_) => json!("float"),
        Value::String(text) => json!(classify(text)),
        Value::Array(items) => {
            json!({"array": items.iter().map(shape).fold(json!("unknown"), merge)})
        }
        Value::Object(fields) => {
            let mut merged: Map<String, Value> = Map::new();
            for (key, field) in fields {
                let key = if identifier(key) {
                    key.clone()
                } else {
                    format!("<{}>", classify(key))
                };
                let next = shape(field);
                let combined = match merged.remove(&key) {
                    Some(existing) => merge(existing, next),
                    None => next,
                };
                merged.insert(key, combined);
            }
            json!({ "object": merged })
        }
    }
}

/// Classify a string by form alone. Every tag is a fixed word, so no character
/// of the original ever reaches the output.
fn classify(text: &str) -> &'static str {
    let bytes = text.as_bytes();
    let digits = |part: &[u8]| !part.is_empty() && part.iter().all(u8::is_ascii_digit);
    if bytes.is_empty() {
        return "empty";
    }
    if digits(bytes) {
        return "digits";
    }
    if bytes.len() >= 10
        && digits(&bytes[..4])
        && bytes[4] == b'-'
        && digits(&bytes[5..7])
        && bytes[7] == b'-'
        && digits(&bytes[8..10])
    {
        return if bytes.len() == 10 {
            "date-iso"
        } else {
            "datetime-iso"
        };
    }
    let danish: Vec<&str> = text.split('.').collect();
    if danish.len() == 3 && danish.iter().all(|part| digits(part.as_bytes())) {
        return "date-dk";
    }
    let clock: Vec<&str> = text.split(':').collect();
    if (2..=3).contains(&clock.len()) && clock.iter().all(|part| digits(part.as_bytes())) {
        return "time";
    }
    "text"
}

fn identifier(key: &str) -> bool {
    key.bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Combine two shapes into one describing both. A key seen in only some
/// occurrences becomes `absent | <shape>` rather than being silently dropped.
fn merge(left: Value, right: Value) -> Value {
    if left == json!("unknown") {
        return right;
    }
    if right == json!("unknown") || left == right {
        return left;
    }
    if let (Some(a), Some(b)) = (left.get("object"), right.get("object")) {
        let empty = Map::new();
        let (a, b) = (
            a.as_object().unwrap_or(&empty),
            b.as_object().unwrap_or(&empty),
        );
        let mut merged = Map::new();
        for key in a.keys().chain(b.keys()) {
            if merged.contains_key(key) {
                continue;
            }
            let pick =
                |fields: &Map<String, Value>| fields.get(key).cloned().unwrap_or(json!("absent"));
            merged.insert(key.clone(), merge(pick(a), pick(b)));
        }
        return json!({ "object": merged });
    }
    if let (Some(a), Some(b)) = (left.get("array"), right.get("array")) {
        return json!({"array": merge(a.clone(), b.clone())});
    }
    let mut options = Vec::new();
    collect(left, &mut options);
    collect(right, &mut options);
    options.sort_by_key(Value::to_string);
    json!({ "any": options })
}

fn collect(value: Value, into: &mut Vec<Value>) {
    match value.get("any").and_then(Value::as_array) {
        Some(existing) => {
            for item in existing.clone() {
                if !into.contains(&item) {
                    into.push(item);
                }
            }
        }
        None => {
            if !into.contains(&value) {
                into.push(value);
            }
        }
    }
}

/// Issue every read the native adapters depend on and record their shapes.
///
/// TeamUp is not included: its reads already tolerate missing and differently
/// typed fields, and it needs no browser session.
pub async fn read_shapes(
    browser: &BrowserSessions,
    config: &LiveConfig,
    from: NaiveDate,
    to: NaiveDate,
    today: NaiveDate,
) -> Result<Value, LiveError> {
    let arrangement = &config.planning.duos_arrangement_id;
    let mut mithf = Map::new();
    let mut duos = Map::new();
    let mut missing = Vec::new();

    let helpers = browser
        .request(Service::Mithf, "hjaelperliste", json!({}))
        .await?;
    mithf.insert("hjaelperliste".into(), shape(&helpers));
    let options = browser
        .request(Service::Mithf, "muligheder", json!({}))
        .await?;
    mithf.insert("muligheder".into(), shape(&options));
    let plan = browser
        .request(
            Service::Mithf,
            "plan",
            json!({"fra": from.to_string(), "til": to.to_string(), "frisk": 1}),
        )
        .await?;
    mithf.insert("plan".into(), shape(&plan));

    // `ekstra` is keyed by a shift id, so it can only be reached through a week
    // that actually has one.
    match shift_id(&plan) {
        Some(identifier) => {
            let extra = browser
                .request(Service::Mithf, "ekstra", json!({ "eids": identifier }))
                .await?;
            mithf.insert("ekstra".into(), shape(&extra));
        }
        None => missing.push("mithf.ekstra: the selected week reported no shifts"),
    }

    let portfolios = browser
        .request(
            Service::Duos,
            "portfolios",
            json!({"dateOfActivePortfolio": today.to_string()}),
        )
        .await?;
    duos.insert("portfolios".into(), shape(&portfolios));
    let types = browser
        .request(Service::Duos, "types", json!({"portfolioId": arrangement}))
        .await?;
    duos.insert("types".into(), shape(&types));
    let employments = browser
        .request(
            Service::Duos,
            "employments",
            json!({"portfolioId": arrangement, "dateOfActiveEmployment": today.to_string()}),
        )
        .await?;
    duos.insert("employments".into(), shape(&employments));
    let search = browser
        .request(
            Service::Duos,
            "search",
            json!({"skip": 0, "take": 100, "includeFields": ["id", "portfolioId", "helperId", "dutyTypeId", "startDate", "endDate", "statusId"]}),
        )
        .await?;
    duos.insert("search".into(), shape(&search));

    Ok(json!({
        "note": "Recorded JSON structure and leaf types only. No values, names or identifiers.",
        "captured": today.to_string(),
        "mithf": mithf,
        "duos": duos,
        "missing": missing,
    }))
}

/// First shift id in a `plan` response, without reusing the reader's own
/// parsing: a capture has to work even when that parsing is what is wrong.
fn shift_id(plan: &Value) -> Option<String> {
    plan["dage"]
        .as_object()?
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .find_map(|row| match &row["id"] {
            Value::String(text) if !text.is_empty() => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_types_are_recorded_without_values() {
        let recorded = shape(&json!({
            "navn": "Jonas Dreiøe",
            "valgbar": 1,
            "aktiv": true,
            "start": "07:30",
            "dato": "21.09.2026",
            "oprettet": "2026-09-21T07:30:00",
            "vid": "4821",
            "note": "",
            "slut": null,
        }));
        assert_eq!(
            recorded,
            json!({"object": {
                "navn": "text", "valgbar": "int", "aktiv": "bool", "start": "time",
                "dato": "date-dk", "oprettet": "datetime-iso", "vid": "digits",
                "note": "empty", "slut": "null",
            }})
        );
    }

    #[test]
    fn data_bearing_object_keys_become_tags() {
        let recorded = shape(&json!({"dage": {"2026-09-21": [], "2026-09-22": []}}));
        assert_eq!(
            recorded,
            json!({"object": {"dage": {"object": {"<date-iso>": {"array": "unknown"}}}}})
        );
    }

    #[test]
    fn disagreeing_rows_merge_into_a_union_with_optional_keys() {
        let recorded = shape(&json!([
            {"id": 7, "suffix": "A"},
            {"id": "7"},
        ]));
        assert_eq!(
            recorded,
            json!({"array": {"object": {
                "id": {"any": ["digits", "int"]},
                "suffix": {"any": ["absent", "text"]},
            }}})
        );
    }
}
