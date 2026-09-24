//! Native Iced workflow using the Rust core and app-owned browser sessions.
//! Setup editing remains in the existing application during migration.
use crate::{setup, template, update};
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Timelike, Utc};
use iced::widget::{
    button, column, container, progress_bar, row, scrollable, space, text, tooltip, Column,
};
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
        app_version, forget_login, load_saved_setup, read_shapes, read_source, read_week,
        redacted_report, BrowserSessions, LiveConfig, LiveDestinations, Service, Setup, Visibility,
    },
    plan_digest, ApplyOutcome, ApplyRequest, Outcome, PlanItem, PlanRequest, PlanSystem, SyncState,
    TransferEvent, TransferOperation,
};
use teamup_shift_sync_gui::{
    files::app_data_dir,
    preview::build_week,
    protocol::{Notice, Tone, Week},
};
use tokio::sync::{Mutex, MutexGuard};

type Result<T> = std::result::Result<T, String>;

/// Project the setup document onto what the setup screen may display.
fn view(document: &Setup) -> Result<setup::SetupState> {
    let mut state: setup::SetupState = serde_json::from_value(document.view())
        .map_err(|_| "Opsætningen kunne ikke vises.".to_owned())?;
    // Confirmation runs by itself once the helper choices are valid, so an
    // invalid choice is the one thing standing between setup and the week.
    if state.stage != "ready" && !state.mappings.is_empty() {
        state.blocked = document
            .validate_choices()
            .err()
            .map(|error| error.0.to_owned());
    }
    Ok(state)
}

/// The state after a source check, carrying what the check found.
fn checked(document: &Setup, found: &str) -> Result<setup::SetupState> {
    let mut state = view(document)?;
    state.result = Some(Notice::from_message(Tone::Info, found));
    Ok(state)
}

/// The state after an action that saved its progress but then failed.
fn reported(document: &Setup, error: &str) -> Result<setup::SetupState> {
    let mut state = view(document)?;
    state.result = Some(Notice::from_message(Tone::Error, error));
    Ok(state)
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
    /// The known cause, or empty when there is nothing more specific to say
    /// than that the transfer did not finish.
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

fn failure_notice(failure: &ApplyFailure) -> Notice {
    let mut detail = failure.message.clone();
    if !detail.is_empty() {
        detail.push(' ');
    }
    detail.push_str(&format!(
        "{} verificeret, {} uafklaret og {} ikke startet.",
        failure.verified, failure.uncertain, failure.remaining
    ));
    if failure.uncertain > 0 {
        detail.push_str(" En uafklaret ændring kan allerede være gemt.");
    }
    Notice::new(Tone::Error, "Overførslen blev ikke færdig", detail)
}

fn progress_line(verified: usize, total: usize) -> String {
    if total == 0 {
        "Overfører.".into()
    } else {
        format!("{verified} af {total} ændringer overført.")
    }
}

/// What a week fetch is doing right now. `Engine::preview` moves through
/// these in order, so both the bar and the line on screen follow real work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum FetchStage {
    #[default]
    Sessions,
    Reading,
    Planning,
}

/// The three stages are the whole fetch, so they are also the bar's scale.
const FETCH_STAGES: f32 = 3.0;

impl FetchStage {
    /// Stages completed before this one: where the bar stands when it begins.
    fn done(self) -> f32 {
        match self {
            FetchStage::Sessions => 0.0,
            FetchStage::Reading => 1.0,
            FetchStage::Planning => 2.0,
        }
    }
    fn label(self) -> &'static str {
        match self {
            FetchStage::Sessions => "Åbner tjenester",
            FetchStage::Reading => "Læser vagtplan og tjenester",
            FetchStage::Planning => "Beregner ugens ændringer",
        }
    }
}

/// How long a stage takes to creep most of the way across its own section.
/// A read that finishes quickly barely creeps; a slow one keeps moving.
const FETCH_CREEP_SECONDS: f32 = 10.0;

/// A running fetch, as the screen shows it. The task owns the same `stage`
/// and sets it as it goes; the rest is what the last tick read.
struct Fetch {
    stage: Arc<StdMutex<FetchStage>>,
    started: std::time::Instant,
    /// When `shown` last changed, so the creep restarts with every stage.
    entered: std::time::Instant,
    shown: FetchStage,
    seconds: u64,
}

impl Fetch {
    fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            stage: Arc::new(StdMutex::new(FetchStage::default())),
            started: now,
            entered: now,
            shown: FetchStage::default(),
            seconds: 0,
        }
    }
    /// Where the bar stands, on a scale of one unit per stage. Finishing a
    /// stage is the only thing that moves it a whole step; inside a stage it
    /// creeps across that stage's own section, slower the longer the stage
    /// lasts, and never into the next one. So the bar says which step is
    /// running, and it keeps moving while a read is slow.
    fn bar(&self) -> f32 {
        let within = 1.0 - (-self.entered.elapsed().as_secs_f32() / FETCH_CREEP_SECONDS).exp();
        self.shown.done() + 0.9 * within
    }
    /// The elapsed seconds keep counting inside a long read, so a slow MitHF
    /// still looks like work in progress rather than a frozen window.
    fn line(&self) -> String {
        format!("{} … {} s", self.shown.label(), self.seconds)
    }
}

fn mark(stage: &StdMutex<FetchStage>, value: FetchStage) {
    if let Ok(mut current) = stage.lock() {
        *current = value;
    }
}

/// A bar with its own explanation beside it. The bar keeps a fixed width so
/// the label always has room: full-width bars pushed it off the screen edge.
fn moving_bar<'a>(value: f32, end: f32, label: String) -> Element<'a, Message> {
    row![
        progress_bar(0.0..=end, value)
            .girth(10)
            .length(Length::Fixed(240.0)),
        text(label)
    ]
    .spacing(12)
    .align_y(iced::alignment::Vertical::Center)
    .width(Length::Fill)
    .into()
}

/// Every counted change was read back and verified before it counted. The
/// week already shows when the last transfer was, so this does not.
fn completion_notice(done: &ApplyDone) -> Notice {
    let counted = |count: usize, one: &str, many: &str, to: &str| {
        let noun = if count == 1 { one } else { many };
        format!("{count} {noun} overført til {to}")
    };
    let mut parts = Vec::new();
    if done.mithf > 0 {
        parts.push(counted(done.mithf, "vagt", "vagter", "MitHF"));
    }
    if done.duos > 0 {
        parts.push(counted(done.duos, "registrering", "registreringer", "DUOS"));
    }
    if parts.is_empty() {
        Notice::new(Tone::Success, "Ingen ændringer overført", "")
    } else {
        Notice::new(Tone::Success, parts.join(" og "), "")
    }
}

/// Where a report can be shared. The repository is public, which the Support
/// screen says before anything is opened.
const ISSUE_FORM: &str = "https://github.com/Jdreioe/BPA_Overblik/issues/new";

/// The saved report, and whether a browser accepted the prefilled issue.
#[derive(Clone, Debug)]
struct Shared {
    path: PathBuf,
    opened: bool,
}

