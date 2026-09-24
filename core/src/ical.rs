//! Convert an iCalendar (ICS) feed into source shifts.
//!
//! Nearly every calendar service publishes a read-only feed, so one reader
//! covers Google Calendar, Outlook, iCloud and Nextcloud. A feed is either one
//! helper's calendar, or a shared calendar that names the helper in each
//! event's title; see [`HelperRule`].
//!
//! A shift's identity is its event UID, plus its original start when the event
//! repeats. Moving or editing an event keeps the identity, like in TeamUp.

use std::collections::{BTreeMap, BTreeSet};

use calcard::{
    common::{timezone::Tz as FeedTz, PartialDateTime},
    icalendar::{
        dates::CalendarErrorType, timezone::TzResolver, ICalendar, ICalendarComponent,
        ICalendarComponentType, ICalendarEntry, ICalendarProperty, ICalendarStatus, ICalendarValue,
    },
    Entry, Parser,
};
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::standard_time::StandardTimes;
use crate::{classify_source_title, SourceShift, SourceTitle};

/// Which part of a shared feed's event title names the helper.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitlePart {
    /// The whole title is the helper's name.
    Whole,
    /// The name comes before the first separator: `Anna - 8-16`.
    #[default]
    Before,
    /// The name comes after the last separator: `Vagt: Anna`.
    After,
}

/// How a feed's events are assigned to helpers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HelperRule {
    /// Each feed is one helper's calendar, like a TeamUp sub-calendar.
    Feed,
    /// One shared feed; each event's title names its helper.
    Title { part: TitlePart, separator: String },
}

impl HelperRule {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            HelperRule::Title {
                part: TitlePart::Before | TitlePart::After,
                separator,
            } if separator.trim().is_empty() => {
                Err("Skriv det tegn, der står mellem navnet og resten af titlen, f.eks. -.")
            }
            _ => Ok(()),
        }
    }

    /// Split a title into the helper's name and what is left of the title.
    /// Without the separator, the whole title is the name. The name is `None`
    /// when it is empty.
    pub fn split<'a>(&self, title: &'a str) -> (Option<String>, &'a str) {
        let HelperRule::Title { part, separator } = self else {
            return (None, title);
        };
        let separator = separator.trim();
        let (name, rest) = match part {
            TitlePart::Whole => (title, ""),
            TitlePart::Before => title.split_once(separator).unwrap_or((title, "")),
            TitlePart::After => title
                .rsplit_once(separator)
                .map_or((title, ""), |(rest, name)| (name, rest)),
        };
        let name = collapse(name);
        ((!name.is_empty()).then_some(name), rest.trim())
    }
}

/// The key a helper name is matched by: case and spacing are ignored.
pub fn helper_key(name: &str) -> String {
    collapse(name).to_lowercase()
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IssueKind {
    /// The title names no helper.
    MissingHelper,
    MultiDay,
    NoStandardTime,
    InvalidStandardTime,
    /// A clock time that does not exist, or exists twice, on a DST change.
    AmbiguousTime,
    NoDuration,
    /// Neither UID nor a readable start or repeat rule.
    Unreadable,
    RepeatedEvent,
}

/// An event the feed does not describe clearly enough to transfer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedIssue {
    pub kind: IssueKind,
    /// The occurrence's local date, when it could be read. `None` means the
    /// issue could belong to any week.
    pub date: Option<NaiveDate>,
    /// The helper's name or calendar name, when known.
    pub helper: Option<String>,
}

