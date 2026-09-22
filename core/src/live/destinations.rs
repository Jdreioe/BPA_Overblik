use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, FixedOffset, NaiveDate};
use chrono_tz::Tz;
use futures_util::{stream, StreamExt, TryStreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    choice, config::selected, id, named, rows, text, timestamp, timing::Stage, truthy, unique,
    BrowserSessions, LiveConfig, LiveError, Service, INVALID,
};
use crate::{
    DestinationSnapshot, DuosRegistration, MitHfShift, PlanItem, PlanSystem, TimeInterval,
};

/// How many shifts one MitHF extras call asks about. The endpoint takes a
/// comma-separated `eids` list and answers keyed by shift id; the bound keeps
/// one request's body predictable over a long range.
const EKSTRA_BATCH: usize = 20;

/// How many MitHF extras calls a read keeps in flight. The browser serves them
/// from one authenticated tab, so this stays low enough not to flood it.
const EKSTRA_CONCURRENCY: usize = 4;

/// The part of a destination read that does not depend on the source range:
/// the catalog check and the whole DUOS listing, which DUOS only serves
/// unfiltered and this app narrows itself.
///
/// A preview starts this while TeamUp is still being read, and finishes it
/// once the reconciliation range is known.
pub(crate) struct DestinationPrelude {
    duos_registrations: Vec<DuosRegistration>,
}

/// Validate the catalog and read DUOS, before any source range is known.
pub(crate) async fn read_before_range(
    browser: &BrowserSessions,
    config: &LiveConfig,
    now: DateTime<FixedOffset>,
) -> Result<DestinationPrelude, LiveError> {
    let today = now.with_timezone(&config.planning.timezone).date_naive();
    let (_, duos_registrations) = futures_util::try_join!(
        validate_catalog_once(browser, config, today),
        read_duos(browser, config)
    )?;
    Ok(DestinationPrelude { duos_registrations })
}

