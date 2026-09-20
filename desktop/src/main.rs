//! Danish Iced desktop shell for weekly shift transfers (issue #2).
//!
//! One primary action per screen, text alongside status colors, and technical
//! diagnostics kept out of the normal flow. Network/browser/SQLite work runs
//! in the app-owned Python worker on blocking threads — never on the UI
//! thread — and every worker failure lands in a readable recovery state.

mod setup;

use chrono::{Datelike, Local, NaiveDate};
use iced::widget::{button, column, container, row, rule, scrollable, space, text};
use iced::{Color, Element, Length, Subscription, Task};
use teamup_shift_sync_gui::protocol::{
    Block, HomeStatus, LoginState, PlanPreview, Service, Sessions, Week, WorkerError,
};
use teamup_shift_sync_gui::worker::{AppFiles, WorkerHandle};

const DANISH_MONTHS: [&str; 12] = [
    "januar",
    "februar",
    "marts",
    "april",
    "maj",
    "juni",
    "juli",
    "august",
    "september",
    "oktober",
    "november",
    "december",
];

/// Run one self-check stage with a bound, so a silent bundled worker
/// fails the packaging step naming the stage instead of hanging it for
/// hours with no output.
fn checked<T: Send + 'static>(
    stage: &'static str,
    f: impl FnOnce() -> Result<T, WorkerError> + Send + 'static,
) -> Result<T, WorkerError> {
    use std::sync::mpsc;
    use std::time::Duration;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(result) => result,
        Err(_) => Err(WorkerError::exited(format!(
            "Selvtesten fik ikke svar i trinnet '{stage}'."
        ))),
    }
}

fn main() -> iced::Result {
    if std::env::args().any(|argument| argument == "--self-check") {
        let files = AppFiles::resolve();
        match WorkerHandle::spawn(&files).and_then(|worker| {
            checked("home_status", {
                let worker = worker.clone();
                move || worker.home_status(Some("2026-09-19T12:00:00+02:00"))
            })?;
            checked("preview_fixture", {
                let worker = worker.clone();
                move || worker.preview_fixture("2026-09-14", "2026-09-20")
            })?;
            Ok(())
        }) {
            Ok(()) => {
                println!("Desktop bundle self-check passed.");
                return Ok(());
            }
            Err(error) => {
                eprintln!("{}\n{}", error.message, error.detail);
                std::process::exit(1);
            }
        }
    }
    iced::application(App::new, App::update, App::view)
        .title("Vagtplanlægning")
        .subscription(App::subscription)
        .run()
}

#[derive(Debug, Clone)]
enum Message {
    Setup(setup::Message),
    SetupLoaded(Result<setup::SetupState, WorkerError>),
    WorkerSpawned(Result<WorkerReady, WorkerError>),
    HomeLoaded(Result<HomeStatus, WorkerError>),
    WeekPrev,
    WeekNext,
    WeekCurrent,
    PreviewRequested,
    PreviewLoaded {
        from: String,
        to: String,
        result: Result<PlanPreview, WorkerError>,
    },
    Retry,
    ApproveRequested,
    ApprovalDismissed,
    SessionTick,
    Login(Service),
    SessionsLoaded(Result<Sessions, WorkerError>),
}

/// Carried from the blocking spawn task back to the UI thread.
#[derive(Debug, Clone)]
struct WorkerReady {
    handle: WorkerHandle,
    home: Result<HomeStatus, WorkerError>,
}

#[derive(Debug)]
enum HomeState {
    Loading,
    Ready(HomeStatus),
    Failed(WorkerError),
}

#[derive(Debug)]
enum PreviewState {
    Idle,
    Loading { from: String, to: String },
    // Boxed: a full week preview dwarfs the other variants.
    Ready(Box<PlanPreview>),
    Failed(WorkerError),
}

/// A plan the user approved by clicking "Overfør ændringer".
///
/// The click approves exactly the plan that was on screen. The digest is
/// kept internally and never shown; a re-read that produces a different
/// digest returns to review instead of transferring unexpected changes.
#[derive(Debug, Clone)]
struct Approval {
    /// Carried internally and never shown; the transfer in #6 sends it back
    /// so the engine can refuse a batch that no longer matches.
    #[allow(dead_code)]
    digest: String,
    summary: String,
    week: String,
}

struct App {
    setup: setup::SetupUi,
    files: AppFiles,
    worker: Option<WorkerHandle>,
    startup_error: Option<WorkerError>,
    week_start: NaiveDate,
    home: HomeState,
    preview: PreviewState,
    sessions: Option<Sessions>,
    sessions_loading: bool,
    session_error: Option<String>,
    /// Digest the user clicked on, awaiting confirmation from a fresh read.
    approving: Option<String>,
    approval: Option<Approval>,
    /// Set when a re-read before transfer found a different plan.
    plan_changed: bool,
}

