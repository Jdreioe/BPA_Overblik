//! Native Iced workflow using the Rust core and app-owned browser sessions.
//! Setup editing remains in the existing application during migration.
use crate::setup;
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Timelike, Utc};
use iced::widget::{button, column, container, progress_bar, row, scrollable, text};
use iced::{Element, Length, Subscription, Task};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
};
use teamup_shift_sync_core::{
    apply_plan_controlled, build_plan,
    live::{
        load_saved_setup, read_destinations, read_shapes, read_teamup, BrowserSessions, LiveConfig,
        LiveDestinations, Service, Setup, Visibility,
    },
    plan_digest, reconciliation_range, ApplyOutcome, ApplyRequest, Outcome, PlanItem, PlanRequest,
    PlanSystem, SyncState, TransferEvent, TransferOperation,
};
use teamup_shift_sync_gui::{files::app_data_dir, preview::build_week, protocol::Week};
use tokio::sync::{Mutex, MutexGuard};

type Result<T> = std::result::Result<T, String>;

/// Project the setup document onto what the setup screen may display.
fn view(document: &Setup) -> Result<setup::SetupState> {
    serde_json::from_value(document.view()).map_err(|_| "Opsætningen kunne ikke vises.".to_owned())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Destination {
    Mithf,
    Duos,
}

impl Destination {
    fn from_system(system: PlanSystem) -> Option<Self> {
        match system {
            PlanSystem::Mithf => Some(Self::Mithf),
            PlanSystem::Duos => Some(Self::Duos),
            PlanSystem::Source | PlanSystem::Mapping => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Mithf => "MitHF",
            Self::Duos => "DUOS",
        }
    }
}

/// One user-visible shift or registration. Several MitHF API writes may make
/// up one shift, while every DUOS interval remains its own registration.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ProgressUnit {
    destination: Destination,
    source: String,
    unit: String,
}

#[derive(Clone, Debug)]
struct StepLabels {
    unit: ProgressUnit,
    started: String,
    verified: String,
}

/// Transfer state shared with the async runner. It contains display labels and
/// opaque identifiers, never plan payloads or source text.
#[derive(Clone, Debug, Default)]
struct TransferProgress {
    expected: BTreeMap<ProgressUnit, usize>,
    current: String,
    uncertain: BTreeSet<ProgressUnit>,
    verified: Vec<VerifiedStep>,
}

#[derive(Clone, Debug)]
struct VerifiedStep {
    unit: ProgressUnit,
}

impl TransferProgress {
    fn verified_units(&self) -> usize {
        self.expected
            .iter()
            .filter(|(unit, total)| {
                self.verified
                    .iter()
                    .filter(|step| &step.unit == *unit)
                    .count()
                    >= **total
            })
            .count()
    }

    fn verified_in(&self, destination: Destination) -> usize {
        self.expected
            .iter()
            .filter(|(unit, total)| {
                unit.destination == destination
                    && self
                        .verified
                        .iter()
                        .filter(|step| &step.unit == *unit)
                        .count()
                        >= **total
            })
            .count()
    }
}

fn progress_unit(item: &PlanItem) -> Option<ProgressUnit> {
    progress_unit_parts(item.system, &item.source_key, &item.step_key)
}

fn operation_unit(operation: &TransferOperation) -> Option<ProgressUnit> {
    progress_unit_parts(operation.system, &operation.source_key, &operation.step_key)
}

fn progress_unit_parts(
    system: PlanSystem,
    source_key: &str,
    step_key: &str,
) -> Option<ProgressUnit> {
    let destination = Destination::from_system(system)?;
    let unit = match destination {
        Destination::Mithf => step_key
            .split_once('#')
            .map_or_else(|| "0".to_owned(), |(_, suffix)| suffix.to_owned()),
        Destination::Duos => step_key.to_owned(),
    };
    Some(ProgressUnit {
        destination,
        source: source_key.to_owned(),
        unit,
    })
}

fn write_counts(items: &[PlanItem]) -> BTreeMap<ProgressUnit, usize> {
    let mut counts = BTreeMap::new();
    for item in items
        .iter()
        .filter(|item| matches!(item.outcome, Outcome::WouldCreate | Outcome::WouldUpdate))
    {
        if let Some(unit) = progress_unit(item) {
            *counts.entry(unit).or_insert(0) += 1;
        }
    }
    counts
}

fn has_unvalidated_duos_writes(items: &[PlanItem]) -> bool {
    items.iter().any(|item| {
        item.system == PlanSystem::Duos
            && matches!(item.outcome, Outcome::WouldCreate | Outcome::WouldUpdate)
    })
}

fn block_unvalidated_duos(week: &mut Week, items: &[PlanItem]) {
    if has_unvalidated_duos_writes(items) {
        week.can_apply = false;
        week.blocked_reason = "DUOS-overførsel er ikke aktiveret endnu. Gemme- og godkendelsesforløbet skal verificeres først.".into();
    }
}

/// The verified result of one approved run.
#[derive(Clone, Debug)]
struct ApplyDone {
    mithf: usize,
    duos: usize,
    verified_label: Option<String>,
    stopped: bool,
}

#[derive(Clone, Debug)]
struct ApplyFailure {
    message: String,
    verified: usize,
    uncertain: usize,
    remaining: usize,
}

type ApplyResult = std::result::Result<ApplyDone, ApplyFailure>;

fn verb_of(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::WouldCreate => "Tilføjer",
        Outcome::WouldUpdate => "Opdaterer",
        _ => "Verificerede",
    }
}

fn short_date_da(day: u32, month0: u32) -> String {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "maj", "jun", "jul", "aug", "sep", "okt", "nov", "dec",
    ];
    format!("{day}. {}", MONTHS[month0 as usize % 12])
}

/// Short Danish timestamp without seconds in the configured local timezone.
fn format_verified_da(moment: DateTime<FixedOffset>, zone: chrono_tz::Tz) -> String {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "maj", "jun", "jul", "aug", "sep", "okt", "nov", "dec",
    ];
    let local = moment.with_timezone(&zone);
    format!(
        "{}. {} kl. {:02}.{:02}",
        local.day(),
        MONTHS[local.month0() as usize],
        local.hour(),
        local.minute()
    )
}

fn failure_summary(failure: &ApplyFailure) -> String {
    let mut summary = format!(
        "Overførslen blev ikke færdig. {} verificeret, {} uafklaret og {} ikke startet.",
        failure.verified, failure.uncertain, failure.remaining
    );
    if failure.uncertain > 0 {
        summary.push_str(" En uafklaret ændring kan allerede være gemt.");
    }
    summary
}

