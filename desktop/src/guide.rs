//! The first-run guide: from a fresh install to the first week, one step at
//! a time.
//!
//! It shows the same setup forms as Indstillinger, in the order they are
//! needed, with what each step is for. Every choice saves as it is made, as
//! in Indstillinger, so the guide can be left at any point and opens again at
//! the first step that is not done.

use iced::widget::{button, column, container, row, space, text, Column};
use iced::{Element, Length};
use serde_json::json;
use teamup_shift_sync_core::live::Service;
use teamup_shift_sync_gui::protocol::{Notice, Tone};

use crate::setup::{self, SetupState, SetupUi};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub enum Step {
    #[default]
    Welcome,
    Source,
    Mithf,
    Duos,
    Helpers,
    StandardTimes,
    Done,
}

impl Step {
    const ALL: [Step; 7] = [
        Step::Welcome,
        Step::Source,
        Step::Mithf,
        Step::Duos,
        Step::Helpers,
        Step::StandardTimes,
        Step::Done,
    ];
    /// The steps in the progress row, numbered from one. Welcome and Done
    /// frame them.
    const NUMBERED: [Step; 5] = [
        Step::Source,
        Step::Mithf,
        Step::Duos,
        Step::Helpers,
        Step::StandardTimes,
    ];

    fn label(self) -> &'static str {
        match self {
            Step::Welcome => "Velkommen",
            Step::Source => "Vagtplan",
            Step::Mithf => "MitHF",
            Step::Duos => "DUOS",
            Step::Helpers => "Hjælpere",
            Step::StandardTimes => "Uden tid",
            Step::Done => "Færdig",
        }
    }
    fn index(self) -> usize {
        Self::ALL.iter().position(|step| *step == self).unwrap_or(0)
    }
    pub fn next(self) -> Step {
        Self::ALL[(self.index() + 1).min(Self::ALL.len() - 1)]
    }
    pub fn previous(self) -> Step {
        Self::ALL[self.index().saturating_sub(1)]
    }
    /// A step finished by its own action, a connection or a login, moves on
    /// by itself. Hjælpere does not: the last choice would otherwise pull the
    /// list away while it is still being read.
    pub fn advances_itself(self) -> bool {
        matches!(self, Step::Source | Step::Mithf | Step::Duos)
    }
}

// Setup messages carry credentials. Do not derive Debug with their contents.
#[derive(Clone)]
pub enum Message {
    Next,
    Back,
    Go(Step),
    /// Leave the guide for Indstillinger.
    Skip,
    /// Leave the guide for the week.
    Finish,
    Compensation,
    Help,
    Retry,
    Setup(setup::Message),
}
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GuideMessage")
    }
}

#[derive(Default)]
pub struct Guide {
    pub step: Step,
    /// Logins checked in this session. A login lives in the service's own
    /// browser profile, not in the setup, so the guide only knows of one it
    /// has seen work.
    pub mithf_checked: bool,
    pub duos_checked: bool,
    /// The guide has been opened in this session, so Vagtplan without a
    /// confirmed setup comes back to it instead of Indstillinger.
    pub started: bool,
    /// Skipped or finished in this session.
    pub dismissed: bool,
}

impl Guide {
    /// Whether `step` has what it asks for. Each step is judged on its own;
    /// [`Guide::reachable`] adds the order.
    pub fn done(&self, step: Step, state: &SetupState) -> bool {
        match step {
            Step::Source => state.stage != "source",
            // A proposal or a chosen arrangement means both services were read.
            Step::Mithf => {
                self.mithf_checked
                    || state.stage == "ready"
                    || !state.mappings.is_empty()
                    || !state.arrangement.is_empty()
            }
            Step::Duos => {
                !state.duos_enabled || !state.arrangement.is_empty() || state.stage == "ready"
            }
            Step::Helpers => state.stage == "ready",
            Step::Welcome | Step::StandardTimes | Step::Done => true,
        }
    }
    /// Every numbered step before `step` is done.
    pub fn reachable(&self, step: Step, state: &SetupState) -> bool {
        Step::NUMBERED
            .iter()
            .take_while(|before| **before < step)
            .all(|before| self.done(*before, state))
    }
    /// Where the guide opens: the first step not done, or the welcome for a
    /// fresh setup and for one that is already complete.
    pub fn start(&self, state: &SetupState) -> Step {
        let fresh =
            state.stage == "source" && !state.has_credentials && state.connected_sources.is_empty();
        match Step::NUMBERED
            .into_iter()
            .find(|step| !self.done(*step, state))
        {
            Some(step) if !fresh => step,
            _ => Step::Welcome,
        }
    }
    pub fn checked(&mut self, service: Service, ok: bool) {
        match service {
            Service::Mithf => self.mithf_checked = ok,
            Service::Duos => self.duos_checked = ok,
        }
    }