fn current_monday() -> NaiveDate {
    let today = Local::now().date_naive();
    today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64)
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let files = AppFiles::resolve();
        let app = Self {
            setup: setup::SetupUi::default(),
            files: files.clone(),
            worker: None,
            startup_error: None,
            week_start: current_monday(),
            home: HomeState::Loading,
            preview: PreviewState::Idle,
            sessions: None,
            sessions_loading: false,
            session_error: None,
            approving: None,
            approval: None,
            plan_changed: false,
        };
        (app, Self::spawn_task(files))
    }

    fn week_end(&self) -> NaiveDate {
        self.week_start + chrono::Duration::days(6)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Setup(message) => {
                match message {
                    setup::Message::Link(value) => self.setup.link = value,
                    setup::Message::Key(value) => self.setup.key = value,
                    setup::Message::Connect => {
                        let params =
                            serde_json::json!({"link":self.setup.link,"api_key":self.setup.key});
                        return self.setup_task("connect", params);
                    }
                    setup::Message::Action(action, params) => {
                        return self.setup_task(action, params)
                    }
                }
                Task::none()
            }
            Message::SetupLoaded(result) => {
                self.setup.busy = false;
                match result {
                    Ok(state) => {
                        let was_ready = self.setup.ready();
                        self.setup.state = Some(state);
                        self.setup.error = None;
                        self.setup.link.clear();
                        self.setup.key.clear();
                        if self.setup.ready() && !was_ready {
                            self.week_start = self
                                .setup
                                .state
                                .as_ref()
                                .and_then(|s| {
                                    NaiveDate::parse_from_str(&s.week_start, "%Y-%m-%d").ok()
                                })
                                .unwrap_or_else(current_monday);
                            return self.update(Message::PreviewRequested);
                        }
                    }
                    Err(error) => {
                        self.setup.error = Some(error.message.clone());
                        if error.code == "worker_exit" {
                            self.worker = None;
                            self.startup_error = Some(error);
                        }
                    }
                }
                Task::none()
            }
            Message::SessionTick => self.session_task(None),
            Message::Login(service) => self.session_task(Some(service)),
            Message::SessionsLoaded(result) => {
                self.sessions_loading = false;
                match result {
                    Ok(status) => {
                        self.sessions = Some(status);
                        self.session_error = None;
                    }
                    Err(error) => {
                        self.sessions = None;
                        self.session_error = Some(error.message.clone());
                        if error.code == "worker_exit" {
                            self.worker = None;
                            self.startup_error = Some(error);
                        }
                    }
                }
                Task::none()
            }

            Message::WorkerSpawned(Err(error)) => {
                self.worker = None;
                self.startup_error = Some(error);
                self.home = HomeState::Failed(WorkerError::exited(
                    "Status kunne ikke hentes, fordi baggrundsarbejderen ikke kører.",
                ));
                Task::none()
            }
            Message::WorkerSpawned(Ok(ready)) => {
                self.startup_error = None;
                self.worker = Some(ready.handle);
                self.home = match ready.home {
                    Ok(status) => HomeState::Ready(status),
                    Err(error) => HomeState::Failed(error),
                };
                self.sessions = None;
                self.sessions_loading = false;
                let sessions = self.session_task(None);
                if self.files.is_fixture {
                    sessions
                } else {
                    Task::batch([sessions, self.setup_task("status", serde_json::json!({}))])
                }
            }
            Message::HomeLoaded(result) => {
                if let Err(error) = &result {
                    if error.code == "worker_exit" {
                        self.worker = None;
                        self.startup_error = Some(error.clone());
                    }
                }
                self.home = match result {
                    Ok(status) => HomeState::Ready(status),
                    Err(error) => HomeState::Failed(error),
                };
                Task::none()
            }
            Message::WeekPrev => {
                self.shift_week(-7);
                Task::none()
            }
            Message::WeekNext => {
                self.shift_week(7);
                Task::none()
            }
            Message::WeekCurrent => {
                self.week_start = current_monday();
                // A new week invalidates the shown preview; the user asks
                // for a fresh "Se ændringer" rather than trusting stale data.
                self.preview = PreviewState::Idle;
                self.invalidate_approval();
                self.reload_home()
            }
            Message::PreviewRequested => {
                let Some(worker) = self.worker.clone() else {
                    self.preview = PreviewState::Failed(WorkerError::exited(
                        "Baggrundsarbejderen kører ikke. Genstart appen.",
                    ));
                    self.invalidate_approval();
                    return Task::none();
                };
                if !self.files.is_fixture && !self.setup.ready() {
                    return Task::none();
                }
                if self.approving.is_none() {
                    self.invalidate_approval();
                }
                let fixture = self.files.is_fixture;
                let from = self.week_start.format("%Y-%m-%d").to_string();
                let to = self.week_end().format("%Y-%m-%d").to_string();
                self.preview = PreviewState::Loading {
                    from: from.clone(),
                    to: to.clone(),
                };
                let request_from = from.clone();
                let request_to = to.clone();
                Task::perform(
                    async move {
                        let result = tokio::task::spawn_blocking(move || {
                            if fixture {
                                worker.preview_fixture(&from, &to)
                            } else {
                                worker.preview_connected(&from, &to)
                            }
                        })
                        .await
                        .map_err(|e| WorkerError::exited(format!("Intern opgave fejlede: {e}")))
                        .and_then(|inner| inner);
                        (request_from, request_to, result)
                    },
                    |(from, to, result)| Message::PreviewLoaded { from, to, result },
                )
            }
            Message::PreviewLoaded { from, to, result } => {
                // Ignore late arrivals for a week the user already left.
                // The check uses the week the request was sent for, not the
                // week a load happens to be in flight for: otherwise a slow
                // response for the previous week can overwrite the week the
                // user is looking at (or vice versa).
                let expected_from = self.week_start.format("%Y-%m-%d").to_string();
                let expected_to = self.week_end().format("%Y-%m-%d").to_string();
                if from == expected_from && to == expected_to {
                    if let Err(error) = &result {
                        if error.code == "worker_exit" {
                            self.worker = None;
                            self.startup_error = Some(error.clone());
                        }
                    }
                    if let Err(error) = &result {
                        if !self.files.is_fixture {
                            if let Some(state) = &mut self.setup.state {
                                state.stage = "destinations".to_string();
                            }
                            self.setup.error = Some(error.message.clone());
                        }
                    }
                    let approving = self.approving.take();
                    self.preview = match result {
                        Ok(preview) => {
                            if let Some(expected) = approving {
                                if expected == preview.digest && preview.week.can_apply {
                                    self.approval = Some(Approval {
                                        digest: preview.digest.clone(),
                                        summary: preview.week.apply_summary.clone(),
                                        week: format_week_da(self.week_start, self.week_end()),
                                    });
                                } else {
                                    self.plan_changed = true;
                                }
                            }
                            PreviewState::Ready(Box::new(preview))
                        }
                        Err(error) => PreviewState::Failed(error),
                    };
                }
                Task::none()
            }
            Message::Retry => {
                self.startup_error = None;
                self.home = HomeState::Loading;
                self.preview = PreviewState::Idle;
                self.invalidate_approval();
                Self::spawn_task(self.files.clone())
            }
            Message::ApprovalDismissed => {
                self.invalidate_approval();
                Task::none()
            }
            Message::ApproveRequested => {
                // Approval binds to the displayed plan, so the source and
                // both destinations are read again before anything is
                // treated as approved.
                let PreviewState::Ready(preview) = &self.preview else {
                    return Task::none();
                };
                if !preview.week.can_apply {
                    return Task::none();
                }
                self.approving = Some(preview.digest.clone());
                self.plan_changed = false;
                self.update(Message::PreviewRequested)
            }
        }
    }

    fn setup_task(&mut self, action: &'static str, params: serde_json::Value) -> Task<Message> {
        if self.setup.busy {
            return Task::none();
        }
        let Some(worker) = self.worker.clone() else {
            return Task::none();
        };
        self.setup.busy = true;
        self.setup.error = None;
        if action != "status" {
            self.preview = PreviewState::Idle;
            self.invalidate_approval();
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    worker.setup(action, params).and_then(|v| {
                        serde_json::from_value(v)
                            .map_err(|_| WorkerError::exited("Opsætningen kunne ikke læses."))
                    })
                })
                .await
                .map_err(|_| WorkerError::exited("Opsætningen stoppede."))
                .and_then(|r| r)
            },
            Message::SetupLoaded,
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        if self.worker.is_some() {
            iced::time::every(std::time::Duration::from_secs(3)).map(|_| Message::SessionTick)
        } else {
            Subscription::none()
        }
    }

    fn session_task(&mut self, login: Option<Service>) -> Task<Message> {
        if self.sessions_loading {
            return Task::none();
        }
        let Some(worker) = self.worker.clone() else {
            return Task::none();
        };
        self.sessions_loading = true;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || worker.sessions(login))
                    .await
                    .map_err(|_| WorkerError::exited("Loginstatus kunne ikke hentes."))
                    .and_then(|result| result)
            },
            Message::SessionsLoaded,
        )
    }

    fn shift_week(&mut self, days: i64) {
        self.week_start += chrono::Duration::days(days);
        self.preview = PreviewState::Idle;
        self.invalidate_approval();
    }

    /// Any change to dates, account, mapping or plan revokes approval: the
    /// next transfer must be approved from a preview the user actually saw.
    fn invalidate_approval(&mut self) {
        self.approving = None;
        self.approval = None;
        self.plan_changed = false;
    }

    fn reload_home(&mut self) -> Task<Message> {
        let Some(worker) = self.worker.clone() else {
            return Task::none();
        };
        self.home = HomeState::Loading;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || worker.home_status(None))
                    .await
                    .map_err(|e| WorkerError::exited(format!("Intern opgave fejlede: {e}")))
                    .and_then(|inner| inner)
            },
            Message::HomeLoaded,
        )
    }

    fn spawn_task(files: AppFiles) -> Task<Message> {
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let handle = WorkerHandle::spawn(&files)?;
                    let home = handle.home_status(None);
                    Ok::<_, WorkerError>(WorkerReady { handle, home })
                })
                .await
                .map_err(|e| WorkerError::exited(format!("Intern opgave fejlede: {e}")))
                .and_then(|inner| inner)
            },
            Message::WorkerSpawned,
        )
    }

    fn view(&self) -> Element<'_, Message> {
        if !self.files.is_fixture && !self.setup.ready() && self.startup_error.is_none() {
            let mut content = column![self.setup.view().map(Message::Setup)]
                .spacing(16)
                .padding(20)
                .max_width(760);
            if self
                .setup
                .state
                .as_ref()
                .is_some_and(|s| s.stage != "source")
            {
                for (service, name) in [(Service::Mithf, "MitHF"), (Service::Duos, "DUOS")] {
                    let status = self.sessions.as_ref().map(|s| match service {
                        Service::Mithf => &s.mithf,
                        Service::Duos => &s.duos,
                    });
                    content = content
                        .push(text(format!(
                            "{}: {}",
                            name,
                            status.map_or("Kontrollerer login …", |s| s.message.as_str())
                        )))
                        .push(
                            button(text(format!("Log ind i {name}")))
                                .padding(12)
                                .on_press(Message::Login(service)),
                        );
                }
            }
            return container(scrollable(content))
                .center_x(Length::Fill)
                .height(Length::Fill)
                .into();
        }
        let content = column![
            text("Vagtplanlægning").size(28),
            rule::horizontal(2),
            self.week_row(),
            rule::horizontal(1),
            self.status_section(),
            rule::horizontal(1),
            self.preview_section(),
        ]
        .spacing(12)
        .padding(20)
        // Wide enough for seven readable day columns in the week grid.
        .max_width(1100);

        container(scrollable(content))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .into()
    }

    fn week_row(&self) -> Element<'_, Message> {
        let label = format_week_da(self.week_start, self.week_end());
        column![
            text(label).size(20),
            row![
                button("‹ Forrige").padding(12).on_press(Message::WeekPrev),
                button("Denne uge")
                    .padding(12)
                    .on_press(Message::WeekCurrent),
                button("Næste ›").padding(12).on_press(Message::WeekNext),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .into()
    }

    fn status_section(&self) -> Element<'_, Message> {
        if let Some(error) = &self.startup_error {
            return failure_block("Appen kunne ikke starte baggrundsarbejderen.", error, true);
        }
        match &self.home {
            HomeState::Loading => text("Henter status …").into(),
            HomeState::Failed(error) => {
                failure_block("Status kunne ikke hentes.", error, self.worker.is_none())
            }
            HomeState::Ready(status) => {
                let mut items: Vec<Element<'_, Message>> = Vec::new();
                items.push(text("Tjenester").size(18).into());
                if self.setup.ready() || status.config_issues.is_empty() {
                    items.push(
                        status_line("TeamUp: klar til kontrol", "Konfigurationen er komplet.")
                            .into(),
                    );
                } else {
                    for issue in &status.config_issues {
                        items.push(status_line("TeamUp: opsætning mangler", issue).into());
                    }
                }
                if let Some(error) = &self.session_error {
                    items.push(text(error).into());
                }
                for (service, name) in [(Service::Mithf, "MitHF"), (Service::Duos, "DUOS")] {
                    let status = self.sessions.as_ref().map(|sessions| match service {
                        Service::Mithf => &sessions.mithf,
                        Service::Duos => &sessions.duos,
                    });
                    let label = match status.map(|s| s.state) {
                        Some(LoginState::Connected) => "forbundet",
                        Some(LoginState::Connecting) => "forbinder …",
                        Some(LoginState::Unavailable) => "ikke tilgængelig",
                        Some(LoginState::SignInRequired) => "login kræves",
                        None => "kontrollerer login …",
                    };
                    let mut action = button("Log ind").padding(12);
                    if !self.sessions_loading
                        && status.is_some()
                        && !matches!(status.map(|s| s.state), Some(LoginState::Connecting))
                    {
                        action = action.on_press(Message::Login(service));
                    }
                    items.push(
                        row![text(format!("{name}: {label}")), action]
                            .spacing(12)
                            .into(),
                    );
                    if let Some(status) = status {
                        items.push(text(&status.message).size(13).into());
                    }
                }
                let transfer = match &status.last_verified_at {
                    Some(at) => format!(
                        "Sidst verificeret overførsel: {} ({} verificerede trin)",
                        at, status.verified_steps
                    ),
                    None => "Sidst verificeret overførsel: aldrig".to_string(),
                };
                items.push(text(transfer).into());
                column(items).spacing(4).into()
            }
        }
    }

    fn preview_section(&self) -> Element<'_, Message> {
        let mut section = column![text("Ugens ændringer").size(18)].spacing(8);
        if self.files.is_fixture {
            section = section.push(text("Viser testuge (eksempeldata).").size(13));
        }
        let body: Element<'_, Message> = match &self.preview {
            PreviewState::Idle => button("Se ændringer")
                .padding(14)
                .on_press(Message::PreviewRequested)
                .into(),
            PreviewState::Loading { .. } => text("Henter ugens ændringer …").into(),
            PreviewState::Failed(error) => {
                if self.worker.is_none() {
                    failure_block("Ugens ændringer kunne ikke hentes.", error, true)
                } else {
                    column![
                        failure_block("Ugens ændringer kunne ikke hentes.", error, false),
                        button("Prøv igen")
                            .padding(12)
                            .on_press(Message::PreviewRequested),
                    ]
                    .spacing(8)
                    .into()
                }
            }
            PreviewState::Ready(preview) => self.week_view(&preview.week),
        };
        section.push(body).into()
    }

    /// The readable week: what needs attention, the grid, then the details.
    fn week_view<'a>(&'a self, week: &'a Week) -> Element<'a, Message> {
        let mut view = column![text(&week.headline).size(16)].spacing(10);
        if !week.notice.is_empty() {
            view = view.push(text(&week.notice).size(13));
        }
        if !week.attention.is_empty() {
            let mut block = column![text("Kræver opmærksomhed").size(16)].spacing(6);
            for item in &week.attention {
                block = block.push(
                    column![
                        text(format!("{} · {}", item.when, item.who)).size(13),
                        text(&item.explanation).size(14),
                        text(format!("Gør sådan: {}", item.action)).size(13),
                    ]
                    .spacing(1),
                );
            }
            view = view.push(block);
        }
        if week.days.iter().any(|day| !day.blocks.is_empty()) {
            view = view.push(week_grid(week)).push(day_details(week));
        }
        if !week.summary.is_empty() {
            let mut lines = column![text("Det overføres").size(16)].spacing(2);
            for line in &week.summary {
                lines = lines.push(text(format!("• {line}")).size(14));
            }
            view = view.push(lines);
        }
        view.push(self.approval_row(week)).into()
    }

    /// The primary action. Enabled only for a reconciled, unblocked plan
    /// that actually writes something.
    fn approval_row<'a>(&'a self, week: &'a Week) -> Element<'a, Message> {
        if let Some(approval) = &self.approval {
            return column![
                text(format!("Godkendt: {}", approval.week)).size(16),
                text(&approval.summary).size(14),
                text(
                    "Selve overførslen til MitHF og DUOS er ikke bygget endnu, \
                     så intet er sendt. Godkendelsen gælder kun den uge, du ser."
                )
                .size(13),
                button("Gennemgå ugen igen")
                    .padding(12)
                    .on_press(Message::ApprovalDismissed),
            ]
            .spacing(6)
            .into();
        }
        let mut block = column![].spacing(6);
        if self.plan_changed {
            block = block.push(
                text(
                    "Ugen har ændret sig, siden du så den. Gennemgå den nye \
                     visning, og godkend igen.",
                )
                .size(14),
            );
        }
        if week.can_apply {
            block = block.push(text(&week.apply_summary).size(14)).push(
                button("Overfør ændringer")
                    .padding(14)
                    .on_press(Message::ApproveRequested),
            );
        } else if !week.blocked_reason.is_empty() {
            block = block
                .push(text(&week.blocked_reason).size(14))
                .push(button("Overfør ændringer").padding(14));
        }
        block
            .push(
                button("Se ændringer igen")
                    .padding(12)
                    .on_press(Message::PreviewRequested),
            )
            .into()
    }
}