impl FeedIssue {
    /// Name the helper and date, so the event can be found in the calendar.
    /// Shown on screen only; it names a helper, so never put it in a report.
    pub fn message(&self) -> String {
        let Some(date) = self.date else {
            return "En begivenhed i kalenderen kan ikke læses. Kontrollér gentagelser og tider."
                .into();
        };
        let day = format!("d. {}/{}", date.day(), date.month());
        let shift = match &self.helper {
            Some(helper) if helper.ends_with(['s', 'x', 'z', 'S', 'X', 'Z']) => {
                format!("{helper}' vagt {day}")
            }
            Some(helper) => format!("{helper}s vagt {day}"),
            None => format!("Vagten {day}"),
        };
        match self.kind {
            IssueKind::MissingHelper => format!("Vagten {day} mangler en hjælper i titlen."),
            IssueKind::MultiDay => {
                format!("{shift} strækker sig over flere hele dage. Giv den egne tider.")
            }
            IssueKind::NoStandardTime => format!(
                "{shift} er heldags, og ugedagen har ingen standardtid. Tilføj en tid i Indstillinger."
            ),
            IssueKind::InvalidStandardTime => {
                format!("{shift} har en standardtid, der ikke kan bruges på grund af sommertid.")
            }
            IssueKind::AmbiguousTime => format!(
                "{shift} har et tidspunkt, der ikke findes eller er tvetydigt på grund af sommertid."
            ),
            IssueKind::NoDuration => format!("{shift} slutter ikke efter, den starter."),
            IssueKind::Unreadable => format!("En begivenhed {day} kan ikke læses."),
            IssueKind::RepeatedEvent => {
                format!("{shift} står flere gange i kalenderen med samme id.")
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ParsedFeed {
    /// The feed's own name (`X-WR-CALNAME`), when it has one.
    pub name: Option<String>,
    pub shifts: Vec<SourceShift>,
    pub issues: Vec<FeedIssue>,
    /// Helpers named in a shared feed, by [`helper_key`], with the name as
    /// written. Includes events that still need a fix, so setup can map a
    /// helper before the event is corrected.
    pub helpers: BTreeMap<String, String>,
}

/// Parse a feed, keeping the occurrences that can touch `from..=to`.
///
/// `calendar_id` names the feed in sync history. Under [`HelperRule::Feed`]
/// it is also every shift's helper key. Floating times and all-day events are
/// read in `zone`.
pub fn parse_feed(
    text: &str,
    calendar_id: &str,
    rule: &HelperRule,
    zone: Tz,
    from: NaiveDate,
    to: NaiveDate,
    standard: &StandardTimes,
) -> Result<ParsedFeed, &'static str> {
    let calendar = match Parser::new(text).entry() {
        Entry::ICalendar(calendar) => calendar,
        _ => return Err("Linket gav ikke en kalender, der kan læses."),
    };
    let name = calendar
        .components
        .iter()
        .find(|component| component.component_type == ICalendarComponentType::VCalendar)
        .and_then(|root| {
            root.entries.iter().find(|entry| {
                matches!(&entry.name, ICalendarProperty::Other(name) if name.eq_ignore_ascii_case("X-WR-CALNAME"))
            })
        })
        .and_then(|entry| entry.values.first())
        .and_then(ICalendarValue::as_text)
        .map(collapse)
        .filter(|name| !name.is_empty());
    let window_start = midnight(from, zone)?;
    let window_end = midnight(to.succ_opt().ok_or(OUT_OF_RANGE)?, zone)?;
    // A shift can start the day before the week and run past midnight.
    let first_day = from.pred_opt().ok_or(OUT_OF_RANGE)?;
    // Repeat rules without an end are cut a few days after the window.
    let cap = (window_end + Duration::days(3)).with_timezone(&Utc);

    let zones = ICalendar {
        components: calendar.timezones().cloned().collect(),
    };
    let resolver = zones.build_tz_resolver().with_default(FeedTz::Tz(zone));
    let timezones = &zones.components;
    let mut series: BTreeMap<String, Vec<&ICalendarComponent>> = BTreeMap::new();
    let mut parsed = ParsedFeed {
        name: name.clone(),
        ..Default::default()
    };
    let feed_name = match rule {
        HelperRule::Feed => name,
        HelperRule::Title { .. } => None,
    };
    for event in calendar
        .components
        .iter()
        .filter(|component| component.component_type == ICalendarComponentType::VEvent)
    {
        match text_of(event, &ICalendarProperty::Uid).filter(|uid| !uid.trim().is_empty()) {
            Some(uid) => series.entry(uid.to_owned()).or_default().push(event),
            None => {
                let date = start_date(event);
                if date.is_none_or(|date| date >= first_day && date <= to) {
                    parsed.issues.push(FeedIssue {
                        kind: IssueKind::Unreadable,
                        date,
                        helper: feed_name.clone(),
                    });
                }
            }
        }
    }

    let mut keys = BTreeSet::new();
    for (uid, components) in series {
        let event_id = format!("{:x}", Sha256::digest(uid.as_bytes()))[..24].to_owned();
        for occurrence in expand(timezones, &resolver, &components, zone, cap) {
            let date = occurrence.date(zone);
            if date.is_some_and(|date| date < first_day || date > to) {
                continue;
            }
            let issue = |kind, helper: Option<String>| FeedIssue { kind, date, helper };
            let (component, start, end) = match occurrence {
                Occurrence::Unreadable { .. } => {
                    parsed
                        .issues
                        .push(issue(IssueKind::Unreadable, feed_name.clone()));
                    continue;
                }
                Occurrence::Ambiguous { .. } => {
                    parsed
                        .issues
                        .push(issue(IssueKind::AmbiguousTime, feed_name.clone()));
                    continue;
                }
                Occurrence::Event {
                    component,
                    start,
                    end,
                } => (component, start, end),
            };
            if matches!(component.status(), Some(ICalendarStatus::Cancelled)) {
                continue;
            }
            let summary = collapse(text_of(component, &ICalendarProperty::Summary).unwrap_or(""));
            if classify_source_title(&summary) == SourceTitle::Reminder {
                continue;
            }
            let (helper, title) = match rule {
                HelperRule::Feed => (calendar_id.to_owned(), summary.clone()),
                HelperRule::Title { .. } => {
                    let (name, rest) = rule.split(&summary);
                    let Some(name) = name else {
                        parsed.issues.push(issue(IssueKind::MissingHelper, None));
                        continue;
                    };
                    let key = helper_key(&name);
                    parsed
                        .helpers
                        .entry(key.clone())
                        .and_modify(|shown| {
                            // The same helper written two ways: show one of
                            // them, the same one every time.
                            if name < *shown {
                                shown.clone_from(&name);
                            }
                        })
                        .or_insert_with(|| name.clone());
                    (key, rest.to_owned())
                }
            };
            let shown = match rule {
                HelperRule::Feed => feed_name.clone(),
                HelperRule::Title { .. } => parsed.helpers.get(&helper).cloned(),
            };
            let all_day = start_has_time(component) == Some(false);
            let (starts_at, ends_at) = if all_day {
                let first = start.date_naive();
                if (end.date_naive() - first).num_days() > 1 {
                    parsed.issues.push(issue(IssueKind::MultiDay, shown));
                    continue;
                }
                match standard.on(first, zone) {
                    Ok(Some(interval)) => interval,
                    Ok(None) => {
                        parsed.issues.push(issue(IssueKind::NoStandardTime, shown));
                        continue;
                    }
                    Err(_) => {
                        parsed
                            .issues
                            .push(issue(IssueKind::InvalidStandardTime, shown));
                        continue;
                    }
                }
            } else {
                (start, end)
            };
            if ends_at <= starts_at {
                parsed.issues.push(issue(IssueKind::NoDuration, shown));
                continue;
            }
            if ends_at <= window_start || starts_at >= window_end {
                continue;
            }
            let original = occurrence_start(&resolver, component).unwrap_or(start);
            let repeats = components.len() > 1 || is_recurrent(component);
            let occurrence_id = if repeats {
                format!(
                    "{event_id}-{}",
                    original.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
                )
            } else {
                event_id.clone()
            };
            if !keys.insert(occurrence_id.clone()) {
                parsed.issues.push(issue(IssueKind::RepeatedEvent, shown));
                continue;
            }
            parsed.shifts.push(SourceShift {
                calendar_id: calendar_id.into(),
                event_id: event_id.clone(),
                occurrence_id,
                title: if title.is_empty() {
                    "Vagt".into()
                } else {
                    title
                },
                helper_key: helper,
                starts_at,
                ends_at,
                notes: text_of(component, &ICalendarProperty::Description)
                    .unwrap_or("")
                    .trim()
                    .into(),
                comments: vec![],
                recurrence_start: repeats.then_some(original),
                source_version: version(component),
                standard_time: all_day,
            });
        }
    }
    parsed.shifts.sort_by_key(|shift| shift.starts_at);
    Ok(parsed)
}

const OUT_OF_RANGE: &str = "Datoen ligger uden for det, kalenderen kan læse.";

fn midnight(date: NaiveDate, zone: Tz) -> Result<DateTime<FixedOffset>, &'static str> {
    zone.from_local_datetime(&date.and_hms_opt(0, 0, 0).ok_or(OUT_OF_RANGE)?)
        .earliest()
        .map(|moment| moment.fixed_offset())
        .ok_or("Midnat findes ikke i den valgte tidszone.")
}

enum Occurrence<'a> {
    Event {
        component: &'a ICalendarComponent,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    },
    /// A repeat falls on a clock time that DST skips or repeats.
    Ambiguous { date: NaiveDate },
    /// The start or repeat rule cannot be read at all.
    Unreadable { date: Option<NaiveDate> },
}

impl Occurrence<'_> {
    fn date(&self, zone: Tz) -> Option<NaiveDate> {
        match self {
            Occurrence::Event { start, .. } => Some(start.with_timezone(&zone).date_naive()),
            Occurrence::Ambiguous { date } => Some(*date),
            Occurrence::Unreadable { date } => *date,
        }
    }
}

/// Every occurrence of one event: its series, overrides and extra dates.
///
/// Expanding one UID at a time keeps an override from replacing an
/// occurrence of another event that happens to start at the same moment.
fn expand<'a>(
    timezones: &[ICalendarComponent],
    resolver: &TzResolver<&str>,
    components: &[&'a ICalendarComponent],
    zone: Tz,
    cap: DateTime<Utc>,
) -> Vec<Occurrence<'a>> {
    let mut group = ICalendar {
        components: timezones.to_vec(),
    };
    let offset = group.components.len();
    for component in components {
        let mut component = (*component).clone();
        cap_repeats(&mut component, cap);
        group.components.push(component);
    }
    // The cap bounds every open-ended rule, so this only stops a feed with
    // an absurd number of explicit repeats.
    let expanded = group.expand_dates(FeedTz::Tz(zone), 100_000);
    let mut occurrences = Vec::new();
    for error in expanded.errors {
        let date = error
            .comp_id
            .checked_sub(offset as u32)
            .and_then(|index| components.get(index as usize))
            .and_then(|component| start_date(component));
        // A written start or end only fails to resolve on a DST change.
        occurrences.push(match (error.error, date) {
            (CalendarErrorType::InvalidDtStart | CalendarErrorType::InvalidDtEnd, Some(date)) => {
                Occurrence::Ambiguous { date }
            }
            _ => Occurrence::Unreadable { date },
        });
    }
    // An override replaces the occurrence it names. The library only pairs
    // them when their SEQUENCE matches, so drop the original here as well.
    let overridden: BTreeSet<DateTime<Utc>> = components
        .iter()
        .filter_map(|component| occurrence_start(resolver, component))
        .map(|moment| moment.with_timezone(&Utc))
        .collect();
    for event in expanded.events {
        let Some(component) = (event.comp_id as usize)
            .checked_sub(offset)
            .and_then(|index| components.get(index).copied())
        else {
            continue;
        };
        let is_override = component.is_recurrence_override();
        if !is_override && overridden.contains(&event.start.with_timezone(&Utc)) {
            continue;
        }
        let local_date = event.start.naive_local().date();
        let Some(event) = event.try_into_date_time() else {
            occurrences.push(Occurrence::Ambiguous { date: local_date });
            continue;
        };
        // Floating means the clock time could not be placed in the zone.
        if event.start.timezone().is_floating() || event.end.timezone().is_floating() {
            occurrences.push(Occurrence::Ambiguous { date: local_date });
            continue;
        }
        // UTC and other zones are shown in the planning zone.
        occurrences.push(Occurrence::Event {
            component,
            start: event.start.with_timezone(&zone).fixed_offset(),
            end: event.end.with_timezone(&zone).fixed_offset(),
        });
    }
    occurrences
}

