//! Danish Iced desktop shell for weekly shift transfers (issue #2).
//!
//! One primary action per screen, text alongside status colors, and technical
//! diagnostics kept out of the normal flow. Network/browser/SQLite work runs
//! in the app-owned Python worker on blocking threads — never on the UI
//! thread — and every worker failure lands in a readable recovery state.

use chrono::{Datelike, Local, NaiveDate};
use iced::widget::{button, column, container, row, rule, scrollable, space, text};
use iced::{Element, Length, Task};
use teamup_shift_sync_gui::protocol::{HomeStatus, PlanPreview, WorkerError};
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

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Vagtplanlægning")
        .run()
}

#[derive(Debug, Clone)]
enum Message {
    WorkerSpawned(Result<WorkerReady, WorkerError>),
    HomeLoaded(Result<HomeStatus, WorkerError>),
    WeekPrev,
    WeekNext,
    WeekCurrent,
    PreviewRequested,
    PreviewLoaded(Result<PlanPreview, WorkerError>),
    Retry,
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
    Ready(PlanPreview),
    Failed(WorkerError),
}

struct App {
    files: AppFiles,
    worker: Option<WorkerHandle>,
    startup_error: Option<WorkerError>,
    week_start: NaiveDate,
    home: HomeState,
    preview: PreviewState,
}

fn current_monday() -> NaiveDate {
    let today = Local::now().date_naive();
    today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64)
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let files = AppFiles::resolve();
        let app = Self {
            files: files.clone(),
            worker: None,
            startup_error: None,
            week_start: current_monday(),
            home: HomeState::Loading,
            preview: PreviewState::Idle,
        };
        (app, Self::spawn_task(files))
    }

    fn week_end(&self) -> NaiveDate {
        self.week_start + chrono::Duration::days(6)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WorkerSpawned(Err(error)) => {
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
                Task::none()
            }
            Message::HomeLoaded(result) => {
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
                self.reload_home()
            }
            Message::PreviewRequested => {
                let Some(worker) = self.worker.clone() else {
                    self.preview = PreviewState::Failed(WorkerError::exited(
                        "Baggrundsarbejderen kører ikke. Genstart appen.",
                    ));
                    return Task::none();
                };
                let from = self.week_start.format("%Y-%m-%d").to_string();
                let to = self.week_end().format("%Y-%m-%d").to_string();
                self.preview = PreviewState::Loading {
                    from: from.clone(),
                    to: to.clone(),
                };
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || worker.preview_fixture(&from, &to))
                            .await
                            .map_err(|e| WorkerError::exited(format!("Intern opgave fejlede: {e}")))
                            .and_then(|inner| inner)
                    },
                    Message::PreviewLoaded,
                )
            }
            Message::PreviewLoaded(result) => {
                // Ignore late arrivals for a week the user already left.
                let current = matches!(&self.preview, PreviewState::Loading { from, to }
                    if *from == self.week_start.format("%Y-%m-%d").to_string()
                        && *to == self.week_end().format("%Y-%m-%d").to_string());
                if current {
                    self.preview = match result {
                        Ok(preview) => PreviewState::Ready(preview),
                        Err(error) => PreviewState::Failed(error),
                    };
                }
                Task::none()
            }
            Message::Retry => {
                self.startup_error = None;
                self.home = HomeState::Loading;
                self.preview = PreviewState::Idle;
                Self::spawn_task(self.files.clone())
            }
        }
    }

    fn shift_week(&mut self, days: i64) {
        self.week_start += chrono::Duration::days(days);
        self.preview = PreviewState::Idle;
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
        .max_width(720);

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
            HomeState::Failed(error) => failure_block("Status kunne ikke hentes.", error, false),
            HomeState::Ready(status) => {
                let mut items: Vec<Element<'_, Message>> = Vec::new();
                items.push(text("Tjenester").size(18).into());
                if status.config_issues.is_empty() {
                    items.push(
                        status_line("Konfiguration: klar", "Konfigurationen er komplet.").into(),
                    );
                } else {
                    for issue in &status.config_issues {
                        items.push(status_line("Konfiguration: mangler", issue).into());
                    }
                }
                items.push(
                    status_line(
                        "Login: afventer",
                        "Login via app-browser følger i næste trin.",
                    )
                    .into(),
                );
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
        } else {
            section = section.push(
                text("Eksempeldata mangler — sæt TEAMUP_FIXTURE for at vise en testuge.").size(13),
            );
        }
        let body: Element<'_, Message> = match &self.preview {
            PreviewState::Idle => button("Se ændringer")
                .padding(14)
                .on_press(Message::PreviewRequested)
                .into(),
            PreviewState::Loading { .. } => text("Henter eksempel …").into(),
            PreviewState::Failed(error) => column![
                failure_block("Eksemplet kunne ikke hentes.", error, false),
                button("Prøv igen")
                    .padding(12)
                    .on_press(Message::PreviewRequested),
            ]
            .spacing(8)
            .into(),
            PreviewState::Ready(preview) => {
                let mut view = column![text(counts_line_da(&preview.counts)).size(16)].spacing(8);
                if preview.items.is_empty() {
                    view = view.push(text("Ingen vagter i denne uge."));
                } else if !preview.blockers.is_empty() {
                    let mut attention = column![text("Kræver opmærksomhed").size(16)].spacing(4);
                    for blocker in preview.blockers.iter().take(20) {
                        attention = attention.push(text(format!("• {blocker}")).size(14));
                    }
                    view = view.push(attention);
                } else {
                    view = view.push(text("Ingen ændringer at overføre i denne uge."));
                }
                let shown = preview.items.iter().take(50);
                let mut rows = column![text("Alle punkter").size(16)].spacing(2);
                for item in shown {
                    rows = rows.push(
                        text(format!(
                            "• [{}] {} — {}",
                            outcome_da(&item.outcome),
                            item.step_key,
                            item.summary
                        ))
                        .size(13),
                    );
                }
                if preview.items.len() > 50 {
                    rows = rows.push(
                        text(format!("… og {} punkter mere.", preview.items.len() - 50)).size(13),
                    );
                }
                view = view.push(rows).push(space::vertical().height(4)).push(
                    button("Se ændringer igen")
                        .padding(12)
                        .on_press(Message::PreviewRequested),
                );
                view.into()
            }
        };
        section.push(body).into()
    }
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