const GRID_HEIGHT: f32 = 520.0;

/// Vagtmøde participants share clock times, so overlapping blocks need
/// separate lanes. Later shifts reuse the first available lane.
fn grid_lanes(blocks: &[Block]) -> Vec<Vec<&Block>> {
    let mut sorted: Vec<_> = blocks.iter().collect();
    sorted.sort_by_key(|block| block.minutes_from);
    let mut lanes: Vec<Vec<&Block>> = vec![Vec::new()];
    for block in sorted {
        if let Some(lane) = lanes.iter_mut().find(|lane| {
            lane.last()
                .is_none_or(|previous| previous.minutes_to <= block.minutes_from)
        }) {
            lane.push(block);
        } else {
            lanes.push(vec![block]);
        }
    }
    lanes
}

fn detail_blocks(blocks: &[Block], first_day: bool) -> impl Iterator<Item = &Block> {
    // A shift starting before Monday has no earlier visible piece.
    blocks
        .iter()
        .filter(move |block| first_day || !block.continues_before)
}

/// Seven day columns with each shift drawn over the hours it covers.
fn week_grid(week: &Week) -> Element<'_, Message> {
    let mut hours = column![].width(Length::Fixed(28.0));
    for hour in 0..24 {
        hours = hours.push(
            container(text(format!("{hour:02}")).size(9))
                .height(Length::Fixed(GRID_HEIGHT / 24.0))
                .width(Length::Fill),
        );
    }
    let mut columns = row![column![
        text(" ").size(11),
        container(hours).height(Length::Fixed(GRID_HEIGHT)),
    ]
    .spacing(4)]
    .spacing(4);
    for day in &week.days {
        let mut lanes = row![].spacing(2).width(Length::Fill);
        for blocks in grid_lanes(&day.blocks) {
            let mut lane = column![].width(Length::Fill);
            let mut cursor = 0;
            for block in blocks {
                let from = block.minutes_from.min(1439);
                let to = block.minutes_to.clamp(from + 1, 1440);
                if from > cursor {
                    lane = lane.push(
                        space::vertical()
                            .height(Length::Fixed(GRID_HEIGHT * (from - cursor) as f32 / 1440.0)),
                    );
                }
                lane = lane.push(
                    container(block_body(block))
                        .height(Length::Fixed(GRID_HEIGHT * (to - from) as f32 / 1440.0))
                        .width(Length::Fill)
                        .padding(3)
                        .clip(true)
                        .style(block_style(&block.status, &block.helper_color)),
                );
                cursor = to;
            }
            if cursor < 1440 {
                lane = lane.push(
                    space::vertical()
                        .height(Length::Fixed(GRID_HEIGHT * (1440 - cursor) as f32 / 1440.0)),
                );
            }
            lanes = lanes.push(lane);
        }
        columns = columns.push(
            column![
                text(&day.label).size(11),
                container(lanes)
                    .height(Length::Fixed(GRID_HEIGHT))
                    .width(Length::Fill),
            ]
            .spacing(4)
            .width(Length::FillPortion(1)),
        );
    }
    columns.into()
}