/// End an open repeat rule shortly after the window, so expanding a weekly
/// shift that started years ago never runs forever.
fn cap_repeats(component: &mut ICalendarComponent, cap: DateTime<Utc>) {
    let start = component
        .property(&ICalendarProperty::Dtstart)
        .and_then(|entry| entry.values.first())
        .and_then(partial)
        .and_then(PartialDateTime::to_date_time)
        .map(|start| start.date_time);
    for entry in component.properties_mut(&ICalendarProperty::Rrule) {
        for value in &mut entry.values {
            let ICalendarValue::RecurrenceRule(rule) = value else {
                continue;
            };
            if rule.count.is_some() {
                continue;
            }
            // An end before the start is invalid, so a series that begins
            // after the window ends where it begins.
            let mut until = cap.naive_utc();
            if let Some(start) = start {
                until = until.max(start);
            }
            let earlier = rule
                .until
                .as_ref()
                .and_then(PartialDateTime::to_date_time)
                .is_some_and(|existing| {
                    let offset = existing.offset.map_or(0, |o| o.local_minus_utc());
                    existing.date_time - Duration::seconds(offset.into()) <= until
                });
            if !earlier {
                rule.until = Some(PartialDateTime {
                    year: Some(until.year() as u16),
                    month: Some(until.month() as u8),
                    day: Some(until.day() as u8),
                    hour: Some(until.hour() as u8),
                    minute: Some(until.minute() as u8),
                    second: Some(until.second() as u8),
                    tz_hour: Some(0),
                    tz_minute: Some(0),
                    tz_minus: false,
                });
            }
        }
    }
}