fn progress_line(verified: usize, total: usize) -> String {
    if total == 0 {
        "Overfører.".into()
    } else {
        format!("{verified} af {total} ændringer overført.")
    }
}

fn completion_summary(done: &ApplyDone) -> String {
    let mut parts = Vec::new();
    if done.mithf > 0 {
        parts.push(format!("{} vagter i MitHF", done.mithf));
    }
    if done.duos > 0 {
        parts.push(format!("{} registreringer i DUOS", done.duos));
    }
    let what = if parts.is_empty() {
        "Ingen nye ændringer.".to_owned()
    } else {
        parts.join(" og ") + " er læst tilbage og verificeret."
    };
    match &done.verified_label {
        Some(label) => format!("Færdig. {what} Sidst verificeret {label}."),
        None => format!("Færdig. {what}"),
    }
}

#[derive(Clone)]
struct Engine {
    data_dir: PathBuf,
    browsers: Arc<Mutex<Option<BrowserSessions>>>,
    setup: Arc<Mutex<Option<Setup>>>,
}

/// The setup document, plus the confirmed configuration once it is ready.
#[derive(Clone)]
struct Loaded {
    state: setup::SetupState,
    account: Option<Arc<LiveConfig>>,
}
impl Engine {
    fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            browsers: Arc::new(Mutex::new(None)),
            setup: Arc::new(Mutex::new(None)),
        }
    }
    /// Read the setup document, and the confirmed configuration if it is
    /// ready. Both touch the disk and the OS keyring, so both run off the UI
    /// thread.
    async fn load(&self) -> Result<Loaded> {
        let dir = self.data_dir.clone();
        let document = tokio::task::spawn_blocking(move || Setup::load(&dir))
            .await
            .map_err(|_| "Opsætningen kunne ikke indlæses.".to_owned())?
            .map_err(|e| e.to_string())?;
        let state = view(&document)?;
        *self.setup.lock().await = Some(document);
        let account = if state.stage == "ready" {
            let dir = self.data_dir.clone();
            Some(Arc::new(
                tokio::task::spawn_blocking(move || load_saved_setup(&dir))
                    .await
                    .map_err(|_| "Opsætningen kunne ikke indlæses.".to_owned())?
                    .map_err(|e| e.to_string())?,
            ))
        } else {
            None
        };
        Ok(Loaded { state, account })
    }
    /// The confirmed configuration, re-read from disk.
    async fn account(&self) -> Result<Arc<LiveConfig>> {
        self.load()
            .await?
            .account
            .ok_or_else(|| "Bekræft opsætningen først.".to_owned())
    }
    async fn connect(&self, link: String, api_key: String) -> Result<setup::SetupState> {
        let mut guard = self.setup.lock().await;
        let document = guard.as_mut().ok_or("Opsætningen er ikke indlæst.")?;
        document
            .connect(&link, &api_key)
            .await
            .map_err(|e| e.to_string())?;
        view(document)
    }
    /// Apply one setup action. `discover` and `confirm` read both services, so
    /// they take the browser sessions first; the lock order is always browsers
    /// before the document.
    async fn act(&self, action: &'static str, params: Value) -> Result<setup::SetupState> {
        if matches!(action, "discover" | "confirm") {
            let sessions = self.sessions().await?;
            let browser = sessions.as_ref().ok_or("Log ind i MitHF og DUOS først.")?;
            let mut guard = self.setup.lock().await;
            let document = guard.as_mut().ok_or("Opsætningen er ikke indlæst.")?;
            if action == "discover" {
                document
                    .discover(browser, params["arrangement"].as_str())
                    .await
            } else {
                document.confirm(browser).await
            }
            .map_err(|e| e.to_string())?;
            return view(document);
        }
        let mut guard = self.setup.lock().await;
        let document = guard.as_mut().ok_or("Opsætningen er ikke indlæst.")?;
        match action {
            "status" => Ok(()),
            "source" => document.edit_source(),
            "retry_source" => document.refresh_source().await,
            "edit" => document.edit(&params),
            _ => Err(teamup_shift_sync_core::live::LiveError(
                "Handlingen findes ikke.",
            )),
        }
        .map_err(|e| e.to_string())?;
        view(document)
    }
    async fn prepared(&self) -> Result<MutexGuard<'_, Option<BrowserSessions>>> {
        let mut guard = self.browsers.lock().await;
        if guard.is_none() {
            let dir = self.data_dir.clone();
            *guard = Some(
                tokio::task::spawn_blocking(move || BrowserSessions::new(dir))
                    .await
                    .map_err(|_| "Browseren kunne ikke forberedes.".to_owned())?
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok(guard)
    }
    async fn login(&self, service: Service) -> Result<()> {
        let mut guard = self.prepared().await?;
        guard
            .as_mut()
            .ok_or("Browseren kunne ikke forberedes.")?
            .open(service, Visibility::Window)
            .await
            .map_err(|e| e.to_string())
    }
    /// Reading only needs the saved profile, so no browser window is opened.
    /// A window left over from login keeps serving these requests.
    async fn sessions(&self) -> Result<MutexGuard<'_, Option<BrowserSessions>>> {
        let mut guard = self.prepared().await?;
        {
            let browser = guard.as_mut().ok_or("Browseren kunne ikke forberedes.")?;
            for service in [Service::Mithf, Service::Duos] {
                browser
                    .open(service, Visibility::Background)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(guard)
    }
    async fn check(&self) -> Result<()> {
        let guard = self.sessions().await?;
        let browser = guard.as_ref().ok_or("Log ind i MitHF og DUOS først.")?;
        browser
            .check(Service::Mithf)
            .await
            .map_err(|e| e.to_string())?;
        browser
            .check(Service::Duos)
            .await
            .map_err(|e| e.to_string())
    }
    /// Record what MitHF and DUOS actually send, for checking the readers'
    /// assumptions. Types only: the file holds no shift, helper or account
    /// content, so it is safe to attach to a report.
    async fn shapes(&self, account: Arc<LiveConfig>, from: NaiveDate) -> Result<PathBuf> {
        let guard = self.sessions().await?;
        let browser = guard.as_ref().ok_or("Log ind i MitHF og DUOS først.")?;
        let (to, _, _) = range(&account, from)?;
        let today = Utc::now()
            .with_timezone(&account.planning.timezone)
            .date_naive();
        let recorded = read_shapes(browser, &account, from, to, today)
            .await
            .map_err(|e| e.to_string())?;
        let path = self.data_dir.join("service-shapes.json");
        let target = path.clone();
        tokio::task::spawn_blocking(move || {
            serde_json::to_string_pretty(&recorded)
                .map_err(|_| "Diagnostikken kunne ikke skrives.".to_owned())
                .and_then(|document| {
                    std::fs::write(&target, document + "\n")
                        .map_err(|_| "Diagnostikken kunne ikke gemmes.".to_owned())
                })
        })
        .await
        .map_err(|_| "Diagnostikken kunne ikke gemmes.".to_owned())??;
        Ok(path)
    }
    async fn preview(&self, account: Arc<LiveConfig>, from: NaiveDate) -> Result<Preview> {
        let guard = self.sessions().await?;
        let browser = guard.as_ref().ok_or("Log ind i MitHF og DUOS først.")?;
        let (to, start, end) = range(&account, from)?;
        let shifts = read_teamup(&account, from, to)
            .await
            .map_err(|e| e.to_string())?;
        let now = Utc::now().fixed_offset();
        let (read_start, read_end) = reconciliation_range(&shifts, start, end);
        let destination = read_destinations(browser, &account, read_start, read_end, now)
            .await
            .map_err(|e| e.to_string())?;
        tokio::task::spawn_blocking(move || {
            let state = SyncState::open(&account.state_path)
                .map_err(|_| "Den lokale overførselshistorik kunne ikke læses.".to_owned())?;
            let plan = build_plan(
                &PlanRequest {
                    config: &account.planning,
                    shifts: &shifts,
                    destination: &destination,
                    range_start: start,
                    range_end: end,
                    now,
                    live: true,
                },
                &state,
            )
            .map_err(|_| "Ugens plan kunne ikke beregnes sikkert.".to_owned())?;
            let mut week = build_week(
                &account.planning,
                &account.helper_names,
                &account.helper_colors,
                &shifts,
                &plan,
                &destination,
                true,
            )
            .map_err(str::to_owned)?;
            block_unvalidated_duos(&mut week, &plan.items);
            let digest = plan_digest(&plan)
                .map_err(|_| "Ugens godkendelse kunne ikke beregnes.".to_owned())?;
            Ok(Preview {
                week,
                digest,
                from,
                state_path: account.state_path.clone(),
                items: plan.items.clone(),
            })
        })
        .await
        .map_err(|_| "Ugens plan kunne ikke indlæses.".to_owned())?
    }
    /// The last fully verified transfer, if any. Snapshots are only stored
    /// after the final read-back, so this never reports a partial attempt.
    /// Failures return None. The home screen then simply shows no timestamp.
    async fn last_verified(&self, state_path: PathBuf, zone: chrono_tz::Tz) -> Option<String> {
        tokio::task::spawn_blocking(move || {
            let state = SyncState::open(&state_path).ok()?;
            let moment = state.last_source_snapshot_at().ok()??;
            Some(format_verified_da(moment, zone))
        })
        .await
        .ok()?
    }
    async fn apply(
        &self,
        approved: Preview,
        progress: Arc<StdMutex<TransferProgress>>,
        stop: Arc<AtomicBool>,
    ) -> ApplyResult {
        // Re-read saved setup as well as TeamUp. A changed account scope must
        // never inherit an approval, even when its week happens to look alike.
        let account = self
            .account()
            .await
            .map_err(|message| apply_failure(message, &progress))?;
        if account.state_path != approved.state_path {
            return Err(apply_failure(
                "Opsætningen er ændret. Genindlæs opsætningen, og gennemgå ugen igen.".into(),
                &progress,
            ));
        }
        if has_unvalidated_duos_writes(&approved.items) {
            return Err(apply_failure(
                "DUOS-overførsel er ikke aktiveret endnu. Gemme- og godkendelsesforløbet skal verificeres først.".into(),
                &progress,
            ));
        }
        let guard = self
            .sessions()
            .await
            .map_err(|message| apply_failure(message, &progress))?;
        let browser = guard
            .as_ref()
            .ok_or_else(|| apply_failure("Log ind i MitHF og DUOS først.".into(), &progress))?;
        let (to, start, end) =
            range(&account, approved.from).map_err(|message| apply_failure(message, &progress))?;
        let shifts = read_teamup(&account, approved.from, to)
            .await
            .map_err(|error| apply_failure(error.to_string(), &progress))?;
        let now = Utc::now().fixed_offset();
        let mut destination = LiveDestinations::connect(browser, &account, now)
            .await
            .map_err(|error| apply_failure(error.to_string(), &progress))?;
        // Display lines for each approved write, keyed by source and step.
        // The progress callback only names the verified item, so resolve
        // helper and date here while shifts and names are at hand.
        let mut context = BTreeMap::new();
        for shift in &shifts {
            let helper = account
                .helper_names
                .get(&shift.helper_key)
                .cloned()
                .or_else(|| {
                    account
                        .planning
                        .helpers
                        .get(&shift.helper_key)
                        .map(|mapping| mapping.mithf_name.clone())
                })
                .unwrap_or_else(|| "Ukendt hjælper".into());
            let at = shift.starts_at.with_timezone(&account.planning.timezone);
            context.insert(shift.key(), (helper, short_date_da(at.day(), at.month0())));
        }
        let mut labels = BTreeMap::new();
        for item in &approved.items {
            let Some(unit) = progress_unit(item) else {
                continue;
            };
            let service = unit.destination.name();
            if let Some((helper, date)) = context.get(&item.source_key) {
                labels.insert(
                    (item.source_key.clone(), item.step_key.clone()),
                    StepLabels {
                        unit,
                        started: format!("{} {helper} {date} i {service}", verb_of(item.outcome)),
                        verified: format!("Verificeret: {helper} {date} i {service}"),
                    },
                );
            }
        }
        let outcome = apply_plan_controlled(
            ApplyRequest {
                config: account.planning.clone(),
                shifts,
                range_start: start,
                range_end: end,
                now,
                expected_digest: approved.digest,
            },
            account.state_path.clone(),
            &mut destination,
            |event| update_transfer_progress(&progress, &labels, event),
            || stop.load(Ordering::SeqCst),
        )
        .await
        .map_err(|error| {
            let message = match error {
                teamup_shift_sync_core::TransferError::Approval(
                    teamup_shift_sync_core::ApprovalError::PlanChanged,
                ) => "Ugen er ændret. Hent ændringerne igen, og gennemgå dem før overførsel."
                    .to_owned(),
                teamup_shift_sync_core::TransferError::State(
                    teamup_shift_sync_core::StateError::ApplyInProgress,
                ) => "En anden overførsel bruger denne konto. Vent, og hent ugen igen.".to_owned(),
                _ => "Overførslen kunne ikke afsluttes og verificeres.".to_owned(),
            };
            apply_failure(message, &progress)
        })?;
        let (mithf, duos) = progress
            .lock()
            .map(|state| {
                (
                    state.verified_in(Destination::Mithf),
                    state.verified_in(Destination::Duos),
                )
            })
            .unwrap_or_default();
        let (stopped, verified_label) = match outcome {
            ApplyOutcome::Completed { verified_at, .. } => (
                false,
                Some(format_verified_da(verified_at, account.planning.timezone)),
            ),
            ApplyOutcome::Stopped { .. } => (true, None),
        };
        Ok(ApplyDone {
            mithf,
            duos,
            verified_label,
            stopped,
        })
    }
}

fn update_transfer_progress(
    progress: &Arc<StdMutex<TransferProgress>>,
    labels: &BTreeMap<(String, String), StepLabels>,
    event: TransferEvent,
) {
    enum Phase {
        Started,
        Uncertain,
        Verified,
    }
    let (phase, operation) = match event {
        TransferEvent::Started(operation) => (Phase::Started, operation),
        TransferEvent::Uncertain(operation) => (Phase::Uncertain, operation),
        TransferEvent::Verified(operation) => (Phase::Verified, operation),
    };
    let Some(unit) = operation_unit(&operation) else {
        return;
    };
    let key = (operation.source_key, operation.step_key);
    let Some(labels) = labels.get(&key) else {
        return;
    };
    if let Ok(mut state) = progress.lock() {
        match phase {
            Phase::Started => state.current = labels.started.clone(),
            Phase::Uncertain => {
                state.uncertain.insert(unit);
            }
            Phase::Verified => {
                state.uncertain.remove(&unit);
                state.current = labels.verified.clone();
                state.verified.push(VerifiedStep {
                    unit: labels.unit.clone(),
                });
            }
        }
    }
}

fn apply_failure(message: String, progress: &Arc<StdMutex<TransferProgress>>) -> ApplyFailure {
    let (verified, uncertain, total) = progress
        .lock()
        .map(|state| {
            (
                state.verified_units(),
                state.uncertain.len(),
                state.expected.len(),
            )
        })
        .unwrap_or_default();
    ApplyFailure {
        message,
        verified,
        uncertain,
        remaining: total.saturating_sub(verified + uncertain),
    }
}

type Bounds = (
    NaiveDate,
    chrono::DateTime<chrono::FixedOffset>,
    chrono::DateTime<chrono::FixedOffset>,
);
fn range(config: &LiveConfig, from: NaiveDate) -> Result<Bounds> {
    let to = from
        .checked_add_signed(Duration::days(6))
        .ok_or("Ugen ligger uden for kalenderens grænser.")?;
    let after = to
        .succ_opt()
        .ok_or("Ugen ligger uden for kalenderens grænser.")?;
    let midnight = |date: NaiveDate| {
        config
            .planning
            .timezone
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight"))
            .single()
            .map(|v| v.fixed_offset())
            .ok_or_else(|| "Ugens datogrænse er ikke entydig i den valgte tidszone.".to_owned())
    };
    Ok((to, midnight(from)?, midnight(after)?))
}

#[derive(Clone)]
struct Preview {
    week: Week,
    digest: String,
    from: NaiveDate,
    state_path: PathBuf,
    items: Vec<PlanItem>,
}

#[derive(Clone)]
enum Message {
    Reload,
    Loaded(Result<Loaded>),
    VerifiedLoaded(PathBuf, Option<String>),
    Setup(setup::Message),
    SetupUpdated(Result<setup::SetupState>),
    Navigate(i64),
    Current,
    Login(Service),
    LoginOpened(Result<()>),
    CheckLogin,
    LoginChecked(Result<()>),
    Capture,
    Captured(Result<PathBuf>),
    Preview,
    PreviewLoaded(Result<Box<Preview>>),
    Apply,
    StopApply,
    ApplyTick,
    Applied(ApplyResult),
    CloseRequested(iced::window::Id),
}
// Events may carry account configuration or private shift data.
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeMessage")
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Activity {
    Idle,
    Setup,
    Login,
    Capture,
    Preview,
    Apply,
}

struct NativeApp {
    engine: Engine,
    account: Option<Arc<LiveConfig>>,
    setup: setup::SetupUi,
    monday: NaiveDate,
    preview: Option<Preview>,
    activity: Activity,
    notice: String,
    error: Option<String>,
    last_verified: Option<String>,
    apply_progress: Option<Arc<StdMutex<TransferProgress>>>,
    apply_stop: Option<Arc<AtomicBool>>,
    apply_total: usize,
    apply_verified: usize,
    apply_write_total: usize,
    apply_write_verified: usize,
    apply_bar: f32,
    apply_current: String,
    apply_summary: String,
    apply_stopping: bool,
    needs_recheck: bool,
    close_after_apply: Option<iced::window::Id>,
}
impl NativeApp {
    fn new() -> (Self, Task<Message>) {
        let mut app = Self {
            engine: Engine::new(app_data_dir()),
            account: None,
            setup: setup::SetupUi::default(),
            monday: Utc::now().date_naive(),
            preview: None,
            activity: Activity::Idle,
            notice: String::new(),
            error: None,
            last_verified: None,
            apply_progress: None,
            apply_stop: None,
            apply_total: 0,
            apply_verified: 0,
            apply_write_total: 0,
            apply_write_verified: 0,
            apply_bar: 0.0,
            apply_current: String::new(),
            apply_summary: String::new(),
            apply_stopping: false,
            needs_recheck: false,
            close_after_apply: None,
        };
        let task = app.update(Message::Reload);
        (app, task)
    }
    fn monday(&self) -> NaiveDate {
        let today = self
            .account
            .as_ref()
            .map(|c| Utc::now().with_timezone(&c.planning.timezone).date_naive())
            .unwrap_or_else(|| Utc::now().date_naive());
        today - Duration::days(today.weekday().num_days_from_monday().into())
    }
    fn update(&mut self, message: Message) -> Task<Message> {
        // One operation at a time. In particular, navigation/setup/login cannot
        // change the selected account or approval while an apply is running.
        let completion = matches!(
            message,
            Message::Loaded(_)
                | Message::VerifiedLoaded(..)
                | Message::SetupUpdated(_)
                | Message::LoginOpened(_)
                | Message::LoginChecked(_)
                | Message::Captured(_)
                | Message::PreviewLoaded(_)
                | Message::Applied(_)
        );
        // Progress ticks only run during a transfer. They never start work.
        if matches!(message, Message::ApplyTick) {
            if self.activity == Activity::Apply {
                self.refresh_progress();
            }
            return Task::none();
        }
        if matches!(message, Message::StopApply) {
            if self.activity == Activity::Apply {
                if let Some(stop) = &self.apply_stop {
                    stop.store(true, Ordering::SeqCst);
                    self.apply_stopping = true;
                }
            }
            return Task::none();
        }
        if let Message::CloseRequested(id) = message {
            if self.activity == Activity::Apply {
                if let Some(stop) = &self.apply_stop {
                    stop.store(true, Ordering::SeqCst);
                    self.apply_stopping = true;
                    self.close_after_apply = Some(id);
                    return Task::none();
                }
            }
            return iced::window::close(id);
        }
        if self.activity != Activity::Idle && !completion {
            return Task::none();
        }
        match message {
            Message::Reload => {
                self.preview = None;
                self.account = None;
                self.notice.clear();
                self.error = None;
                self.last_verified = None;
                self.apply_progress = None;
                self.apply_stop = None;
                self.apply_total = 0;
                self.apply_verified = 0;
                self.apply_write_total = 0;
                self.apply_write_verified = 0;
                self.apply_bar = 0.0;
                self.apply_current.clear();
                self.apply_summary.clear();
                self.apply_stopping = false;
                self.needs_recheck = false;
                self.close_after_apply = None;
                self.activity = Activity::Setup;
                let engine = self.engine.clone();
                return Task::perform(async move { engine.load().await }, Message::Loaded);
            }
            Message::Loaded(result) => {
                if self.activity != Activity::Setup {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                self.setup.busy = false;
                match result {
                    Ok(loaded) => {
                        self.account = loaded.account.clone();
                        self.setup.state = Some(loaded.state);
                        self.setup.error = None;
                        self.monday = self.monday();
                        if let Some(account) = loaded.account {
                            let engine = self.engine.clone();
                            let path = account.state_path.clone();
                            let lookup_path = path.clone();
                            let zone = account.planning.timezone;
                            return Task::perform(
                                async move { (lookup_path, engine.last_verified(path, zone).await) },
                                |(path, label)| Message::VerifiedLoaded(path, label),
                            );
                        }
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Message::VerifiedLoaded(path, label) => {
                if self
                    .account
                    .as_ref()
                    .is_some_and(|account| account.state_path == path)
                {
                    self.last_verified = label;
                }
            }
            Message::Setup(setup::Message::Link(link)) => self.setup.link = link,
            Message::Setup(setup::Message::Key(key)) => self.setup.key = key,
            Message::Setup(setup::Message::Connect) => {
                self.invalidate();
                self.activity = Activity::Setup;
                self.setup.busy = true;
                self.setup.error = None;
                let engine = self.engine.clone();
                let (link, key) = (self.setup.link.clone(), self.setup.key.clone());
                return Task::perform(
                    async move { engine.connect(link, key).await },
                    Message::SetupUpdated,
                );
            }
            Message::Setup(setup::Message::Action(action, params)) => {
                self.invalidate();
                self.activity = Activity::Setup;
                self.setup.busy = true;
                self.setup.error = None;
                let engine = self.engine.clone();
                return Task::perform(
                    async move { engine.act(action, params).await },
                    Message::SetupUpdated,
                );
            }
            Message::SetupUpdated(result) => {
                if self.activity != Activity::Setup {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                self.setup.busy = false;
                match result {
                    Ok(state) => {
                        // The link and key are only needed until they are stored.
                        self.setup.link.clear();
                        self.setup.key.clear();
                        let confirmed = state.stage == "ready";
                        self.setup.state = Some(state);
                        self.setup.error = None;
                        if confirmed {
                            return self.update(Message::Reload);
                        }
                    }
                    Err(error) => self.setup.error = Some(error),
                }
            }
            Message::Navigate(days) => {
                if let Some(date) = self
                    .monday
                    .checked_add_signed(Duration::days(days))
                    .filter(|date| date.checked_add_signed(Duration::days(7)).is_some())
                {
                    self.monday = date;
                    self.invalidate();
                }
            }
            Message::Current => {
                self.monday = self.monday();
                self.invalidate();
            }
            Message::Login(service) => {
                self.invalidate();
                self.activity = Activity::Login;
                let engine = self.engine.clone();
                return Task::perform(
                    async move { engine.login(service).await },
                    Message::LoginOpened,
                );
            }
            Message::LoginOpened(result) => {
                if self.activity != Activity::Login {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                match result {Ok(())=>self.notice="Gennemfør login i browseren. Åbn din vagtplan i MitHF, og vælg derefter Kontrollér login.".into(),Err(e)=>self.error=Some(e)}
            }
            Message::CheckLogin => {
                self.error = None;
                self.notice.clear();
                self.activity = Activity::Login;
                let engine = self.engine.clone();
                return Task::perform(async move { engine.check().await }, Message::LoginChecked);
            }
            Message::LoginChecked(result) => {
                if self.activity != Activity::Login {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                match result {
                    Ok(()) => {
                        self.notice =
                            "MitHF og DUOS er forbundet. Du kan hente ugens ændringer.".into()
                    }
                    Err(e) => {
                        self.preview = None;
                        self.error = Some(e);
                    }
                }
            }
            Message::Capture => {
                let Some(account) = self.account.clone() else {
                    return Task::none();
                };
                self.error = None;
                self.notice.clear();
                self.activity = Activity::Capture;
                let engine = self.engine.clone();
                let from = self.monday;
                return Task::perform(
                    async move { engine.shapes(account, from).await },
                    Message::Captured,
                );
            }
            Message::Captured(result) => {
                if self.activity != Activity::Capture {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                match result {
                    Ok(path) => {
                        self.notice = format!(
                            "Tjenestediagnostik gemt i {}. Den beskriver kun felttyper og indeholder ingen navne, vagter eller kontooplysninger.",
                            path.display()
                        )
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Message::Preview => {
                let Some(account) = self.account.clone() else {
                    return Task::none();
                };
                self.invalidate();
                self.activity = Activity::Preview;
                let engine = self.engine.clone();
                let from = self.monday;
                return Task::perform(
                    async move { engine.preview(account, from).await.map(Box::new) },
                    Message::PreviewLoaded,
                );
            }
            Message::PreviewLoaded(result) => {
                if self.activity != Activity::Preview {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                match result {
                    Ok(preview) if preview.from == self.monday => self.preview = Some(*preview),
                    Ok(_) => {}
                    Err(e) => self.error = Some(e),
                }
            }
            Message::Apply => {
                let Some(preview) = self
                    .preview
                    .as_ref()
                    .filter(|p| p.week.can_apply && p.from == self.monday && !p.digest.is_empty())
                    .cloned()
                else {
                    return Task::none();
                };
                self.apply_summary = preview.week.apply_summary.clone();
                let expected = write_counts(&preview.items);
                self.apply_total = expected.len();
                self.apply_verified = 0;
                self.apply_write_total = expected.values().sum();
                self.apply_write_verified = 0;
                self.apply_bar = 0.0;
                self.apply_current.clear();
                let progress = Arc::new(StdMutex::new(TransferProgress {
                    expected,
                    current: String::new(),
                    uncertain: BTreeSet::new(),
                    verified: Vec::new(),
                }));
                let stop = Arc::new(AtomicBool::new(false));
                self.apply_progress = Some(progress.clone());
                self.apply_stop = Some(stop.clone());
                self.preview = None;
                self.error = None;
                self.notice.clear();
                self.needs_recheck = false;
                self.apply_stopping = false;
                self.activity = Activity::Apply;
                let engine = self.engine.clone();
                return Task::perform(
                    async move { engine.apply(preview, progress, stop).await },
                    Message::Applied,
                );
            }
            Message::Applied(result) => {
                if self.activity != Activity::Apply {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                self.apply_progress = None;
                self.apply_stop = None;
                // Consume approval on success and failure. A retry requires a
                // fresh preview, which can reconcile uncertain persisted steps.
                self.preview = None;
                match result {
                    Ok(done) => {
                        self.apply_verified = done.mithf + done.duos;
                        if done.stopped {
                            self.needs_recheck = true;
                            self.notice = format!(
                                "Overførslen blev stoppet sikkert. {} af {} ændringer er verificeret. Kontrollér igen, før du overfører resten.",
                                self.apply_verified, self.apply_total
                            );
                        } else {
                            self.last_verified = done.verified_label.clone();
                            self.notice = completion_summary(&done);
                        }
                    }
                    Err(failure) => {
                        self.needs_recheck = true;
                        self.notice = failure_summary(&failure);
                        self.error = Some(failure.message);
                    }
                }
                self.apply_stopping = false;
                if let Some(id) = self.close_after_apply.take() {
                    return iced::window::close(id);
                }
            }
            Message::StopApply | Message::ApplyTick | Message::CloseRequested(_) => {}
        }
        Task::none()
    }
    fn invalidate(&mut self) {
        self.preview = None;
        self.error = None;
        self.notice.clear();
        self.needs_recheck = false;
    }
    fn refresh_progress(&mut self) {
        let Some(shared) = self.apply_progress.clone() else {
            return;
        };
        let Ok(state) = shared.lock() else {
            return;
        };
        self.apply_write_verified = state.verified.len();
        self.apply_write_total = state.expected.values().sum();
        // The bar chases the verified count so it glides instead of jumping.
        let target = self.apply_write_verified as f32;
        if target > self.apply_bar {
            let step = (target - self.apply_bar) * 0.3;
            self.apply_bar = if step < 0.02 {
                target
            } else {
                self.apply_bar + step
            };
        } else {
            self.apply_bar = target;
        }
        self.apply_verified = state.verified_units();
        self.apply_current = state.current.clone();
    }
    fn subscription(&self) -> Subscription<Message> {
        let ticks = if self.activity == Activity::Apply {
            iced::time::every(std::time::Duration::from_millis(50)).map(|_| Message::ApplyTick)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            ticks,
            iced::window::close_requests().map(Message::CloseRequested),
        ])
    }
    fn action<'a>(&self, label: &'a str, message: Message) -> iced::widget::Button<'a, Message> {
        button(text(label))
            .padding(12)
            .on_press_maybe((self.activity == Activity::Idle).then_some(message))
    }
    fn view(&self) -> Element<'_, Message> {
        let mut content = column![text("Vagtplanlægning").size(28)]
            .spacing(12)
            .padding(20)
            .max_width(1100);
        if let Some(error) = &self.error {
            content = content.push(text(error));
        }
        if !self.notice.is_empty() {
            content = content.push(text(&self.notice));
        }
        if self.account.is_none() {
            // Both services have to be reachable before the catalog can be
            // read, so login stays available throughout setup.
            content = content
                .push(
                    row![
                        self.action("Log ind i MitHF", Message::Login(Service::Mithf)),
                        self.action("Log ind i DUOS", Message::Login(Service::Duos)),
                        self.action("Kontrollér login", Message::CheckLogin)
                    ]
                    .spacing(8),
                )
                .push(self.setup.view().map(Message::Setup))
                .push(self.action("Genindlæs opsætning", Message::Reload));
        } else {
            content = content
                .push(
                    text(super::widgets::format_week_da(
                        self.monday,
                        self.monday + Duration::days(6),
                    ))
                    .size(20),
                )
                .push(
                    row![
                        self.action("‹ Forrige", Message::Navigate(-7)),
                        self.action("Denne uge", Message::Current),
                        self.action("Næste ›", Message::Navigate(7))
                    ]
                    .spacing(8),
                )
                .push(
                    row![
                        self.action("Log ind i MitHF", Message::Login(Service::Mithf)),
                        self.action("Log ind i DUOS", Message::Login(Service::Duos)),
                        self.action("Kontrollér login", Message::CheckLogin)
                    ]
                    .spacing(8),
                )
                .push(
                    row![
                        self.action("Genindlæs opsætning", Message::Reload),
                        self.action("Gem tjenestediagnostik", Message::Capture)
                    ]
                    .spacing(8),
                );
            if self.activity != Activity::Apply {
                if let Some(stamp) = &self.last_verified {
                    content = content.push(text(format!("Sidst verificeret {stamp}.")));
                }
            }
            if let Some(preview) = &self.preview {
                let week = &preview.week;
                content = content.push(text(&week.headline).size(18));
                if !week.notice.is_empty() {
                    content = content.push(text(&week.notice));
                }
                if !week.attention.is_empty() {
                    content = content.push(text("Kræver opmærksomhed").size(18));
                }
                for item in &week.attention {
                    content = content.push(
                        column![
                            text(format!("{} · {}", item.when, item.who)),
                            text(&item.explanation),
                            text(format!("Gør sådan: {}", item.action))
                        ]
                        .spacing(4),
                    );
                }
                if week.days.iter().any(|d| !d.blocks.is_empty()) {
                    content = content
                        .push(super::widgets::week_grid(week))
                        .push(super::widgets::day_details(week));
                }
                for line in &week.summary {
                    content = content.push(text(line));
                }
                if week.can_apply {
                    content = content
                        .push(text(&week.apply_summary))
                        .push(self.action("Overfør ændringer", Message::Apply));
                } else {
                    content = content
                        .push(text(&week.blocked_reason))
                        .push(button("Overfør ændringer").padding(12));
                }
            }
            content = if self.needs_recheck {
                content.push(self.action("Kontrollér igen", Message::Preview))
            } else {
                content.push(self.action("Se ændringer", Message::Preview))
            };
        }
        if self.activity == Activity::Apply {
            if !self.apply_summary.is_empty() {
                content = content.push(text(&self.apply_summary));
            }
            let bar_total = self.apply_write_total.max(1);
            content = content.push(
                row![
                    progress_bar(0.0..=bar_total as f32, self.apply_bar).girth(16),
                    text(progress_line(self.apply_verified, self.apply_total))
                ]
                .spacing(8),
            );
            if self.apply_current.is_empty() {
                content = content.push(text(
                    "Begynder. Hver ændring læses tilbage, før den næste begynder.",
                ));
            } else {
                content = content.push(text(format!("{}.", self.apply_current)));
            }
            content = content.push(text(
                "Hvis du stopper eller lukker, afslutter appen den igangværende ændring og kontrollerer den, før den standser.",
            ));
            if self.apply_stopping {
                content = content.push(text(
                    "Stopper sikkert efter den igangværende ændring er kontrolleret …",
                ));
            } else {
                content = content.push(
                    button("Stop efter denne ændring")
                        .padding(12)
                        .on_press(Message::StopApply),
                );
            }
        } else {
            let busy = match self.activity {
                Activity::Idle => "",
                Activity::Setup => "Indlæser opsætning …",
                Activity::Login => "Kontakter browseren …",
                Activity::Capture => "Læser tjenesternes svar …",
                Activity::Preview => "Henter ugens ændringer …",
                Activity::Apply => "",
            };
            if !busy.is_empty() {
                content = content.push(text(busy));
            }
        }
        container(scrollable(content))
            .center_x(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

pub fn run() -> iced::Result {
    // Fixture mode must never accidentally turn into a live account workflow.
    if std::env::var_os("TEAMUP_FIXTURE").is_some() {
        eprintln!("The desktop app uses saved live setup. Run fixture previews with the CLI.");
        std::process::exit(2);
    }
    iced::application(NativeApp::new, NativeApp::update, NativeApp::view)
        .subscription(NativeApp::subscription)
        .exit_on_close_request(false)
        .title("Vagtplanlægning")
        .run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup_state(stage: &str) -> setup::SetupState {
        serde_json::from_value(json!({
            "stage": stage, "week_start": "2026-09-14", "calendars": [], "arrangements": [],
            "types": [], "mithf": [], "duos": [], "mappings": [], "arrangement": "",
            "registration_type": "", "account": "", "notice": "", "can_import": false,
            "has_credentials": true,
        }))
        .expect("setup state")
    }

    #[test]
    fn a_stored_connection_clears_the_typed_credentials() {
        let mut app = app();
        app.activity = Activity::Setup;
        app.setup.busy = true;
        app.setup.link = "https://teamup.com/ksAbc123".into();
        app.setup.key = "an-api-key".into();
        let _ = app.update(Message::SetupUpdated(Ok(setup_state("destinations"))));
        // The credentials live in the OS keyring now; nothing keeps a copy.
        assert!(app.setup.link.is_empty());
        assert!(app.setup.key.is_empty());
        assert!(!app.setup.busy);
        assert_eq!(app.activity, Activity::Idle);
    }

    #[test]
    fn a_failed_setup_action_reports_on_the_setup_panel_and_allows_another_try() {
        let mut app = app();
        app.activity = Activity::Setup;
        app.setup.busy = true;
        app.setup.link = "https://teamup.com/ksAbc123".into();
        app.setup.key = "an-api-key".into();
        let _ = app.update(Message::SetupUpdated(Err(
            "Kalenderlinket er ugyldigt.".into()
        )));
        assert_eq!(app.setup.link, "https://teamup.com/ksAbc123");
        assert_eq!(app.setup.key, "an-api-key");
        assert_eq!(
            app.setup.error.as_deref(),
            Some("Kalenderlinket er ugyldigt.")
        );
        assert!(app.error.is_none());
        assert_eq!(app.activity, Activity::Idle);
    }

    #[test]
    fn setup_input_is_ignored_while_an_operation_is_running() {
        let mut app = app();
        app.activity = Activity::Apply;
        let _ = app.update(Message::Setup(setup::Message::Key("typed".into())));
        let _ = app.update(Message::Setup(setup::Message::Action("confirm", json!({}))));
        assert!(app.setup.key.is_empty());
        assert_eq!(app.activity, Activity::Apply);
    }

    fn app() -> NativeApp {
        NativeApp {
            engine: Engine::new("unused-test-directory".into()),
            account: None,
            setup: setup::SetupUi::default(),
            monday: NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(),
            preview: None,
            activity: Activity::Idle,
            notice: String::new(),
            error: None,
            last_verified: None,
            apply_progress: None,
            apply_stop: None,
            apply_total: 0,
            apply_verified: 0,
            apply_write_total: 0,
            apply_write_verified: 0,
            apply_bar: 0.0,
            apply_current: String::new(),
            apply_summary: String::new(),
            apply_stopping: false,
            needs_recheck: false,
            close_after_apply: None,
        }
    }
    fn write_item(source: &str, step: &str) -> PlanItem {
        PlanItem {
            source_key: source.into(),
            system: teamup_shift_sync_core::PlanSystem::Mithf,
            step_key: step.into(),
            outcome: Outcome::WouldCreate,
            summary: String::new(),
            payload: Default::default(),
            destination_id: None,
            reason: String::new(),
        }
    }
    fn preview(app: &NativeApp, can_apply: bool) -> Preview {
        Preview {
            week: Week {
                can_apply,
                apply_summary: "Overfører 1 ny vagt til MitHF.".into(),
                ..Week::default()
            },
            digest: "reviewed-digest".into(),
            from: app.monday,
            state_path: "synthetic-account.sqlite3".into(),
            items: vec![
                write_item("s", "mithf.create_shift"),
                write_item("s", "mithf.assign_helper"),
            ],
        }
    }
    fn done() -> ApplyDone {
        ApplyDone {
            mithf: 1,
            duos: 1,
            verified_label: Some("20. sep kl. 20.00".into()),
            stopped: false,
        }
    }
    #[test]
    fn apply_consumes_approval_and_blocks_changes_until_completion() {
        let mut app = app();
        app.preview = Some(preview(&app, true));
        let _ = app.update(Message::Apply);
        assert_eq!(app.activity, Activity::Apply);
        assert!(app.preview.is_none());
        assert_eq!(app.apply_total, 1);
        assert!(!app.apply_summary.is_empty());
        let monday = app.monday;
        for message in [
            Message::Navigate(7),
            Message::Reload,
            Message::Apply,
            Message::Login(Service::Duos),
            Message::Preview,
        ] {
            let _ = app.update(message);
        }
        assert_eq!(app.monday, monday);
        assert_eq!(app.activity, Activity::Apply);
        let _ = app.update(Message::Applied(Err(ApplyFailure {
            message: "Uafklaret ændring".into(),
            verified: 0,
            uncertain: 1,
            remaining: 0,
        })));
        assert_eq!(app.activity, Activity::Idle);
        assert!(app.preview.is_none());
        assert!(app.error.is_some());
        assert!(app.notice.contains("1 uafklaret"));
        assert!(app.needs_recheck);
        let _ = app.update(Message::Apply);
        assert_eq!(app.activity, Activity::Idle);
        // Widget construction exercises the recoverable failure screen.
        let _ = app.view();
    }
    #[test]
    fn stop_request_waits_for_a_safe_boundary_and_requires_recheck() {
        let mut app = app();
        app.preview = Some(preview(&app, true));
        let _ = app.update(Message::Apply);
        let stop = app.apply_stop.clone().expect("stop token");

        let _ = app.update(Message::StopApply);

        assert!(stop.load(Ordering::SeqCst));
        assert!(app.apply_stopping);
        let _ = app.view();
        let _ = app.update(Message::Applied(Ok(ApplyDone {
            mithf: 1,
            duos: 0,
            verified_label: None,
            stopped: true,
        })));
        assert_eq!(app.activity, Activity::Idle);
        assert!(app.needs_recheck);
        assert!(app.notice.contains("stoppet sikkert"));
        assert!(app.last_verified.is_none());
    }
    #[test]
    fn blocked_and_wrong_week_previews_cannot_start_apply() {
        let mut app = app();
        app.preview = Some(preview(&app, false));
        let _ = app.update(Message::Apply);
        assert_eq!(app.activity, Activity::Idle);
        let mut wrong = preview(&app, true);
        wrong.from += Duration::days(7);
        app.preview = Some(wrong);
        let _ = app.update(Message::Apply);
        assert_eq!(app.activity, Activity::Idle);
        app.preview = Some(preview(&app, true));
        let _ = app.update(Message::Navigate(7));
        assert!(app.preview.is_none());
    }
    #[test]
    fn live_duos_writes_stay_blocked_until_save_semantics_are_verified() {
        let mut week = Week {
            can_apply: true,
            ..Week::default()
        };
        let mut item = write_item("s", "duos.interval:0");
        item.system = PlanSystem::Duos;

        block_unvalidated_duos(&mut week, &[item]);

        assert!(!week.can_apply);
        assert!(week.blocked_reason.contains("verificeres først"));
    }
    #[test]
    fn late_results_cannot_restore_consumed_approval() {
        let mut app = app();
        let old = preview(&app, true);
        app.activity = Activity::Apply;
        let _ = app.update(Message::PreviewLoaded(Ok(Box::new(old.clone()))));
        assert!(app.preview.is_none());
        let _ = app.update(Message::Applied(Ok(done())));
        assert!(app.preview.is_none());
        let _ = app.update(Message::PreviewLoaded(Ok(Box::new(old))));
        assert!(app.preview.is_none());
        assert!(app.notice.contains("verificeret"));
        assert_eq!(app.last_verified.as_deref(), Some("20. sep kl. 20.00"));
        let _ = app.view();
    }
    #[test]
    fn progress_ticks_show_verified_destination_and_count() {
        let mut app = app();
        app.preview = Some(preview(&app, true));
        let _ = app.update(Message::Apply);
        assert_eq!(app.activity, Activity::Apply);
        let shared = app.apply_progress.clone().expect("progress state");
        let unit = ProgressUnit {
            destination: Destination::Mithf,
            source: "s".into(),
            unit: "0".into(),
        };
        {
            let mut progress = shared.lock().unwrap();
            progress.current = "Verificeret: Anna Hansen 28. sep i MitHF".into();
            progress.verified.push(VerifiedStep { unit: unit.clone() });
        }
        let _ = app.update(Message::ApplyTick);
        // One of two writes is verified, so no shift is done yet.
        assert_eq!(app.apply_verified, 0);
        assert_eq!(app.apply_write_verified, 1);
        assert_eq!(app.apply_write_total, 2);
        // The bar glides toward the target instead of jumping to it.
        assert!(app.apply_bar > 0.0 && app.apply_bar < 1.0);
        let gliding = app.apply_bar;
        assert_eq!(
            app.apply_current,
            "Verificeret: Anna Hansen 28. sep i MitHF"
        );
        let _ = app.view();
        shared.lock().unwrap().verified.push(VerifiedStep { unit });
        let _ = app.update(Message::ApplyTick);
        assert_eq!(app.apply_verified, 1);
        assert_eq!(app.apply_write_verified, 2);
        assert!(app.apply_bar > gliding && app.apply_bar < 2.0);
        let _ = app.update(Message::Applied(Ok(done())));
        assert_eq!(app.activity, Activity::Idle);
        assert!(app.apply_progress.is_none());
        assert!(app.notice.contains("MitHF"));
        assert!(app.notice.contains("DUOS"));
        assert!(app.notice.contains("20. sep kl. 20.00"));
    }
    #[test]
    fn step_labels_stay_plain_and_count_by_destination() {
        assert_eq!(Destination::Mithf.name(), "MitHF");
        assert_eq!(Destination::Duos.name(), "DUOS");
        assert_eq!(verb_of(Outcome::WouldCreate), "Tilføjer");
        assert_eq!(verb_of(Outcome::WouldUpdate), "Opdaterer");
        assert_eq!(short_date_da(28, 8), "28. sep");
        assert_eq!(progress_line(1, 2), "1 af 2 ændringer overført.");
        assert!(completion_summary(&done()).contains("Sidst verificeret 20. sep kl. 20.00."));
    }
    #[test]
    fn verification_time_uses_the_configured_timezone() {
        let utc = DateTime::parse_from_rfc3339("2026-09-20T18:00:00+00:00").unwrap();
        assert_eq!(
            format_verified_da(utc, chrono_tz::Europe::Copenhagen),
            "20. sep kl. 20.00"
        );
    }
    #[test]
    fn setup_reload_revokes_preview_and_messages_redact_private_data() {
        let mut app = app();
        app.preview = Some(preview(&app, true));
        let _ = app.update(Message::Reload);
        assert!(app.preview.is_none());
        assert_eq!(app.activity, Activity::Setup);
        let _ = app.view();
        let _ = app.update(Message::Loaded(Err("Ingen gemt opsætning.".into())));
        let _ = app.view();
        assert_eq!(
            format!(
                "{:?}",
                Message::PreviewLoaded(Ok(Box::new(preview(&app, true))))
            ),
            "NativeMessage"
        );
    }
}