fn block_body(block: &Block) -> Element<'_, Message> {
    let mut body = column![
        text(format!("{} {}", status_marker(&block.status), block.helper)).size(11),
        text(&block.time_label).size(10),
    ]
    .spacing(0);
    if !block.sps_label.is_empty() {
        body = body.push(text(format!("◆ SPS {}", block.sps_label)).size(10));
    }
    if !block.part_label.is_empty() {
        body = body.push(text(&block.part_label).size(9));
    }
    if block.continues_before {
        body = body.push(text("▲ fortsat fra dagen før").size(9));
    }
    if block.continues_after {
        body = body.push(text("▼ fortsætter i morgen").size(9));
    }
    body.into()
}

/// The same week as text, with the values the grid has no room for.
fn day_details(week: &Week) -> Element<'_, Message> {
    let mut list = column![text("Dag for dag").size(16)].spacing(8);
    for (index, day) in week.days.iter().enumerate() {
        let mut blocks = detail_blocks(&day.blocks, index == 0).peekable();
        if blocks.peek().is_none() {
            continue;
        }
        let mut entry = column![text(&day.label).size(14)].spacing(2);
        for block in blocks {
            let mut heading = format!(
                "{} {} · {} · {}",
                status_marker(&block.status),
                block.helper,
                block.time_label,
                block.status_label
            );
            if !block.part_label.is_empty() {
                heading.push_str(&format!(" · {}", block.part_label));
            }
            entry = entry.push(text(heading).size(13));
            for detail in &block.details {
                entry = entry.push(text(format!("    {detail}")).size(13));
            }
        }
        list = list.push(entry);
    }
    list.into()
}