    /// The whole guide page. `busy` disables what would start work while
    /// something runs; `error` is why the setup could not be loaded.
    pub fn view<'a>(
        &'a self,
        ui: &'a SetupUi,
        busy: bool,
        error: Option<&'a str>,
    ) -> Element<'a, Message> {
        let Some(state) = &ui.state else {
            let mut page = column![text("Opsætning").size(26)].spacing(12);
            page = match error {
                Some(error) => page
                    .push(text(format!("Opsætningen kunne ikke indlæses: {error}")))
                    .push(primary("Prøv igen", Message::Retry, !busy)),
                None => page.push(text("Indlæser opsætningen …")),
            };
            return page.into();
        };
        let body = match self.step {
            Step::Welcome => self.welcome(busy),
            Step::Source => self.source(ui, state, busy),
            Step::Mithf => self.login(state, Service::Mithf, busy),
            Step::Duos => self.duos(state, busy),
            Step::Helpers => self.helpers(ui, state, busy),
            Step::StandardTimes => self.standard_times(ui),
            Step::Done => self.done_page(busy),
        };
        let mut page = column![self.progress(state)]
            .spacing(20)
            .height(Length::Fill);
        page = page.push(
            iced::widget::scrollable(
                container(body.spacing(14).max_width(760))
                    .center_x(Length::Fill)
                    .padding(iced::Padding::ZERO.right(12)),
            )
            .height(Length::Fill)
            .width(Length::Fill),
        );
        if Step::NUMBERED.contains(&self.step) {
            page = page.push(self.navigation(ui, state, busy));
        }
        page.into()
    }

    /// One chip per numbered step: ✓ when done, the current one marked. A
    /// step can be opened once every step before it is done.
    fn progress(&self, state: &SetupState) -> Element<'_, Message> {
        let mut chips = row![].spacing(6).align_y(iced::alignment::Vertical::Center);
        for (number, step) in Step::NUMBERED.into_iter().enumerate() {
            let mark = if self.done(step, state) && step != Step::StandardTimes {
                "✓".to_owned()
            } else {
                (number + 1).to_string()
            };
            if number > 0 {
                chips = chips.push(text("›").size(14));
            }
            chips = chips.push(
                button(text(format!("{mark}  {}", step.label())).size(14))
                    .style(crate::widgets::chosen(
                        self.step == step,
                        crate::widgets::CHIP,
                    ))
                    .padding([6, 14])
                    .on_press_maybe(self.reachable(step, state).then_some(Message::Go(step))),
            );
        }
        row![
            chips,
            space::horizontal(),
            iced::widget::tooltip(
                button(text("?").size(18))
                    .style(crate::widgets::chosen(false, crate::widgets::CHIP))
                    .padding([4, 13])
                    .on_press(Message::Help),
                "Support",
                iced::widget::tooltip::Position::Bottom,
            ),
        ]
        .align_y(iced::alignment::Vertical::Center)
        .into()
    }

    /// Back, what is missing, leaving the guide, and Next.
    fn navigation<'a>(
        &'a self,
        ui: &'a SetupUi,
        state: &'a SetupState,
        busy: bool,
    ) -> Element<'a, Message> {
        let next = self.step.next();
        let can_next = self.reachable(next, state);
        let next_label = match self.step {
            Step::StandardTimes if ui.standard_times() == Default::default() => "Spring over ›",
            Step::StandardTimes => "Færdig ›",
            _ => "Næste ›",
        };
        let hint = if busy {
            "Et øjeblik …".to_owned()
        } else if can_next {
            String::new()
        } else {
            self.missing(ui, state)
        };
        row![
            quiet("‹ Tilbage", Message::Back, true),
            text(hint).size(13).width(Length::Fill),
            quiet("Spring guiden over", Message::Skip, true),
            primary(next_label, Message::Next, can_next),
        ]
        .spacing(12)
        .align_y(iced::alignment::Vertical::Center)
        .into()
    }

    /// What stands between this step and the next, as the thing to do.
    fn missing(&self, ui: &SetupUi, state: &SetupState) -> String {
        let open = Step::NUMBERED
            .into_iter()
            .find(|step| *step <= self.step && !self.done(*step, state));
        match open {
            Some(step) if step < self.step => {
                format!("Gør »{}« færdig først.", step.label())
            }
            Some(Step::Source) => "Tilslut vagtplanen.".into(),
            Some(Step::Mithf) => "Log ind, og check forbindelsen.".into(),
            Some(Step::Duos) if state.arrangements.is_empty() => {
                "Log ind, og check forbindelsen.".into()
            }
            Some(Step::Duos) => "Vælg SPS-ordning.".into(),
            // The page shows why the choices are blocked until that notice
            // hides; the reason stays here after it.
            Some(Step::Helpers) => state
                .blocked
                .clone()
                .filter(|_| ui.blocked_hidden)
                .unwrap_or_else(|| {
                    // With every calendar chosen, only the confirmation is left.
                    if state
                        .mappings
                        .iter()
                        .all(|m| m.excluded || !m.mithf.is_empty())
                    {
                        "Vælg Bekræft.".into()
                    } else {
                        "Vælg en hjælper for hver kalender.".into()
                    }
                }),
            _ => String::new(),
        }
    }

    fn welcome(&self, busy: bool) -> Column<'_, Message> {
        let needs = column![
            text("• Din vagtplan: TeamUp, et regneark eller en kalender"),
            text("• Login til MitHF"),
            text("• Login til DUOS, hvis du har SPS-timer"),
        ]
        .spacing(6);
        column![
            text("Velkommen til BPA Overblik").size(28),
            text("Overfør ugens vagter til MitHF og DUOS. Du godkender altid først."),
            crate::widgets::group(text("Det skal du bruge").size(14), needs),
            text("Tager ca. 5 minutter. Alt gemmes undervejs.").size(13),
            row![
                primary("Kom i gang", Message::Next, true),
                quiet("Spring over", Message::Skip, true),
            ]
            .spacing(12),
            space::vertical().height(12),
            crate::widgets::card(
                row![
                    text("Kun Kompensationsydelse? Kræver ingen opsætning.")
                        .size(13)
                        .width(Length::Fill),
                    quiet("Åbn Kompensationsydelse", Message::Compensation, !busy),
                ]
                .spacing(12)
                .align_y(iced::alignment::Vertical::Center),
            ),
        ]
    }

    fn source<'a>(
        &'a self,
        ui: &'a SetupUi,
        state: &'a SetupState,
        busy: bool,
    ) -> Column<'a, Message> {
        let mut choices = row![].spacing(10);
        for (id, name, about) in SOURCES {
            let active = state.source == id;
            choices = choices.push(
                button(
                    column![text(name).size(16), text(about).size(12)]
                        .spacing(4)
                        .width(Length::Fill),
                )
                .style(crate::widgets::chosen(active, 8.0))
                .padding([12, 14])
                .width(Length::FillPortion(1))
                .height(Length::Fixed(96.0))
                .on_press_maybe((!busy).then(|| {
                    Message::Setup(if active {
                        setup::Message::Noop
                    } else {
                        setup::Message::Action("choose_source", json!({"source": id}))
                    })
                })),
            );
        }
        let name = SOURCES
            .iter()
            .find(|(id, _, _)| *id == state.source)
            .map_or("Vagtplanen", |(_, name, _)| *name);
        let mut content = column![
            title("Hvor ligger din vagtplan?"),
            text("Appen læser kun. Den ændrer intet i vagtplanen."),
            choices,
        ];
        if state.stage == "source" {
            let mut form = column![].spacing(10);
            for line in source_hints(&state.source) {
                form = form.push(text(*line).size(13));
            }
            form = form.push(ui.source_connection(state).map(Message::Setup));
            content = content.push(crate::widgets::group(
                text(format!("Tilslut {name}")).size(14),
                form,
            ));
        } else {
            content = content
                .push(success(format!("{name} er tilsluttet.")))
                .push(row![quiet(
                    "Tilslut igen",
                    Message::Setup(setup::Message::Action("source", json!({}))),
                    !busy,
                )]);
        }
        content
    }

    /// Log in to `service` and check it. Login happens in the service's own
    /// window, so the steps say where to come back.
    fn login<'a>(
        &'a self,
        state: &'a SetupState,
        service: Service,
        busy: bool,
    ) -> Column<'a, Message> {
        let name = service.name();
        let checked = match service {
            Service::Mithf => self.mithf_checked,
            Service::Duos => self.duos_checked,
        };
        let mut content = column![].spacing(14);
        if service == Service::Mithf {
            content = content
                .push(title("Log ind i MitHF"))
                .push(text("Log ind i vinduet, og check så forbindelsen her."));
        }
        content = content
            .push(numbered(
                1,
                String::new(),
                Some(primary_or_quiet(
                    format!("Log ind i {name}"),
                    Message::Setup(setup::Message::Login(service)),
                    !checked,
                    !busy,
                )),
            ))
            .push(numbered(
                2,
                String::new(),
                Some(primary_or_quiet(
                    "Check forbindelse".to_owned(),
                    Message::Setup(setup::Message::CheckLogin(service)),
                    false,
                    !busy,
                )),
            ));
        if checked {
            content = content.push(success(format!("Du er logget ind i {name}.")));
        }
        if service == Service::Duos && checked && state.arrangements.is_empty() {
            content = content.push(if busy {
                column![text("Henter ordninger …").size(13)]
            } else {
                column![row![quiet(
                    "Hent ordninger",
                    Message::Setup(setup::Message::Action("discover", json!({}))),
                    true,
                )]]
            });
        }
        content
    }

    fn duos<'a>(&'a self, state: &'a SetupState, busy: bool) -> Column<'a, Message> {
        let mut choices = row![].spacing(10);
        for (enabled, name, about) in [
            (true, "Ja", "Registrér SPS-timer"),
            (false, "Nej", "Kun MitHF"),
        ] {
            let active = state.duos_enabled == enabled;
            choices = choices.push(
                button(
                    column![text(name).size(16), text(about).size(12)]
                        .spacing(4)
                        .width(Length::Fill),
                )
                .style(crate::widgets::chosen(active, 8.0))
                .padding([12, 14])
                .width(Length::FillPortion(1))
                .on_press_maybe((!busy).then(|| {
                    Message::Setup(if active {
                        setup::Message::Noop
                    } else {
                        setup::Message::Action("duos_enabled", json!({"enabled": enabled}))
                    })
                })),
            );
        }
        let mut content = column![
            title("Registrerer du SPS-timer i DUOS?"),
            text("Hjælperen godkender selv registreringen i DUOS."),
            choices,
        ];
        if !state.duos_enabled {
            return content.push(text("Kan slås til senere under Udbydere.").size(13));
        }
        content = content.push(self.login(state, Service::Duos, busy));
        if let Some(picker) = setup::arrangement_picker(state) {
            content = content.push(numbered(
                3,
                "Vælg SPS-ordning.".to_owned(),
                Some(picker.map(Message::Setup)),
            ));
        }
        if let Some(arrangement) = setup::arrangement_name(state) {
            content = content.push(success(format!("DUOS: {arrangement}")));
        }
        content
    }

    fn helpers<'a>(
        &'a self,
        ui: &'a SetupUi,
        state: &'a SetupState,
        busy: bool,
    ) -> Column<'a, Message> {
        let services = if state.duos_enabled {
            "MitHF og DUOS"
        } else {
            "MitHF"
        };
        let mut content = column![
            row![
                title("Hvem er hvem?"),
                space::horizontal(),
                quiet(
                    "⟳ Hent navne igen",
                    Message::Setup(setup::Message::Action("discover", json!({}))),
                    !busy,
                ),
            ]
            .align_y(iced::alignment::Vertical::Center),
            text(format!("Vælg hjælperen i {services} for hver kalender.")),
        ];
        if busy && state.mappings.is_empty() {
            content = content.push(text("Henter hjælpere …"));
        } else {
            content = content.push(ui.view(setup::Section::Helpers).map(Message::Setup));
        }
        if state.stage == "ready" {
            content = content.push(success("Alle hjælpere er klar.".to_owned()));
        } else if state.blocked.is_none() && !state.mappings.is_empty() {
            // Valid choices confirm themselves after a fresh check of the
            // services, so a failed check is what is left.
            content = content.push(row![primary(
                "Bekræft",
                Message::Setup(setup::Message::Action("auto_confirm", json!({}))),
                !busy,
            )]);
        }
        content
    }

    fn standard_times<'a>(&'a self, ui: &'a SetupUi) -> Column<'a, Message> {
        column![
            title(crate::untimed::NAME),
            text(crate::untimed::INTRO),
            ui.untimed_fields().map(Message::Setup),
        ]
    }

    fn done_page(&self, busy: bool) -> Column<'_, Message> {
        let mut steps = column![].spacing(10);
        for (number, line) in [
            "Vælg Se vagtplan.",
            "Ret eventuelle advarsler i vagtplanen.",
            "Vælg Godkend ændringer.",
        ]
        .into_iter()
        .enumerate()
        {
            steps = steps.push(numbered(number + 1, line.to_owned(), None));
        }
        column![
            title("✓ Du er klar"),
            text("Hver uge:"),
            steps,
            text("Appen sletter aldrig noget i MitHF eller DUOS.").size(13),
            row![
                quiet("‹ Tilbage", Message::Back, true),
                primary("Se denne uges vagtplan", Message::Finish, !busy),
            ]
            .spacing(12),
        ]
    }
}