fn partial(value: &ICalendarValue) -> Option<&PartialDateTime> {
    match value {
        ICalendarValue::PartialDateTime(moment) => Some(moment),
        _ => None,
    }
}

fn text_of<'a>(component: &'a ICalendarComponent, property: &ICalendarProperty) -> Option<&'a str> {
    component
        .property(property)
        .and_then(|entry| entry.values.first())
        .and_then(ICalendarValue::as_text)
}

fn start_entry(component: &ICalendarComponent) -> Option<&ICalendarEntry> {
    component.property(&ICalendarProperty::Dtstart)
}

fn start_has_time(component: &ICalendarComponent) -> Option<bool> {
    start_entry(component)
        .and_then(|entry| entry.values.first())
        .and_then(partial)
        .map(PartialDateTime::has_time)
}

/// The date an event starts on as written, for naming an unreadable one.
fn start_date(component: &ICalendarComponent) -> Option<NaiveDate> {
    start_entry(component)
        .or_else(|| component.property(&ICalendarProperty::RecurrenceId))
        .and_then(|entry| entry.values.first())
        .and_then(partial)
        .and_then(PartialDateTime::to_date_time)
        .map(|start| start.date_time.date())
}

fn is_recurrent(component: &ICalendarComponent) -> bool {
    component.is_recurrent_or_override()
}