impl DestinationPrelude {
    /// Read MitHF for the resolved range and narrow DUOS to the same range.
    pub(crate) async fn finish(
        self,
        browser: &BrowserSessions,
        config: &LiveConfig,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<DestinationSnapshot, LiveError> {
        Ok(DestinationSnapshot {
            mithf_shifts: read_mithf(browser, config, start, end).await?,
            duos_registrations: within(self.duos_registrations, start, end),
        })
    }
}

/// A read of both destinations for a known range, without the catalog check.
/// The transfer's read-back uses this: the catalog was validated when the
/// transfer connected.
async fn read_snapshot(
    browser: &BrowserSessions,
    config: &LiveConfig,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> Result<DestinationSnapshot, LiveError> {
    let (mithf_shifts, duos_registrations) = futures_util::try_join!(
        read_mithf(browser, config, start, end),
        read_duos(browser, config)
    )?;
    Ok(DestinationSnapshot {
        mithf_shifts,
        duos_registrations: within(duos_registrations, start, end),
    })
}

/// Keep the registrations overlapping the range. DUOS has no range parameter,
/// so every read narrows the full listing here.
fn within(
    registrations: Vec<DuosRegistration>,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> Vec<DuosRegistration> {
    registrations
        .into_iter()
        .filter(|registration| registration.ends_at > start && registration.starts_at < end)
        .collect()
}

/// Service identities a write depends on, resolved once before any submission.
/// Reads never need them, so only `LiveDestinations` carries them.
struct Identities {
    /// Confirmed MitHF helper name to internal helper id (`vid`).
    helpers: BTreeMap<String, String>,
    customer: String,
    grant: String,
}

pub(crate) fn employment_available(value: &Value, today: NaiveDate) -> bool {
    if value["isActive"].as_bool() == Some(true) {
        return true;
    }
    let date = |v: &Value| {
        v.as_str()
            .and_then(|s| s.get(..10))
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
    };
    let Some(start) = date(&value["startDate"]) else {
        return false;
    };
    start <= today
        && (value["endDate"].is_null()
            || value["endDate"] == ""
            || date(&value["endDate"]).is_some_and(|end| end >= today))
}

/// Build the destination catalog the setup document records.
///
/// Takes the arrangement directly rather than a `LiveConfig`, because setup has
/// to build a catalog before a confirmed configuration exists. An empty
/// `arrangement` leaves `types` and `duos` empty, as the setup flow does before
/// an arrangement is chosen.
pub(crate) async fn build_catalog(
    browser: &BrowserSessions,
    arrangement: &str,
    today: NaiveDate,
) -> Result<Value, LiveError> {
    let helpers = browser
        .request(Service::Mithf, "hjaelperliste", json!({}))
        .await?;
    let mut mithf = Vec::new();
    for group in rows(&helpers["grupper"])? {
        for helper in rows(&group["hjaelpere"])? {
            mithf.push(choice(&helper["vid"], &helper["navn"])?);
        }
    }
    let mithf = unique(mithf)?;
    let options = browser
        .request(Service::Mithf, "muligheder", json!({}))
        .await?;
    let customers = rows(&options["muligheder"]["kunder"])?;
    let grants: Vec<_> = rows(&options["muligheder"]["bevillinger"])?
        .iter()
        .filter(|g| truthy(&g["valgbar"]))
        .collect();
    if customers.len() != 1 || grants.len() != 1 {
        return Err(LiveError(
            "MitHF skal have én kunde og én valgbar bevilling. Kontrollér konto og opsætning.",
        ));
    }
    let customer = named(&customers[0])?;
    let grant = named(grants[0])?;
    let portfolios = browser
        .request(
            Service::Duos,
            "portfolios",
            json!({"dateOfActivePortfolio": today.to_string()}),
        )
        .await?;
    let mut arrangements = Vec::new();
    for portfolio in rows(&portfolios)? {
        if portfolio["type"] != "Ordnings SPS" {
            continue;
        }
        let mut name = text(&portfolio["name"])?.to_owned();
        if let Some(suffix) = portfolio["suffix"].as_str().filter(|s| !s.is_empty()) {
            name.push_str(&format!(" · {suffix}"));
        }
        arrangements.push(choice(&portfolio["id"], &json!(name))?);
    }
    let arrangements = unique(arrangements)?;
    if rows(&arrangements)?.is_empty() {
        return Err(LiveError("DUOS har ingen aktiv SPS-ordning. Kontrollér kontoen og ordningens gyldighed hos DUOS."));
    }
    let (mut types, mut duos) = (json!([]), json!([]));
    if !arrangement.is_empty() {
        selected(&arrangements, &json!(arrangement))?;
        let raw = browser
            .request(Service::Duos, "types", json!({"portfolioId": arrangement}))
            .await?;
        types = unique(rows(&raw)?.iter().map(named).collect::<Result<_, _>>()?)?;
        let employments = browser
            .request(
                Service::Duos,
                "employments",
                json!({"portfolioId": arrangement, "dateOfActiveEmployment": today.to_string()}),
            )
            .await?;
        duos = unique(
            rows(&employments)?
                .iter()
                .filter(|e| employment_available(e, today))
                .map(|e| choice(&e["helperId"], &e["helperName"]))
                .collect::<Result<_, _>>()?,
        )?;
        if rows(&types)?.is_empty() || rows(&duos)?.is_empty() {
            return Err(LiveError("Ordningen har ingen tilgængelige registreringstyper eller aktive hjælpere. Kontrollér ansættelserne hos DUOS."));
        }
    }
    Ok(
        json!({"mithf": mithf, "arrangements": arrangements, "types": types, "duos": duos,
        "account": format!("MitHF: {} · {}", text(&customer["name"])?, text(&grant["name"])?),
        "account_ids": {"customer": customer["id"], "grant": grant["id"]}}),
    )
}

/// Validate the catalog unless this browser session already validated exactly
/// this setup today.
///
/// Reading is the only thing that depends on it here, and a read changes
/// nothing, so a catalog that changes mid-session costs a preview that is out
/// of date rather than a wrong transfer: `LiveDestinations::connect` always
/// validates in full, so a changed catalog still invalidates the approval
/// before anything is written.
async fn validate_catalog_once(
    browser: &BrowserSessions,
    config: &LiveConfig,
    today: NaiveDate,
) -> Result<(), LiveError> {
    let stamp = catalog_stamp(config, today);
    if browser.catalog_validated(&stamp) {
        Stage::start("destination.catalog").done(0);
        return Ok(());
    }
    validate_catalog(browser, config, today).await?;
    browser.remember_catalog(stamp);
    Ok(())
}

/// What a catalog check covered, as a digest: the day it was checked for, the
/// chosen arrangement and the confirmed setup it was compared against. A
/// digest rather than the values themselves, so no account or helper name is
/// held for the session beyond the configuration itself.
fn catalog_stamp(config: &LiveConfig, today: NaiveDate) -> [u8; 32] {
    let scope = json!([
        today.to_string(),
        config.planning.duos_arrangement_id,
        config.planning.duos_registration_type,
        config.setup,
    ]);
    Sha256::digest(scope.to_string().as_bytes()).into()
}

async fn validate_catalog(
    browser: &BrowserSessions,
    config: &LiveConfig,
    today: NaiveDate,
) -> Result<Identities, LiveError> {
    let stage = Stage::start("destination.catalog");
    let catalog = build_catalog(browser, &config.planning.duos_arrangement_id, today).await?;
    stage.done(5);
    if catalog != config.setup["catalog"] {
        return Err(LiveError("Navne, ansættelser eller kontovalg er ændret. Bekræft opsætningen igen i den almindelige app."));
    }
    selected(
        &catalog["types"],
        &json!(config.planning.duos_registration_type),
    )?;
    let mut selected_mithf = BTreeSet::new();
    let mut selected_duos = BTreeSet::new();
    let mut helpers = BTreeMap::new();
    for mapping in rows(&config.setup["mappings"])? {
        if mapping["excluded"].as_bool().ok_or(INVALID)? {
            continue;
        }
        let mit = selected(&catalog["mithf"], &mapping["mithf"])?;
        let duos = selected(&catalog["duos"], &mapping["duos"])?;
        if !selected_mithf.insert(id(&mit["id"])?)
            || !selected_duos.insert(id(&duos["id"])?)
            || rows(&catalog["mithf"])?
                .iter()
                .filter(|r| r["name"] == mit["name"])
                .count()
                != 1
        {
            return Err(LiveError(
                "Hjælpermappingen er ikke entydig. Bekræft opsætningen igen.",
            ));
        }
        // The name is unique in the catalog, so assignment resolves without guessing.
        helpers.insert(text(&mit["name"])?.to_owned(), id(&mit["id"])?);
    }
    Ok(Identities {
        helpers,
        customer: id(&catalog["account_ids"]["customer"])?,
        grant: id(&catalog["account_ids"]["grant"])?,
    })
}

fn local(
    config: &LiveConfig,
    day: &Value,
    clock: &Value,
) -> Result<DateTime<FixedOffset>, LiveError> {
    let day = text(day)?;
    let day = if day.contains('.') {
        NaiveDate::parse_from_str(day, "%d.%m.%Y")
            .map_err(|_| INVALID)?
            .to_string()
    } else {
        day.to_owned()
    };
    Ok(timestamp(
        &json!(format!("{day}T{}", text(clock)?)),
        config.planning.timezone,
    )?
    .with_timezone(&config.planning.timezone)
    .fixed_offset())
}

async fn read_mithf(
    browser: &BrowserSessions,
    config: &LiveConfig,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> Result<Vec<MitHfShift>, LiveError> {
    let first = start
        .with_timezone(&config.planning.timezone)
        .date_naive()
        .checked_sub_signed(chrono::Duration::days(config.lookback_days))
        .ok_or(INVALID)?
        .to_string();
    let last = end
        .with_timezone(&config.planning.timezone)
        .date_naive()
        .to_string();
    let stage = Stage::start("mithf.plan");
    let response = browser
        .request(
            Service::Mithf,
            "plan",
            json!({"fra":first,"til":last,"frisk":1}),
        )
        .await?;
    stage.done(1);
    let raw_by_id = mithf_rows(config, &response, &first, &last, start, end)?;
    // `ekstra` answers for several shifts at once, keyed by shift id, so a
    // week is a couple of calls rather than one per row. Batches are bounded
    // so a long range cannot grow a single request without limit, and the
    // remaining calls do not depend on each other.
    let stage = Stage::start("mithf.ekstra");
    let identifiers: Vec<&String> = raw_by_id.keys().collect();
    let mut calls = Vec::new();
    for batch in identifiers.chunks(EKSTRA_BATCH) {
        let eids = batch
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        calls.push(browser.request(Service::Mithf, "ekstra", json!({ "eids": eids })));
    }
    let extras: Vec<Value> = stream::iter(calls)
        .buffered(EKSTRA_CONCURRENCY)
        .try_collect()
        .await?;
    stage.done(extras.len());
    let mut shifts = Vec::with_capacity(identifiers.len());
    for (batch, extra) in identifiers.chunks(EKSTRA_BATCH).zip(&extras) {
        for identifier in batch {
            // A shift missing from its own batch's answer fails the read: the
            // planner may not treat absent extras as "no SPS hours".
            shifts.push(mithf_shift(
                config,
                identifier,
                &raw_by_id[*identifier],
                extra,
            )?);
        }
    }
    Ok(shifts)
}

fn mithf_rows(
    config: &LiveConfig,
    response: &Value,
    first: &str,
    last: &str,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> Result<BTreeMap<String, Value>, LiveError> {
    if response["fra"] != first || response["til"] != last {
        return Err(LiveError(
            "MitHF bekræftede ikke hele den valgte periode. Prøv igen.",
        ));
    }
    let days = response["dage"].as_object().ok_or(INVALID)?;
    let mut result = BTreeMap::new();
    for day in days.values() {
        for row in rows(day)? {
            let begins = local(config, &row["startFaktisk"], &row["start"])?;
            let finishes = local(config, &row["slutdato"], &row["slut"])?;
            if finishes <= begins {
                return Err(INVALID);
            }
            if finishes > start && begins < end {
                let key = id(&row["id"])?;
                if result.get(&key).is_some_and(|existing| existing != row) {
                    return Err(INVALID);
                }
                result.insert(key, row.clone());
            }
        }
    }
    Ok(result)
}

fn mithf_shift(
    config: &LiveConfig,
    identifier: &str,
    row: &Value,
    extra: &Value,
) -> Result<MitHfShift, LiveError> {
    let mut shift = MitHfShift {
        id: identifier.into(),
        starts_at: local(config, &row["startFaktisk"], &row["start"])?,
        ends_at: local(config, &row["slutdato"], &row["slut"])?,
        helper_count: 1,
        helper_name: if row.get("daekket").map(truthy).ok_or(INVALID)? {
            Some(text(&row["navn"])?.into())
        } else {
            None
        },
        sps_intervals: vec![],
        meeting_intervals: vec![],
        sps_record_ids: vec![],
        meeting_record_ids: vec![],
    };
    for record in rows(&extra["ekstra"][identifier]["paa"])? {
        let name = record["navn"].as_str().unwrap_or("");
        if !matches!(name, "SPS timer" | "Vagtmøde") {
            continue;
        }
        let interval = TimeInterval {
            starts_at: local(config, &record["fraDato"], &record["fra"])?,
            ends_at: local(config, &record["tilDato"], &record["til"])?,
        };
        if interval.ends_at <= interval.starts_at {
            return Err(INVALID);
        }
        let (intervals, ids) = if name == "SPS timer" {
            (&mut shift.sps_intervals, &mut shift.sps_record_ids)
        } else {
            (&mut shift.meeting_intervals, &mut shift.meeting_record_ids)
        };
        intervals.push(interval);
        ids.push(id(&record["id"])?);
    }
    Ok(shift)
}

/// Page the account's whole registration listing. DUOS offers no range
/// filter, so the caller narrows the result with `within`.
async fn read_duos(
    browser: &BrowserSessions,
    config: &LiveConfig,
) -> Result<Vec<DuosRegistration>, LiveError> {
    let stage = Stage::start("duos.search");
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    let mut pages = 0;
    let mut skip = 0;
    loop {
        let response = browser.request(Service::Duos, "search", json!({"skip":skip,"take":100,"includeFields":["id","portfolioId","helperId","dutyTypeId","startDate","endDate","statusId"]})).await?;
        let page = rows(&response["data"])?;
        pages += 1;
        let has_more = response["hasMore"].as_bool().ok_or(INVALID)?;
        if has_more && page.is_empty() {
            return Err(LiveError(
                "DUOS returnerede en ufuldstændig liste. Prøv igen.",
            ));
        }
        for row in page {
            let key = id(&row["id"])?;
            if !seen.insert(key.clone()) {
                return Err(LiveError(
                    "DUOS gentog en registrering under læsningen. Prøv igen.",
                ));
            }
            let begins = timestamp(&row["startDate"], config.planning.timezone)?
                .with_timezone(&config.planning.timezone)
                .fixed_offset();
            let finishes = timestamp(&row["endDate"], config.planning.timezone)?
                .with_timezone(&config.planning.timezone)
                .fixed_offset();
            if finishes <= begins {
                return Err(INVALID);
            }
            result.push(DuosRegistration {
                id: key,
                arrangement_id: id(&row["portfolioId"])?,
                employee_number: id(&row["helperId"])?,
                registration_type: id(&row["dutyTypeId"])?,
                starts_at: begins,
                ends_at: finishes,
                status_id: row["statusId"]
                    .as_i64()
                    .or_else(|| row["statusId"].as_str().and_then(|s| s.parse().ok()))
                    .ok_or(INVALID)?,
            });
        }
        skip += page.len();
        if !has_more {
            break;
        }
        if skip > 100_000 {
            return Err(LiveError(
                "DUOS-listen overskred grænsen for en komplet læsning.",
            ));
        }
    }
    stage.done(pages);
    Ok(result)
}

/// The live `Destinations` adapter used by `apply_plan`.
///
/// Construction resolves MitHF and DUOS identities once, as the Python adapter
/// does before an apply; the per-step reads that follow never re-resolve them.
/// Every write goes through `BrowserSessions::submit`, and a submission error
/// leaves the outcome uncertain for the runner's recovery marker to describe.
pub struct LiveDestinations<'a> {
    browser: &'a BrowserSessions,
    config: &'a LiveConfig,
    identities: Identities,
}

impl<'a> LiveDestinations<'a> {
    /// Both browser sessions must already be signed in. `now` only scopes the
    /// identity lookups to today, never the records that are read.
    pub async fn connect(
        browser: &'a BrowserSessions,
        config: &'a LiveConfig,
        now: DateTime<FixedOffset>,
    ) -> Result<Self, LiveError> {
        let today = now.with_timezone(&config.planning.timezone).date_naive();
        // A transfer always validates in full, whatever a preview cached.
        let identities = validate_catalog(browser, config, today).await?;
        browser.remember_catalog(catalog_stamp(config, today));
        Ok(Self {
            browser,
            config,
            identities,
        })
    }

    async fn submit_item(
        &self,
        item: &PlanItem,
        snapshot: &DestinationSnapshot,
    ) -> Result<Option<String>, LiveError> {
        // Step keys carry the shift segment they belong to; the operation is
        // the same for every segment of a split shift.
        let step = item.step_key.split('#').next().unwrap_or_default();
        if item.system == PlanSystem::Duos {
            return self.register_duos(item).await;
        }
        if step == "mithf.create_shift" {
            return self.create_shift(item, snapshot).await;
        }
        let existing = snapshot
            .mithf_shifts
            .iter()
            .find(|shift| Some(&shift.id) == item.destination_id.as_ref())
            .ok_or(LiveError(
                "MitHF-vagten skal læses tilbage, før hjælper eller kategori kan sættes.",
            ))?;
        match step {
            "mithf.assign_helper" => self.assign_helper(item, existing).await,
            "mithf.set_sps" | "mithf.set_meeting" => {
                self.set_category(item, existing, step == "mithf.set_meeting")
                    .await
            }
            _ => Err(LiveError("Handlingen er ikke en understøttet overførsel.")),
        }
    }

    async fn register_duos(&self, item: &PlanItem) -> Result<Option<String>, LiveError> {
        if let Some(registration) = &item.destination_id {
            let detail = self
                .browser
                .request(Service::Duos, "detail", json!({"id": registration}))
                .await?;
            if detail["status"] != "Afventer" {
                return Err(LiveError(
                    "DUOS har ikke bekræftet, at registreringen kan rettes.",
                ));
            }
        }
        // The 26-hour limit is checked at submission, so an apply that
        // outlives its own preview still validates what it actually sends.
        let request = duos_request(self.config.planning.timezone, item)?;
        self.browser
            .submit(request.service, request.action, request.body)
            .await?;
        // The registration endpoint's response need not contain an ID.
        // Reconciliation finds the unique persisted interval after the write.
        Ok(item.destination_id.clone())
    }

    async fn create_shift(
        &self,
        item: &PlanItem,
        snapshot: &DestinationSnapshot,
    ) -> Result<Option<String>, LiveError> {
        let request = shift_request(
            &self.identities,
            self.config.planning.timezone,
            item,
            snapshot,
        )?;
        let response = self
            .browser
            .submit(request.service, request.action, request.body)
            .await?;
        let created = returned_id(&response)?.or_else(|| item.destination_id.clone());
        if created.is_none()
            || response
                .get("eids")
                .is_some_and(|eids| eids.as_array().is_none_or(|rows| rows.len() != 1))
        {
            return Err(LiveError("MitHF bekræftede ikke præcis én oprettet vagt."));
        }
        Ok(created)
    }

    async fn assign_helper(
        &self,
        item: &PlanItem,
        existing: &MitHfShift,
    ) -> Result<Option<String>, LiveError> {
        let request = assign_request(&self.identities, item, existing)?;
        let response = self
            .browser
            .submit(request.service, request.action, request.body)
            .await?;
        // Booking may move the shift to a new identity; the caller re-reads it.
        Ok(Some(
            returned_id(&response)?.unwrap_or_else(|| existing.id.clone()),
        ))
    }

    async fn set_category(
        &self,
        item: &PlanItem,
        existing: &MitHfShift,
        meeting: bool,
    ) -> Result<Option<String>, LiveError> {
        let request = category_request(self.config.planning.timezone, item, existing, meeting)?;
        self.browser
            .submit(request.service, request.action, request.body)
            .await?;
        Ok(Some(existing.id.clone()))
    }
}

/// One submission: the service, its action, and the exact body to send.
/// Building a request is pure, so it is checked against Python directly.
struct Request {
    service: Service,
    action: &'static str,
    body: Value,
}

fn duos_request(zone: Tz, item: &PlanItem) -> Result<Request, LiveError> {
    let start = moment(zone, &item.payload["starts_at"])?;
    let end = moment(zone, &item.payload["ends_at"])?;
    let hours = (end - start).num_seconds() as f64 / 3600.0;
    if !(0.0 < hours && hours <= 26.0) {
        return Err(LiveError("DUOS kræver et interval på højst 26 timer."));
    }
    let mut body = json!({
        "portfolioId": number(&item.payload["arrangement_id"])?,
        "helperId": number(&item.payload["employee_number"])?,
        "typeId": number(&item.payload["registration_type"])?,
        "startDate": crate::state::isoformat(start),
        "endDate": crate::state::isoformat(end),
        "clientTimeSnapshot": {
            "visibleStartLocal": naive_isoformat(start),
            "visibleEndLocal": naive_isoformat(end),
            "visibleDurationMinutes": (end - start).num_seconds() as f64 / 60.0,
            "clientTimeZoneId": zone.name(),
            "clientStartOffsetMinutes": start.offset().local_minus_utc() / 60,
            "clientEndOffsetMinutes": end.offset().local_minus_utc() / 60,
        },
    });
    if let Some(registration) = &item.destination_id {
        body["registrationId"] = json!(number(&json!(registration))?);
    }
    Ok(Request {
        service: Service::Duos,
        action: "register",
        body,
    })
}

fn shift_request(
    identities: &Identities,
    zone: Tz,
    item: &PlanItem,
    snapshot: &DestinationSnapshot,
) -> Result<Request, LiveError> {
    if item.payload["helper_count"].as_i64() != Some(1) {
        return Err(LiveError(
            "Automatisk oprettelse i MitHF understøtter præcis én hjælper pr. kildevagt.",
        ));
    }
    let start = moment(zone, &item.payload["starts_at"])?;
    let end = moment(zone, &item.payload["ends_at"])?;
    let mut body = fields(json!({
        "dato": start.date_naive().to_string(), "start": clock(start),
        "slut": clock(end), "slutdato": end.date_naive().to_string(),
    }));
    let action = match &item.destination_id {
        Some(identifier) => {
            let existing = snapshot
                .mithf_shifts
                .iter()
                .find(|shift| &shift.id == identifier)
                .ok_or(LiveError(
                    "MitHF-vagten skal læses tilbage, før tiden kan rettes.",
                ))?;
            body.insert("eid".into(), json!(identifier));
            body.insert(
                "gammeldato".into(),
                json!(existing.starts_at.date_naive().to_string()),
            );
            "rettid"
        }
        None => {
            body.insert("antal".into(), json!(1));
            body.insert("klient".into(), json!(identities.customer));
            body.insert("bevilling".into(), json!(identities.grant));
            "opret"
        }
    };
    Ok(Request {
        service: Service::Mithf,
        action,
        body: Value::Object(body),
    })
}

fn assign_request(
    identities: &Identities,
    item: &PlanItem,
    existing: &MitHfShift,
) -> Result<Request, LiveError> {
    let name = text(&item.payload["helper_name"])?;
    let helper = identities.helpers.get(name).ok_or(LiveError(
        "Hjælperen findes ikke entydigt i MitHF. Bekræft opsætningen igen.",
    ))?;
    Ok(Request {
        service: Service::Mithf,
        action: "book",
        body: json!({
            "eid": existing.id, "vid": helper,
            "dato": existing.starts_at.date_naive().to_string(), "start": clock(existing.starts_at),
        }),
    })
}

fn category_request(
    zone: Tz,
    item: &PlanItem,
    existing: &MitHfShift,
    meeting: bool,
) -> Result<Request, LiveError> {
    // Meetings cover the whole segment; SPS carries its interval list.
    // Splitting gives every MitHF shift exactly one interval, so a plan that
    // reaches here with more is a planner defect.
    let (start, end) = if meeting {
        (
            moment(zone, &item.payload["starts_at"])?,
            moment(zone, &item.payload["ends_at"])?,
        )
    } else {
        let intervals = rows(&item.payload["intervals"])?;
        if intervals.len() != 1 {
            return Err(LiveError("En MitHF-vagt har præcis ét SPS-interval."));
        }
        (
            moment(zone, &intervals[0]["starts_at"])?,
            moment(zone, &intervals[0]["ends_at"])?,
        )
    };
    let records = if meeting {
        &existing.meeting_record_ids
    } else {
        &existing.sps_record_ids
    };
    if records.len() > 1 {
        return Err(LiveError(
            "Eksisterende kategoriintervaller lægges ikke sammen.",
        ));
    }
    let mut body = fields(json!({
        "eid": existing.id, "type": "4", "dato": existing.starts_at.date_naive().to_string(),
        "fradato": start.date_naive().to_string(), "tildato": end.date_naive().to_string(),
        "start": clock(start), "slut": clock(end),
    }));
    let action = match records.first() {
        Some(record) => {
            body.insert("rid".into(), json!(record));
            "retreg"
        }
        None => {
            body.insert("typeid".into(), json!(if meeting { "1" } else { "5" }));
            "tilfoejreg"
        }
    };
    Ok(Request {
        service: Service::Mithf,
        action,
        body: Value::Object(body),
    })
}

impl crate::Destinations for LiveDestinations<'_> {
    type Error = LiveError;

    fn read(
        &mut self,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> impl std::future::Future<Output = Result<DestinationSnapshot, LiveError>> + Send {
        read_snapshot(self.browser, self.config, start, end)
    }

    fn write(
        &mut self,
        item: &PlanItem,
        snapshot: &DestinationSnapshot,
    ) -> impl std::future::Future<Output = Result<Option<String>, LiveError>> + Send {
        self.submit_item(item, snapshot)
    }
}

fn fields(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().expect("constant object").clone()
}
fn clock(value: DateTime<FixedOffset>) -> String {
    value.format("%H:%M").to_string()
}
fn moment(zone: Tz, value: &Value) -> Result<DateTime<FixedOffset>, LiveError> {
    Ok(timestamp(value, zone)?.with_timezone(&zone).fixed_offset())
}
/// Python's naive `datetime.isoformat`: seconds, and microseconds only when set.
fn naive_isoformat(value: DateTime<FixedOffset>) -> String {
    let local = value.naive_local();
    match local.and_utc().timestamp_subsec_micros() {
        0 => local.format("%Y-%m-%dT%H:%M:%S").to_string(),
        _ => local.format("%Y-%m-%dT%H:%M:%S%.6f").to_string(),
    }
}
fn number(value: &Value) -> Result<i64, LiveError> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .ok_or(INVALID)
}
/// A falsy `eid` means the service kept the existing identity, as in Python.
fn returned_id(response: &Value) -> Result<Option<String>, LiveError> {
    match response.get("eid") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.is_empty() => Ok(None),
        Some(value) if *value == json!(0) => Ok(None),
        Some(value) => id(value).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PlanItem;

    /// Same synthetic identities the recorded cases were primed with.
    fn identities() -> Identities {
        Identities {
            helpers: [("Anna Hansen", "va-1"), ("Bo Jensen", "va-2")]
                .into_iter()
                .map(|(name, vid)| (name.to_owned(), vid.to_owned()))
                .collect(),
            customer: "k-7".into(),
            grant: "b-3".into(),
        }
    }

    fn build(item: &PlanItem, snapshot: &DestinationSnapshot) -> Result<Request, LiveError> {
        let zone = chrono_tz::Europe::Copenhagen;
        let step = item.step_key.split('#').next().unwrap_or_default();
        if item.system == PlanSystem::Duos {
            return duos_request(zone, item);
        }
        if step == "mithf.create_shift" {
            return shift_request(&identities(), zone, item, snapshot);
        }
        let existing = snapshot
            .mithf_shifts
            .iter()
            .find(|shift| Some(&shift.id) == item.destination_id.as_ref())
            .ok_or(LiveError("missing parent"))?;
        match step {
            "mithf.assign_helper" => assign_request(&identities(), item, existing),
            "mithf.set_sps" | "mithf.set_meeting" => {
                category_request(zone, item, existing, step == "mithf.set_meeting")
            }
            _ => Err(LiveError("unsupported")),
        }
    }

    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        item: PlanItem,
        snapshot: DestinationSnapshot,
        request: Submitted,
    }
    #[derive(serde::Deserialize)]
    struct Submitted {
        system: String,
        action: String,
        payload: Value,
    }

    fn config(arrangement: &str, setup: Value) -> LiveConfig {
        LiveConfig {
            planning: crate::PlanningConfig {
                timezone: chrono_tz::Europe::Copenhagen,
                default_helper_count: 1,
                duos_arrangement_id: arrangement.into(),
                duos_registration_type: "type-1".into(),
                helpers: BTreeMap::new(),
            },
            calendar: "cal".into(),
            api_key: "key".into(),
            bearer: String::new(),
            setup,
            lookback_days: 7,
            state_path: "state.sqlite3".into(),
            helper_names: BTreeMap::new(),
            helper_colors: BTreeMap::new(),
        }
    }

    /// A session skips the catalog check only for exactly what it validated:
    /// a different day, arrangement or confirmed setup has to be checked again
    /// before the preview it feeds can be trusted.
    #[test]
    fn a_remembered_catalog_check_covers_only_what_it_validated() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 14).expect("date");
        let setup = json!({"catalog": {"mithf": [{"id": "va-1", "name": "Anna Hansen"}]}});
        let stamp = catalog_stamp(&config("ord-1", setup.clone()), day);

        assert_eq!(stamp, catalog_stamp(&config("ord-1", setup.clone()), day));
        assert_ne!(
            stamp,
            catalog_stamp(
                &config("ord-1", setup.clone()),
                day.succ_opt().expect("next day")
            )
        );
        assert_ne!(stamp, catalog_stamp(&config("ord-2", setup), day));
        assert_ne!(
            stamp,
            catalog_stamp(
                &config(
                    "ord-1",
                    json!({"catalog": {"mithf": [{"id": "va-1", "name": "Anna Hansen Nielsen"}]}})
                ),
                day,
            )
        );
    }