/// The shift sources: id, name and what each covers.
const SOURCES: [(&str, &str, &str); 3] = [
    ("teamup", "TeamUp", "Én kalender pr. hjælper."),
    ("sheets", "Regneark", "Sheets, Excel eller en fil."),
    ("ical", "Kalender (iCal)", "Google, Outlook eller iCloud."),
];

/// Where to find what the source's form asks for. TeamUp's form already
/// says it beside its fields.
fn source_hints(source: &str) -> &'static [&'static str] {
    match source {
        "sheets" => &["Del arket, så alle med linket kan se det."],
        "ical" => &[
            "Google: Indstillinger → kalenderen → Hemmelig adresse i iCal-format.",
            "Outlook: Indstillinger → Delte kalendere → Udgiv en kalender.",
            "iCloud: Del kalenderen som offentlig kalender.",
        ],
        _ => &[],
    }
}

fn title<'a>(label: &'a str) -> Element<'a, Message> {
    text(label).size(24).into()
}

/// A numbered instruction, with the button that carries it out.
fn numbered<'a>(
    number: usize,
    line: String,
    action: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut lines = column![].spacing(8).width(Length::Fill);
    if !line.is_empty() {
        lines = lines.push(text(line));
    }
    if let Some(action) = action {
        lines = lines.push(action);
    }
    row![
        container(text(number.to_string()).size(14))
            .center_x(Length::Fixed(28.0))
            .center_y(Length::Fixed(28.0))
            .style(|theme: &iced::Theme| {
                let palette = theme.extended_palette();
                container::Style {
                    border: iced::Border {
                        color: palette.primary.base.color,
                        width: 1.5,
                        radius: 14.0.into(),
                    },
                    ..container::Style::default()
                }
            }),
        lines,
    ]
    .spacing(12)
    .into()
}