fn outcome_da(outcome: &str) -> &str {
    match outcome {
        "already_matched" => "matcher",
        "would_create" => "oprettes",
        "would_update" => "opdateres",
        "review" => "gennemgå",
        "conflicted" => "konflikt",
        "failed" => "fejlet",
        "excluded" => "udeladt",
        "pending_integration" => "afventer",
        "pending_mithf_bug" => "afventer",
        other => other,
    }
}

fn counts_line_da(counts: &std::collections::BTreeMap<String, u64>) -> String {
    const ORDER: [&str; 9] = [
        "already_matched",
        "would_create",
        "would_update",
        "review",
        "conflicted",
        "failed",
        "excluded",
        "pending_integration",
        "pending_mithf_bug",
    ];
    let mut parts = Vec::new();
    for key in ORDER {
        if let Some(n) = counts.get(key) {
            parts.push(format!("{n} {}", outcome_da_plural(key, *n)));
        }
    }
    for (key, n) in counts {
        if !ORDER.contains(&key.as_str()) {
            parts.push(format!("{n} {key}"));
        }
    }
    if parts.is_empty() {
        return "Ingen punkter.".to_string();
    }
    parts.join(" · ")
}

fn outcome_da_plural(outcome: &str, n: u64) -> String {
    let singular = outcome_da(outcome);
    if n == 1 {
        singular.to_string()
    } else {
        format!("{singular}")
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
            "blockers": ["k mithf.set_sps: gennemgå SPS-teksten"],
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

        app.preview = PreviewState::Ready(ready_preview());
        let _ = app.view(); // preview with attention items + rows

        let mut empty = ready_preview();
        empty.items = Vec::new();
        empty.counts.clear();
        empty.blockers.clear();
        app.preview = PreviewState::Ready(empty);
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
        app.preview = PreviewState::Ready(ready_preview());

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
        let _ = app.update(Message::PreviewLoaded(Ok(ready_preview())));
        assert!(matches!(app.preview, PreviewState::Loading { .. }));
    }

    #[test]
    fn preview_without_worker_is_a_readable_failure() {
        let mut app = test_app();
        let _ = app.update(Message::PreviewRequested);
        assert!(matches!(app.preview, PreviewState::Failed(_)));
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
