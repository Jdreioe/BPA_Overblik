//! Native Iced workflow using the Rust core and app-owned browser sessions.
//! Setup editing remains in the existing application during migration.
use crate::setup;
use chrono::{Datelike, Duration, NaiveDate, TimeZone, Utc};
use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Element, Length, Task};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};
use teamup_shift_sync_core::{
    apply_plan, build_plan,
    live::{
        load_saved_setup, read_destinations, read_shapes, read_teamup, BrowserSessions, LiveConfig,
        LiveDestinations, Service, Setup, Visibility,
    },
    plan_digest, reconciliation_range, ApplyRequest, PlanRequest, SyncState,
};
use teamup_shift_sync_gui::{files::app_data_dir, preview::build_week, protocol::Week};
use tokio::sync::{Mutex, MutexGuard};

type Result<T> = std::result::Result<T, String>;

/// Project the setup document onto what the setup screen may display.
fn view(document: &Setup) -> Result<setup::SetupState> {
    serde_json::from_value(document.view()).map_err(|_| "Opsætningen kunne ikke vises.".to_owned())
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
            })
        })
        .await
        .map_err(|_| "Ugens plan kunne ikke indlæses.".to_owned())?
    }
    async fn apply(&self, approved: Preview) -> Result<()> {
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
        apply_plan(ApplyRequest {config:account.planning.clone(),shifts,range_start:start,range_end:end,now,expected_digest:approved.digest},account.state_path.clone(),&mut destination, |_|{}).await
            .map_err(|e| match e {
                teamup_shift_sync_core::TransferError::Approval(teamup_shift_sync_core::ApprovalError::PlanChanged) => "Ugen er ændret. Hent ændringerne igen, og gennemgå dem før overførsel.".to_owned(),
                teamup_shift_sync_core::TransferError::State(teamup_shift_sync_core::StateError::ApplyInProgress) => "En anden overførsel bruger denne konto. Vent, og hent ugen igen.".to_owned(),
                _ => "Overførslen kunne ikke afsluttes og verificeres. Nogle trin kan være gemt. Hent ugen igen, før du prøver at overføre mere.".to_owned(),
            })?;
        Ok(())
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
}

#[derive(Clone)]
enum Message {
    Reload,
    Loaded(Result<Loaded>),
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
    Applied(Result<()>),
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
                | Message::SetupUpdated(_)
                | Message::LoginOpened(_)
                | Message::LoginChecked(_)
                | Message::Captured(_)
                | Message::PreviewLoaded(_)
                | Message::Applied(_)
        );
        if self.activity != Activity::Idle && !completion {
            return Task::none();
        }
        match message {
            Message::Reload => {
                self.preview = None;
                self.account = None;
                self.notice.clear();
                self.error = None;
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
                        self.account = loaded.account;
                        self.setup.state = Some(loaded.state);
                        self.setup.error = None;
                        self.monday = self.monday();
                    }
                    Err(error) => self.error = Some(error),
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
                self.preview = None;
                self.error = None;
                self.notice.clear();
                self.activity = Activity::Apply;
                let engine = self.engine.clone();
                return Task::perform(async move { engine.apply(preview).await }, Message::Applied);
            }
            Message::Applied(result) => {
                if self.activity != Activity::Apply {
                    return Task::none();
                }
                self.activity = Activity::Idle;
                // Consume approval on success and failure. A retry requires a
                // fresh preview, which can reconcile uncertain persisted steps.
                self.preview = None;
                match result {Ok(())=>self.notice="Overførslen er afsluttet. Alle valgte trin er læst tilbage og verificeret.".into(),Err(e)=>self.error=Some(e)}
            }
        }
        Task::none()
    }
    fn invalidate(&mut self) {
        self.preview = None;
        self.error = None;
        self.notice.clear();
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
            content = content.push(self.action("Se ændringer", Message::Preview));
        }
        let busy = match self.activity {
            Activity::Idle => "",
            Activity::Setup => "Indlæser opsætning …",
            Activity::Login => "Kontakter browseren …",
            Activity::Capture => "Læser tjenesternes svar …",
            Activity::Preview => "Henter ugens ændringer …",
            Activity::Apply => "Overfører og kontrollerer hvert trin. Lad appen være åben …",
        };
        if !busy.is_empty() {
            content = content.push(text(busy));
        }
        container(scrollable(content))
            .center_x(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

pub fn run() -> iced::Result {
    // Fixture mode must never accidentally turn into a live account workflow.
    if std::env::var_os("TEAMUP_FIXTURE").is_some()
        || std::env::args().any(|arg| arg == "--self-check")
    {
        eprintln!("--native uses saved live setup. Run fixture/self-check mode without --native.");
        std::process::exit(2);
    }
    iced::application(NativeApp::new, NativeApp::update, NativeApp::view)
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
        }
    }
    fn preview(app: &NativeApp, can_apply: bool) -> Preview {
        Preview {
            week: Week {
                can_apply,
                ..Week::default()
            },
            digest: "reviewed-digest".into(),
            from: app.monday,
            state_path: "synthetic-account.sqlite3".into(),
        }
    }
    #[test]
    fn apply_consumes_approval_and_blocks_changes_until_completion() {
        let mut app = app();
        app.preview = Some(preview(&app, true));
        let _ = app.update(Message::Apply);
        assert_eq!(app.activity, Activity::Apply);
        assert!(app.preview.is_none());
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
    fn late_results_cannot_restore_consumed_approval() {
        let mut app = app();
        let old = preview(&app, true);
        app.activity = Activity::Apply;
        let _ = app.update(Message::PreviewLoaded(Ok(Box::new(old.clone()))));
        assert!(app.preview.is_none());
        let _ = app.update(Message::Applied(Ok(())));
        assert!(app.preview.is_none());
        let _ = app.update(Message::PreviewLoaded(Ok(Box::new(old))));
        assert!(app.preview.is_none());
        assert!(app.notice.contains("verificeret"));
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