/// Text cue beside every colour, so status never depends on colour alone.
fn status_marker(status: &str) -> &'static str {
    match status {
        "create" => "[NY]",
        "update" => "[ÆNDRET]",
        "matched" => "[OK]",
        "attention" => "[!]",
        _ => "[?]",
    }
}

/// Fill the block with the helper's own Teamup colour, tinted so the text
/// stays readable, and outline it in the status colour. Colour therefore
/// carries who is on the shift, exactly as in Teamup, while status keeps the
/// outline plus its text marker and the day-by-day list below the grid.
fn block_style(
    status: &str,
    helper_color: &str,
) -> impl Fn(&iced::Theme) -> container::Style + use<> {
    let accent = status_color(status);
    let fill = parse_hex(helper_color)
        .map(|color| tint(color, 0.72))
        .unwrap_or(Color::from_rgb8(0xE8, 0xE8, 0xE8));
    move |_theme: &iced::Theme| container::Style {
        background: Some(fill.into()),
        // Tinted fills are light in both themes, so the text is always dark.
        text_color: Some(Color::from_rgb8(0x1A, 0x1A, 0x1A)),
        border: iced::Border {
            color: accent,
            width: 2.0,
            radius: 4.0.into(),
        },
        ..container::Style::default()
    }
}

/// Outline colour per status, alongside the text marker in `status_marker`.
fn status_color(status: &str) -> Color {
    match status {
        "create" => Color::from_rgb8(0x1B, 0x5E, 0x20),
        "update" => Color::from_rgb8(0x7A, 0x33, 0x00),
        "matched" => Color::from_rgb8(0x75, 0x75, 0x75),
        "attention" => Color::from_rgb8(0xB7, 0x1C, 0x1C),
        _ => Color::from_rgb8(0x0D, 0x47, 0xA1),
    }
}

/// Mix towards white. Teamup's palette is saturated; the raw colours would
/// drown the block text.
fn tint(color: Color, amount: f32) -> Color {
    Color::from_rgb(
        color.r + (1.0 - color.r) * amount,
        color.g + (1.0 - color.g) * amount,
        color.b + (1.0 - color.b) * amount,
    )
}

