//! Native Iced workflow using the Rust core and app-owned browser sessions.
//! Setup editing remains in the existing application during migration.
use crate::setup;
use chrono::{Datelike, DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Timelike, Utc};
use iced::widget::{button, column, container, progress_bar, row, scrollable, text};
use iced::{Element, Length, Subscription, Task};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};
use teamup_shift_sync_core::{
    apply_plan, build_plan,
    live::{
        load_saved_setup, read_destinations, read_shapes, read_teamup, BrowserSessions, LiveConfig,
        LiveDestinations, Service, Setup, Visibility,
    },
    plan_digest, reconciliation_range, ApplyRequest, Outcome, PlanItem, PlanRequest, SyncState,
};
use teamup_shift_sync_gui::{files::app_data_dir, preview::build_week, protocol::Week};
use tokio::sync::{Mutex, MutexGuard};

type Result<T> = std::result::Result<T, String>;

/// Project the setup document onto what the setup screen may display.
fn view(document: &Setup) -> Result<setup::SetupState> {
    serde_json::from_value(document.view()).map_err(|_| "Opsætningen kunne ikke vises.".to_owned())
}

/// One verified write: its destination and a plain display line naming the
/// helper and date. No payloads, ids, or times beyond the day.
#[derive(Clone, Debug, Default)]
struct TransferProgress {
    expected: BTreeMap<String, usize>,
    verified: Vec<VerifiedStep>,
}

#[derive(Clone, Debug)]
struct VerifiedStep {
    service: String,
    label: String,
    source: String,
}

fn write_counts(items: &[PlanItem]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for item in items.iter().filter(|item| {
        matches!(
            item.outcome,
            Outcome::WouldCreate | Outcome::WouldUpdate
        )
    }) {
        *counts.entry(item.source_key.clone()).or_insert(0) += 1;
    }
    counts
}

/// The verified result of one approved run.
#[derive(Clone, Debug)]
struct ApplyDone {
    mithf: usize,
    duos: usize,
    verified_label: String,
}

fn service_of(step_key: &str) -> String {
    let base = step_key.split('#').next().unwrap_or("");
    match base {
        "mithf.create_shift" | "mithf.assign_helper" | "mithf.set_sps" | "mithf.set_meeting" => {
            "MitHF".into()
        }
        _ if base.starts_with("duos") => "DUOS".into(),
        _ => String::new(),
    }
}

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

fn counted(steps: &[VerifiedStep], destination: &str) -> usize {
    steps
        .iter()
        .filter(|step| step.service == destination)
        .count()
}

/// Short Danish timestamp without seconds. Keeps the stored offset, so the
/// time matches what the services saw.
fn format_verified_da(moment: DateTime<FixedOffset>) -> String {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "maj", "jun", "jul", "aug", "sep", "okt", "nov", "dec",
    ];
    format!(
        "{}. {} kl. {:02}.{:02}",
        moment.day(),
        MONTHS[moment.month0() as usize],
        moment.hour(),
        moment.minute()
    )
}

fn progress_line(verified: usize, total: usize) -> String {
    if total == 0 {
        "Overfører.".into()
    } else {
        format!("{verified} af {total} vagter overført.")
    }
}