/// The original start an override replaces (`RECURRENCE-ID`).
fn occurrence_start(
    resolver: &TzResolver<&str>,
    component: &ICalendarComponent,
) -> Option<DateTime<FixedOffset>> {
    let entry = component.property(&ICalendarProperty::RecurrenceId)?;
    let moment = entry.values.first().and_then(partial)?.to_date_time()?;
    let tzid = entry
        .tz_id()
        .or_else(|| start_entry(component).and_then(ICalendarEntry::tz_id));
    let tz = resolver.resolve_or_default(tzid);
    let resolved = moment.to_date_time_with_tz(tz)?;
    (!resolved.timezone().is_floating()).then(|| resolved.fixed_offset())
}

/// What changes when the event is edited. Feeds rewrite DTSTAMP on every
/// export, so it is not used.
fn version(component: &ICalendarComponent) -> Option<String> {
    let modified = component
        .property(&ICalendarProperty::LastModified)
        .and_then(|entry| entry.values.first())
        .and_then(partial)
        .and_then(PartialDateTime::to_date_time)
        .map(|moment| moment.date_time.format("%Y%m%dT%H%M%S").to_string());
    let sequence = component
        .property(&ICalendarProperty::Sequence)
        .and_then(|entry| entry.values.first())
        .and_then(|value| match value {
            ICalendarValue::Integer(sequence) => Some(sequence.to_string()),
            _ => None,
        });
    match (sequence, modified) {
        (None, None) => None,
        (sequence, modified) => Some(format!(
            "{}:{}",
            sequence.unwrap_or_default(),
            modified.unwrap_or_default()
        )),
    }
}