fn parse_hex(value: &str) -> Option<Color> {
    let digits = value.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some(Color::from_rgb8(channel(0)?, channel(2)?, channel(4)?))
}

fn status_line<'a>(label: &'a str, detail: &'a str) -> iced::widget::Column<'a, Message> {
    column![text(label), text(detail).size(13)].spacing(0)
}

fn failure_block<'a>(
    message: &'a str,
    error: &'a WorkerError,
    retry: bool,
) -> Element<'a, Message> {
    let mut block = column![
        text(message).size(16),
        text(error.message.clone()),
        text(error.detail.clone()).size(12),
    ]
    .spacing(4);
    if retry {
        block = block.push(button("Prøv igen").padding(12).on_press(Message::Retry));
    }
    block.into()
}

fn format_week_da(start: NaiveDate, end: NaiveDate) -> String {
    let sm = DANISH_MONTHS[start.month0() as usize];
    let em = DANISH_MONTHS[end.month0() as usize];
    if start.month() == end.month() {
        format!(
            "Uge {}: {}.–{}. {} {}",
            start.iso_week().week(),
            start.day(),
            end.day(),
            sm,
            start.year()
        )
    } else {
        format!(
            "Uge {}: {}. {} – {}. {} {}",
            start.iso_week().week(),
            start.day(),
            sm,
            end.day(),
            em,
            start.year()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::path::PathBuf;
    use teamup_shift_sync_gui::worker::AppFiles;

    fn test_app() -> App {
        App {
            setup: setup::SetupUi::default(),
            files: AppFiles {
                config_path: PathBuf::from("fixtures/offline-config.toml"),
                state_path: PathBuf::from(":memory:"),
                fixture_path: PathBuf::from("fixtures/representative-week.json"),
                is_fixture: true,
            },
            worker: None,
            startup_error: None,
            week_start: NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(),
            home: HomeState::Loading,
            preview: PreviewState::Idle,
            sessions: None,
            sessions_loading: false,
            session_error: None,
            approving: None,
            approval: None,
            plan_changed: false,
        }
    }

    fn ready_home() -> HomeStatus {
        serde_json::from_value(serde_json::json!({
            "timezone": "Europe/Copenhagen",
            "week_start": "2026-09-14",
            "week_end": "2026-09-20",
            "config_issues": [],
            "last_verified_at": null,
            "verified_steps": 0,
            "login_state": "unknown",
            "login_detail": "Login via app-browser følger i næste trin."
        }))
        .unwrap()
    }

    fn ready_preview() -> PlanPreview {
        serde_json::from_value(serde_json::json!({
            "starts_at": "2026-09-14T00:00:00+02:00",
            "ends_at": "2026-09-21T00:00:00+02:00",
            "generated_at": "2026-09-20T20:00:00+02:00",
            "digest": "abc123",
            "has_conflicts": false,
            "counts": {"already_matched": 3, "would_create": 5, "review": 1},
            "blockers": [],
            "week": {
                "days": [
                    {"date": "2026-09-14", "label": "man 14. sep", "blocks": [
                        {"helper": "Zain Alnemr", "status": "matched",
                         "status_label": "Uændret", "minutes_from": 450,
                         "minutes_to": 780, "time_label": "07:30–13:00",
                         "sps_label": "08:00–10:00", "part_label": "Del 1 af 2",
                         "continues_before": false, "continues_after": false,
                         "details": ["SPS-timer 08:00–10:00 er allerede sat."]},
                        {"helper": "Zain Alnemr", "status": "create",
                         "status_label": "Oprettes", "minutes_from": 780,
                         "minutes_to": 1440, "time_label": "13:00–24:00",
                         "sps_label": "13:00–14:00", "part_label": "Del 2 af 2",
                         "continues_before": false, "continues_after": true,
                         "details": ["Vagten oprettes i MitHF."]}
                    ]},
                    {"date": "2026-09-15", "label": "tir 15. sep", "blocks": [
                        {"helper": "Zain Alnemr", "status": "create",
                         "status_label": "Oprettes", "minutes_from": 0,
                         "minutes_to": 420, "time_label": "13:00 14. sep – 07:00 15. sep",
                         "sps_label": "", "part_label": "",
                         "continues_before": true, "continues_after": false,
                         "details": []}
                    ]}
                ],
                "attention": [],
                "headline": "Ugen er klar til overførsel.",
                "notice": "",
                "summary": ["1 ny vagt i MitHF"],
                "apply_summary": "Overfører 1 ny vagt til MitHF.",
                "can_apply": true, "blocked_reason": "", "destination_read": true
            },
            "items": [
                {
                    "source_key": "k", "system": "mithf",
                    "step_key": "mithf.create_shift", "outcome": "would_create",
                    "summary": "Would create the MitHF shift",
                    "payload": {}, "destination_id": null
                },
                {
                    "source_key": "k2", "system": "duos",
                    "step_key": "duos.interval:1", "outcome": "already_matched",
                    "summary": "DUOS registration already matches",
                    "payload": {}, "destination_id": "duos-1"
                }
            ]
        }))
        .unwrap()
    }

    fn failed() -> WorkerError {
        WorkerError::exited("proof detail")
    }

    /// Every UI state must build its widget tree without panicking, so the
    /// loading, empty, and failure states are covered even where no display
    /// is available (headless CI, Xvfb without DRI3).
    #[test]
    fn all_states_render_without_panicking() {
        let mut app = test_app();
        let _ = app.view(); // home loading + preview idle

        app.home = HomeState::Ready(ready_home());
        let _ = app.view(); // home ready, never transferred

        app.preview = PreviewState::Loading {
            from: "2026-09-14".to_string(),
            to: "2026-09-20".to_string(),
        };
        let _ = app.view(); // preview loading

        app.preview = PreviewState::Ready(Box::new(ready_preview()));
        let _ = app.view(); // preview with attention items + rows

        let mut empty = ready_preview();
        empty.items = Vec::new();
        empty.counts.clear();
        empty.blockers.clear();
        app.preview = PreviewState::Ready(Box::new(empty));
        let _ = app.view(); // empty week

        app.preview = PreviewState::Failed(failed());
        app.home = HomeState::Failed(failed());
        let _ = app.view(); // failure states with retry

        app.startup_error = Some(WorkerError::startup("no python"));
        let _ = app.view(); // startup recovery screen

        app.files.is_fixture = false;
        app.startup_error = None;
        app.home = HomeState::Loading;
        app.preview = PreviewState::Idle;
        let _ = app.view(); // missing fixture hint
    }

    #[test]
    fn week_navigation_shifts_monday_and_invalidates_preview() {
        let mut app = test_app();
        app.preview = PreviewState::Ready(Box::new(ready_preview()));

        let _ = app.update(Message::WeekNext);
        assert_eq!(
            app.week_start,
            NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()
        );
        assert!(matches!(app.preview, PreviewState::Idle));

        let _ = app.update(Message::WeekPrev);
        let _ = app.update(Message::WeekPrev);
        assert_eq!(app.week_start, NaiveDate::from_ymd_opt(2026, 9, 7).unwrap());
    }

    #[test]
    fn stale_preview_arrivals_are_ignored() {
        let mut app = test_app();
        // User already navigated away from the loading week.
        app.preview = PreviewState::Loading {
            from: "2026-09-21".to_string(),
            to: "2026-09-27".to_string(),
        };
        app.week_start = NaiveDate::from_ymd_opt(2026, 9, 28).unwrap();
        let _ = app.update(Message::PreviewLoaded {
            from: "2026-09-21".to_string(),
            to: "2026-09-27".to_string(),
            result: Ok(ready_preview()),
        });
        assert!(matches!(app.preview, PreviewState::Loading { .. }));
    }

    #[test]
    fn slow_previous_response_does_not_overwrite_current_week() {
        // Reported bug: navigate two weeks back, ask for that week, and a
        // slow response for the current week overwrote the old week's grid
        // (header showed Uge 36 while the grid showed 14.–20. september).
        let mut app = test_app();
        app.week_start = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        app.preview = PreviewState::Loading {
            from: "2026-08-31".to_string(),
            to: "2026-09-06".to_string(),
        };
        // The stale current-week response arrives while the old week loads:
        // it must be ignored even though a load is in flight.
        let _ = app.update(Message::PreviewLoaded {
            from: "2026-09-14".to_string(),
            to: "2026-09-20".to_string(),
            result: Ok(ready_preview()),
        });
        assert!(matches!(app.preview, PreviewState::Loading { .. }));
        assert!(app.approval.is_none());
        // The response for the week on screen is accepted.
        let _ = app.update(Message::PreviewLoaded {
            from: "2026-08-31".to_string(),
            to: "2026-09-06".to_string(),
            result: Ok(ready_preview()),
        });
        assert!(matches!(app.preview, PreviewState::Ready(_)));
    }

    #[test]
    fn preview_without_worker_is_a_readable_failure() {
        let mut app = test_app();
        let _ = app.update(Message::PreviewRequested);
        assert!(matches!(app.preview, PreviewState::Failed(_)));
    }

    #[test]
    fn unexpected_worker_exit_offers_a_fresh_spawn() {
        let mut app = test_app();
        app.preview = PreviewState::Loading {
            from: "2026-09-14".to_string(),
            to: "2026-09-20".to_string(),
        };
        let _ = app.update(Message::PreviewLoaded {
            from: "2026-09-14".to_string(),
            to: "2026-09-20".to_string(),
            result: Err(WorkerError::exited("stopped")),
        });
        assert!(app.worker.is_none());
        assert!(app.startup_error.is_some());
        let _ = app.view();
    }

    fn setup_state(stage: &str) -> setup::SetupState {
        serde_json::from_value(serde_json::json!({
            "stage": stage, "week_start":"2026-09-14", "calendars":[{"id":"1","name":"Helper"}],
            "arrangements":[{"id":"4","name":"SPS"}], "types":[{"id":"0","name":"Timer"}],
            "mithf":[{"id":"2","name":"Helper"}], "duos":[{"id":"3","name":"Helper"}],
            "mappings":[{"source":"1","mithf":"2","duos":"3","excluded":false}],
            "arrangement":"4", "registration_type":"0", "account":"Kunde", "notice":"",
            "can_import":true, "has_credentials":true
        }))
        .unwrap()
    }

    #[test]
    fn setup_screens_render_and_preserve_partial_choices_on_failure() {
        let mut app = test_app();
        app.files.is_fixture = false;
        for stage in ["source", "destinations", "helpers"] {
            app.setup.state = Some(setup_state(stage));
            let _ = app.view();
        }
        app.setup.busy = true;
        let _ = app.view();
        let _ = app.update(Message::SetupLoaded(Err(WorkerError {
            code: "setup_error".into(),
            message: "Log ind igen".into(),
            detail: String::new(),
        })));
        assert!(!app.setup.busy);
        assert_eq!(app.setup.state.as_ref().unwrap().mappings[0].duos, "3");
        assert_eq!(app.setup.error.as_deref(), Some("Log ind igen"));
        let _ = app.view();
    }

    #[test]
    fn changed_mapping_returns_to_setup_without_retrying_preview() {
        let mut app = test_app();
        app.files.is_fixture = false;
        app.setup.state = Some(setup_state("ready"));
        app.preview = PreviewState::Loading {
            from: "2026-09-14".into(),
            to: "2026-09-20".into(),
        };
        let _ = app.update(Message::PreviewLoaded {
            from: "2026-09-14".to_string(),
            to: "2026-09-20".to_string(),
            result: Err(WorkerError {
                code: "setup_error".into(),
                message: "Bekræft igen".into(),
                detail: String::new(),
            }),
        });
        assert!(!app.setup.ready());
        assert!(!app.setup.busy);
        assert!(matches!(app.preview, PreviewState::Failed(_)));
        let _ = app.view();
    }

    /// Stand in for the worker round trip: the app has asked for a fresh
    /// read of the week it is trying to approve and is waiting for it.
    fn awaiting_reread(app: &mut App, digest: &str) {
        app.approving = Some(digest.to_string());
        app.preview = PreviewState::Loading {
            from: "2026-09-14".to_string(),
            to: "2026-09-20".to_string(),
        };
    }

    /// The click approves exactly the plan on screen: the app re-reads
    /// before treating anything as approved, and only a matching digest
    /// counts.
    #[test]
    fn approval_requires_an_unchanged_re_read() {
        let mut app = test_app();
        awaiting_reread(&mut app, "abc123");
        assert!(app.approval.is_none(), "not approved before the re-read");

        let _ = app.update(Message::PreviewLoaded {
            from: "2026-09-14".to_string(),
            to: "2026-09-20".to_string(),
            result: Ok(ready_preview()),
        });
        let approval = app.approval.as_ref().expect("approved");
        assert_eq!(approval.digest, "abc123");
        assert_eq!(approval.summary, "Overfører 1 ny vagt til MitHF.");
        assert!(!app.plan_changed);
        let _ = app.view();
    }

    #[test]
    fn a_changed_plan_returns_to_review_instead_of_approving() {
        let mut app = test_app();
        awaiting_reread(&mut app, "abc123");

        let mut changed = ready_preview();
        changed.digest = "def456".to_string();
        let _ = app.update(Message::PreviewLoaded {
            from: "2026-09-14".to_string(),
            to: "2026-09-20".to_string(),
            result: Ok(changed),
        });

        assert!(app.approval.is_none());
        assert!(app.plan_changed);
        let _ = app.view();
    }

    /// A week that became blocked between the click and the re-read must not
    /// approve either, even though nothing else changed.
    #[test]
    fn a_week_blocked_since_the_click_is_not_approved() {
        let mut app = test_app();
        awaiting_reread(&mut app, "abc123");

        let mut blocked = ready_preview();
        blocked.week.can_apply = false;
        blocked.week.blocked_reason = "Løs punkterne først.".to_string();
        let _ = app.update(Message::PreviewLoaded {
            from: "2026-09-14".to_string(),
            to: "2026-09-20".to_string(),
            result: Ok(blocked),
        });

        assert!(app.approval.is_none());
        assert!(app.plan_changed);
        let _ = app.view();
    }

    #[test]
    fn a_blocked_week_cannot_be_approved() {
        let mut app = test_app();
        let mut blocked = ready_preview();
        blocked.week.can_apply = false;
        blocked.week.blocked_reason = "Løs punkterne først.".to_string();
        app.preview = PreviewState::Ready(Box::new(blocked));

        let _ = app.update(Message::ApproveRequested);
        assert!(app.approving.is_none());
        assert!(app.approval.is_none());
        let _ = app.view();
    }

    #[test]
    fn changing_the_week_or_the_mapping_revokes_approval() {
        for revoke in [Message::WeekNext, Message::WeekCurrent, Message::Retry] {
            let mut app = test_app();
            awaiting_reread(&mut app, "abc123");
            let _ = app.update(Message::PreviewLoaded {
                from: "2026-09-14".to_string(),
                to: "2026-09-20".to_string(),
                result: Ok(ready_preview()),
            });
            assert!(app.approval.is_some());

            let _ = app.update(revoke);
            assert!(app.approval.is_none());
        }
    }

    /// A helper colour tints the fill; anything unusable falls back to a
    /// neutral block rather than a wrong or unreadable one.
    #[test]
    fn helper_colours_parse_and_fall_back() {
        assert_eq!(
            parse_hex("#4770d8"),
            Some(Color::from_rgb8(0x47, 0x70, 0xd8))
        );
        for bad in ["", "#4770d", "4770d8", "#zzzzzz", "#4770d8ff"] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
        let tinted = tint(Color::from_rgb8(0, 0, 0), 0.72);
        assert!(tinted.r > 0.7 && tinted.r < 0.75);
        // Every state builds a style, with and without a colour.
        for status in ["create", "update", "matched", "attention", "pending"] {
            let _ = block_style(status, "#4770d8")(&iced::Theme::Light);
            let _ = block_style(status, "")(&iced::Theme::Dark);
        }
    }

    #[test]
    fn meeting_helpers_share_times_but_not_lanes() {
        let template = ready_preview().week.days[0].blocks[0].clone();
        let mut first = template.clone();
        first.minutes_from = 480;
        first.minutes_to = 720;
        first.details = vec!["Vagtmøde sættes på hele vagten.".into()];
        let mut second = first.clone();
        second.helper = "Anden hjælper".into();
        let mut later = template;
        later.minutes_from = 720;
        later.minutes_to = 900;
        let blocks = vec![first, second, later];
        let lanes = grid_lanes(&blocks);
        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0][0].minutes_from, lanes[1][0].minutes_from);
        assert_eq!(lanes[0][0].minutes_to, lanes[1][0].minutes_to);
        assert_eq!(lanes[0][1].minutes_from, 720);
        for lane in lanes {
            assert!(lane
                .windows(2)
                .all(|pair| pair[0].minutes_to <= pair[1].minutes_from));
        }
    }

    #[test]
    fn monday_continuation_keeps_details_without_repeating_them_on_tuesday() {
        let mut block = ready_preview().week.days[0].blocks[0].clone();
        block.continues_before = true;
        block.details = vec!["DUOS: registreringen ændres: 22:00–23:00 → 22:00–24:00.".into()];
        let blocks = vec![block];
        let visible: Vec<_> = detail_blocks(&blocks, true).collect();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].details, blocks[0].details);
        assert_eq!(detail_blocks(&blocks, false).count(), 0);
    }

    #[test]
    fn week_label_formats_danish() {
        let start = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
        assert_eq!(format_week_da(start, end), "Uge 38: 14.–20. september 2026");
        let cross = format_week_da(
            NaiveDate::from_ymd_opt(2026, 8, 31).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 6).unwrap(),
        );
        assert!(cross.contains("august") && cross.contains("september"));
    }
}