fn completion_summary(done: &ApplyDone) -> String {
    let mut parts = Vec::new();
    if done.mithf > 0 {
        parts.push(format!("{} trin i MitHF", done.mithf));
    }
    if done.duos > 0 {
        parts.push(format!("{} trin i DUOS", done.duos));
    }
    let what = if parts.is_empty() {
        "Ingen nye trin.".to_owned()
    } else {
        parts.join(" og ") + " er læst tilbage og verificeret."
    };
    format!("Færdig. {what} Sidst verificeret {}.", done.verified_label)
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
            let week = build_week(
                &account.planning,
                &account.helper_names,
                &account.helper_colors,
                &shifts,
                &plan,
                &destination,
                true,
            )
            .map_err(str::to_owned)?;
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
    /// Forget local step records for explicitly named sources, so deleted
    /// destination entries can be transferred again after a fresh preview.
    /// Only local history is forgotten. Nothing is deleted in any service.
    async fn forget_sources(&self, state_path: PathBuf, sources: Vec<String>) -> Result<usize> {
        tokio::task::spawn_blocking(move || {
            let mut state = SyncState::open(&state_path)
                .map_err(|_| "Den lokale overførselshistorik kunne ikke læses.".to_owned())?;
            let _guard = state.exclusive_apply().map_err(|_| {
                "En anden overførsel bruger denne konto. Vent, og prøv igen.".to_owned()
            })?;
            let mut count = 0;
            for source in sources {
                count += state
                    .forget_steps(&source)
                    .map_err(|_| "Historikken kunne ikke glemmes. Prøv igen.".to_owned())?
                    .len();
            }
            Ok(count)
        })
        .await
        .map_err(|_| "Historikken kunne ikke glemmes. Prøv igen.".to_owned())?
    }
    /// The last fully verified transfer, if any. Snapshots are only stored
    /// after the final read-back, so this never reports a partial attempt.
    /// Failures return None. The home screen then simply shows no timestamp.
    async fn last_verified(&self, state_path: PathBuf) -> Option<String> {        tokio::task::spawn_blocking(move || {
            let state = SyncState::open(&state_path).ok()?;
            let moment = state.last_source_snapshot_at().ok()??;
            Some(format_verified_da(moment))
        })
        .await
        .ok()?
    }
    async fn apply(
        &self,
        approved: Preview,
        progress: Arc<StdMutex<TransferProgress>>,
    ) -> Result<ApplyDone> {
        // Re-read saved setup as well as TeamUp. A changed account scope must
        // never inherit an approval, even when its week happens to look alike.
        let account = self.account().await?;
        if account.state_path != approved.state_path {
            return Err(
                "Opsætningen er ændret. Genindlæs opsætningen, og gennemgå ugen igen.".into(),
            );
        }
        let guard = self.sessions().await?;
        let browser = guard.as_ref().ok_or("Log ind i MitHF og DUOS først.")?;
        let (to, start, end) = range(&account, approved.from)?;
        let shifts = read_teamup(&account, approved.from, to)
            .await
            .map_err(|e| e.to_string())?;
        let now = Utc::now().fixed_offset();
        let mut destination = LiveDestinations::connect(browser, &account, now)
            .await
            .map_err(|e| e.to_string())?;
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
            context.insert(
                shift.key(),
                (
                    helper,
                    short_date_da(at.day(), at.month0()),
                ),
            );
        }
        let mut labels = BTreeMap::new();
        for item in &approved.items {
            let service = service_of(&item.step_key);
            if service.is_empty() {
                continue;
            }
            if let Some((helper, date)) = context.get(&item.source_key) {
                labels.insert(
                    (item.source_key.clone(), item.step_key.clone()),
                    (
                        service.clone(),
                        format!(
                            "{} {helper} {date} i {service}",
                            verb_of(item.outcome)
                        ),
                    ),
                );
            }
        }
        apply_plan(ApplyRequest {config:account.planning.clone(),shifts,range_start:start,range_end:end,now,expected_digest:approved.digest},account.state_path.clone(),&mut destination, |item: &PlanItem|{
            if let Ok(mut state) = progress.lock() {
                let key = (item.source_key.clone(), item.step_key.clone());
                match labels.get(&key) {
                    Some((service, label)) => state.verified.push(VerifiedStep {
                        service: service.clone(),
                        label: label.clone(),
                        source: item.source_key.clone(),
                    }),
                    None => state.verified.push(VerifiedStep {
                        service: service_of(&item.step_key),
                        label: String::new(),
                        source: item.source_key.clone(),
                    }),
                }
            }
        }).await
            .map_err(|e| match e {
                teamup_shift_sync_core::TransferError::Approval(teamup_shift_sync_core::ApprovalError::PlanChanged) => "Ugen er ændret. Hent ændringerne igen, og gennemgå dem før overførsel.".to_owned(),
                teamup_shift_sync_core::TransferError::State(teamup_shift_sync_core::StateError::ApplyInProgress) => "En anden overførsel bruger denne konto. Vent, og hent ugen igen.".to_owned(),
                _ => "Overførslen kunne ikke afsluttes og verificeres. Nogle trin kan være gemt. Hent ugen igen, før du prøver at overføre mere.".to_owned(),
            })?;
        let steps = progress
            .lock()
            .map(|state| state.verified.clone())
            .unwrap_or_default();
        Ok(ApplyDone {
            mithf: counted(&steps, "MitHF"),
            duos: counted(&steps, "DUOS"),
            verified_label: format_verified_da(Utc::now().fixed_offset()),
        })
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
    VerifiedLoaded(Option<String>),
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
    Forget,
    Forgotten(Result<usize>),
    Apply,
    ApplyTick,
    Applied(Result<ApplyDone>),
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
    Forget,
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
    apply_total: usize,
    apply_verified: usize,
    apply_write_total: usize,
    apply_write_verified: usize,
    apply_bar: f32,
    apply_current: String,
    apply_summary: String,
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
            apply_total: 0,
            apply_verified: 0,
            apply_write_total: 0,
            apply_write_verified: 0,
            apply_bar: 0.0,
            apply_current: String::new(),
            apply_summary: String::new(),
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
                | Message::VerifiedLoaded(_)
                | Message::SetupUpdated(_)
                | Message::LoginOpened(_)
                | Message::LoginChecked(_)
                | Message::Captured(_)
                | Message::PreviewLoaded(_)
                | Message::Forgotten(_)
                | Message::Applied(_)
        );
        // Progress ticks only run during a transfer. They never start work.
        if matches!(message, Message::ApplyTick) {
            if self.activity == Activity::Apply {
                self.refresh_progress();
            }
            return Task::none();
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
                self.apply_total = 0;
                self.apply_verified = 0;
                self.apply_write_total = 0;
                self.apply_write_verified = 0;
                self.apply_bar = 0.0;
                self.apply_current.clear();
                self.apply_summary.clear();
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
                            return Task::perform(
                                async move { engine.last_verified(path).await },
                                Message::VerifiedLoaded,
                            );
                        }
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Message::VerifiedLoaded(label) => {
                if self.account.is_some() {
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
            Message::Forget => {
                let (state_path, sources) = match self.preview.as_ref() {
                    Some(preview) => {
                        let sources: BTreeSet<String> = preview
                            .items
                            .iter()
                            .filter(|item| item.reason == "destination_missing")
                            .map(|item| item.source_key.clone())
                            .collect();
                        (preview.state_path.clone(), sources)
                    }
                    None => return Task::none(),
                };
                if sources.is_empty() {
                    return Task::none();
                }
                self.preview = None;
                self.error = None;
                self.notice.clear();
                self.activity = Activity::Forget;
                let engine = self.engine.clone();
                let sources: Vec<String> = sources.into_iter().collect();
                return Task::perform(
                    async move { engine.forget_sources(state_path, sources).await },
                    Message::Forgotten,
                );
            }
            Message::Forgotten(result) => {
                if self.activity != Activity::Forget {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                // A fresh preview re-reads destinations, so anything recreated
                // elsewhere reconciles before another approval can happen.
                self.preview = None;
                match result {
                    Ok(count) => {
                        self.notice = format!(
                            "Tillader overførsel igen for {count} gemte trin. Hent ugen igen, og gennemgå den før overførsel."
                        )
                    }
                    Err(error) => self.error = Some(error),
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
                    verified: Vec::new(),
                }));
                self.apply_progress = Some(progress.clone());
                self.preview = None;
                self.error = None;
                self.notice.clear();
                self.activity = Activity::Apply;
                let engine = self.engine.clone();
                return Task::perform(
                    async move { engine.apply(preview, progress).await },
                    Message::Applied,
                );
            }
            Message::Applied(result) => {
                if self.activity != Activity::Apply {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                self.apply_progress = None;
                // Consume approval on success and failure. A retry requires a
                // fresh preview, which can reconcile uncertain persisted steps.
                self.preview = None;
                match result {
                    Ok(done) => {
                        self.apply_verified = done.mithf + done.duos;
                        self.last_verified = Some(done.verified_label.clone());
                        self.notice = completion_summary(&done);
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Message::ApplyTick => {}
        }
        Task::none()
    }
    fn invalidate(&mut self) {
        self.preview = None;
        self.error = None;
        self.notice.clear();
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
            self.apply_bar = if step < 0.02 { target } else { self.apply_bar + step };
        } else {
            self.apply_bar = target;
        }
        self.apply_verified = state
            .expected
            .iter()
            .filter(|(source, total)| {
                state
                    .verified
                    .iter()
                    .filter(|step| &step.source == *source)
                    .count()
                    >= **total
            })
            .count();
        self.apply_current = state
            .verified
            .last()
            .map(|step| {
                if step.label.is_empty() {
                    if step.service.is_empty() {
                        "Verificeret".into()
                    } else {
                        format!("Verificeret i {}", step.service)
                    }
                } else {
                    step.label.clone()
                }
            })
            .unwrap_or_default();
    }
    fn subscription(&self) -> Subscription<Message> {
        if self.activity == Activity::Apply {
            iced::time::every(std::time::Duration::from_millis(50)).map(|_| Message::ApplyTick)
        } else {
            Subscription::none()
        }
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
                let missing: BTreeSet<&str> = preview
                    .items
                    .iter()
                    .filter(|item| item.reason == "destination_missing")
                    .map(|item| item.source_key.as_str())
                    .collect();
                if !missing.is_empty() {
                    content = content.push(text(
                        "Nogle tidligere overførte vagter mangler i destinationen. Appen genskaber dem ikke af sig selv.",
                    ));
                    content = content.push(text(
                        "Tillad overførsel igen glemmer kun den lokale historik. Intet slettes i destinationerne.",
                    ));
                    content = content.push(self.action("Tillad overførsel igen", Message::Forget));
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
            content = content.push(self.action("Se ændringer", Message::Preview));
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
                "Lad appen være åben, til alt er verificeret. Det, der allerede er verificeret, gemmes.",
            ));
        } else {
            let busy = match self.activity {
                Activity::Idle => "",
                Activity::Setup => "Indlæser opsætning …",
                Activity::Login => "Kontakter browseren …",
                Activity::Capture => "Læser tjenesternes svar …",
                Activity::Preview => "Henter ugens ændringer …",
                Activity::Forget => "Glemmer lokal historik …",
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
            apply_total: 0,
            apply_verified: 0,
            apply_write_total: 0,
            apply_write_verified: 0,
            apply_bar: 0.0,
            apply_current: String::new(),
            apply_summary: String::new(),
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
            verified_label: "20. sep kl. 20.00".into(),
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
        let _ = app.update(Message::Applied(Err("Uncertain write".into())));
        assert_eq!(app.activity, Activity::Idle);
        assert!(app.preview.is_none());
        assert!(app.error.is_some());
        let _ = app.update(Message::Apply);
        assert_eq!(app.activity, Activity::Idle);
        // Widget construction exercises the recoverable failure screen.
        let _ = app.view();
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
    fn forget_without_deleted_sources_does_nothing() {
        let mut pending = app();
        pending.preview = Some(preview(&pending, true));
        let _ = pending.update(Message::Forget);
        assert_eq!(pending.activity, Activity::Idle);
        assert!(pending.preview.is_some());
        let mut fresh = app();
        let _ = fresh.update(Message::Forgotten(Ok(1)));
        assert!(fresh.notice.is_empty());
    }
    fn missing_preview(app: &NativeApp) -> Preview {
        let mut preview = preview(app, false);
        preview.items.push(PlanItem {
            source_key: "cal:ev:oc".into(),
            system: teamup_shift_sync_core::PlanSystem::Mithf,
            step_key: "mithf.create_shift".into(),
            outcome: Outcome::Conflicted,
            summary: String::new(),
            payload: Default::default(),
            destination_id: Some("gone".into()),
            reason: "destination_missing".into(),
        });
        preview
    }
    #[test]
    fn retransfer_forgets_only_deleted_sources_and_needs_a_fresh_preview() {
        let mut app = app();
        app.preview = Some(missing_preview(&app));
        let _ = app.view();
        let _ = app.update(Message::Forget);
        assert_eq!(app.activity, Activity::Forget);
        assert!(app.preview.is_none());
        let _ = app.update(Message::Applied(Ok(done())));
        assert_eq!(app.activity, Activity::Forget);
        let _ = app.update(Message::Forgotten(Ok(2)));
        assert_eq!(app.activity, Activity::Idle);
        assert!(app.preview.is_none());
        assert!(app.notice.contains("Hent ugen igen"));
        let _ = app.view();
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
        shared.lock().unwrap().verified.push(VerifiedStep {
            service: "MitHF".into(),
            label: "Tilføjer Anna Hansen 28. sep i MitHF".into(),
            source: "s".into(),
        });
        let _ = app.update(Message::ApplyTick);
        // One of two writes is verified, so no shift is done yet.
        assert_eq!(app.apply_verified, 0);
        assert_eq!(app.apply_write_verified, 1);
        assert_eq!(app.apply_write_total, 2);
        // The bar glides toward the target instead of jumping to it.
        assert!(app.apply_bar > 0.0 && app.apply_bar < 1.0);
        let gliding = app.apply_bar;
        assert_eq!(app.apply_current, "Tilføjer Anna Hansen 28. sep i MitHF");
        let _ = app.view();
        shared.lock().unwrap().verified.push(VerifiedStep {
            service: "MitHF".into(),
            label: "Tilføjer Anna Hansen 28. sep i MitHF".into(),
            source: "s".into(),
        });
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
        assert_eq!(service_of("mithf.create_shift"), "MitHF");
        assert_eq!(service_of("duos.interval:3"), "DUOS");
        assert_eq!(verb_of(Outcome::WouldCreate), "Tilføjer");
        assert_eq!(verb_of(Outcome::WouldUpdate), "Opdaterer");
        assert_eq!(short_date_da(28, 8), "28. sep");
        let steps = vec![
            VerifiedStep {
                service: "MitHF".into(),
                label: "Tilføjer Anna 28. sep i MitHF".into(),
                source: "s".into(),
            },
            VerifiedStep {
                service: "DUOS".into(),
                label: "Tilføjer Anna 28. sep i DUOS".into(),
                source: "s".into(),
            },
        ];
        assert_eq!(counted(&steps, "MitHF"), 1);
        assert_eq!(counted(&steps, "DUOS"), 1);
        assert_eq!(progress_line(1, 2), "1 af 2 vagter overført.");
        assert!(completion_summary(&done()).contains("Sidst verificeret 20. sep kl. 20.00."));
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