/// Percent-encode one query value. Only the unreserved set survives, which is
/// always safe inside a query string.
fn query_encoded(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// A prefilled issue the person can read and send themselves. A long report is
/// left out of the address, which browsers and GitHub both limit; the saved
/// file can be attached in the form instead.
fn issue_url(report: &Value, path: &std::path::Path) -> String {
    const BUDGET: usize = 6000;
    const INTRO: &str = "Skriv kort, hvad du gjorde, og hvad der skete:\n\n\n";
    let details = serde_json::to_string_pretty(report).unwrap_or_default();
    let mut body = format!("{INTRO}Oplysninger fra appen:\n\n```json\n{details}\n```\n");
    let title = query_encoded("Der gik noget galt i BPA Overblik");
    if ISSUE_FORM.len() + title.len() + query_encoded(&body).len() > BUDGET {
        body = format!(
            "{INTRO}Vedhæft filen {} fra din computer.\n",
            path.display()
        );
    }
    format!("{ISSUE_FORM}?title={title}&body={}", query_encoded(&body))
}

/// Open an address in the person's own browser. The app's own browser profiles
/// belong to MitHF and DUOS and are never used for anything else.
fn open_in_browser(url: &str) -> bool {
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        // The empty argument is the window title `start` expects first.
        command.args(["/C", "start", "", url]);
        command
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
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
        let found = document
            .connect(&link, &api_key)
            .await
            .map_err(|e| e.to_string())?;
        checked(document, &found)
    }
    async fn connect_sheets(
        &self,
        link: String,
        layout: teamup_shift_sync_core::sheets::SheetLayout,
    ) -> Result<setup::SetupState> {
        let mut guard = self.setup.lock().await;
        let document = guard.as_mut().ok_or("Opsætningen er ikke indlæst.")?;
        let found = document
            .connect_sheets(&link, layout)
            .await
            .map_err(|e| e.to_string())?;
        checked(document, &found)
    }
    /// Apply one setup action. `discover` and `confirm` read both services, so
    /// they take the browser sessions first; the lock order is always browsers
    /// before the document.
    async fn act(&self, action: &'static str, params: Value) -> Result<setup::SetupState> {
        if action == "edit" {
            {
                let mut guard = self.setup.lock().await;
                let document = guard.as_mut().ok_or("Opsætningen er ikke indlæst.")?;
                document.edit(&params).map_err(|e| e.to_string())?;
            }
            return self.confirm_if_valid().await;
        }
        if action == "auto_confirm" {
            return self.confirm_if_valid().await;
        }
        if matches!(action, "discover" | "confirm") {
            let sessions = self.sessions().await?;
            let browser = sessions.as_ref().ok_or("Log ind i MitHF og DUOS først.")?;
            let mut guard = self.setup.lock().await;
            let document = guard.as_mut().ok_or("Opsætningen er ikke indlæst.")?;
            if action == "discover" {
                document
                    .discover(browser, params["arrangement"].as_str())
                    .await
                    .map_err(|e| e.to_string())?;
                if document.validate_choices().is_ok() {
                    if let Err(error) = document.confirm(browser).await {
                        return reported(document, &error.to_string());
                    }
                }
                return view(document);
            } else {
                document.confirm(browser).await.map_err(|e| e.to_string())?;
            }
            return view(document);
        }
        let mut guard = self.setup.lock().await;
        let document = guard.as_mut().ok_or("Opsætningen er ikke indlæst.")?;
        if action == "retry_source" {
            let found = document.refresh_source().await.map_err(|e| e.to_string())?;
            return checked(document, &found);
        }
        match action {
            "status" => Ok(()),
            "source" => document.edit_source(),
            "choose_source" => document.choose_source(params["source"].as_str().unwrap_or("")),
            "duos_enabled" => document.choose_duos(params["enabled"].as_bool().unwrap_or(true)),
            "markers" => serde_json::from_value::<Vec<String>>(params)
                .map_err(|_| {
                    teamup_shift_sync_core::live::LiveError("Markeringerne kunne ikke læses.")
                })
                .and_then(|titles| document.set_markers(&titles)),
            "standard_times" => serde_json::from_value(params)
                .map_err(|_| {
                    teamup_shift_sync_core::live::LiveError("Standardtiderne kunne ikke læses.")
                })
                .and_then(|standard| document.set_standard_times(standard)),
            _ => Err(teamup_shift_sync_core::live::LiveError(
                "Handlingen findes ikke.",
            )),
        }
        .map_err(|e| e.to_string())?;
        view(document)
    }

    async fn confirm_if_valid(&self) -> Result<setup::SetupState> {
        {
            let guard = self.setup.lock().await;
            let document = guard.as_ref().ok_or("Opsætningen er ikke indlæst.")?;
            if document.validate_choices().is_err() {
                return view(document);
            }
        }
        let sessions = match self.sessions().await {
            Ok(sessions) => sessions,
            Err(error) => {
                let guard = self.setup.lock().await;
                return reported(
                    guard.as_ref().ok_or("Opsætningen er ikke indlæst.")?,
                    &error,
                );
            }
        };
        let browser = sessions.as_ref().ok_or("Log ind i MitHF og DUOS først.")?;
        let mut guard = self.setup.lock().await;
        let document = guard.as_mut().ok_or("Opsætningen er ikke indlæst.")?;
        if let Err(error) = document.confirm(browser).await {
            return reported(document, &error.to_string());
        }
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
    /// Forget the saved MitHF and DUOS logins, so a different account cannot
    /// inherit another one's session. Sync history is keyed by account and
    /// stays; nothing is changed in either service.
    async fn forget_login(&self, service: Service) -> Result<()> {
        {
            let mut guard = self.browsers.lock().await;
            if let Some(sessions) = guard.as_mut() {
                sessions.close(service);
            }
        }
        let dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || forget_login(&dir, service))
            .await
            .map_err(|_| "Det gemte login kunne ikke fjernes.".to_owned())?
            .map_err(|error| error.to_string())
    }
    /// Reading only needs the saved profile, so no browser window is opened.
    /// A window left over from login keeps serving these requests.
    async fn sessions(&self) -> Result<MutexGuard<'_, Option<BrowserSessions>>> {
        let duos_enabled = self
            .setup
            .lock()
            .await
            .as_ref()
            .map(Setup::duos_enabled)
            .unwrap_or(true);
        let mut services = vec![Service::Mithf];
        if duos_enabled {
            services.push(Service::Duos);
        }
        self.sessions_for(&services).await
    }
    async fn sessions_for(
        &self,
        services: &[Service],
    ) -> Result<MutexGuard<'_, Option<BrowserSessions>>> {
        let mut guard = self.prepared().await?;
        {
            let browser = guard.as_mut().ok_or("Browseren kunne ikke forberedes.")?;
            for service in services {
                browser
                    .open(*service, Visibility::Background)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(guard)
    }
    async fn check_service(&self, service: Service) -> Result<()> {
        let guard = self.sessions_for(&[service]).await?;
        let browser = guard.as_ref().ok_or("Log ind først.")?;
        browser.check(service).await.map_err(|e| e.to_string())
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
        let path = self.data_dir.join("fejlrapport-tjenester.json");
        let target = path.clone();
        tokio::task::spawn_blocking(move || {
            serde_json::to_string_pretty(&recorded)
                .map_err(|_| "Fejlrapporten kunne ikke skrives.".to_owned())
                .and_then(|document| {
                    std::fs::write(&target, document + "\n")
                        .map_err(|_| "Fejlrapporten kunne ikke gemmes.".to_owned())
                })
        })
        .await
        .map_err(|_| "Fejlrapporten kunne ikke gemmes.".to_owned())??;
        Ok(path)
    }
    /// Save the redacted report and open a prefilled issue in the person's own
    /// browser. Nothing is published here: they read the text and decide to
    /// send it. It needs no service and no confirmed account, so it also works
    /// when setup is stuck.
    async fn share_problem(&self) -> Result<Shared> {
        let dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            let now = Utc::now().fixed_offset();
            let report = redacted_report(&dir, now).map_err(|e| e.to_string())?;
            let path = dir.join("fejlrapport.json");
            let document = serde_json::to_string_pretty(&report)
                .map_err(|_| "Fejlrapporten kunne ikke skrives.".to_owned())?;
            std::fs::write(&path, document + "\n")
                .map_err(|_| "Fejlrapporten kunne ikke gemmes.".to_owned())?;
            let opened = open_in_browser(&issue_url(&report, &path));
            Ok(Shared { path, opened })
        })
        .await
        .map_err(|_| "Fejlrapporten kunne ikke gemmes.".to_owned())?
    }
    async fn preview(
        &self,
        account: Arc<LiveConfig>,
        from: NaiveDate,
        stage: Arc<StdMutex<FetchStage>>,
    ) -> Result<Preview> {
        let guard = self.sessions().await?;
        let browser = guard.as_ref().ok_or("Log ind i MitHF og DUOS først.")?;
        let (to, start, end) = range(&account, from)?;
        mark(&stage, FetchStage::Reading);
        let now = Utc::now().fixed_offset();
        let (source, destination) = read_week(browser, &account, from, to, start, end, now)
            .await
            .map_err(|e| e.to_string())?;
        mark(&stage, FetchStage::Planning);
        tokio::task::spawn_blocking(move || {
            let state = SyncState::open(&account.state_path)
                .map_err(|_| "Den lokale overførselshistorik kunne ikke læses.".to_owned())?;
            let plan = build_plan(
                &PlanRequest {
                    config: &account.planning,
                    shifts: &source.shifts,
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
                &source.shifts,
                &source.markers,
                &plan,
                &destination,
                true,
            )
            .map_err(str::to_owned)?;
            let digest = plan_digest(&plan)
                .map_err(|_| "Ugens godkendelse kunne ikke beregnes.".to_owned())?;
            Ok(Preview {
                week,
                status_dismissed: false,
                digest,
                from,
                state_path: account.state_path.clone(),
                standard_times: account.standard_times.clone(),
                markers: account.markers.clone(),
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
    /// Forget this app's own local sync records for one shift, so a fresh
    /// preview may propose the transfer again. Nothing is deleted in MitHF or
    /// DUOS, and the apply lock keeps this out of a running transfer.
    async fn allow_retransfer(
        &self,
        account: Arc<LiveConfig>,
        source_key: String,
    ) -> Result<usize> {
        tokio::task::spawn_blocking(move || {
            let mut state = SyncState::open(&account.state_path)
                .map_err(|_| "Den lokale overførselshistorik kunne ikke åbnes.".to_owned())?;
            let _guard = state.exclusive_apply().map_err(|error| match error {
                teamup_shift_sync_core::StateError::ApplyInProgress => {
                    "En overførsel bruger denne konto. Vent, til den er færdig.".to_owned()
                }
                _ => "Den lokale overførselshistorik kunne ikke låses.".to_owned(),
            })?;
            state
                .forget_steps(&source_key)
                .map(|keys| keys.len())
                .map_err(|_| "De lokale registreringer kunne ikke glemmes.".to_owned())
        })
        .await
        .map_err(|_| "De lokale registreringer kunne ikke glemmes.".to_owned())?
    }
    async fn apply(
        &self,
        approved: Preview,
        progress: Arc<StdMutex<TransferProgress>>,
        stop: Arc<AtomicBool>,
    ) -> ApplyResult {
        // Re-read saved setup and the selected source. A changed account scope must
        // never inherit an approval, even when its week happens to look alike.
        let account = self
            .account()
            .await
            .map_err(|message| apply_failure(message, &progress))?;
        if account.state_path != approved.state_path
            || account.standard_times != approved.standard_times
            || account.markers != approved.markers
        {
            return Err(apply_failure(
                "Opsætningen er ændret. Genstart appen, og gennemgå ugen igen.".into(),
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
        let shifts = read_source(&account, approved.from, to)
            .await
            .map_err(|error| apply_failure(error.to_string(), &progress))?
            .shifts;
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
                _ => String::new(),
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
    /// The week's status notice was dismissed. A new preview shows it again.
    status_dismissed: bool,
    digest: String,
    from: NaiveDate,
    state_path: PathBuf,
    standard_times: teamup_shift_sync_core::standard_time::StandardTimes,
    /// The marker titles this preview was built with. Changing them can turn
    /// an event into a shift or back, so it revokes approval like standard times.
    markers: Vec<String>,
    items: Vec<PlanItem>,
}

#[derive(Clone)]
enum Message {
    Reload,
    Loaded(Result<Loaded>),
    VerifiedLoaded(PathBuf, Option<String>),
    Setup(setup::Message),
    SetupUpdated(Result<setup::SetupState>),
    AccountUpdated(Result<Arc<LiveConfig>>),
    Open(Screen),
    DismissStatus,
    DismissWeekStatus,
    SelectSettings(SettingsSection),
    AutosaveStandard(u64),
    Navigate(i64),
    Current,
    Login(Service),
    LoginOpened(Result<()>),
    CheckLogin(Service),
    LoginChecked(Service, Result<()>),
    ForgetLogins(Service),
    LoginsForgotten(Service, Result<()>),
    Capture,
    Captured(Result<PathBuf>),
    ShareProblem,
    ProblemShared(Result<Shared>),
    Preview,
    PreviewLoaded(Result<Box<Preview>>),
    AllowRetransfer(String),
    CancelRetransfer,
    ConfirmRetransfer,
    Forgotten(Result<usize>),
    Apply,
    StopApply,
    ApplyTick,
    Applied(ApplyResult),
    CloseRequested(iced::window::Id),
    CheckUpdates,
    PeriodicUpdateCheck,
    UpdateChecked(Result<Option<update::Offer>>),
    InstallUpdate,
    UpdateTick,
    UpdateApplied(Result<update::ApplyOutcome>),
    RestartUpdate,
}
// Events may carry account configuration or private shift data.
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeMessage")
    }
}
/// Moving between screens only changes what is shown, so it stays possible
/// while an operation runs. Opening Indstillinger revokes a shown approval,
/// which is always safe, and its automatic helper fetch waits for idle.
fn is_navigation(message: &Message) -> bool {
    matches!(message, Message::Open(_) | Message::SelectSettings(_))
}

/// Show the selected source's week and plan.
const SHOW_WEEK: &str = "Se vagtplan";

/// Short Danish guidance for the Support screen.
const HELP: [&str; 5] = [
    "1. Log ind i MitHF og eventuelt DUOS under Udbydere. Login holder, indtil tjenesten selv logger dig ud.",
    "2. Vælg ugen på forsiden, og vælg Se vagtplan.",
    "3. Løs først punkterne under Kræver opmærksomhed. Rettelser laves i kilden eller i tjenesten, ikke i appen.",
    "4. Vælg Godkend ændringer. Hver ændring læses tilbage og bekræftes, før den næste begynder.",
    "Appen sletter aldrig noget i MitHF eller DUOS, og den godkender ikke registreringer for hjælperen.",
];

/// The screen in view. Everything technical lives away from the week.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Home,
    Settings,
    Help,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingsSection {
    Helpers,
    StandardTimes,
    Markers,
    Integrations,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Activity {
    Idle,
    Setup,
    /// A local setup edit. It blocks other operations like any activity but is
    /// not shown, because it finishes before a status would be readable.
    Save,
    Login,
    Capture,
    Preview,
    Recover,
    Apply,
    Update,
}

struct NativeApp {
    engine: Engine,
    screen: Screen,
    settings_section: SettingsSection,
    standard_revision: u64,
    standard_saved_revision: u64,
    account: Option<Arc<LiveConfig>>,
    setup: setup::SetupUi,
    monday: NaiveDate,
    preview: Option<Preview>,
    activity: Activity,
    /// The last action's result or error, until the next action or dismissal.
    status: Option<Notice>,
    last_verified: Option<String>,
    apply_progress: Option<Arc<StdMutex<TransferProgress>>>,
    apply_stop: Option<Arc<AtomicBool>>,
    apply_total: usize,
    apply_verified: usize,
    apply_write_total: usize,
    apply_write_verified: usize,
    apply_bar: f32,
    /// Set for exactly as long as a week fetch runs.
    fetch: Option<Fetch>,
    apply_current: String,
    apply_summary: String,
    apply_stopping: bool,
    needs_recheck: bool,
    /// The shift whose local records the user has been asked to confirm
    /// forgetting. Set only from that shift's own conflict.
    forget_source: Option<String>,
    close_after_apply: Option<iced::window::Id>,
    update_offer: Option<update::Offer>,
    update_ready: Option<update::ApplyOutcome>,
    update_manual: bool,
    update_frame: usize,
    helpers_auto_fetch_attempted: bool,
}

impl NativeApp {
    /// Silent GitHub check while the window stays open. Startup already ran one.
    const UPDATE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

    fn new() -> (Self, Task<Message>) {
        let mut app = Self {
            engine: Engine::new(app_data_dir()),
            screen: Screen::Home,
            settings_section: SettingsSection::Helpers,
            standard_revision: 0,
            standard_saved_revision: 0,
            account: None,
            setup: setup::SetupUi::default(),
            monday: Utc::now().date_naive(),
            preview: None,
            activity: Activity::Idle,
            status: None,
            last_verified: None,
            apply_progress: None,
            apply_stop: None,
            apply_total: 0,
            apply_verified: 0,
            apply_write_total: 0,
            apply_write_verified: 0,
            apply_bar: 0.0,
            fetch: None,
            apply_current: String::new(),
            apply_summary: String::new(),
            apply_stopping: false,
            needs_recheck: false,
            forget_source: None,
            close_after_apply: None,
            update_offer: None,
            update_ready: None,
            update_manual: false,
            update_frame: 0,
            helpers_auto_fetch_attempted: false,
        };
        update::cleanup_replaced_backup();
        let reload = app.update(Message::Reload);
        (
            app,
            Task::batch([
                reload,
                Task::perform(update::check_latest(), Message::UpdateChecked),
            ]),
        )
    }
    fn monday(&self) -> NaiveDate {
        let today = self
            .account
            .as_ref()
            .map(|c| Utc::now().with_timezone(&c.planning.timezone).date_naive())
            .unwrap_or_else(|| Utc::now().date_naive());
        today - Duration::days(today.weekday().num_days_from_monday().into())
    }
    fn accept_standard(&mut self, state: &setup::SetupState) {
        if self.standard_revision == self.standard_saved_revision
            || self.setup.standard_times() == state.standard_times
        {
            self.setup.load_standard(state);
            self.standard_saved_revision = self.standard_revision;
        }
    }
    fn refresh_helpers_if_needed(&mut self) -> Task<Message> {
        if !self.helpers_auto_fetch_attempted
            && self.activity == Activity::Idle
            && self
                .setup
                .state
                .as_ref()
                .is_some_and(|state| state.stage != "source" && state.mappings.is_empty())
        {
            self.helpers_auto_fetch_attempted = true;
            return self.update(Message::Setup(setup::Message::Action(
                "discover",
                serde_json::json!({}),
            )));
        }
        Task::none()
    }
    fn update(&mut self, message: Message) -> Task<Message> {
        // One operation at a time. In particular, navigation/setup/login cannot
        // change the selected account or approval while an apply is running.
        let completion = matches!(
            message,
            Message::Loaded(_)
                | Message::VerifiedLoaded(..)
                | Message::SetupUpdated(_)
                | Message::AccountUpdated(_)
                | Message::LoginOpened(_)
                | Message::LoginChecked(_, _)
                | Message::LoginsForgotten(_, _)
                | Message::Captured(_)
                | Message::ProblemShared(_)
                | Message::PreviewLoaded(_)
                | Message::Forgotten(_)
                | Message::Applied(_)
                | Message::UpdateChecked(_)
                | Message::UpdateApplied(_)
                | Message::AutosaveStandard(_)
        );
        if matches!(message, Message::UpdateTick) {
            if self.activity == Activity::Update {
                self.update_frame = (self.update_frame + 1) % 8;
            }
            return Task::none();
        }
        // Ticks only refresh what a running transfer or fetch shows. They
        // never start work.
        if matches!(message, Message::ApplyTick) {
            match self.activity {
                Activity::Apply => self.refresh_progress(),
                Activity::Preview => self.refresh_fetch(),
                _ => {}
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
        // Opening the TeamUp key form uses the person's own browser and
        // touches neither the setup document nor the MitHF/DUOS profiles, so
        // it is safe at any time, including mid-transfer.
        if matches!(message, Message::Setup(setup::Message::OpenTeamupKeys)) {
            open_in_browser(setup::TEAMUP_KEYS_URL);
            return Task::none();
        }
        // Dismissing only hides text, so it is safe at any time.
        if matches!(message, Message::DismissStatus) {
            self.status = None;
            return Task::none();
        }
        if matches!(message, Message::DismissWeekStatus) {
            if let Some(preview) = &mut self.preview {
                preview.status_dismissed = true;
            }
            return Task::none();
        }
        if matches!(message, Message::Setup(setup::Message::CopyOrganization)) {
            return iced::clipboard::write(setup::TEAMUP_ORG_SUGGESTION.to_owned());
        }
        if matches!(message, Message::Setup(setup::Message::CopyPurpose)) {
            return iced::clipboard::write(setup::TEAMUP_PURPOSE_SUGGESTION.to_owned());
        }
        let standard_input_during_save = self.activity == Activity::Save
            && matches!(
                message,
                Message::Setup(setup::Message::StandardDefault(_))
                    | Message::Setup(setup::Message::StandardDay(_, _))
            );
        if self.activity != Activity::Idle
            && !completion
            && !standard_input_during_save
            && !is_navigation(&message)
        {
            return Task::none();
        }
        match message {
            Message::Reload => {
                self.preview = None;
                self.account = None;
                self.status = None;
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
                self.forget_source = None;
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
                match result {
                    Ok(loaded) => {
                        self.account = loaded.account.clone();
                        self.accept_standard(&loaded.state);
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
                    Err(error) => self.status = Some(Notice::from_message(Tone::Error, &error)),
                }
                if self.visible_screen() == Screen::Settings
                    && self.settings_section == SettingsSection::Helpers
                {
                    return self.refresh_helpers_if_needed();
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
            Message::Setup(setup::Message::MarkerDraft(value)) => self.setup.marker_draft = value,
            Message::Setup(setup::Message::AddMarker | setup::Message::RemoveMarker(_)) => {
                let Some(state) = &self.setup.state else {
                    return Task::none();
                };
                let mut titles = state.markers.clone();
                match message {
                    Message::Setup(setup::Message::RemoveMarker(index)) if index < titles.len() => {
                        titles.remove(index);
                    }
                    Message::Setup(setup::Message::AddMarker) => {
                        titles.push(std::mem::take(&mut self.setup.marker_draft));
                    }
                    _ => return Task::none(),
                }
                return self.update(Message::Setup(setup::Message::Action(
                    "markers",
                    serde_json::json!(titles),
                )));
            }
            Message::Setup(setup::Message::Key(key)) => self.setup.key = key,
            Message::Setup(setup::Message::StandardDefault(value)) => {
                self.setup.standard_default = value;
                self.setup.error = None;
                self.standard_revision += 1;
                let revision = self.standard_revision;
                return Task::perform(
                    async move {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        revision
                    },
                    Message::AutosaveStandard,
                );
            }
            Message::Setup(setup::Message::StandardDay(index, value)) => {
                if index < 7 {
                    self.setup.standard_days[index] = value;
                    self.setup.error = None;
                    self.standard_revision += 1;
                    let revision = self.standard_revision;
                    return Task::perform(
                        async move {
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            revision
                        },
                        Message::AutosaveStandard,
                    );
                }
            }
            Message::AutosaveStandard(revision) => {
                if revision != self.standard_revision || self.setup.state.is_none() {
                    return Task::none();
                }
                if self.activity != Activity::Idle {
                    return Task::perform(
                        async move {
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                            revision
                        },
                        Message::AutosaveStandard,
                    );
                }
                let standard = self.setup.standard_times();
                if let Err(error) = standard.validate() {
                    self.setup.error = Some(error.into());
                    return Task::none();
                }
                if self
                    .setup
                    .state
                    .as_ref()
                    .is_some_and(|state| state.standard_times == standard)
                {
                    return Task::none();
                }
                let params = serde_json::to_value(self.setup.standard_times())
                    .expect("standard times serialize");
                return self.update(Message::Setup(setup::Message::Action(
                    "standard_times",
                    params,
                )));
            }
            Message::Setup(setup::Message::Template(template::Message::Paste)) => {
                return iced::clipboard::read().map(|clipboard| {
                    Message::Setup(setup::Message::Template(template::Message::Pasted(
                        clipboard,
                    )))
                });
            }
            Message::Setup(setup::Message::Template(message)) => {
                self.setup.template.update(message);
            }
            Message::Setup(setup::Message::ToggleEdit(source)) => {
                self.setup.toggle_edit(&source);
            }
            Message::Setup(setup::Message::ToggleProvider(id)) => {
                self.setup.toggle_provider(&id);
            }
            Message::Setup(setup::Message::Noop) => {}
            Message::Setup(setup::Message::Login(service)) => {
                return self.update(Message::Login(service));
            }
            Message::Setup(setup::Message::CheckLogin(service)) => {
                return self.update(Message::CheckLogin(service));
            }
            Message::Setup(setup::Message::ForgetLogin(service)) => {
                return self.update(Message::ForgetLogins(service));
            }
            Message::Setup(setup::Message::OpenTeamupKeys) => {
                // Already handled before the busy guard; kept for exhaustiveness.
            }
            Message::Setup(setup::Message::CopyOrganization) => {
                // Already handled before the busy guard; kept for exhaustiveness.
            }
            Message::DismissStatus | Message::DismissWeekStatus => {
                // Already handled before the busy guard; kept for exhaustiveness.
            }
            Message::Setup(setup::Message::CopyPurpose) => {
                // Already handled before the busy guard; kept for exhaustiveness.
            }
            Message::Setup(setup::Message::Connect) => {
                self.invalidate();
                self.activity = Activity::Setup;
                let engine = self.engine.clone();
                let (link, key) = (self.setup.link.clone(), self.setup.key.clone());
                return Task::perform(
                    async move { engine.connect(link, key).await },
                    Message::SetupUpdated,
                );
            }
            Message::Setup(setup::Message::ConnectSheets) => {
                let layout = match self.setup.sheet_layout() {
                    Ok(layout) => layout,
                    Err(error) => {
                        self.status = Some(Notice::from_message(Tone::Error, &error));
                        return Task::none();
                    }
                };
                self.invalidate();
                self.activity = Activity::Setup;
                let engine = self.engine.clone();
                let link = self.setup.link.clone();
                return Task::perform(
                    async move { engine.connect_sheets(link, layout).await },
                    Message::SetupUpdated,
                );
            }
            Message::Setup(setup::Message::Action(action, params)) => {
                if matches!(action, "source" | "choose_source") {
                    self.helpers_auto_fetch_attempted = false;
                }
                // Settings has no preview to revoke, and a local edit should
                // leave the rest of the screen alone.
                if matches!(
                    action,
                    "edit" | "duos_enabled" | "standard_times" | "markers"
                ) {
                    self.activity = Activity::Save;
                } else {
                    self.invalidate();
                    self.activity = Activity::Setup;
                }
                let engine = self.engine.clone();
                return Task::perform(
                    async move { engine.act(action, params).await },
                    Message::SetupUpdated,
                );
            }
            Message::SetupUpdated(result) => {
                if !matches!(self.activity, Activity::Setup | Activity::Save) {
                    return Task::none();
                }
                let was_save = self.activity == Activity::Save;
                self.activity = Activity::Idle;
                match result {
                    Ok(mut state) => {
                        // What the action found is an app status, not setup.
                        let found = state.result.take();
                        if found.is_some() {
                            self.status = found.clone();
                        }
                        // The link and key are only needed until they are stored.
                        self.setup.link.clear();
                        self.setup.key.clear();
                        let confirmed = state.stage == "ready";
                        if !confirmed {
                            self.account = None;
                            self.preview = None;
                        }
                        self.accept_standard(&state);
                        self.setup.state = Some(state);
                        self.setup.error = None;
                        if confirmed {
                            if was_save {
                                self.preview = None;
                                self.needs_recheck = false;
                                self.activity = Activity::Save;
                                let engine = self.engine.clone();
                                return Task::perform(
                                    async move { engine.account().await },
                                    Message::AccountUpdated,
                                );
                            }
                            // Reloading clears the status, so restore what
                            // this action found once the reload has started.
                            let reload = self.update(Message::Reload);
                            self.status = found;
                            return reload;
                        }
                    }
                    Err(error) => self.status = Some(Notice::from_message(Tone::Error, &error)),
                }
            }
            Message::AccountUpdated(result) => {
                if self.activity != Activity::Save {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                match result {
                    Ok(account) => {
                        let changed_path = self
                            .account
                            .as_ref()
                            .is_none_or(|old| old.state_path != account.state_path);
                        self.account = Some(account.clone());
                        if changed_path {
                            self.last_verified = None;
                            let engine = self.engine.clone();
                            let path = account.state_path.clone();
                            let zone = account.planning.timezone;
                            return Task::perform(
                                async move { (path.clone(), engine.last_verified(path, zone).await) },
                                |(path, label)| Message::VerifiedLoaded(path, label),
                            );
                        }
                    }
                    Err(error) => {
                        self.account = None;
                        self.status = Some(Notice::from_message(Tone::Error, &error));
                    }
                }
            }
            Message::Open(screen) => {
                // Settings may change the account, so no approval survives the
                // trip. Any message that sent the user here is kept.
                if screen == Screen::Settings {
                    self.preview = None;
                    self.forget_source = None;
                    self.settings_section = SettingsSection::Helpers;
                }
                self.screen = screen;
                if screen == Screen::Settings {
                    return self.refresh_helpers_if_needed();
                }
            }
            Message::SelectSettings(section) => {
                self.settings_section = section;
                if section == SettingsSection::Helpers {
                    return self.refresh_helpers_if_needed();
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
                match result {
                    Ok(()) => {
                        self.status = Some(Notice::new(
                            Tone::Info,
                            "Browseren er åbnet",
                            "Log ind, og vælg derefter Check forbindelse.",
                        ))
                    }
                    Err(e) => self.status = Some(Notice::from_message(Tone::Error, &e)),
                }
            }
            Message::CheckLogin(service) => {
                self.status = None;
                self.activity = Activity::Login;
                let engine = self.engine.clone();
                return Task::perform(
                    async move { engine.check_service(service).await },
                    move |result| Message::LoginChecked(service, result),
                );
            }
            Message::LoginChecked(service, result) => {
                if self.activity != Activity::Login {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                match result {
                    Ok(()) => {
                        self.helpers_auto_fetch_attempted = false;
                        self.status = Some(Notice::new(
                            Tone::Success,
                            format!("{} er forbundet", service.name()),
                            "",
                        ));
                        if self.settings_section == SettingsSection::Helpers {
                            if self
                                .setup
                                .state
                                .as_ref()
                                .is_some_and(|state| state.mappings.is_empty())
                            {
                                return self.refresh_helpers_if_needed();
                            }
                            if self
                                .setup
                                .state
                                .as_ref()
                                .is_some_and(|state| state.stage == "helpers")
                            {
                                return self.update(Message::Setup(setup::Message::Action(
                                    "auto_confirm",
                                    serde_json::json!({}),
                                )));
                            }
                        }
                    }
                    Err(e) => {
                        self.preview = None;
                        self.status = Some(Notice::from_message(Tone::Error, &e));
                    }
                }
            }
            Message::ForgetLogins(service) => {
                self.invalidate();
                self.activity = Activity::Login;
                let engine = self.engine.clone();
                return Task::perform(
                    async move { engine.forget_login(service).await },
                    move |result| Message::LoginsForgotten(service, result),
                );
            }
            Message::LoginsForgotten(service, result) => {
                if self.activity != Activity::Login {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                match result {
                    Ok(()) => {
                        self.status = Some(Notice::new(
                            Tone::Info,
                            format!("Logget ud af {}", service.name()),
                            "Log ind igen for at fortsætte.",
                        ))
                    }
                    Err(e) => self.status = Some(Notice::from_message(Tone::Error, &e)),
                }
            }
            Message::Capture => {
                let Some(account) = self.account.clone() else {
                    return Task::none();
                };
                self.status = None;
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
                        self.status = Some(Notice::new(
                            Tone::Success,
                            "Fejlrapporten er gemt",
                            format!(
                                "Den ligger i {}. Den beskriver kun, hvilke felter MitHF og DUOS sender, og indeholder ingen navne, vagter eller kontooplysninger.",
                                path.display()
                            ),
                        ))
                    }
                    Err(e) => self.status = Some(Notice::from_message(Tone::Error, &e)),
                }
            }
            Message::ShareProblem => {
                self.status = None;
                self.activity = Activity::Capture;
                let engine = self.engine.clone();
                return Task::perform(
                    async move { engine.share_problem().await },
                    Message::ProblemShared,
                );
            }
            Message::ProblemShared(result) => {
                if self.activity != Activity::Capture {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                match result {
                    Ok(shared) if shared.opened => {
                        self.status = Some(Notice::new(
                            Tone::Success,
                            "Opslaget er åbnet i din browser",
                            format!(
                                "Læs det igennem, og send det selv. Oplysningerne er også gemt i {}, hvis du hellere vil sende filen.",
                                shared.path.display()
                            ),
                        ))
                    }
                    Ok(shared) => {
                        self.status = Some(Notice::new(
                            Tone::Warning,
                            "Browseren kunne ikke åbnes",
                            format!(
                                "Oplysningerne er gemt i {}. Send filen til den, der vedligeholder appen.",
                                shared.path.display()
                            ),
                        ))
                    }
                    Err(e) => self.status = Some(Notice::from_message(Tone::Error, &e)),
                }
            }
            Message::Preview => {
                let Some(account) = self.account.clone() else {
                    return Task::none();
                };
                self.invalidate();
                self.activity = Activity::Preview;
                let fetch = Fetch::new();
                let stage = fetch.stage.clone();
                self.fetch = Some(fetch);
                let engine = self.engine.clone();
                let from = self.monday;
                return Task::perform(
                    async move { engine.preview(account, from, stage).await.map(Box::new) },
                    Message::PreviewLoaded,
                );
            }
            Message::PreviewLoaded(result) => {
                if self.activity != Activity::Preview {
                    return Task::none();
                }
                self.fetch = None;
                self.activity = Activity::Idle;
                match result {
                    Ok(preview) if preview.from == self.monday => self.preview = Some(*preview),
                    Ok(_) => {}
                    Err(e) => self.status = Some(Notice::from_message(Tone::Error, &e)),
                }
            }
            Message::AllowRetransfer(source_key) => {
                // Only the conflict this shift produced may offer recovery.
                let offered = self.preview.as_ref().is_some_and(|preview| {
                    preview
                        .week
                        .attention
                        .iter()
                        .any(|item| item.can_allow_retransfer && item.source_key == source_key)
                });
                if offered {
                    self.status = None;
                    self.forget_source = Some(source_key);
                }
            }
            Message::CancelRetransfer => self.forget_source = None,
            Message::ConfirmRetransfer => {
                let (Some(account), Some(source_key)) =
                    (self.account.clone(), self.forget_source.clone())
                else {
                    return Task::none();
                };
                self.status = None;
                self.activity = Activity::Recover;
                let engine = self.engine.clone();
                return Task::perform(
                    async move { engine.allow_retransfer(account, source_key).await },
                    Message::Forgotten,
                );
            }
            Message::Forgotten(result) => {
                if self.activity != Activity::Recover {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                self.forget_source = None;
                match result {
                    Ok(count) => {
                        // The approval is gone with the records it was built on.
                        self.preview = None;
                        self.needs_recheck = true;
                        self.status = Some(if count == 0 {
                            Notice::new(
                                Tone::Info,
                                "Der var ingen lokale registreringer for vagten",
                                "Vælg Kontrollér igen, og gennemgå ugen.",
                            )
                        } else {
                            Notice::new(
                                Tone::Success,
                                "Lokale registreringer er glemt",
                                "Intet er slettet i MitHF eller DUOS. Vælg Kontrollér igen, og godkend ugen på ny.",
                            )
                        });
                    }
                    Err(error) => self.status = Some(Notice::from_message(Tone::Error, &error)),
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
                self.status = None;
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
                            self.status = Some(Notice::new(
                                Tone::Warning,
                                "Overførslen er stoppet sikkert",
                                format!(
                                    "{} af {} ændringer er overført. Vælg Kontrollér igen, før du overfører resten.",
                                    self.apply_verified, self.apply_total
                                ),
                            ));
                        } else {
                            self.last_verified = done.verified_label.clone();
                            self.status = Some(completion_notice(&done));
                        }
                    }
                    Err(failure) => {
                        self.needs_recheck = true;
                        self.status = Some(failure_notice(&failure));
                    }
                }
                self.apply_stopping = false;
                if let Some(id) = self.close_after_apply.take() {
                    return iced::window::close(id);
                }
            }
            Message::StopApply | Message::ApplyTick | Message::CloseRequested(_) => {}
            Message::CheckUpdates => {
                if self.update_manual || self.update_ready.is_some() || self.update_offer.is_some()
                {
                    return Task::none();
                }
                self.status = None;
                self.update_manual = true;
                return Task::perform(update::check_latest(), Message::UpdateChecked);
            }
            Message::PeriodicUpdateCheck => {
                if self.update_offer.is_some() || self.update_ready.is_some() || self.update_manual
                {
                    return Task::none();
                }
                return Task::perform(update::check_latest(), Message::UpdateChecked);
            }
            Message::UpdateChecked(result) => {
                let manual = std::mem::take(&mut self.update_manual);
                if self.update_ready.is_some() {
                    return Task::none();
                }
                match result {
                    Ok(Some(offer)) => {
                        self.update_offer = Some(offer);
                    }
                    Ok(None) => {
                        self.update_offer = None;
                        if manual {
                            self.status = Some(Notice::new(
                                Tone::Success,
                                "Du har den nyeste version",
                                format!("Version {}.", app_version()),
                            ));
                        }
                    }
                    Err(error) => {
                        if manual {
                            self.status = Some(Notice::from_message(Tone::Error, &error));
                        }
                    }
                }
            }
            Message::InstallUpdate => {
                let Some(offer) = self.update_offer.clone() else {
                    return Task::none();
                };
                self.status = None;
                self.activity = Activity::Update;
                self.update_frame = 0;
                return Task::perform(
                    async move { update::apply(offer).await },
                    Message::UpdateApplied,
                );
            }
            Message::UpdateTick => {}
            Message::UpdateApplied(result) => {
                if self.activity != Activity::Update {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                match result {
                    Ok(ready) => {
                        self.update_offer = None;
                        self.status = Some(Notice::new(
                            Tone::Success,
                            "Opdateringen er hentet",
                            match ready {
                                update::ApplyOutcome::Restart(_) => {
                                    "Genstart appen fra ikonet i sidepanelet."
                                }
                                update::ApplyOutcome::OpenInstaller(_) => {
                                    "Åbn installationsprogrammet fra ikonet i sidepanelet."
                                }
                            },
                        ));
                        self.update_ready = Some(ready);
                    }
                    Err(error) => self.status = Some(Notice::from_message(Tone::Error, &error)),
                }
            }
            Message::RestartUpdate => {
                let Some(ready) = self.update_ready.as_ref() else {
                    return Task::none();
                };
                match ready {
                    update::ApplyOutcome::Restart(path) => match update::restart(path) {
                        Ok(()) => return iced::exit(),
                        Err(error) => self.status = Some(Notice::from_message(Tone::Error, &error)),
                    },
                    update::ApplyOutcome::OpenInstaller(path) => {
                        #[cfg(target_os = "macos")]
                        match update::open_installer(path) {
                            Ok(()) => {
                                self.update_ready = None;
                                self.status = Some(Notice::new(
                                    Tone::Info,
                                    "Installationsprogrammet er åbnet",
                                    "Følg trinnene, og åbn BPA Overblik igen bagefter.",
                                ));
                            }
                            Err(error) => {
                                self.status = Some(Notice::from_message(Tone::Error, &error))
                            }
                        }
                        #[cfg(not(target_os = "macos"))]
                        let _ = path;
                    }
                }
            }
        }
        Task::none()
    }
    fn invalidate(&mut self) {
        self.preview = None;
        self.status = None;
        self.needs_recheck = false;
        self.forget_source = None;
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
    fn refresh_fetch(&mut self) {
        let Some(fetch) = &mut self.fetch else {
            return;
        };
        if let Ok(stage) = fetch.stage.lock() {
            if *stage != fetch.shown {
                fetch.shown = *stage;
                fetch.entered = std::time::Instant::now();
            }
        }
        fetch.seconds = fetch.started.elapsed().as_secs();
    }
    fn subscription(&self) -> Subscription<Message> {
        // The transfer bar glides, so it needs frames; the fetch line only
        // counts whole seconds.
        let ticks = match self.activity {
            Activity::Apply => Some(std::time::Duration::from_millis(50)),
            Activity::Preview => Some(std::time::Duration::from_millis(250)),
            _ => None,
        };
        let ticks = match ticks {
            Some(every) => iced::time::every(every).map(|_| Message::ApplyTick),
            None => Subscription::none(),
        };
        Subscription::batch([
            ticks,
            if self.activity == Activity::Update {
                iced::time::every(std::time::Duration::from_millis(120))
                    .map(|_| Message::UpdateTick)
            } else {
                Subscription::none()
            },
            iced::window::close_requests().map(Message::CloseRequested),
            iced::time::every(Self::UPDATE_CHECK_INTERVAL).map(|_| Message::PeriodicUpdateCheck),
        ])
    }
    fn helpers_blocked(&self) -> bool {
        self.setup
            .state
            .as_ref()
            .is_some_and(|state| state.blocked.is_some())
    }
    /// Whether a button sending `message` can be pressed now.
    fn enabled(&self, message: &Message) -> bool {
        self.buttons_enabled() || is_navigation(message)
    }
    /// A local save is too short to show, so buttons keep their look. A press
    /// during it is still ignored by the one-operation guard in `update`.
    fn buttons_enabled(&self) -> bool {
        matches!(self.activity, Activity::Idle | Activity::Save)
    }
    fn action<'a>(&self, label: &'a str, message: Message) -> iced::widget::Button<'a, Message> {
        button(text(label))
            .style(iced::widget::button::secondary)
            .padding([8, 12])
            .on_press_maybe(self.enabled(&message).then_some(message))
    }
    /// Outlined chrome: clearly a button, but not a second primary.
    fn quiet<'a>(&self, label: &'a str, message: Message) -> iced::widget::Button<'a, Message> {
        button(text(label).size(14))
            .style(super::widgets::outlined)
            .padding([7, 12])
            .on_press_maybe(self.enabled(&message).then_some(message))
    }
    /// The single filled action on a screen.
    fn primary<'a>(&self, label: &'a str, message: Message) -> iced::widget::Button<'a, Message> {
        button(text(label))
            .style(iced::widget::button::primary)
            .padding([8, 14])
            .on_press_maybe(self.enabled(&message).then_some(message))
    }
    fn view(&self) -> Element<'_, Message> {
        // No app-name headline here: the window title already says
        // BPA Overblik, and each screen brings its own heading.
        let mut page = column![]
            .spacing(12)
            .padding(20)
            .width(Length::Fill)
            .height(Length::Fill)
            .max_width(1100);
        page = match self.visible_screen() {
            Screen::Home => self.home(page),
            Screen::Settings => page.push(self.settings()),
            Screen::Help => page.push(
                scrollable(self.help(column![].spacing(12)))
                    .height(Length::Fill)
                    .width(Length::Fill),
            ),
        };
        if self.activity == Activity::Apply {
            page = self.apply_status(page);
        }
        if self.visible_screen() == Screen::Home {
            page = page.push(
                row![
                    tooltip(
                        self.circular_icon("⚙", Message::Open(Screen::Settings)),
                        "Indstillinger",
                        tooltip::Position::Top,
                    ),
                    tooltip(
                        self.circular_icon("?", Message::Open(Screen::Help)),
                        "Support",
                        tooltip::Position::Top,
                    ),
                ]
                .spacing(8),
            );
        }
        let base = container(page)
            .center_x(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill);
        // Notices float in the bottom-right corner over the page, so showing
        // or dismissing one never moves anything under it.
        let mut notices = column![]
            .spacing(8)
            .align_x(iced::alignment::Horizontal::Right);
        if self.visible_screen() == Screen::Home {
            if let Some(preview) = self.preview.as_ref().filter(|p| !p.status_dismissed) {
                notices = notices.push(super::widgets::notice_card(
                    preview.week.status.clone(),
                    Some(Message::DismissWeekStatus),
                ));
            }
        }
        if let Some(status) = &self.status {
            notices = notices.push(super::widgets::notice_card(
                status.clone(),
                Some(Message::DismissStatus),
            ));
        }
        iced::widget::stack![
            base,
            container(notices)
                .align_right(Length::Fill)
                .align_bottom(Length::Fill)
                .padding(20),
        ]
        .into()
    }

    fn apply_status<'a>(&'a self, mut page: Column<'a, Message>) -> Column<'a, Message> {
        if !self.apply_summary.is_empty() {
            page = page.push(text(&self.apply_summary));
        }
        let bar_total = self.apply_write_total.max(1) as f32;
        page = page.push(moving_bar(
            self.apply_bar,
            bar_total,
            progress_line(self.apply_verified, self.apply_total),
        ));
        page = if self.apply_current.is_empty() {
            page.push(text("Begynder …"))
        } else {
            page.push(text(format!("{}.", self.apply_current)))
        };
        // The button already says a stop waits for the current change, so
        // the line beside it only adds that closing the app is just as safe.
        if self.apply_stopping {
            page.push(text(
                "Stopper, når den igangværende ændring er kontrolleret …",
            ))
        } else {
            page.push(
                row![
                    button(text("Stop efter denne ændring"))
                        .style(iced::widget::button::secondary)
                        .padding([8, 12])
                        .on_press(Message::StopApply),
                    text("Det er også sikkert at lukke appen.").size(13),
                ]
                .spacing(12)
                .align_y(iced::alignment::Vertical::Center),
            )
        }
    }

    /// There is no week to show before an account is confirmed, but help and
    /// its diagnostics stay reachable: that is when they are needed most.
    fn visible_screen(&self) -> Screen {
        match self.screen {
            Screen::Home if self.account.is_none() => Screen::Settings,
            screen => screen,
        }
    }

    /// Week title on the left, the one primary action fixed on the right.
    /// The grid scrolls under that chrome.
    fn home<'a>(&'a self, mut content: Column<'a, Message>) -> Column<'a, Message> {
        let is_current = self.monday == self.monday();
        content = content
            .push(
                row![
                    text(super::widgets::format_week_da(
                        self.monday,
                        self.monday + Duration::days(6),
                    ))
                    .size(20),
                    space::horizontal(),
                    self.week_action(),
                ]
                .align_y(iced::alignment::Vertical::Center)
                .width(Length::Fill),
            )
            .push(
                row![
                    self.quiet("‹ Forrige", Message::Navigate(-7)),
                    // Same quiet button, additionally disabled on the week
                    // that is already shown.
                    self.quiet("Denne uge", Message::Current).on_press_maybe(
                        (self.buttons_enabled() && !is_current).then_some(Message::Current),
                    ),
                    self.quiet("Næste ›", Message::Navigate(7)),
                ]
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center)
                .width(Length::Fill),
            );
        if self.activity != Activity::Apply {
            if let Some(stamp) = &self.last_verified {
                content = content.push(text(format!("Seneste overførsel: {stamp}.")).size(12));
            }
        }
        if let Some(fetch) = &self.fetch {
            content = content.push(moving_bar(fetch.bar(), FETCH_STAGES, fetch.line()));
        }
        let mut body = column![].spacing(12).width(Length::Fill);
        if let Some(preview) = &self.preview {
            let week = &preview.week;
            // The week's status floats with the other notices (see `view`).
            // Dismissed, it stays gone for this preview only; what blocks a
            // transfer is still listed here and marked in the grid.
            if !week.attention.is_empty() {
                body = body.push(text("Kræver opmærksomhed").size(18));
            }
            for item in &week.attention {
                let mut entry = column![
                    text(format!("{} · {}", item.when, item.who)),
                    text(&item.explanation),
                    text(format!("Gør sådan: {}", item.action))
                ]
                .spacing(4);
                if item.can_allow_retransfer {
                    entry = if self.forget_source.as_deref() == Some(item.source_key.as_str()) {
                        entry
                            .push(text(
                                "Appen glemmer kun sine egne registreringer for denne vagt. Intet slettes i MitHF eller DUOS. Bagefter skal du hente ugen igen og godkende den på ny.",
                            ))
                            .push(
                                row![
                                    self.action(
                                        "Ja, tillad overførsel igen",
                                        Message::ConfirmRetransfer
                                    ),
                                    self.action("Fortryd", Message::CancelRetransfer)
                                ]
                                .spacing(8),
                            )
                    } else {
                        entry.push(self.action(
                            "Tillad overførsel igen",
                            Message::AllowRetransfer(item.source_key.clone()),
                        ))
                    };
                }
                body = body.push(entry);
            }
            // Worth a look, but these never hold back the week.
            if !week.notes.is_empty() {
                body = body.push(text("Bemærk").size(18));
            }
            for item in &week.notes {
                body = body.push(
                    column![
                        text(format!("{} · {}", item.when, item.who)),
                        text(&item.explanation),
                    ]
                    .spacing(4),
                );
            }
            if week
                .days
                .iter()
                .any(|d| !d.blocks.is_empty() || !d.markers.is_empty())
            {
                body = body.push(super::widgets::week_grid(week));
            }
        }
        content.push(scrollable(body).height(Length::Fill).width(Length::Fill))
    }

    fn week_action<'a>(&'a self) -> Element<'a, Message> {
        let transferable = self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.week.can_apply);
        let mut actions = row![].spacing(8);
        if transferable {
            actions = actions.push(self.quiet(
                if self.needs_recheck {
                    "Kontrollér igen"
                } else {
                    SHOW_WEEK
                },
                Message::Preview,
            ));
            actions = actions.push(self.primary("Godkend ændringer", Message::Apply));
        } else if self.needs_recheck {
            actions = actions.push(self.primary("Kontrollér igen", Message::Preview));
        } else {
            actions = actions.push(self.primary(SHOW_WEEK, Message::Preview));
        }
        actions.into()
    }

    fn settings(&self) -> Element<'_, Message> {
        let mut header = row![text("Indstillinger").size(20), space::horizontal()]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center);
        // Without a confirmed setup there is no week to go back to, so the
        // button stays in place, greyed out, and its tooltip says why.
        let has_week = self.account.is_some();
        header = header.push(tooltip(
            self.circular_icon("⌂", Message::Open(Screen::Home))
                .on_press_maybe(has_week.then_some(Message::Open(Screen::Home))),
            if has_week {
                "Tilbage til ugen"
            } else {
                "Ugen vises, når opsætningen er bekræftet"
            },
            tooltip::Position::Bottom,
        ));
        let mut sidebar = column![header].spacing(10);
        for (section, icon, label) in [
            (SettingsSection::Helpers, "👥", "Hjælpere"),
            (SettingsSection::StandardTimes, "◷", "Standardtider"),
            (SettingsSection::Markers, "⚑", "Markeringer"),
            (SettingsSection::Integrations, "⇄", "Udbydere"),
        ] {
            let selected = self.settings_section == section;
            let mut entry = row![text(icon).size(18), text(label).size(14)]
                .spacing(10)
                .align_y(iced::alignment::Vertical::Center);
            // Blocked helper choices keep the whole setup from confirming, so
            // the mark shows from every section, not only on Hjælpere.
            if section == SettingsSection::Helpers && self.helpers_blocked() {
                entry = entry
                    .push(space::horizontal())
                    .push(text("! Ret").size(13).font(iced::Font {
                        weight: iced::font::Weight::Bold,
                        ..iced::Font::DEFAULT
                    }));
            }
            sidebar = sidebar.push(
                button(entry)
                    .style(move |theme, status| {
                        if selected {
                            iced::widget::button::primary(theme, status)
                        } else {
                            super::widgets::outlined(theme, status)
                        }
                    })
                    .padding([10, 12])
                    .width(Length::Fill)
                    .on_press(Message::SelectSettings(section)),
            );
        }
        let (icon, label, action) = if let Some(ready) = &self.update_ready {
            match ready {
                update::ApplyOutcome::Restart(_) => {
                    ("↻", "Genstart for at opdatere", Message::RestartUpdate)
                }
                update::ApplyOutcome::OpenInstaller(_) => {
                    ("↻", "Åbn installationsprogram", Message::RestartUpdate)
                }
            }
        } else if self.activity == Activity::Update {
            const FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
            (
                FRAMES[self.update_frame],
                "Henter opdatering …",
                Message::UpdateTick,
            )
        } else if self.update_offer.is_some() {
            ("⇩", "Hent opdatering", Message::InstallUpdate)
        } else {
            ("⟳", "Søg efter opdateringer", Message::CheckUpdates)
        };
        let update_control = tooltip(
            self.circular_icon(icon, action),
            text(if let Some(offer) = &self.update_offer {
                format!("{label}: version {}", offer.version)
            } else {
                label.to_owned()
            }),
            tooltip::Position::Right,
        );
        sidebar = sidebar.push(space().height(Length::Fill)).push(
            row![
                tooltip(
                    self.circular_icon("?", Message::Open(Screen::Help)),
                    "Support",
                    tooltip::Position::Top,
                ),
                update_control,
            ]
            .spacing(8),
        );
        let content = match self.settings_section {
            SettingsSection::Helpers => column![
                row![
                    text("Hjælpere").size(20),
                    tooltip(
                        self.circular_icon(
                            "⟳",
                            Message::Setup(setup::Message::Action(
                                "discover",
                                serde_json::json!({})
                            ))
                        ),
                        "Opdatér hjælpere",
                        tooltip::Position::Bottom,
                    ),
                ]
                .spacing(12)
                .align_y(iced::alignment::Vertical::Center),
                self.setup.view(setup::Section::Helpers).map(Message::Setup)
            ]
            .spacing(12),
            SettingsSection::StandardTimes => {
                column![self.setup.standard_view().map(Message::Setup)].spacing(12)
            }
            SettingsSection::Markers => {
                column![self.setup.markers_view().map(Message::Setup)].spacing(12)
            }
            SettingsSection::Integrations => self.providers(),
        };
        row![
            container(sidebar).width(Length::Fixed(210.0)),
            scrollable(content).height(Length::Fill).width(Length::Fill),
        ]
        .spacing(20)
        .height(Length::Fill)
        .into()
    }

    fn circular_icon<'a>(
        &self,
        icon: &'a str,
        message: Message,
    ) -> iced::widget::Button<'a, Message> {
        button(
            text(icon)
                .size(22)
                .align_x(iced::alignment::Horizontal::Center),
        )
        .width(Length::Fixed(40.0))
        .height(Length::Fixed(40.0))
        .style(|theme, status| {
            let mut style = super::widgets::outlined(theme, status);
            style.border.radius = 20.0.into();
            style
        })
        .on_press_maybe(self.enabled(&message).then_some(message))
    }

    fn providers(&self) -> Column<'_, Message> {
        column![
            text("Udbydere").size(20),
            self.setup
                .view(setup::Section::Integrations)
                .map(Message::Setup),
        ]
        .spacing(12)
    }

    /// Short guidance and the diagnostics the maintainer may ask for.
    fn help<'a>(&'a self, mut content: Column<'a, Message>) -> Column<'a, Message> {
        content = content.push(text("Support").size(22));
        for line in HELP {
            content = content.push(text(line));
        }
        content = content.push(text(format!("Version {}.", app_version())));
        content = content
            .push(text(
                "Går noget galt, åbner knappen et offentligt opslag på GitHub, som du læser igennem før afsendelse. Det kræver en GitHub-konto og indeholder ingen navne, vagttekst, adgangskoder, cookies eller links.",
            ))
            .push(self.quiet("Del hvad der gik galt", Message::ShareProblem));
        if self.account.is_some() {
            content = content
                .push(self.quiet("Gem en fejlrapport om MitHF og DUOS", Message::Capture))
                .push(self.quiet("Tilbage til ugen", Message::Open(Screen::Home)))
        } else {
            content = content
                .push(self.quiet("Tilbage til Indstillinger", Message::Open(Screen::Settings)))
        }
        content
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
        .title("BPA Overblik")
        .run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shown status as one line of text, or empty when there is none.
    fn shown(app: &NativeApp) -> String {
        app.status
            .as_ref()
            .map(|status| format!("{} {}", status.title, status.detail))
            .unwrap_or_default()
    }

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
        app.setup.link = "https://teamup.com/ksAbc123".into();
        app.setup.key = "an-api-key".into();
        let _ = app.update(Message::SetupUpdated(Ok(setup_state("destinations"))));
        // The credentials live in the OS keyring now; nothing keeps a copy.
        assert!(app.setup.link.is_empty());
        assert!(app.setup.key.is_empty());
        assert_eq!(app.activity, Activity::Idle);
    }

    #[test]
    fn a_local_setup_edit_keeps_the_screen_still_but_blocks_other_work() {
        let mut app = app();
        app.screen = Screen::Settings;
        app.status = Some(Notice::new(Tone::Success, "MitHF er forbundet", ""));
        let _ = app.update(Message::Setup(setup::Message::Action(
            "edit",
            json!({"registration_type": "t1"}),
        )));
        assert_eq!(app.activity, Activity::Save);
        assert!(app.buttons_enabled());
        assert_eq!(shown(&app).trim(), "MitHF er forbundet");
        // Still one operation at a time.
        let _ = app.update(Message::Setup(setup::Message::Action(
            "discover",
            json!({}),
        )));
        assert_eq!(app.activity, Activity::Save);
        let _ = app.update(Message::SetupUpdated(Ok(setup_state("mappings"))));
        assert_eq!(app.activity, Activity::Idle);
    }

    #[test]
    fn a_failed_setup_action_reports_its_cause_and_allows_another_try() {
        let mut app = app();
        app.activity = Activity::Setup;
        app.setup.link = "https://teamup.com/ksAbc123".into();
        app.setup.key = "an-api-key".into();
        let _ = app.update(Message::SetupUpdated(Err(
            "Kalenderlinket er ugyldigt.".into()
        )));
        assert_eq!(app.setup.link, "https://teamup.com/ksAbc123");
        assert_eq!(app.setup.key, "an-api-key");
        assert_eq!(
            app.status,
            Some(Notice::new(Tone::Error, "Kalenderlinket er ugyldigt", ""))
        );
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

    /// A source check result is shown once as the app status, including when
    /// the check confirmed the setup and the app reloads.
    #[test]
    fn a_source_check_result_survives_the_reload_it_triggers() {
        let mut app = app();
        app.activity = Activity::Setup;
        let mut state = setup_state("ready");
        state.result = Some(Notice::new(
            Tone::Info,
            "Vagter og kommentarer er kontrolleret",
            "",
        ));
        let _ = app.update(Message::SetupUpdated(Ok(state)));
        assert_eq!(app.activity, Activity::Setup);
        assert_eq!(shown(&app).trim(), "Vagter og kommentarer er kontrolleret");
        assert!(app
            .setup
            .state
            .as_ref()
            .is_some_and(|state| state.result.is_none()));
    }

    /// The fetch line follows what the task reports and disappears with the
    /// fetch, so the screen never keeps a stale stage.
    #[test]
    fn a_fetch_shows_the_stage_the_task_has_reached() {
        let mut app = app();
        app.activity = Activity::Preview;
        let fetch = Fetch::new();
        let stage = fetch.stage.clone();
        app.fetch = Some(fetch);
        let _ = app.update(Message::ApplyTick);
        let first = app.fetch.as_ref().expect("fetch");
        assert_eq!(first.shown, FetchStage::Sessions);
        // The first stage creeps inside its own section and no further.
        assert!(first.bar() >= 0.0 && first.bar() < 1.0);
        mark(&stage, FetchStage::Reading);
        let _ = app.update(Message::ApplyTick);
        let shown = app.fetch.as_ref().expect("fetch");
        assert_eq!(shown.shown, FetchStage::Reading);
        assert!(shown.line().starts_with("Læser vagtplan og tjenester …"));
        // One stage is done, so the bar stands in the second section.
        assert!(shown.bar() >= 1.0 && shown.bar() < 2.0);
        let _ = app.view();
        let _ = app.update(Message::PreviewLoaded(
            Err("Ugen kunne ikke hentes.".into()),
        ));
        assert!(app.fetch.is_none());
        assert_eq!(app.activity, Activity::Idle);
    }

    fn app() -> NativeApp {
        NativeApp {
            engine: Engine::new("unused-test-directory".into()),
            screen: Screen::Home,
            settings_section: SettingsSection::Helpers,
            standard_revision: 0,
            standard_saved_revision: 0,
            account: None,
            setup: setup::SetupUi::default(),
            monday: NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(),
            preview: None,
            activity: Activity::Idle,
            status: None,
            last_verified: None,
            apply_progress: None,
            apply_stop: None,
            apply_total: 0,
            apply_verified: 0,
            apply_write_total: 0,
            apply_write_verified: 0,
            apply_bar: 0.0,
            fetch: None,
            apply_current: String::new(),
            apply_summary: String::new(),
            apply_stopping: false,
            needs_recheck: false,
            forget_source: None,
            close_after_apply: None,
            update_offer: None,
            update_ready: None,
            update_manual: false,
            update_frame: 0,
            helpers_auto_fetch_attempted: false,
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
            standard_times: Default::default(),
            markers: Vec::new(),
            status_dismissed: false,
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
        let status = app.status.as_ref().expect("failure notice");
        assert_eq!(status.tone, Tone::Error);
        assert_eq!(status.title, "Overførslen blev ikke færdig");
        assert!(status.detail.starts_with("Uafklaret ændring "));
        assert!(status.detail.contains("1 uafklaret"));
        assert!(app.needs_recheck);
        let _ = app.update(Message::Apply);
        assert_eq!(app.activity, Activity::Idle);
        // Widget construction exercises the recoverable failure screen.
        let _ = app.view();
    }
    #[test]
    fn approved_duos_registration_can_start_apply() {
        let mut app = app();
        let mut approved = preview(&app, true);
        let mut registration = write_item("s", "duos.interval:0");
        registration.system = PlanSystem::Duos;
        approved.items = vec![registration];
        approved.week.apply_summary = "Overfører 1 registrering til DUOS.".into();
        app.preview = Some(approved);

        let _ = app.update(Message::Apply);

        assert_eq!(app.activity, Activity::Apply);
        assert!(app.preview.is_none());
        assert_eq!(app.apply_total, 1);
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
        assert!(shown(&app).contains("stoppet sikkert"));
        assert!(app.last_verified.is_none());
    }
    fn conflicted(app: &NativeApp, source_key: &str) -> Preview {
        let mut blocked = preview(app, false);
        blocked.week.attention = vec![teamup_shift_sync_gui::protocol::Attention {
            when: "man 14. sep 07:30".into(),
            who: "Ida".into(),
            explanation: "…".into(),
            action: "…".into(),
            source_key: source_key.into(),
            can_allow_retransfer: true,
        }];
        blocked
    }
    #[test]
    fn allowing_a_transfer_again_needs_its_own_conflict_and_a_confirmation() {
        let mut app = app();
        app.preview = Some(conflicted(&app, "shift-a"));

        // A shift without an offered conflict can never reach the confirmation.
        let _ = app.update(Message::AllowRetransfer("shift-b".into()));
        assert!(app.forget_source.is_none());
        let _ = app.update(Message::ConfirmRetransfer);
        assert_eq!(app.activity, Activity::Idle);

        let _ = app.update(Message::AllowRetransfer("shift-a".into()));
        assert_eq!(app.forget_source.as_deref(), Some("shift-a"));
        let _ = app.view();
        let _ = app.update(Message::CancelRetransfer);
        assert!(app.forget_source.is_none());
    }
    #[test]
    fn forgetting_local_records_revokes_the_preview_and_requires_a_fresh_one() {
        let mut app = app();
        app.preview = Some(conflicted(&app, "shift-a"));
        app.forget_source = Some("shift-a".into());
        app.activity = Activity::Recover;

        let _ = app.update(Message::Forgotten(Ok(2)));

        assert_eq!(app.activity, Activity::Idle);
        assert!(app.preview.is_none());
        assert!(app.forget_source.is_none());
        assert!(app.needs_recheck);
        assert!(shown(&app).contains("Intet er slettet i MitHF eller DUOS"));
        let _ = app.view();
    }
    #[test]
    fn a_blocked_forget_reports_and_keeps_the_conflict_visible() {
        let mut app = app();
        app.preview = Some(conflicted(&app, "shift-a"));
        app.forget_source = Some("shift-a".into());
        app.activity = Activity::Recover;

        let _ = app.update(Message::Forgotten(Err(
            "En overførsel bruger denne konto.".into()
        )));

        assert_eq!(app.activity, Activity::Idle);
        assert!(app.preview.is_some());
        assert!(app.forget_source.is_none());
        assert_eq!(
            app.status.as_ref().map(|status| status.tone),
            Some(Tone::Error)
        );
    }
    #[test]
    fn going_home_works_while_settings_are_still_loading() {
        let mut app = app();
        app.screen = Screen::Settings;
        app.activity = Activity::Setup;
        let _ = app.update(Message::Open(Screen::Home));
        assert_eq!(app.screen, Screen::Home);
        assert!(app.enabled(&Message::Open(Screen::Settings)));
        // Week navigation still waits, because it revokes a shown approval.
        assert!(!app.enabled(&Message::Navigate(7)));
        let monday = app.monday;
        let _ = app.update(Message::Navigate(7));
        assert_eq!(app.monday, monday);
        assert_eq!(app.activity, Activity::Setup);
    }
    #[test]
    fn settings_are_reachable_from_the_week_and_revoke_a_shown_approval() {
        let mut app = app();
        app.account = None;
        // Without a confirmed account there is nowhere else to be.
        assert_eq!(app.visible_screen(), Screen::Settings);
        let _ = app.view();

        app.preview = Some(preview(&app, true));
        app.settings_section = SettingsSection::Integrations;
        let _ = app.update(Message::Open(Screen::Settings));
        assert_eq!(app.screen, Screen::Settings);
        assert_eq!(app.settings_section, SettingsSection::Helpers);
        assert!(app.preview.is_none());

        let _ = app.update(Message::Open(Screen::Help));
        assert_eq!(app.screen, Screen::Help);
        let _ = app.view();
        let _ = app.update(Message::Open(Screen::Home));
        assert_eq!(app.screen, Screen::Home);
    }
    #[test]
    fn an_unsaved_standard_draft_survives_an_unrelated_setup_refresh() {
        let mut app = app();
        app.setup.standard_default = "6-22".into();
        app.standard_revision = 1;
        app.accept_standard(&setup_state("ready"));
        assert_eq!(app.setup.standard_default, "6-22");
        assert_eq!(app.standard_saved_revision, 0);
    }
    #[test]
    fn opening_helpers_fetches_mappings_without_a_button() {
        let mut app = app();
        app.setup.state = Some(setup_state("destinations"));
        app.settings_section = SettingsSection::Integrations;
        let _ = app.update(Message::Open(Screen::Settings));
        assert_eq!(app.settings_section, SettingsSection::Helpers);
        assert_eq!(app.activity, Activity::Setup);
        let _ = app.update(Message::SetupUpdated(Err("Log ind først".into())));
        let _ = app.update(Message::Open(Screen::Settings));
        assert_eq!(app.activity, Activity::Idle);
        let _ = app.update(Message::Setup(setup::Message::Action(
            "discover",
            json!({}),
        )));
        assert_eq!(app.activity, Activity::Setup);
    }
    #[test]
    fn the_providers_page_renders_sources_and_services() {
        let mut app = app();
        app.screen = Screen::Settings;
        app.settings_section = SettingsSection::Integrations;
        app.setup.state = Some(setup_state("ready"));
        let _ = app.view();
        let _ = app.update(Message::Setup(setup::Message::ToggleProvider(
            "duos".into(),
        )));
        assert!(app.setup.collapsed.contains("duos"));
        let _ = app.view();
        let _ = app.update(Message::Setup(setup::Message::Noop));
        let _ = app.view();
    }
    #[test]
    fn valid_standard_time_starts_an_automatic_save() {
        let mut app = app();
        app.setup.state = Some(setup_state("ready"));
        let _ = app.update(Message::Setup(setup::Message::StandardDefault(
            "6-22".into(),
        )));
        assert_eq!(app.standard_revision, 1);
        let _ = app.update(Message::AutosaveStandard(0));
        assert_eq!(app.activity, Activity::Idle);
        let _ = app.update(Message::AutosaveStandard(1));
        assert_eq!(app.activity, Activity::Save);
    }
    #[test]
    fn an_auto_save_keeps_the_settings_form_mounted() {
        let mut app = app();
        app.screen = Screen::Settings;
        app.settings_section = SettingsSection::StandardTimes;
        app.activity = Activity::Save;
        app.setup.standard_default = "6-22".into();
        app.standard_revision = 1;
        let mut state = setup_state("ready");
        state.standard_times.everyday = "6-22".into();

        let _ = app.update(Message::SetupUpdated(Ok(state)));

        assert_eq!(app.activity, Activity::Save);
        assert_eq!(app.settings_section, SettingsSection::StandardTimes);
        assert_eq!(app.setup.standard_default, "6-22");
        let _ = app.update(Message::Setup(setup::Message::StandardDefault(
            "6-223".into(),
        )));
        assert_eq!(app.setup.standard_default, "6-223");
    }
    #[test]
    fn forgetting_a_login_revokes_a_shown_approval() {
        let mut app = app();
        app.screen = Screen::Settings;
        app.preview = Some(preview(&app, true));

        let _ = app.update(Message::ForgetLogins(Service::Mithf));

        assert_eq!(app.activity, Activity::Login);
        assert!(app.preview.is_none());
        let _ = app.update(Message::LoginsForgotten(Service::Mithf, Ok(())));
        assert_eq!(app.activity, Activity::Idle);
        assert!(shown(&app).contains("Log ind igen"));
        let _ = app.view();
    }
    #[test]
    fn checking_one_service_leaves_the_other_alone() {
        let mut app = app();
        let _ = app.update(Message::CheckLogin(Service::Duos));
        assert_eq!(app.activity, Activity::Login);
        let _ = app.update(Message::LoginChecked(Service::Duos, Ok(())));
        assert_eq!(app.activity, Activity::Idle);
        assert!(shown(&app).contains("DUOS er forbundet"));
    }
    #[test]
    fn help_and_its_diagnostics_stay_reachable_before_setup_is_finished() {
        let mut app = app();
        app.account = None;
        let _ = app.update(Message::Open(Screen::Help));
        assert_eq!(app.visible_screen(), Screen::Help);
        let _ = app.view();

        let _ = app.update(Message::ShareProblem);
        assert_eq!(app.activity, Activity::Capture);
        let _ = app.update(Message::ProblemShared(Ok(Shared {
            path: "fejlrapport.json".into(),
            opened: true,
        })));
        assert_eq!(app.activity, Activity::Idle);
        assert!(shown(&app).contains("fejlrapport.json"));

        // Without a browser the file is still there to send by hand.
        app.activity = Activity::Capture;
        let _ = app.update(Message::ProblemShared(Ok(Shared {
            path: "fejlrapport.json".into(),
            opened: false,
        })));
        assert!(shown(&app).contains("Send filen"));
    }
    fn newer_offer() -> update::Offer {
        update::offer_from_release(
            r#"{
                "tag_name": "v2026.09.22",
                "assets": [{"name": "teamup-shift-sync-2026.09.22-x86_64.AppImage", "browser_download_url": "https://example.test/linux.AppImage"}]
            }"#,
            "2026.09.21",
            "linux",
        )
        .expect("parse")
        .expect("offer")
    }
    #[test]
    fn a_background_update_check_survives_a_transfer_and_a_reload() {
        let mut app = app();
        app.activity = Activity::Apply;
        let _ = app.update(Message::UpdateChecked(Ok(Some(newer_offer()))));
        assert_eq!(app.activity, Activity::Apply);
        assert_eq!(
            app.update_offer
                .as_ref()
                .map(|offer| offer.version.as_str()),
            Some("2026.09.22")
        );

        app.activity = Activity::Idle;
        let _ = app.update(Message::Reload);
        assert!(app.update_offer.is_some());
        app.activity = Activity::Idle;
        let _ = app.update(Message::Navigate(7));
        assert!(app.update_offer.is_some());
        let _ = app.view();
    }
    #[test]
    fn an_update_waits_for_a_restart_and_a_transfer_cannot_start_one() {
        let mut app = app();
        app.update_offer = Some(newer_offer());
        app.activity = Activity::Apply;
        let _ = app.update(Message::InstallUpdate);
        assert_eq!(app.activity, Activity::Apply);
        app.activity = Activity::Update;
        let _ = app.update(Message::UpdateApplied(Ok(update::ApplyOutcome::Restart(
            "/tmp/new-app".into(),
        ))));
        assert_eq!(app.activity, Activity::Idle);
        assert!(app.update_ready.is_some());
    }
    #[test]
    fn a_periodic_check_stays_silent_and_waits_while_busy() {
        let mut app = app();
        app.activity = Activity::Apply;
        let _ = app.update(Message::PeriodicUpdateCheck);
        assert!(app.status.is_none());

        app.activity = Activity::Idle;
        let _ = app.update(Message::PeriodicUpdateCheck);
        assert!(app.status.is_none());
        assert!(!app.update_manual);

        app.update_offer = Some(newer_offer());
        let _ = app.update(Message::PeriodicUpdateCheck);
        assert!(app.status.is_none());
        assert!(app.update_offer.is_some());
    }
    #[test]
    fn the_shared_issue_is_prefilled_and_falls_back_to_the_saved_file() {
        let path = std::path::Path::new("/hjem/fejlrapport.json");
        let url = issue_url(&json!({"version": 1, "setup": {"stage": "ready"}}), path);
        assert!(url.starts_with("https://github.com/Jdreioe/BPA_Overblik/issues/new?title="));
        assert!(url.contains("BPA%20Overblik"));
        assert!(url.contains("%22stage%22"));

        // A report too long for an address points at the file instead.
        let long = json!({"padding": "x".repeat(8000)});
        let url = issue_url(&long, path);
        assert!(!url.contains("xxxx"));
        assert!(url.contains("fejlrapport.json"));
    }
    #[test]
    fn an_auto_confirmed_setup_stays_on_settings() {
        let mut app = app();
        app.screen = Screen::Settings;
        app.activity = Activity::Setup;
        let _ = app.update(Message::SetupUpdated(Ok(setup_state("ready"))));
        assert_eq!(app.screen, Screen::Settings);
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
        assert_eq!(
            shown(&app).trim(),
            "1 vagt overført til MitHF og 1 registrering overført til DUOS"
        );
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
        assert!(shown(&app).contains("MitHF"));
        assert!(shown(&app).contains("DUOS"));
        // The week shows the last transfer time; the notice does not repeat it.
        assert!(!shown(&app).contains("20. sep"));
    }
    #[test]
    fn step_labels_stay_plain_and_count_by_destination() {
        assert_eq!(Destination::Mithf.name(), "MitHF");
        assert_eq!(Destination::Duos.name(), "DUOS");
        assert_eq!(verb_of(Outcome::WouldCreate), "Tilføjer");
        assert_eq!(verb_of(Outcome::WouldUpdate), "Opdaterer");
        assert_eq!(short_date_da(28, 8), "28. sep");
        assert_eq!(progress_line(1, 2), "1 af 2 ændringer overført.");
    }
    #[test]
    fn a_completed_transfer_names_each_destination_with_its_own_count() {
        let title = |mithf, duos| {
            completion_notice(&ApplyDone {
                mithf,
                duos,
                verified_label: Some("20. sep kl. 20.00".into()),
                stopped: false,
            })
            .title
        };
        assert_eq!(title(1, 0), "1 vagt overført til MitHF");
        assert_eq!(title(0, 3), "3 registreringer overført til DUOS");
        assert_eq!(
            title(2, 1),
            "2 vagter overført til MitHF og 1 registrering overført til DUOS"
        );
        assert_eq!(title(0, 0), "Ingen ændringer overført");
    }
    #[test]
    fn a_dismissed_week_status_stays_dismissed_until_the_next_preview() {
        let mut app = app();
        app.preview = Some(preview(&app, true));
        // Dismissing is only text, so it works while other work is running.
        app.activity = Activity::Login;
        let _ = app.update(Message::DismissWeekStatus);
        assert!(app.preview.as_ref().is_some_and(|p| p.status_dismissed));
        app.activity = Activity::Preview;
        let _ = app.update(Message::PreviewLoaded(Ok(Box::new(preview(&app, true)))));
        assert!(app.preview.as_ref().is_some_and(|p| !p.status_dismissed));
        let _ = app.view();
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
    /// A week with writes, an overnight continuation and SPS marks renders
    /// its grid, the one status line and the attention items — and the same
    /// week with nothing left renders its blocked reason instead of a
    /// transfer button.
    #[test]
    fn full_and_empty_weeks_render_their_single_status() {
        let week: Week = serde_json::from_value(json!({
            "days": [
                {"date": "2026-09-14", "label": "man 14. sep", "blocks": [
                    {"helper": "Zain Alnemr", "helper_color": "#4770d8",
                     "status": "create", "status_label": "Oprettes",
                     "minutes_from": 780, "minutes_to": 1440,
                     "time_label": "13:00–24:00", "sps_label": "13:00–14:00",
                     "part_label": "Del 1 af 2",
                     "continues_before": false, "continues_after": true,
                     "details": ["Vagten oprettes i MitHF."]},
                ]},
                {"date": "2026-09-15", "label": "tir 15. sep", "blocks": [
                    {"helper": "Zain Alnemr", "helper_color": "#4770d8",
                     "status": "create", "status_label": "Oprettes",
                     "minutes_from": 0, "minutes_to": 420,
                     "time_label": "13:00 14. sep – 07:00 15. sep",
                     "sps_label": "", "part_label": "Del 2 af 2",
                     "continues_before": true, "continues_after": false,
                     "details": []},
                    {"helper": "Vikar", "helper_color": "",
                     "status": "attention", "status_label": "Kræver opmærksomhed",
                     "minutes_from": 480, "minutes_to": 720,
                     "time_label": "08:00–12:00",
                     "sps_label": "", "part_label": "",
                     "continues_before": false, "continues_after": false,
                     "details": []},
                ]},
            ],
            "attention": [{"when": "tir 15. sep 08:00", "who": "Vikar",
                           "explanation": "Flere vagter matcher.",
                           "action": "Ret konflikten i MitHF.",
                           "source_key": "shift-b",
                           "can_allow_retransfer": false}],
            "status": {"tone": "warning", "title": "Ugen kan ikke godkendes endnu",
                       "detail": "Løs punktet herunder først."},
            "summary": [],
            "apply_summary": "Der er ingen ændringer at overføre.",
            "can_apply": false, "destination_read": true,
        }))
        .expect("week");
        let mut app = app();
        app.preview = Some(Preview {
            week,
            digest: "reviewed-digest".into(),
            from: app.monday,
            state_path: "synthetic-account.sqlite3".into(),
            standard_times: Default::default(),
            markers: Vec::new(),
            status_dismissed: false,
            items: vec![],
        });
        let _ = app.view();
        // Dette uge-knappen bærer ingen vægt, når ugen allerede er valgt.
        let _ = app.update(Message::Current);
    }
}
