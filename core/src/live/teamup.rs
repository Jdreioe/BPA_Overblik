use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{classify_source_title, SourceComment, SourceShift, SourceTitle};
use super::{choice, id, rows, text, timestamp, unique, LiveConfig, LiveError, INVALID};

/// The three TeamUp credentials a request needs. Setup holds these before a
/// confirmed configuration exists, so requests take them directly.
pub(crate) struct Access<'a> { pub calendar: &'a str, pub api_key: &'a str, pub bearer: &'a str }
impl LiveConfig {
    pub(crate) fn access(&self) -> Access<'_> {
        Access { calendar: &self.calendar, api_key: &self.api_key, bearer: &self.bearer }
    }
}

fn client() -> Result<reqwest::Client, LiveError> {
    reqwest::Client::builder().timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none()).user_agent("teamup-shift-sync/0.1").build().map_err(|_| INVALID)
}

async fn get(client: &reqwest::Client, config: &Access<'_>, path: &[&str], params: &[(&str, String)]) -> Result<Value, LiveError> {
    let mut url = reqwest::Url::parse("https://api.teamup.com/").expect("constant URL");
    { let mut segments = url.path_segments_mut().map_err(|_| INVALID)?;
      segments.pop_if_empty().push(config.calendar);
      for component in path { segments.push(component); }
    }
    for (key, value) in params { url.query_pairs_mut().append_pair(key, value); }
    let mut request = client.get(url).header("Teamup-Token", config.api_key).header("Accept", "application/json");
    if !config.bearer.is_empty() { request = request.bearer_auth(config.bearer); }
    let response = request.send().await.map_err(|_| LiveError("TeamUp kunne ikke kontaktes. Kontrollér forbindelsen, og prøv igen."))?;
    if !response.status().is_success() { return Err(LiveError("TeamUp afviste forbindelsen. Kontrollér kalenderadgang og API-nøgle i opsætningen.")); }
    response.json().await.map_err(|_| INVALID)
}

/// Read configuration, hydrate all occurrences/comments, then resolve only the
/// confirmed helper calendars. No request changes TeamUp.
pub async fn read_teamup(config: &LiveConfig, start: NaiveDate, end: NaiveDate) -> Result<Vec<SourceShift>, LiveError> {
    if end < start { return Err(LiveError("Slutdatoen skal være på eller efter startdatoen.")); }
    let client = client()?;
    let access = config.access();
    let configuration = get(&client, &access, &["configuration"], &[]).await?;
    let calendars: Vec<_> = rows(&configuration["configuration"]["subcalendars"])?
        .iter().filter(|c| c["active"].as_bool().unwrap_or(true))
        .map(|c| Ok(json!({"id": id(&c["id"])?, "name": text(&c["name"])?.split_whitespace().collect::<Vec<_>>().join(" ")})))
        .collect::<Result<_, LiveError>>()?;
    if Value::Array(calendars) != config.setup["calendars"] {
        return Err(LiveError("TeamUp-kalendere eller navne er ændret. Bekræft opsætningen igen i den almindelige app."));
    }
    let from = midnight(start, config.planning.timezone)?;
    let to = midnight(end.succ_opt().ok_or(INVALID)?, config.planning.timezone)?;
    let lookback = start.checked_sub_signed(chrono::Duration::days(config.lookback_days)).ok_or(INVALID)?;
    let listing = get(&client, &access, &["events"], &[
        ("startDate", lookback.to_string()), ("endDate", end.to_string()),
        ("tz", config.planning.timezone.to_string()), ("format", "markdown".into()),
    ]).await?;
    let mut shifts = Vec::new();
    for summary in rows(&listing["events"])? {
        let event_id = id(&summary["id"])?;
        let detail = get(&client, &access, &["events", &event_id], &[("format", "markdown".into())]).await?;
        let raw = &detail["event"];
        let shift = parse_occurrence(config, raw)?;
        if shift.ends_at <= from || shift.starts_at >= to { continue; }
        if raw["all_day"].as_bool().unwrap_or(false) || classify_source_title(&shift.title) == SourceTitle::Reminder { continue; }
        let assignments: Vec<_> = rows(&raw["subcalendar_ids"])?.iter().map(id).collect::<Result<_, _>>()?;
        if assignments.is_empty() { return Err(LiveError("En vagt mangler en læsbar kalender. Kontrollér TeamUp-adgangen.")); }
        let selected: std::collections::BTreeSet<_> = assignments.iter().filter(|id| config.planning.helpers.contains_key(*id)).collect();
        if selected.is_empty() { continue; }
        if selected.len() != 1 { return Err(LiveError("En TeamUp-vagt tilhører flere bekræftede hjælpere. Ret kalenderne, og prøv igen.")); }
        shifts.push(SourceShift { helper_key: (*selected.into_iter().next().unwrap()).clone(), ..shift });
    }
    Ok(shifts)
}