fn success<'a>(line: String) -> Element<'a, Message> {
    crate::widgets::notice_card(Notice::new(Tone::Success, line, ""), None)
}

fn primary<'a>(label: &'a str, message: Message, enabled: bool) -> Element<'a, Message> {
    button(text(label))
        .style(button::primary)
        .padding([8, 16])
        .on_press_maybe(enabled.then_some(message))
        .into()
}

fn quiet<'a>(label: &'a str, message: Message, enabled: bool) -> Element<'a, Message> {
    button(text(label).size(14))
        .style(crate::widgets::outlined)
        .padding([7, 12])
        .on_press_maybe(enabled.then_some(message))
        .into()
}

/// The step's next action is filled; once done it steps back to outlined.
fn primary_or_quiet<'a>(
    label: String,
    message: Message,
    filled: bool,
    enabled: bool,
) -> Element<'a, Message> {
    button(text(label).size(14))
        .style(if filled {
            button::primary
        } else {
            crate::widgets::outlined
        })
        .padding([7, 14])
        .on_press_maybe(enabled.then_some(message))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(stage: &str) -> SetupState {
        serde_json::from_value(json!({
            "stage": stage, "calendars": [], "arrangements": [], "types": [],
            "mithf": [], "duos": [], "mappings": [], "arrangement": "",
            "registration_type": "", "account": "", "has_credentials": false,
        }))
        .expect("setup state")
    }

    #[test]
    fn a_fresh_setup_opens_on_the_welcome_and_a_started_one_where_it_stopped() {
        let guide = Guide::default();
        assert_eq!(guide.start(&state("source")), Step::Welcome);

        let mut connected = state("destinations");
        connected.has_credentials = true;
        assert_eq!(guide.start(&connected), Step::Mithf);

        let checked = Guide {
            mithf_checked: true,
            ..Guide::default()
        };
        assert_eq!(checked.start(&connected), Step::Duos);
        connected.duos_enabled = false;
        assert_eq!(checked.start(&connected), Step::Helpers);

        // Everything done: start from the top, so it can be reviewed.
        assert_eq!(guide.start(&state("ready")), Step::Welcome);
    }

    #[test]
    fn a_step_opens_only_when_every_step_before_it_is_done() {
        let guide = Guide::default();
        let mut setup = state("destinations");
        assert!(guide.reachable(Step::Mithf, &setup));
        assert!(!guide.reachable(Step::Duos, &setup));
        // Answering no to DUOS does not skip the MitHF login.
        setup.duos_enabled = false;
        assert!(guide.done(Step::Duos, &setup));
        assert!(!guide.reachable(Step::Helpers, &setup));
        setup.arrangement = "35505".into();
        setup.duos_enabled = true;
        assert!(guide.reachable(Step::Helpers, &setup));
        assert!(!guide.reachable(Step::Done, &setup));
        assert!(guide.reachable(Step::Done, &state("ready")));
    }

    #[test]
    fn every_step_renders_with_and_without_a_setup() {
        let mut ui = SetupUi::default();
        let guide = Guide::default();
        let _ = guide.view(&ui, false, None);
        let _ = guide.view(&ui, false, Some("Nøgleringen er låst."));
        for stage in ["source", "destinations", "helpers", "ready"] {
            ui.state = Some(state(stage));
            for step in Step::ALL {
                for busy in [false, true] {
                    let guide = Guide {
                        step,
                        duos_checked: true,
                        ..Guide::default()
                    };
                    let _ = guide.view(&ui, busy, None);
                }
            }
        }
    }

    #[test]
    fn the_steps_run_in_order_and_stop_at_the_ends() {
        assert_eq!(Step::Welcome.previous(), Step::Welcome);
        assert_eq!(Step::Welcome.next(), Step::Source);
        assert_eq!(Step::StandardTimes.next(), Step::Done);
        assert_eq!(Step::Done.next(), Step::Done);
        assert!(Step::Duos.advances_itself());
        assert!(!Step::Helpers.advances_itself());
    }
}
