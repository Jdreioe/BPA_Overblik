use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone, Timelike};
use futures_util::{stream, StreamExt, TryStreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    choice, id, rows, text, timestamp, timing::Stage, unique, LiveConfig, LiveError,
    SourceReadError, INVALID,
};
use crate::standard_time::StandardTimes;
use crate::{classify_source_title, SourceComment, SourceShift, SourceTitle};

/// The three TeamUp credentials a request needs. Setup holds these before a
/// confirmed configuration exists, so requests take them directly.
pub(crate) struct Access<'a> {
    pub calendar: &'a str,
    pub api_key: &'a str,
    pub bearer: &'a str,
}
impl LiveConfig {
    pub(crate) fn access(&self) -> Access<'_> {
        Access {
            calendar: &self.calendar,
            api_key: &self.api_key,
            bearer: &self.bearer,
        }
    }
}

/// How many event detail calls a read keeps in flight. Enough to hide the
/// round trip on a week's shifts, low enough to stay a polite API client.
const DETAIL_CONCURRENCY: usize = 6;

fn client() -> Result<reqwest::Client, LiveError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("teamup-shift-sync/0.1")
        .build()
        .map_err(|_| INVALID)
}

async fn get(
    client: &reqwest::Client,
    config: &Access<'_>,
    path: &[&str],
    params: &[(&str, String)],
) -> Result<Value, LiveError> {
    let mut url = reqwest::Url::parse("https://api.teamup.com/").expect("constant URL");
    {
        let mut segments = url.path_segments_mut().map_err(|_| INVALID)?;
        segments.pop_if_empty().push(config.calendar);
        for component in path {
            segments.push(component);
        }
    }
    for (key, value) in params {
        url.query_pairs_mut().append_pair(key, value);
    }
    let mut request = client
        .get(url)
        .header("Teamup-Token", config.api_key)
        .header("Accept", "application/json");
    if !config.bearer.is_empty() {
        request = request.bearer_auth(config.bearer);
    }
    let response = request.send().await.map_err(|_| {
        LiveError("TeamUp kunne ikke kontaktes. Kontrollér forbindelsen, og prøv igen.")
    })?;
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(LiveError(
            "TeamUp modtog for mange forespørgsler. Vent et øjeblik, og prøv igen.",
        ));
    }
    if !response.status().is_success() {
        return Err(LiveError(
            "TeamUp afviste forbindelsen. Kontrollér kalenderadgang og API-nøgle i opsætningen.",
        ));
    }
    response.json().await.map_err(|_| INVALID)
}

async fn occurrence(
    client: &reqwest::Client,
    access: &Access<'_>,
    event_id: &str,
    zone: chrono_tz::Tz,
) -> Result<Value, LiveError> {
    get(
        client,
        access,
        &["events", event_id],
        &[("format", "markdown".into()), ("tz", zone.to_string())],
    )
    .await
}