/// List the TeamUp helper calendars and confirm the link exposes shift details
/// and comments, before any mapping can be confirmed against them.
///
/// Returns the calendar choices, their colour ids and a Danish notice about
/// what was checked. Colour is appearance, not identity, so it is kept out of
/// `calendars`: recolouring a calendar in TeamUp must never discard a
/// confirmed mapping or an approval.
pub(crate) async fn source_catalog(access: &Access<'_>, zone: chrono_tz::Tz, today: NaiveDate) -> Result<(Value, Value, String), LiveError> {
    let client = client()?;
    let configuration = get(&client, access, &["configuration"], &[]).await?;
    let active: Vec<_> = rows(&configuration["configuration"]["subcalendars"])?
        .iter().filter(|c| c["active"].as_bool().unwrap_or(true)).collect();
    let calendars = unique(active.iter().map(|c| choice(&c["id"], &c["name"])).collect::<Result<_, _>>()?)?;
    if rows(&calendars)?.is_empty() {
        return Err(LiveError("Kalenderlinket giver ingen aktive kalendere. Kontrollér delingsrettighederne."));
    }
    let mut colors = serde_json::Map::new();
    for calendar in &active {
        if let Some(color) = calendar["color"].as_u64() { colors.insert(id(&calendar["id"])?, json!(color)); }
    }
    // Check the current week, matching the Python setup's access probe.
    let monday = today - chrono::Duration::days(i64::from(chrono::Datelike::weekday(&today).num_days_from_monday()));
    let sunday = monday.checked_add_signed(chrono::Duration::days(6)).ok_or(INVALID)?;
    let listing = get(&client, access, &["events"], &[
        ("startDate", monday.to_string()), ("endDate", sunday.to_string()),
        ("tz", zone.to_string()), ("format", "markdown".into()),
    ]).await?;
    let events = rows(&listing["events"])?;
    // Every listed event is hydrated, so an unreadable one blocks setup here
    // rather than during a transfer.
    for summary in events {
        let detail = get(&client, access, &["events", &id(&summary["id"])?], &[("format", "markdown".into())]).await?;
        let raw = &detail["event"];
        let title = raw["title"].as_str().unwrap_or("").to_lowercase();
        if raw["details_hidden"].as_bool() == Some(true)
            || matches!(raw["permission"].as_str(), Some("read_only_without_details" | "add_only_without_details"))
            || (raw["readonly"].as_bool() == Some(true) && matches!(title.as_str(), "reserved" | "reserveret")) {
            return Err(LiveError("Kalenderlinket skjuler vagtdetaljer. Bed kalenderens ejer om læseadgang til detaljer og kommentarer."));
        }
    }
    let notice = if events.is_empty() {
        "Ugen er tom. Adgang til hændelseslisten er kontrolleret; kommentarer kontrolleres, når der findes vagter."
    } else {
        "Vagter og kommentarer er kontrolleret for denne uge."
    };
    Ok((calendars, Value::Object(colors), notice.to_owned()))
}

pub fn midnight(date: NaiveDate, zone: chrono_tz::Tz) -> Result<DateTime<FixedOffset>, LiveError> {
    zone.from_local_datetime(&date.and_hms_opt(0, 0, 0).ok_or(INVALID)?).single()
        .map(|date| date.fixed_offset()).ok_or(INVALID)
}

fn parse_occurrence(config: &LiveConfig, raw: &Value) -> Result<SourceShift, LiveError> {
    let occurrence_id = id(&raw["id"])?;
    let title = raw["title"].as_str().unwrap_or("");
    if raw["details_hidden"].as_bool() == Some(true)
        || matches!(raw["permission"].as_str(), Some("read_only_without_details" | "add_only_without_details"))
        || (raw["readonly"].as_bool() == Some(true) && matches!(title.to_lowercase().as_str(), "reserved" | "reserveret")) {
        return Err(LiveError("TeamUp skjuler vagtdetaljer. Kalenderlinket skal give adgang til detaljer og kommentarer."));
    }
    let comments_enabled = raw["comments_enabled"].as_bool().unwrap_or(false);
    let raw_comments = &raw["comments"];
    if comments_enabled && (raw_comments.is_null() || (raw["comments_visibility"] == "users_with_modify_permission"
        && raw["readonly"].as_bool().unwrap_or(true) && raw_comments.as_array().is_none_or(|rows| rows.is_empty()))) {
        return Err(LiveError("TeamUp-kommentarer kunne ikke læses. Kontrollér delingsrettighederne."));
    }
    let mut comments = Vec::new();
    if !raw_comments.is_null() {
        for comment in rows(raw_comments)? {
            let message = if comment["message"].is_object() { text(&comment["message"]["markdown"])? } else { text(&comment["message"])? };
            let updated = if comment["update_dt"].is_null() { &comment["creation_dt"] } else { &comment["update_dt"] };
            comments.push(SourceComment { id: id(&comment["id"])?, text: message.into(), updated_at: if updated.is_null() { None } else { Some(timestamp(updated, config.planning.timezone)?) } });
        }
    }
    let starts_at = timestamp(&raw["start_dt"], config.planning.timezone)?;
    let ends_at = timestamp(&raw["end_dt"], config.planning.timezone)?;
    if ends_at <= starts_at { return Err(INVALID); }
    let hash = format!("{:x}", Sha256::digest(config.calendar.as_bytes()));
    Ok(SourceShift {
        calendar_id: hash[..24].into(), event_id: if raw["series_id"].is_null() { occurrence_id.split("-rid-").next().unwrap().into() } else { id(&raw["series_id"])? },
        occurrence_id, title: title.into(), helper_key: String::new(), starts_at, ends_at,
        notes: raw["notes"].as_str().unwrap_or("").into(), comments,
        recurrence_start: if raw["ristart_dt"].is_null() { None } else { Some(timestamp(&raw["ristart_dt"], config.planning.timezone)?) },
        source_version: if raw["version"].is_null() { None } else { Some(id(&raw["version"])?) },
    })
}