    /// One batched answer covers several shifts, so each shift must take the
    /// records filed under its own id and no other's.
    #[test]
    fn a_batched_extras_answer_is_read_per_shift() {
        let config = config("ord-1", json!({}));
        let row = |day: &str| {
            json!({"startFaktisk": day, "start": "08:00", "slutdato": day, "slut": "16:00",
                   "daekket": 1, "navn": "Anna Hansen"})
        };
        let record = |id: &str, name: &str, day: &str, from: &str, to: &str| json!({"id": id, "navn": name, "fraDato": day, "fra": from, "tilDato": day, "til": to});
        let extra = json!({"ekstra": {
            "11": {"paa": [record("r-1", "SPS timer", "2026-09-14", "09:00", "10:00")]},
            "12": {"paa": [record("r-2", "Vagtmøde", "2026-09-15", "12:00", "13:00")]},
        }});

        let first = mithf_shift(&config, "11", &row("2026-09-14"), &extra).expect("first shift");
        let second = mithf_shift(&config, "12", &row("2026-09-15"), &extra).expect("second shift");

        assert_eq!(first.sps_record_ids, ["r-1"]);
        assert!(first.meeting_record_ids.is_empty());
        assert_eq!(second.meeting_record_ids, ["r-2"]);
        assert!(second.sps_record_ids.is_empty());
        // A shift the answer does not cover is a failed read, never "no hours".
        assert!(mithf_shift(&config, "13", &row("2026-09-16"), &extra).is_err());
    }