/// Read configuration, hydrate all occurrences/comments, then resolve only the
/// confirmed helper calendars. No request changes TeamUp.
pub async fn read_teamup(
    config: &LiveConfig,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<SourceShift>, SourceReadError> {
    if end < start {
        return Err(LiveError("Slutdatoen skal være på eller efter startdatoen.").into());
    }
    let client = client()?;
    let access = config.access();
    let stage = Stage::start("teamup.catalog");
    let configuration = get(&client, &access, &["configuration"], &[]).await?;
    let calendars: Vec<_> = rows(&configuration["configuration"]["subcalendars"])?
        .iter().filter(|c| c["active"].as_bool().unwrap_or(true))
        .map(|c| Ok(json!({"id": id(&c["id"])?, "name": text(&c["name"])?.split_whitespace().collect::<Vec<_>>().join(" ")})))
        .collect::<Result<_, LiveError>>()?;
    if Value::Array(calendars) != config.setup["calendars"] {
        return Err(LiveError("TeamUp-kalendere eller navne er ændret. Bekræft hjælperne igen under Indstillinger → Hjælpere.").into());
    }
    let from = midnight(start, config.planning.timezone)?;
    let to = midnight(end.succ_opt().ok_or(INVALID)?, config.planning.timezone)?;
    let lookback = start
        .checked_sub_signed(chrono::Duration::days(config.lookback_days))
        .ok_or(INVALID)?;
    let listing = get(
        &client,
        &access,
        &["events"],
        &[
            ("startDate", lookback.to_string()),
            ("endDate", end.to_string()),
            ("tz", config.planning.timezone.to_string()),
            ("format", "markdown".into()),
        ],
    )
    .await?;
    stage.done(2);
    // Every listed event needs its own detail call, and they do not depend on
    // each other, so a week's worth is fetched a few at a time. Order is kept,
    // so the same event still reports the first failure.
    let stage = Stage::start("teamup.details");
    let event_ids: Vec<String> = rows(&listing["events"])?
        .iter()
        .map(|summary| id(&summary["id"]))
        .collect::<Result<_, _>>()?;
    let mut calls = Vec::with_capacity(event_ids.len());
    for event_id in &event_ids {
        calls.push(occurrence(
            &client,
            &access,
            event_id,
            config.planning.timezone,
        ));
    }
    let details: Vec<Value> = stream::iter(calls)
        .buffered(DETAIL_CONCURRENCY)
        .try_collect()
        .await?;
    stage.done(details.len());
    let mut shifts = Vec::new();
    for detail in &details {
        let raw = &detail["event"];
        let shift = parse_occurrence(config, raw)?;
        let all_day = raw["all_day"].as_bool().unwrap_or(false);
        if shift.starts_at >= to
            || (shift.ends_at <= from
                && (!all_day
                    || shift
                        .starts_at
                        .with_timezone(&config.planning.timezone)
                        .date_naive()
                        < start.pred_opt().unwrap_or(start)))
        {
            continue;
        }
        if classify_source_title(&shift.title) == SourceTitle::Reminder {
            continue;
        }
        let assignments: Vec<_> = rows(&raw["subcalendar_ids"])?
            .iter()
            .map(id)
            .collect::<Result<_, _>>()?;
        if assignments.is_empty() {
            return Err(LiveError(
                "En vagt mangler en læsbar kalender. Kontrollér TeamUp-adgangen.",
            )
            .into());
        }
        let selected: std::collections::BTreeSet<_> = assignments
            .iter()
            .filter(|id| config.planning.helpers.contains_key(*id))
            .collect();
        if selected.is_empty() {
            continue;
        }
        if selected.len() != 1 {
            return Err(LiveError(
                "En TeamUp-vagt tilhører flere bekræftede hjælpere. Ret kalenderne, og prøv igen.",
            )
            .into());
        }
        let mut shift = SourceShift {
            helper_key: (*selected.into_iter().next().unwrap()).clone(),
            ..shift
        };
        if all_day {
            use_standard_time(&mut shift, &config.standard_times, config.planning.timezone)?;
        }
        if shift.ends_at > from && shift.starts_at < to {
            shifts.push(shift);
        }
    }
    Ok(shifts)
}

fn use_standard_time(
    shift: &mut SourceShift,
    standard: &StandardTimes,
    zone: chrono_tz::Tz,
) -> Result<(), SourceReadError> {
    let local_start = shift.starts_at.with_timezone(&zone);
    let local_end = shift.ends_at.with_timezone(&zone);
    let date = local_start.date_naive();
    let single_day = local_start.hour() == 0
        && local_start.minute() == 0
        && local_start.second() == 0
        && ((local_end.hour() == 23
            && local_end.minute() == 59
            && local_end.second() == 59
            && local_end.date_naive() == date)
            || (local_end.hour() == 0
                && local_end.minute() == 0
                && local_end.second() == 0
                && date.succ_opt() == Some(local_end.date_naive())));
    if !single_day {
        return Err(SourceReadError::Review(format!(
            "TeamUp-vagten d. {} strækker sig over flere hele dage. Giv den egne tider, før ugen overføres.",
            date.format("%d/%m")
        )));
    }
    let interval = standard.on(date, zone)
        .map_err(|_| SourceReadError::Review(format!(
            "Standardtiden d. {} kan ikke bruges på grund af sommertid. Ret tiden før overførsel.",
            date.format("%d/%m")
        )))?
        .ok_or_else(|| SourceReadError::Review(format!(
            "TeamUp-vagten d. {} mangler en standardtid for denne ugedag. Tilføj en tid i Indstillinger før overførsel.",
            date.format("%d/%m")
        )))?;
    shift.starts_at = interval.0;
    shift.ends_at = interval.1;
    shift.standard_time = true;
    Ok(())
}

/// List the TeamUp helper calendars and confirm the link exposes shift details
/// and comments, before any mapping can be confirmed against them.
///
/// Returns the calendar choices, their colour ids and a Danish notice about
/// what was checked. Colour is appearance, not identity, so it is kept out of
/// `calendars`: recolouring a calendar in TeamUp must never discard a
/// confirmed mapping or an approval.
pub(crate) async fn source_catalog(
    access: &Access<'_>,
    zone: chrono_tz::Tz,
    today: NaiveDate,
) -> Result<(Value, Value, String), LiveError> {
    let client = client()?;
    let configuration = get(&client, access, &["configuration"], &[]).await?;
    let active: Vec<_> = rows(&configuration["configuration"]["subcalendars"])?
        .iter()
        .filter(|c| c["active"].as_bool().unwrap_or(true))
        .collect();
    let calendars = unique(
        active
            .iter()
            .map(|c| choice(&c["id"], &c["name"]))
            .collect::<Result<_, _>>()?,
    )?;
    if rows(&calendars)?.is_empty() {
        return Err(LiveError(
            "Kalenderlinket giver ingen aktive kalendere. Kontrollér delingsrettighederne.",
        ));
    }
    let mut colors = serde_json::Map::new();
    for calendar in &active {
        if let Some(color) = calendar["color"].as_u64() {
            colors.insert(id(&calendar["id"])?, json!(color));
        }
    }
    // Check the current week, matching the Python setup's access probe.
    let monday = today
        - chrono::Duration::days(i64::from(
            chrono::Datelike::weekday(&today).num_days_from_monday(),
        ));
    let sunday = monday
        .checked_add_signed(chrono::Duration::days(6))
        .ok_or(INVALID)?;
    let listing = get(
        &client,
        access,
        &["events"],
        &[
            ("startDate", monday.to_string()),
            ("endDate", sunday.to_string()),
            ("tz", zone.to_string()),
            ("format", "markdown".into()),
        ],
    )
    .await?;
    let events = rows(&listing["events"])?;
    // Every listed event is hydrated, so an unreadable one blocks setup here
    // rather than during a transfer.
    for summary in events {
        let detail = get(
            &client,
            access,
            &["events", &id(&summary["id"])?],
            &[("format", "markdown".into())],
        )
        .await?;
        let raw = &detail["event"];
        let title = raw["title"].as_str().unwrap_or("").to_lowercase();
        if raw["details_hidden"].as_bool() == Some(true)
            || matches!(
                raw["permission"].as_str(),
                Some("read_only_without_details" | "add_only_without_details")
            )
            || (raw["readonly"].as_bool() == Some(true)
                && matches!(title.as_str(), "reserved" | "reserveret"))
        {
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
    zone.from_local_datetime(&date.and_hms_opt(0, 0, 0).ok_or(INVALID)?)
        .single()
        .map(|date| date.fixed_offset())
        .ok_or(INVALID)
}

/// The `SourceShift::calendar_id` of every shift read with this share key.
pub(crate) fn calendar_id(calendar: &str) -> String {
    format!("{:x}", Sha256::digest(calendar.as_bytes()))[..24].into()
}

fn parse_occurrence(config: &LiveConfig, raw: &Value) -> Result<SourceShift, LiveError> {
    let occurrence_id = id(&raw["id"])?;
    let title = raw["title"].as_str().unwrap_or("");
    if raw["details_hidden"].as_bool() == Some(true)
        || matches!(
            raw["permission"].as_str(),
            Some("read_only_without_details" | "add_only_without_details")
        )
        || (raw["readonly"].as_bool() == Some(true)
            && matches!(title.to_lowercase().as_str(), "reserved" | "reserveret"))
    {
        return Err(LiveError("TeamUp skjuler vagtdetaljer. Kalenderlinket skal give adgang til detaljer og kommentarer."));
    }
    let comments_enabled = raw["comments_enabled"].as_bool().unwrap_or(false);
    let raw_comments = &raw["comments"];
    if comments_enabled
        && (raw_comments.is_null()
            || (raw["comments_visibility"] == "users_with_modify_permission"
                && raw["readonly"].as_bool().unwrap_or(true)
                && raw_comments.as_array().is_none_or(|rows| rows.is_empty())))
    {
        return Err(LiveError(
            "TeamUp-kommentarer kunne ikke læses. Kontrollér delingsrettighederne.",
        ));
    }
    let mut comments = Vec::new();
    if !raw_comments.is_null() {
        for comment in rows(raw_comments)? {
            let message = if comment["message"].is_object() {
                text(&comment["message"]["markdown"])?
            } else {
                text(&comment["message"])?
            };
            let updated = if comment["update_dt"].is_null() {
                &comment["creation_dt"]
            } else {
                &comment["update_dt"]
            };
            comments.push(SourceComment {
                id: id(&comment["id"])?,
                text: message.into(),
                updated_at: if updated.is_null() {
                    None
                } else {
                    Some(timestamp(updated, config.planning.timezone)?)
                },
            });
        }
    }
    let starts_at = timestamp(&raw["start_dt"], config.planning.timezone)?;
    let ends_at = timestamp(&raw["end_dt"], config.planning.timezone)?;
    if ends_at <= starts_at {
        return Err(INVALID);
    }
    Ok(SourceShift {
        calendar_id: calendar_id(&config.calendar),
        event_id: if raw["series_id"].is_null() {
            occurrence_id.split("-rid-").next().unwrap().into()
        } else {
            id(&raw["series_id"])?
        },
        occurrence_id,
        title: title.into(),
        helper_key: String::new(),
        starts_at,
        ends_at,
        notes: raw["notes"].as_str().unwrap_or("").into(),
        comments,
        recurrence_start: if raw["ristart_dt"].is_null() {
            None
        } else {
            Some(timestamp(&raw["ristart_dt"], config.planning.timezone)?)
        },
        source_version: if raw["version"].is_null() {
            None
        } else {
            Some(id(&raw["version"])?)
        },
        standard_time: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_day(start: &str, end: &str) -> SourceShift {
        SourceShift {
            calendar_id: "calendar".into(),
            event_id: "event".into(),
            occurrence_id: "event".into(),
            title: "Vagt".into(),
            helper_key: "helper".into(),
            starts_at: DateTime::parse_from_rfc3339(start).unwrap(),
            ends_at: DateTime::parse_from_rfc3339(end).unwrap(),
            notes: "uni 22-24".into(),
            comments: vec![],
            recurrence_start: None,
            source_version: None,
            standard_time: false,
        }
    }

    #[test]
    fn single_day_all_day_event_uses_overnight_standard_without_losing_notes() {
        let mut shift = all_day("2026-09-26T00:00:00+02:00", "2026-09-26T23:59:59+02:00");
        let standard = StandardTimes {
            everyday: "22-8".into(),
            ..Default::default()
        };
        use_standard_time(&mut shift, &standard, chrono_tz::Europe::Copenhagen).unwrap();
        assert_eq!(shift.starts_at.to_rfc3339(), "2026-09-26T22:00:00+02:00");
        assert_eq!(shift.ends_at.to_rfc3339(), "2026-09-27T08:00:00+02:00");
        assert_eq!(shift.notes, "uni 22-24");
        assert!(shift.standard_time);
    }

    #[test]
    fn missing_standard_and_multi_day_events_name_the_date_for_review() {
        let mut shift = all_day("2026-09-26T00:00:00+02:00", "2026-09-27T00:00:00+02:00");
        let message = use_standard_time(
            &mut shift,
            &StandardTimes::default(),
            chrono_tz::Europe::Copenhagen,
        )
        .unwrap_err()
        .to_string();
        assert!(message.contains("26/09"));
        let mut longer = all_day("2026-09-26T00:00:00+02:00", "2026-09-28T00:00:00+02:00");
        let message = use_standard_time(
            &mut longer,
            &StandardTimes {
                everyday: "6-22".into(),
                ..Default::default()
            },
            chrono_tz::Europe::Copenhagen,
        )
        .unwrap_err()
        .to_string();
        assert!(message.contains("flere hele dage"));
    }
}
