//! Danish presentation of a finished Rust plan. No network, state writes, or
//! approval authority lives here; the core revalidates every actual transfer.
use crate::protocol::{Attention, Block, Day, Notice, Tone, Week};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use teamup_shift_sync_core::{
    DestinationSnapshot, Outcome, PlanItem, PlanSystem, PlanningConfig, SourceShift, SyncPlan,
};
#[path = "preview_explanations.rs"]
mod explanations;

const DAYS: [&str; 7] = ["man", "tir", "ons", "tor", "fre", "lør", "søn"];
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "maj", "jun", "jul", "aug", "sep", "okt", "nov", "dec",
];
const INVALID: &str = "Ugens tider kunne ikke vises sikkert. Hent ugen igen.";
type Result<T> = std::result::Result<T, &'static str>;

fn blocker(outcome: Outcome) -> bool {
    matches!(
        outcome,
        Outcome::Review | Outcome::Conflicted | Outcome::Failed | Outcome::PendingIntegration
    )
}
fn writes(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::WouldCreate | Outcome::WouldUpdate)
}
fn base(item: &PlanItem) -> &str {
    item.step_key.split('#').next().unwrap_or("")
}
fn segment(item: &PlanItem) -> Result<usize> {
    item.step_key
        .split_once('#')
        .map_or(Ok(0), |(_, v)| v.parse().map_err(|_| INVALID))
}
fn time(value: &Value, zone: Tz) -> Result<DateTime<Tz>> {
    DateTime::parse_from_rfc3339(value.as_str().ok_or(INVALID)?)
        .map(|v| v.with_timezone(&zone))
        .map_err(|_| INVALID)
}
fn bounds(payload: &Map<String, Value>, zone: Tz) -> Result<(DateTime<Tz>, DateTime<Tz>)> {
    let start = time(payload.get("starts_at").ok_or(INVALID)?, zone)?;
    let end = time(payload.get("ends_at").ok_or(INVALID)?, zone)?;
    if end <= start {
        return Err(INVALID);
    }
    Ok((start, end))
}
fn minutes(v: DateTime<Tz>) -> u32 {
    v.hour() * 60 + v.minute()
}
fn date_label(day: NaiveDate) -> String {
    format!(
        "{} {}. {}",
        DAYS[day.weekday().num_days_from_monday() as usize],
        day.day(),
        MONTHS[day.month0() as usize]
    )
}
fn span(start: DateTime<Tz>, end: DateTime<Tz>) -> String {
    if minutes(end) == 0 && start.date_naive().succ_opt() == Some(end.date_naive()) {
        format!("{}–24:00", start.format("%H:%M"))
    } else if start.date_naive() == end.date_naive() {
        format!("{}–{}", start.format("%H:%M"), end.format("%H:%M"))
    } else {
        format!(
            "{} {}. {} – {} {}. {}",
            start.format("%H:%M"),
            start.day(),
            MONTHS[start.month0() as usize],
            end.format("%H:%M"),
            end.day(),
            MONTHS[end.month0() as usize]
        )
    }
}
fn interval(payload: &Map<String, Value>, zone: Tz) -> Result<String> {
    let (s, e) = bounds(payload, zone)?;
    Ok(span(s, e))
}
fn hours(start: DateTime<Tz>, end: DateTime<Tz>) -> String {
    let hours = (end.timestamp_micros() - start.timestamp_micros()) as f64 / 3_600_000_000.0;
    // Match the existing report's six significant digits for service hours.
    let decimals = (5 - hours.log10().floor() as i32).max(0) as usize;
    let amount = format!("{hours:.decimals$}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .replace('.', ",");
    format!("{amount} {}", if hours == 1.0 { "time" } else { "timer" })
}
fn sps(payload: &Map<String, Value>, zone: Tz) -> Result<String> {
    match payload.get("intervals") {
        None | Some(Value::Null) => Ok(String::new()),
        Some(value) => value
            .as_array()
            .ok_or(INVALID)?
            .iter()
            .map(|v| interval(v.as_object().ok_or(INVALID)?, zone))
            .collect::<Result<Vec<_>>>()
            .map(|v| v.join(", ")),
    }
}
fn detail(item: &PlanItem, destination: &DestinationSnapshot, zone: Tz) -> Result<String> {
    use Outcome::*;
    if blocker(item.outcome) && item.outcome != PendingIntegration {
        return Ok(String::new());
    }
    let existing = destination
        .mithf_shifts
        .iter()
        .find(|s| Some(&s.id) == item.destination_id.as_ref());
    Ok(match base(item) {
        "mithf.create_shift" => match item.outcome {
            WouldCreate => "Vagten oprettes i MitHF.".into(),
            WouldUpdate => format!(
                "Vagtens tid ændres i MitHF: {} → {}.",
                existing
                    .map(|s| span(
                        s.starts_at.with_timezone(&zone),
                        s.ends_at.with_timezone(&zone)
                    ))
                    .unwrap_or("ukendt tid".into()),
                interval(&item.payload, zone)?
            ),
            _ => String::new(),
        },
        "mithf.assign_helper" => {
            let name = item
                .payload
                .get("helper_name")
                .and_then(Value::as_str)
                .unwrap_or("");
            match item.outcome {
                WouldCreate => format!("{name} sættes på vagten."),
                WouldUpdate => format!("Vagten tildeles {name}."),
                _ => String::new(),
            }
        }
        "mithf.set_sps" => {
            let after = sps(&item.payload, zone)?;
            match item.outcome {
                WouldCreate => format!("SPS-timer sættes til {after}."),
                WouldUpdate => {
                    let before = existing
                        .filter(|s| !s.sps_intervals.is_empty())
                        .map(|s| {
                            s.sps_intervals
                                .iter()
                                .map(|i| {
                                    span(
                                        i.starts_at.with_timezone(&zone),
                                        i.ends_at.with_timezone(&zone),
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or("ingen SPS-timer".into());
                    format!("SPS-timer ændres i MitHF: {before} → {after}.")
                }
                PendingIntegration => format!("SPS-timer {after} kræver aflæsning af MitHF."),
                AlreadyMatched => format!("SPS-timer {after} er allerede sat."),
                _ => String::new(),
            }
        }
        "mithf.set_meeting" if writes(item.outcome) => "Vagtmøde sættes på hele vagten.".into(),
        "mithf.set_meeting" if item.outcome == PendingIntegration => {
            "Vagtmøde kræver aflæsning af MitHF.".into()
        }
        _ if item.system == PlanSystem::Duos => {
            let (start, end) = bounds(&item.payload, zone)?;
            let label = span(start, end);
            let hours = hours(start, end);
            match item.outcome {
                WouldCreate => format!("DUOS: {hours} registreres for {label}."),
                WouldUpdate => {
                    let before = destination
                        .duos_registrations
                        .iter()
                        .find(|r| Some(&r.id) == item.destination_id.as_ref())
                        .map(|r| {
                            let s = r.starts_at.with_timezone(&zone);
                            let e = r.ends_at.with_timezone(&zone);
                            format!("{} ({})", span(s, e), self::hours(s, e))
                        })
                        .unwrap_or("ukendte timer".into());
                    format!("DUOS: registreringen ændres: {before} → {label} ({hours}).")
                }
                AlreadyMatched => format!("DUOS: {hours} er allerede registreret."),
                _ => String::new(),
            }
        }
        _ => String::new(),
    })
}

/// Build the existing desktop view types directly from core models.
/// Invalid display payloads fail closed rather than omitting an approved write.
pub fn build_week(
    config: &PlanningConfig,
    names: &BTreeMap<String, String>,
    colors: &BTreeMap<String, String>,
    shifts: &[SourceShift],
    plan: &SyncPlan,
    destination: &DestinationSnapshot,
    destination_read: bool,
) -> Result<Week> {
    let zone = config.timezone;
    if plan.ends_at <= plan.starts_at {
        return Err(INVALID);
    }
    let start = plan.starts_at.with_timezone(&zone).date_naive();
    let end = (plan.ends_at - Duration::microseconds(1))
        .with_timezone(&zone)
        .date_naive();
    // The desktop shows a week. Reject corrupt/unbounded input before allocation.
    if (end - start).num_days() > 366 {
        return Err(INVALID);
    }
    let mut days: BTreeMap<NaiveDate, Vec<Block>> = BTreeMap::new();
    let mut day = start;
    loop {
        days.insert(day, vec![]);
        if day == end {
            break;
        }
        day = day.succ_opt().ok_or(INVALID)?;
    }
    let mut by_source: BTreeMap<&str, Vec<&PlanItem>> = BTreeMap::new();
    for item in &plan.items {
        by_source.entry(&item.source_key).or_default().push(item);
    }
    let mut ordered: Vec<_> = shifts.iter().collect();
    ordered.sort_by_key(|s| s.starts_at);
    let mut attention = Vec::new();
    let mut seen = BTreeSet::new();
    let mut planned = 0;
    for shift in ordered {
        let key = shift.key();
        let Some(items) = by_source.get(key.as_str()) else {
            continue;
        };
        if items.iter().all(|i| i.outcome == Outcome::Excluded) {
            continue;
        }
        planned += 1;
        let helper = config
            .helpers
            .get(&shift.helper_key)
            .map(|h| h.mithf_name.as_str())
            .or_else(|| names.get(&shift.helper_key).map(String::as_str))
            .unwrap_or("Ukendt hjælper");
        let shift_blocked = items.iter().any(|i| {
            matches!(i.system, PlanSystem::Source | PlanSystem::Mapping) && blocker(i.outcome)
        });
        let mut segments: BTreeMap<usize, Vec<&PlanItem>> = BTreeMap::new();
        for item in items.iter().filter(|i| i.system == PlanSystem::Mithf) {
            segments.entry(segment(item)?).or_default().push(item);
        }
        if let Some(first) = segments.keys().next().copied() {
            for item in items.iter().filter(|i| i.system == PlanSystem::Duos) {
                let at = item
                    .payload
                    .get("starts_at")
                    .map(|v| time(v, zone))
                    .transpose()?;
                let mut index = first;
                if let Some(at) = at {
                    for (candidate, group) in &segments {
                        if let Some(create) = group.iter().find(|i| base(i) == "mithf.create_shift")
                        {
                            if create.payload.contains_key("starts_at") {
                                let (s, e) = bounds(&create.payload, zone)?;
                                if s <= at && at < e {
                                    index = *candidate;
                                    break;
                                }
                            }
                        }
                    }
                }
                segments.get_mut(&index).ok_or(INVALID)?.push(item);
            }
        }
        for (index, group) in &segments {
            let Some(create) = group.iter().find(|i| base(i) == "mithf.create_shift") else {
                continue;
            };
            if !create.payload.contains_key("starts_at") {
                if writes(create.outcome) {
                    return Err(INVALID);
                }
                continue;
            }
            let (starts_at, ends_at) = bounds(&create.payload, zone)?;
            let status = if shift_blocked
                || group
                    .iter()
                    .any(|i| blocker(i.outcome) && i.outcome != Outcome::PendingIntegration)
            {
                "attention"
            } else if create.outcome == Outcome::WouldCreate {
                "create"
            } else if group.iter().any(|i| writes(i.outcome)) {
                "update"
            } else if group
                .iter()
                .any(|i| i.outcome == Outcome::PendingIntegration)
            {
                "pending"
            } else {
                "matched"
            };
            let status_label = match status {
                "attention" => "Kræver opmærksomhed",
                "create" => "Oprettes",
                "update" => "Ændres",
                "pending" => "Ikke aflæst",
                _ => "Uændret",
            };
            let mut details = Vec::new();
            let mut sps_label = String::new();
            for item in group {
                let line = detail(item, destination, zone)?;
                if !line.is_empty() {
                    details.push(line);
                }
                if base(item) == "mithf.set_sps" {
                    sps_label = sps(&item.payload, zone)?;
                }
            }
            let block = Block {
                helper: helper.into(),
                helper_color: colors.get(&shift.helper_key).cloned().unwrap_or_default(),
                status: status.into(),
                status_label: status_label.into(),
                minutes_from: 0,
                minutes_to: 1440,
                time_label: span(starts_at, ends_at),
                sps_label,
                part_label: if segments.len() > 1 {
                    format!("Del {} af {}", index + 1, segments.len())
                } else {
                    String::new()
                },
                standard_time: shift.standard_time,
                continues_before: false,
                continues_after: false,
                details,
            };
            // Iterate visible days only, retaining the true continuation markers.
            for (day, blocks) in &mut days {
                let midnight = |date: NaiveDate| {
                    zone.from_local_datetime(&date.and_hms_opt(0, 0, 0).ok_or(INVALID)?)
                        .single()
                        .ok_or(INVALID)
                };
                let left = midnight(*day)?;
                let right = midnight(day.succ_opt().ok_or(INVALID)?)?;
                if ends_at <= left || starts_at >= right {
                    continue;
                }
                let s = starts_at.max(left);
                let e = ends_at.min(right);
                blocks.push(Block {
                    minutes_from: minutes(s),
                    minutes_to: if e == right { 1440 } else { minutes(e) },
                    continues_before: s != starts_at,
                    continues_after: e != ends_at,
                    ..block.clone()
                });
            }
        }
        for item in items {
            if !blocker(item.outcome) || item.reason == "offline_preview" {
                continue;
            }
            let (explanation, action) = explanations::explanation(&item.reason);
            let start = shift.starts_at.with_timezone(&zone);
            let when = format!(
                "{} {}",
                date_label(start.date_naive()),
                start.format("%H:%M")
            );
            if seen.insert((key.clone(), when.clone(), explanation, action)) {
                attention.push(Attention {
                    when,
                    who: helper.into(),
                    explanation: explanation.into(),
                    action: action.into(),
                    source_key: key.clone(),
                    // Only a destination entry deleted by hand can be recovered
                    // by forgetting this app's own records for the shift.
                    can_allow_retransfer: item.reason == "destination_missing",
                });
            }
        }
    }
    let counts = counts(plan)?;
    let has_writes = plan.items.iter().any(|i| writes(i.outcome));
    let has_blockers = plan.items.iter().any(|i| blocker(i.outcome));
    let can_apply = destination_read && attention.is_empty() && has_writes && !has_blockers;
    let summary = summary(&counts);
    let parts = labels(&counts);
    let mithf = parts[..5]
        .iter()
        .filter(|v| !v.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let duos = &parts[5];
    let apply_summary = match (mithf.is_empty(), duos.is_empty()) {
        (false, false) => format!("Overfører {mithf} til MitHF og {duos} til DUOS."),
        (false, true) => format!("Overfører {mithf} til MitHF."),
        (true, false) => format!("Overfører {duos} til DUOS."),
        (true, true) => "Der er ingen ændringer at overføre.".into(),
    };
    // One status per week: the outcome, and a next step only when one exists.
    let unread = if config.duos_enabled {
        "MitHF og DUOS"
    } else {
        "MitHF"
    };
    let status = if planned == 0 {
        Notice::new(Tone::Info, "Der er ingen vagter i denne uge", "")
    } else if !attention.is_empty() {
        Notice::new(
            Tone::Warning,
            "Ugen kan ikke overføres endnu",
            if attention.len() == 1 {
                "Løs punktet herunder først.".to_owned()
            } else {
                format!("Løs de {} punkter herunder først.", attention.len())
            },
        )
    } else if !destination_read {
        Notice::new(
            Tone::Warning,
            format!("{unread} kan ikke kontrolleres"),
            format!("Log ind i {unread}, og hent ugen igen."),
        )
    } else if has_blockers {
        Notice::new(
            Tone::Warning,
            "Ugen kan ikke overføres endnu",
            "Hent ugen igen.",
        )
    } else if !has_writes {
        Notice::new(Tone::Success, "Ugen er allerede overført", "")
    } else {
        Notice::new(
            Tone::Info,
            "Ugen er klar til overførsel",
            apply_summary.clone(),
        )
    };
    Ok(Week {
        days: days
            .into_iter()
            .map(|(day, mut blocks)| {
                blocks.sort_by_key(|b| b.minutes_from);
                Day {
                    date: day.to_string(),
                    label: date_label(day),
                    blocks,
                }
            })
            .collect(),
        attention,
        status,
        summary,
        apply_summary,
        can_apply,
        destination_read,
    })
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}
// created, updated, sps, assignments, meetings, duos, matched
fn counts(plan: &SyncPlan) -> Result<[usize; 7]> {
    let mut counts = [0; 7];
    let mut changed = BTreeSet::new();
    let mut created = BTreeSet::new();
    for item in &plan.items {
        if item.system == PlanSystem::Mithf && writes(item.outcome) {
            changed.insert((&item.source_key, segment(item)?));
        }
        if base(item) == "mithf.create_shift" && item.outcome == Outcome::WouldCreate {
            created.insert((&item.source_key, segment(item)?));
        }
    }
    for item in &plan.items {
        let key = (&item.source_key, segment(item)?);
        match base(item) {
            "mithf.create_shift" => match item.outcome {
                Outcome::WouldCreate => counts[0] += 1,
                Outcome::WouldUpdate => counts[1] += 1,
                Outcome::AlreadyMatched if !changed.contains(&key) => counts[6] += 1,
                _ => {}
            },
            "mithf.set_sps" if writes(item.outcome) => counts[2] += 1,
            "mithf.assign_helper" if writes(item.outcome) && !created.contains(&key) => {
                counts[3] += 1
            }
            "mithf.set_meeting" if writes(item.outcome) => counts[4] += 1,
            _ if item.system == PlanSystem::Duos && writes(item.outcome) => {
                counts[5] += 1;
            }
            _ => {}
        }
    }
    Ok(counts)
}
fn labels(counts: &[usize; 7]) -> Vec<String> {
    [
        ("ny vagt", "nye vagter"),
        ("ændret vagt", "ændrede vagter"),
        ("SPS-tidsrum", "SPS-tidsrum"),
        ("hjælpertildeling", "hjælpertildelinger"),
        ("vagtmøde", "vagtmøder"),
        ("registrering", "registreringer"),
        ("vagt", "vagter"),
    ]
    .iter()
    .zip(counts)
    .map(|((one, many), count)| {
        if *count == 0 {
            String::new()
        } else {
            plural(*count, one, many)
        }
    })
    .collect()
}
fn summary(counts: &[usize; 7]) -> Vec<String> {
    labels(counts)
        .into_iter()
        .enumerate()
        .filter(|(_, s)| !s.is_empty())
        .map(|(i, s)| {
            format!(
                "{s}{}",
                match i {
                    0..=4 => " i MitHF",
                    5 => " i DUOS",
                    _ => " er allerede på plads",
                }
            )
        })
        .collect()
}