    #[test]
    fn write_requests_match_recorded_cases() {
        // Frozen at the Python removal cutover: 12 supported operations.
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("../../tests/goldens/destinations.json")).unwrap();
        assert_eq!(
            cases.len(),
            12,
            "the oracle must cover every supported operation"
        );
        for case in cases {
            let request = build(&case.item, &case.snapshot)
                .unwrap_or_else(|error| panic!("{}: {error}", case.name));
            assert_eq!(request.service.key(), case.request.system, "{}", case.name);
            assert_eq!(request.action, case.request.action, "{}", case.name);
            assert_eq!(request.body, case.request.payload, "{}", case.name);
        }
    }

    fn zone() -> Tz {
        chrono_tz::Europe::Copenhagen
    }
    fn item(
        system: PlanSystem,
        step_key: &str,
        payload: Value,
        destination_id: Option<&str>,
    ) -> PlanItem {
        PlanItem {
            source_key: "cal-1:ev-1:oc-1".into(),
            system,
            step_key: step_key.into(),
            outcome: crate::Outcome::WouldCreate,
            summary: String::new(),
            payload: fields(payload),
            destination_id: destination_id.map(str::to_owned),
            reason: String::new(),
        }
    }
    fn parent(records: &[&str]) -> DestinationSnapshot {
        DestinationSnapshot {
            mithf_shifts: vec![MitHfShift {
                id: "7001".into(),
                starts_at: DateTime::parse_from_rfc3339("2024-03-04T08:00:00+01:00").unwrap(),
                ends_at: DateTime::parse_from_rfc3339("2024-03-04T16:00:00+01:00").unwrap(),
                helper_count: 1,
                helper_name: None,
                sps_intervals: vec![],
                meeting_intervals: vec![],
                sps_record_ids: records.iter().map(|id| (*id).to_owned()).collect(),
                meeting_record_ids: vec![],
            }],
            duos_registrations: vec![],
        }
    }
    fn duos(starts_at: &str, ends_at: &str) -> PlanItem {
        item(
            PlanSystem::Duos,
            "duos.interval:1",
            json!({"arrangement_id": "55", "employee_number": "801",
            "registration_type": "4", "starts_at": starts_at, "ends_at": ends_at}),
            None,
        )
    }
    #[test]
    fn duos_accepts_future_and_ongoing_but_rejects_overlong_intervals() {
        let future = duos("2025-06-01T08:00:00+02:00", "2025-06-01T16:00:00+02:00");
        assert!(
            duos_request(zone(), &future).is_ok(),
            "future intervals are accepted"
        );
        let ongoing = duos("2024-12-31T22:00:00+01:00", "2025-01-01T06:00:00+01:00");
        assert!(
            duos_request(zone(), &ongoing).is_ok(),
            "ongoing intervals are accepted"
        );
        let long = duos("2024-03-01T08:00:00+01:00", "2024-03-02T11:00:00+01:00");
        assert!(duos_request(zone(), &long).is_err());
        let limit = duos("2024-03-01T08:00:00+01:00", "2024-03-02T10:00:00+01:00");
        assert!(
            duos_request(zone(), &limit).is_ok(),
            "26 hours is still allowed"
        );
        let empty = duos("2024-03-01T08:00:00+01:00", "2024-03-01T08:00:00+01:00");
        assert!(duos_request(zone(), &empty).is_err());
    }

    #[test]
    fn mithf_creation_requires_exactly_one_helper() {
        let two = item(
            PlanSystem::Mithf,
            "mithf.create_shift",
            json!({"starts_at": "2024-03-04T08:00:00+01:00", "ends_at": "2024-03-04T16:00:00+01:00", "helper_count": 2}),
            None,
        );
        assert!(
            shift_request(&identities(), zone(), &two, &DestinationSnapshot::default()).is_err()
        );
    }

    #[test]
    fn categories_never_collapse_several_existing_records() {
        let sps = item(
            PlanSystem::Mithf,
            "mithf.set_sps",
            json!({"intervals": [{"starts_at": "2024-03-04T10:00:00+01:00", "ends_at": "2024-03-04T12:00:00+01:00"}]}),
            Some("7001"),
        );
        let snapshot = parent(&["r-11", "r-12"]);
        assert!(category_request(zone(), &sps, &snapshot.mithf_shifts[0], false).is_err());
    }

    /// The planner gives a meeting the segment's own bounds, not an interval
    /// list, so the meeting request reads them directly. Python reads
    /// ``intervals`` here and raises on the payload its planner produces.
    #[test]
    fn meeting_uses_the_segment_bounds_from_its_payload() {
        let meeting = item(
            PlanSystem::Mithf,
            "mithf.set_meeting",
            json!({"category": "Vagtmøde", "category_code": "4:1",
                   "starts_at": "2024-03-04T09:00:00+01:00", "ends_at": "2024-03-04T15:00:00+01:00"}),
            Some("7001"),
        );
        let snapshot = parent(&[]);
        let request = category_request(zone(), &meeting, &snapshot.mithf_shifts[0], true).unwrap();
        assert_eq!(request.action, "tilfoejreg");
        assert_eq!(request.body["typeid"], json!("1"));
        assert_eq!(request.body["start"], json!("09:00"));
        assert_eq!(request.body["slut"], json!("15:00"));
        assert_eq!(request.body["dato"], json!("2024-03-04"));
    }

    #[test]
    fn writes_are_the_only_transport_actions_they_claim_to_be() {
        let create = item(
            PlanSystem::Mithf,
            "mithf.create_shift",
            json!({"starts_at": "2024-03-04T08:00:00+01:00", "ends_at": "2024-03-04T16:00:00+01:00", "helper_count": 1}),
            None,
        );
        let request = shift_request(
            &identities(),
            zone(),
            &create,
            &DestinationSnapshot::default(),
        )
        .unwrap();
        assert_eq!(request.action, "opret");
        // An update without a read-back parent must not silently create instead.
        let update = item(
            PlanSystem::Mithf,
            "mithf.create_shift",
            json!({"starts_at": "2024-03-04T08:00:00+01:00", "ends_at": "2024-03-04T16:00:00+01:00", "helper_count": 1}),
            Some("7001"),
        );
        assert!(shift_request(
            &identities(),
            zone(),
            &update,
            &DestinationSnapshot::default()
        )
        .is_err());
    }
}
