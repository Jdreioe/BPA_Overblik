use iced::widget::{
    button, column, image, pick_list, radio, row, space, text, text_input, toggler,
};
use iced::{Element, Length};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use teamup_shift_sync_core::ical::{HelperRule, TitlePart};
use teamup_shift_sync_core::live::Service;
use teamup_shift_sync_core::sheets::SheetLayout;
use teamup_shift_sync_core::standard_time::StandardTimes;
use teamup_shift_sync_gui::protocol::{Notice, Tone};

use crate::template::{self, Template};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Choice {
    pub id: String,
    pub name: String,
}
impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Mapping {
    pub source: String,
    pub mithf: String,
    pub duos: String,
    pub excluded: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetupState {
    pub stage: String,
    #[serde(default = "duos_on")]
    pub duos_enabled: bool,
    #[serde(default = "teamup_source")]
    pub source: String,
    #[serde(default)]
    pub sheet_layout: Option<SheetLayout>,
    #[serde(default)]
    pub ical_rule: Option<HelperRule>,
    #[serde(default)]
    pub standard_times: StandardTimes,
    pub calendars: Vec<Choice>,
    pub arrangements: Vec<Choice>,
    pub types: Vec<Choice>,
    pub mithf: Vec<Choice>,
    pub duos: Vec<Choice>,
    pub mappings: Vec<Mapping>,
    pub arrangement: String,
    pub registration_type: String,
    pub account: String,
    /// What the action that produced this state found, when it has something
    /// to report beyond the state itself. The engine sets it; the app moves it
    /// to its own status notice.
    #[serde(skip)]
    pub result: Option<Notice>,
    /// Why the reviewed helper choices cannot be confirmed yet. Until this is
    /// resolved there is no account, so no week to go back to.
    #[serde(skip)]
    pub blocked: Option<String>,
    pub has_credentials: bool,
    /// Sources with stored credentials, the active one included.
    #[serde(default)]
    pub connected_sources: Vec<String>,
}

fn teamup_source() -> String {
    "teamup".into()
}
fn duos_on() -> bool {
    true
}

// These inputs contain credentials. Do not derive Debug with their contents.
#[derive(Clone)]
pub enum Message {
    Link(String),
    Key(String),
    Template(template::Message),
    StandardDefault(String),
    StandardDay(usize, String),
    Connect,
    ConnectSheets,
    IcalLink(usize, String),
    AddIcalLink,
    RemoveIcalLink(usize),
    IcalShared(bool),
    IcalPart(TitlePart),
    IcalSeparator(String),
    ConnectIcal,
    OpenTeamupKeys,
    CopyOrganization,
    CopyPurpose,
    ToggleEdit(String),
    /// Expand or collapse one provider's settings on the providers tab.
    ToggleProvider(String),
    /// Provider login actions. The app maps these onto its own engine
    /// messages, like the TeamUp key shortcut.
    Login(Service),
    CheckLogin(Service),
    ForgetLogin(Service),
    /// Clicking the already active shift source. Deliberately nothing.
    Noop,
    Action(&'static str, Value),
}
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetupMessage")
    }
}

/// TeamUp udsteder API-nøgler via deres egen side. Åbnes i personens egen
/// browser, aldrig i appens MitHF/DUOS-profiler.
pub const TEAMUP_KEYS_URL: &str = "https://teamup.com/api-keys/request";

/// Forslag til TeamUps nøgleformular. Organisationen er ligegyldig, men skal
/// være over 5 bogstaver. Formålet er valgfrit og kun vejledende for TeamUp.
pub const TEAMUP_ORG_SUGGESTION: &str = "Vagtplanlaegning";
pub const TEAMUP_PURPOSE_SUGGESTION: &str =
    "Personal tool syncing my own TeamUp shifts to the services where I register my working hours.";

/// Provider marks, icon only. The full lockups stay out of the app.
const DUOS_ICON: &[u8] = include_bytes!("../assets/duos-icon.png");
const MITHF_ICON: &[u8] = include_bytes!("../assets/mithf-icon.png");

/// Stable input ids. Iced tracks focus by widget position, so an error line
/// appearing under a field would otherwise move every field and drop focus
/// on each keystroke.
const STANDARD_DEFAULT_ID: &str = "standard-falles";
const STANDARD_DAY_IDS: [&str; 7] = [
    "standard-mandag",
    "standard-tirsdag",
    "standard-onsdag",
    "standard-torsdag",
    "standard-fredag",
    "standard-lordag",
    "standard-sondag",
];
const SOURCE_LINK_ID: &str = "source-link";
const ICAL_SEPARATOR_ID: &str = "ical-separator";
const TEAMUP_KEY_ID: &str = "teamup-key";

#[derive(Default)]
pub struct SetupUi {
    pub state: Option<SetupState>,
    pub link: String,
    pub key: String,
    pub template: Template,
    /// Why the typed standard times cannot be saved. Shown beside the fields;
    /// action results and errors go to the app's status notice instead.
    pub error: Option<String>,
    pub standard_default: String,
    pub standard_days: [String; 7],
    /// Calendars the person reopened for editing. A row shows dropdowns
    /// while it is ambiguous (no match yet) or reopened here; a unique
    /// suggestion otherwise renders as plain text. Nothing is confirmed
    /// until the joint confirmation below.
    pub editing: std::collections::BTreeSet<String>,
    /// Providers collapsed on the providers tab. Everything starts open.
    pub collapsed: std::collections::BTreeSet<String>,
    /// iCal links being entered. Like `link`, only kept until stored.
    pub ical_links: Vec<String>,
    /// One shared calendar naming helpers in titles, not one per helper.
    pub ical_shared: bool,
    pub ical_part: TitlePart,
    pub ical_separator: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
    Integrations,
    Helpers,
}

impl SetupUi {
    pub fn load_standard(&mut self, state: &SetupState) {
        self.standard_default = state.standard_times.everyday.clone();
        for (index, day) in ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
            .iter()
            .enumerate()
        {
            self.standard_days[index] = match state.standard_times.weekdays.get(*day) {
                None => String::new(),
                Some(None) => "ingen".into(),
                Some(Some(value)) => value.clone(),
            };
        }
    }

    pub fn standard_times(&self) -> StandardTimes {
        let mut weekdays = std::collections::BTreeMap::new();
        for (index, day) in ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
            .iter()
            .enumerate()
        {
            let value = self.standard_days[index].trim();
            if !value.is_empty() {
                weekdays.insert(
                    (*day).into(),
                    if value.eq_ignore_ascii_case("ingen") {
                        None
                    } else {
                        Some(value.into())
                    },
                );
            }
        }
        StandardTimes {
            everyday: self.standard_default.trim().into(),
            weekdays,
        }
    }
    /// Show the saved helper rule, so reconnecting starts from it.
    pub fn load_ical(&mut self, state: &SetupState) {
        match &state.ical_rule {
            Some(HelperRule::Title { part, separator }) => {
                self.ical_shared = true;
                self.ical_part = *part;
                self.ical_separator.clone_from(separator);
            }
            Some(HelperRule::Feed) => self.ical_shared = false,
            None => {}
        }
    }

    /// How the entered iCal feeds name their helpers.
    pub fn ical_rule(&self) -> HelperRule {
        if !self.ical_shared {
            return HelperRule::Feed;
        }
        let separator = self.ical_separator.trim();
        HelperRule::Title {
            part: self.ical_part,
            separator: if separator.is_empty() { "-" } else { separator }.into(),
        }
    }

    /// The links to connect. A shared calendar uses only the first field.
    pub fn ical_links_to_connect(&self) -> Vec<String> {
        let fields = if self.ical_shared {
            1
        } else {
            self.ical_links.len()
        };
        self.ical_links.iter().take(fields).cloned().collect()
    }

    pub fn update_ical(&mut self, message: &Message) {
        match message {
            Message::IcalLink(index, link) => {
                if self.ical_links.len() <= *index {
                    self.ical_links.resize(index + 1, String::new());
                }
                self.ical_links[*index].clone_from(link);
            }
            Message::AddIcalLink => {
                let fields = self.ical_links.len().max(1);
                self.ical_links.resize(fields + 1, String::new());
            }
            Message::RemoveIcalLink(index) => {
                if *index < self.ical_links.len() {
                    self.ical_links.remove(*index);
                }
            }
            Message::IcalShared(shared) => self.ical_shared = *shared,
            Message::IcalPart(part) => self.ical_part = *part,
            Message::IcalSeparator(separator) => self.ical_separator.clone_from(separator),
            _ => {}
        }
    }

    /// The layout learned from the pasted shift, or the saved one while no
    /// new shift has been pasted.
    pub fn sheet_layout(&self) -> Result<SheetLayout, String> {
        match self.template.layout() {
            Some(result) => result.map_err(str::to_owned),
            None if self.template.is_empty() => self
                .state
                .as_ref()
                .and_then(|state| state.sheet_layout.clone())
                .ok_or_else(|| "Indsæt en vagt fra regnearket først.".to_owned()),
            None => Err("Vælg vagtens celler først.".to_owned()),
        }
    }

    pub fn standard_view(&self) -> Element<'_, Message> {
        let mut content = column![
            text("Standardtider").size(20),
            text("Bruges når en vagt ikke har egne tider.").size(13),
        ]
        .spacing(10);
        content = content
            .push(text("Standardtid for alle dage").size(14))
            .push(
                text_input("F.eks. 6-22", &self.standard_default)
                    .id(iced::widget::Id::new(STANDARD_DEFAULT_ID))
                    .on_input(Message::StandardDefault)
                    .padding(10),
            )
            .push(
                text("Lad en dag stå tom for at bruge tiden ovenfor. Skriv »ingen« for ingen standardtid.")
                    .size(12),
            );
        for (index, day) in [
            "Mandag", "Tirsdag", "Onsdag", "Torsdag", "Fredag", "Lørdag", "Søndag",
        ]
        .iter()
        .enumerate()
        {
            content = content.push(
                row![
                    text(*day).width(Length::Fixed(90.0)),
                    text_input("Brug tiden ovenfor", &self.standard_days[index])
                        .id(iced::widget::Id::new(STANDARD_DAY_IDS[index]))
                        .on_input(move |value| Message::StandardDay(index, value))
                        .padding(10)
                        .width(Length::Fixed(200.0)),
                ]
                .spacing(10),
            );
        }
        content
            .push(text(self.error.as_deref().unwrap_or("")).size(13))
            .push(text("Gyldige tider gemmes automatisk.").size(12))
            .into()
    }

    pub fn view(&self, section: Section) -> Element<'_, Message> {
        let content = column![].spacing(8);
        // The form stays on screen while an action runs, so a quick save does
        // not blink. The app ignores input until the action has finished and
        // shows its own status line below the page.
        let Some(state) = &self.state else {
            return content
                .push(primary_button(
                    "Hent opsætning",
                    Message::Action("status", json!({})),
                ))
                .into();
        };
        if section == Section::Integrations {
            return content.push(self.provider_groups(state)).into();
        }
        // Only the helpers section is left, and it needs a connected source.
        if state.stage == "source" {
            return content
                .push(text("Tilslut en vagtplan under Udbydere først."))
                .into();
        }
        let mut content = content;
        if state.mappings.is_empty() {
            content = content.push(text(
                "Hjælperne hentes automatisk, når forbindelserne er klar.",
            ));
        } else {
            let mut header = row![
                text(match state.source.as_str() {
                    "sheets" => "Regneark",
                    "ical" => "Kalender",
                    _ => "TeamUp",
                })
                .width(Length::FillPortion(2)),
                text("MitHF").width(Length::FillPortion(2)),
            ]
            .spacing(8);
            if state.duos_enabled {
                header = header.push(text("DUOS").width(Length::FillPortion(2)));
            }
            if let Some(blocked) = &state.blocked {
                content = content.push(crate::widgets::notice_card(
                    Notice::from_message(Tone::Warning, blocked),
                    None,
                ));
            }
            content = content.push(header);
            for mapping in &state.mappings {
                content = content.push(self.mapping_row(state, mapping));
            }
        }
        content.into()
    }

    /// The providers tab: one group per side. The shift source is
    /// single-select; each payroll service keeps its own login and settings.
    fn provider_groups<'a>(&'a self, state: &'a SetupState) -> Element<'a, Message> {
        column![
            crate::widgets::group(text("Vagtplan").size(14), self.source_section(state)),
            crate::widgets::group(text("Løn").size(14), self.service_section(state)),
        ]
        .spacing(12)
        .into()
    }

    fn source_section<'a>(&'a self, state: &'a SetupState) -> Element<'a, Message> {
        let mut content = column![
            self.source_row(state, "teamup", "TeamUp"),
            self.source_row(state, "sheets", "Google Sheets"),
            self.source_row(state, "ical", "iCal-kalender"),
        ]
        .spacing(8);
        if state.stage == "source" {
            content = content.push(self.source_connection(state));
        } else {
            let mut resume = row![quiet_button(
                "Genopsæt forbindelsen",
                Message::Action("source", json!({})),
            )]
            .spacing(8);
            if state.has_credentials {
                resume = resume.push(quiet_button(
                    "Prøv den gemte forbindelse igen",
                    Message::Action("retry_source", json!({})),
                ));
            }
            // The source row already says whether it is connected.
            content = content.push(resume);
        }
        content.into()
    }

    /// One shift source. The radio both shows and switches the active source;
    /// clicking the active one is nothing.
    fn source_row<'a>(
        &'a self,
        state: &'a SetupState,
        id: &'static str,
        name: &str,
    ) -> Element<'a, Message> {
        let active = state.source == id;
        let has_credentials = if active {
            state.has_credentials
        } else {
            state.connected_sources.iter().any(|source| source == id)
        };
        row![
            radio(name, id, active.then_some(id), move |picked| {
                if picked == state.source {
                    Message::Noop
                } else {
                    Message::Action("choose_source", json!({"source": picked}))
                }
            }),
            space::horizontal(),
            text(if has_credentials {
                "Tilsluttet"
            } else {
                "Ikke tilsluttet"
            })
            .size(12),
        ]
        .spacing(10)
        .align_y(iced::alignment::Vertical::Center)
        .into()
    }

    /// The connection form for the not-yet-connected source.
    fn source_connection<'a>(&'a self, state: &'a SetupState) -> Element<'a, Message> {
        let mut content = column![].spacing(8);
        if state.source == "ical" {
            return self.ical_connection();
        }
        if state.source == "sheets" {
            content = content
                .push(
                    text_input("Google Sheets-link til den valgte fane", &self.link)
                        .id(iced::widget::Id::new(SOURCE_LINK_ID))
                        .on_input(Message::Link)
                        .secure(true)
                        .padding(12),
                )
                .push(text("Del arket som »Alle med linket kan se«.").size(12));
            content = content.push(
                self.template
                    .view(state.sheet_layout.is_some())
                    .map(Message::Template),
            );
            content = content.push(
                primary_button("Tilslut regneark", Message::ConnectSheets).on_press_maybe(
                    self.sheet_layout()
                        .is_ok()
                        .then_some(Message::ConnectSheets),
                ),
            );
        } else {
            content = content
                .push(text("På PC: TeamUp → ☰ → Settings → Shared").size(12))
                .push(text("Kopiér Reader-linket, og indsæt det her:").size(12))
                .push(
                    text_input("TeamUp-kalenderlink", &self.link)
                        .id(iced::widget::Id::new(SOURCE_LINK_ID))
                        .on_input(Message::Link)
                        .secure(true)
                        .padding(12),
                )
                .push(
                    text_input("TeamUp API-nøgle", &self.key)
                        .id(iced::widget::Id::new(TEAMUP_KEY_ID))
                        .on_input(Message::Key)
                        .secure(true)
                        .padding(12),
                )
                .push(
                    text("Ingen API-nøgle? Åbn ansøgningen, og brug din egen mail.").size(12),
                )
                .push(
                    text("Kopiér organisation og formål herunder. Indsæt nøglen ovenfor, når du får den.").size(12),
                )
                .push(
                    row![
                        text(TEAMUP_ORG_SUGGESTION).size(13),
                        quiet_button("Kopiér", Message::CopyOrganization),
                        quiet_button("Kopiér formål", Message::CopyPurpose),
                        quiet_button("Anmod om API-nøgle", Message::OpenTeamupKeys),
                    ]
                    .spacing(8),
                )
                .push(primary_button("Tilslut kalender", Message::Connect));
        }
        content.into()
    }

    /// Links to one or more published calendars, and where the helper is.
    fn ical_connection(&self) -> Element<'_, Message> {
        let mut content = column![
            text("Brug kalenderens private iCal-link.").size(12),
            radio(
                "Én kalender pr. hjælper",
                false,
                Some(self.ical_shared),
                Message::IcalShared,
            ),
            radio(
                "Én fælles kalender",
                true,
                Some(self.ical_shared),
                Message::IcalShared,
            ),
        ]
        .spacing(8);
        let fields = if self.ical_shared {
            1
        } else {
            self.ical_links.len().max(1)
        };
        for index in 0..fields {
            let value = self.ical_links.get(index).map_or("", String::as_str);
            let mut line = row![text_input("iCal-link", value)
                .id(iced::widget::Id::from(format!("ical-link-{index}")))
                .on_input(move |link| Message::IcalLink(index, link))
                .secure(true)
                .padding(12)]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center);
            if fields > 1 {
                line = line.push(quiet_button("Fjern", Message::RemoveIcalLink(index)));
            }
            content = content.push(line);
        }
        if self.ical_shared {
            content = content
                .push(radio(
                    "Navn før tegnet",
                    TitlePart::Before,
                    Some(self.ical_part),
                    Message::IcalPart,
                ))
                .push(radio(
                    "Navn efter tegnet",
                    TitlePart::After,
                    Some(self.ical_part),
                    Message::IcalPart,
                ))
                .push(radio(
                    "Kun navn",
                    TitlePart::Whole,
                    Some(self.ical_part),
                    Message::IcalPart,
                ));
            if self.ical_part != TitlePart::Whole {
                content = content.push(
                    row![
                        text("Tegn").size(13).width(Length::Fixed(90.0)),
                        text_input("-", &self.ical_separator)
                            .id(iced::widget::Id::new(ICAL_SEPARATOR_ID))
                            .on_input(Message::IcalSeparator)
                            .padding(10)
                            .width(Length::Fixed(80.0)),
                    ]
                    .spacing(10)
                    .align_y(iced::alignment::Vertical::Center),
                );
            }
        } else {
            content = content.push(quiet_button("Tilføj kalender", Message::AddIcalLink));
        }
        let ready = self.ical_links.iter().any(|link| !link.trim().is_empty());
        content
            .push(
                primary_button("Tilslut kalender", Message::ConnectIcal)
                    .on_press_maybe(ready.then_some(Message::ConnectIcal)),
            )
            .into()
    }

    fn service_section<'a>(&'a self, state: &'a SetupState) -> Element<'a, Message> {
        let mut content = column![self.service_row(state, Service::Mithf)].spacing(8);
        if !self.collapsed.contains(Service::Mithf.key()) {
            content = content.push(self.service_logins(Service::Mithf));
        }
        content = content.push(self.service_row(state, Service::Duos));
        if !self.collapsed.contains(Service::Duos.key()) {
            content = content.push(self.duos_settings(state));
        }
        content.into()
    }

    fn service_row<'a>(&'a self, state: &'a SetupState, service: Service) -> Element<'a, Message> {
        let id = service.key();
        let open = !self.collapsed.contains(id);
        let mut header = row![
            service_icon(service),
            text(service.name()).size(14),
            space::horizontal(),
        ]
        .spacing(10)
        .align_y(iced::alignment::Vertical::Center);
        if service == Service::Duos {
            header =
                header.push(toggler(state.duos_enabled).on_toggle(|enabled| {
                    Message::Action("duos_enabled", json!({"enabled": enabled}))
                }));
        }
        header
            .push(quiet_button(
                if open { "▾" } else { "▸" },
                Message::ToggleProvider(id.to_owned()),
            ))
            .into()
    }

    fn service_logins(&self, service: Service) -> Element<'_, Message> {
        row![
            quiet_button("Log ind", Message::Login(service)),
            quiet_button("Check forbindelse", Message::CheckLogin(service)),
            quiet_button("Log ud", Message::ForgetLogin(service)),
        ]
        .spacing(8)
        .into()
    }

    fn duos_settings<'a>(&'a self, state: &'a SetupState) -> Element<'a, Message> {
        if !state.duos_enabled {
            return text("Slå til for at registrere SPS-timer i DUOS.")
                .size(13)
                .into();
        }
        let mut content = column![self.service_logins(Service::Duos)].spacing(8);
        if !state.account.is_empty() {
            content = content.push(text(&state.account));
        }
        if !state.arrangements.is_empty() {
            content = content.push(text("DUOS SPS-ordning").size(13)).push(
                pick_list(
                    state.arrangements.clone(),
                    find(&state.arrangements, &state.arrangement),
                    |c: Choice| Message::Action("discover", json!({"arrangement": c.id})),
                )
                .placeholder("Vælg ordning")
                .padding(8)
                .width(Length::Fixed(280.0)),
            );
        }
        // Registration is always the ordinary type until per-shift choice
        // lands, so the picker only appears as an escape hatch when nothing
        // valid is selected and no ordinary type is offered.
        if !state.types.is_empty() && find(&state.types, &state.registration_type).is_none() {
            content = content.push(text("Registreringstype").size(13)).push(
                pick_list(
                    state.types.clone(),
                    find(&state.types, &state.registration_type),
                    |c: Choice| Message::Action("edit", json!({"registration_type": c.id})),
                )
                .placeholder("Vælg registreringstype")
                .padding(8)
                .width(Length::Fixed(280.0)),
            );
        }
        content.into()
    }

    /// One calendar as one table row. Column headers appear once above, so
    /// the row itself carries no per-cell labels.
    fn mapping_row<'a>(
        &'a self,
        state: &'a SetupState,
        mapping: &'a Mapping,
    ) -> Element<'a, Message> {
        let name = state
            .calendars
            .iter()
            .find(|c| c.id == mapping.source)
            .map_or("Ukendt kalender", |c| c.name.as_str());
        if mapping.excluded {
            let source_id = mapping.source.clone();
            return row![
                text(name).width(Length::FillPortion(2)),
                text("Udeladt").size(13).width(Length::FillPortion(2)),
                row![quiet_button(
                    "Fortryd",
                    Message::Action("edit", json!({"source": source_id, "excluded": false})),
                )]
                .width(Length::FillPortion(2)),
            ]
            .spacing(8)
            .into();
        }
        let source_id = mapping.source.clone();
        let teamup = column![
            text(name),
            quiet_button(
                "Udelad",
                Message::Action("edit", json!({"source": source_id, "excluded": true})),
            ),
        ]
        .spacing(4)
        .width(Length::FillPortion(2));
        let mut cells = row![
            teamup,
            self.choice_cell(
                &state.mithf,
                &mapping.mithf,
                &mapping.source,
                "mithf",
                "Vælg hjælper"
            ),
        ]
        .spacing(8);
        if state.duos_enabled {
            cells = cells.push(self.choice_cell(
                &state.duos,
                &mapping.duos,
                &mapping.source,
                "duos",
                "Vælg aktiv hjælper",
            ));
        }
        cells.into()
    }

    /// A MitHF/DUOS cell: plain text with a ret affordance for a unique
    /// suggestion, an open dropdown for ambiguous or unmapped rows.
    fn choice_cell<'a>(
        &'a self,
        choices: &'a [Choice],
        selected: &str,
        source: &str,
        service: &'static str,
        placeholder: &'static str,
    ) -> Element<'a, Message> {
        let options = disambiguate(choices);
        if needs_dropdown(&options, selected, &self.editing, source) {
            let source_id = source.to_owned();
            return pick_list(options, find(choices, selected), move |c: Choice| {
                let mut params = json!({"source":source_id});
                params[service] = json!(c.id);
                Message::Action("edit", params)
            })
            .placeholder(placeholder)
            .padding(12)
            .width(Length::FillPortion(2))
            .into();
        }
        let label =
            find(choices, selected).map_or_else(|| "Ukendt valg".to_owned(), |choice| choice.name);
        let reopen = if self.editing.contains(source) {
            "Færdig"
        } else {
            "Ret"
        };
        row![
            text(label).size(13).width(Length::Fill),
            quiet_button(reopen, Message::ToggleEdit(source.to_owned())),
        ]
        .spacing(8)
        .width(Length::FillPortion(2))
        .into()
    }

    pub fn toggle_edit(&mut self, source: &str) {
        if !self.editing.remove(source) {
            self.editing.insert(source.to_owned());
        }
    }

    pub fn toggle_provider(&mut self, id: &str) {
        if !self.collapsed.remove(id) {
            self.collapsed.insert(id.to_owned());
        }
    }
}

/// One provider mark beside its name. Icon only, never the full lockup.
fn service_icon(service: Service) -> Element<'static, Message> {
    let bytes = match service {
        Service::Mithf => MITHF_ICON,
        Service::Duos => DUOS_ICON,
    };
    image(iced::widget::image::Handle::from_bytes(bytes))
        .width(Length::Fixed(28.0))
        .into()
}

fn primary_button<'a>(label: &'a str, message: Message) -> iced::widget::Button<'a, Message> {
    button(text(label))
        .style(iced::widget::button::primary)
        .padding([8, 14])
        .on_press(message)
}

/// Low-emphasis setup action. Outlined like the quiet buttons in `native.rs`,
/// so a row of them reads as buttons rather than as labels.
fn quiet_button<'a>(label: &'a str, message: Message) -> iced::widget::Button<'a, Message> {
    button(text(label).size(14))
        .style(crate::widgets::outlined)
        .padding([7, 12])
        .on_press(message)
}

fn find(choices: &[Choice], id: &str) -> Option<Choice> {
    choices.iter().find(|c| c.id == id).cloned()
}

/// Whether a MitHF/DUOS cell shows an open dropdown. A unique suggestion
/// renders as text until reopened; anything ambiguous or unmapped stays open.
fn needs_dropdown(
    choices: &[Choice],
    selected: &str,
    editing: &std::collections::BTreeSet<String>,
    source: &str,
) -> bool {
    find(choices, selected).is_none() || editing.contains(source)
}

fn disambiguate(choices: &[Choice]) -> Vec<Choice> {
    choices
        .iter()
        .map(|c| {
            let mut c = c.clone();
            if choices.iter().filter(|other| other.name == c.name).count() > 1 {
                c.name = format!("{} · nr. {}", c.name, c.id);
            }
            c
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(id: &str, name: &str) -> Choice {
        Choice {
            id: id.into(),
            name: name.into(),
        }
    }

    /// Several calendars at once: a unique suggestion, an ambiguous row, and
    /// an excluded one. The table renders each once, with headers, and the
    /// joint confirmation stays the single loud action.
    fn confirmation_state() -> SetupState {
        SetupState {
            source: "teamup".into(),
            sheet_layout: None,
            standard_times: Default::default(),
            duos_enabled: true,
            stage: "mappings".into(),
            calendars: vec![
                choice("cal-ft", "FT > Zain"),
                choice("cal-vikar", "Vikar"),
                choice("cal-møde", "Møde"),
            ],
            arrangements: vec![],
            types: vec![],
            mithf: vec![choice("m1", "Zain Alnemr")],
            duos: vec![choice("d1", "Zain Alnemr"), choice("d2", "Zain Alnemr")],
            mappings: vec![
                Mapping {
                    source: "cal-ft".into(),
                    mithf: "m1".into(),
                    duos: "d1".into(),
                    excluded: false,
                },
                Mapping {
                    source: "cal-vikar".into(),
                    mithf: String::new(),
                    duos: String::new(),
                    excluded: false,
                },
                Mapping {
                    source: "cal-møde".into(),
                    mithf: String::new(),
                    duos: String::new(),
                    excluded: true,
                },
            ],
            arrangement: String::new(),
            registration_type: String::new(),
            account: String::new(),
            result: None,
            blocked: None,
            has_credentials: true,
            connected_sources: vec!["teamup".into()],
            ical_rule: None,
        }
    }

    #[test]
    fn only_ambiguous_or_reopened_rows_show_dropdowns() {
        let state = confirmation_state();
        let ui = SetupUi {
            state: Some(state),
            ..SetupUi::default()
        };
        // Unique suggestions render as text.
        assert!(!needs_dropdown(
            &ui.state.as_ref().unwrap().mithf,
            "m1",
            &ui.editing,
            "cal-ft"
        ));
        // Ambiguous and unmapped rows stay open.
        assert!(needs_dropdown(
            &ui.state.as_ref().unwrap().mithf,
            "",
            &ui.editing,
            "cal-vikar"
        ));
        assert!(needs_dropdown(
            &ui.state.as_ref().unwrap().duos,
            "gone",
            &ui.editing,
            "cal-ft"
        ));
        // Reopening a suggestion shows its dropdowns until closed again.
        let mut ui = ui;
        ui.toggle_edit("cal-ft");
        assert!(needs_dropdown(
            &ui.state.as_ref().unwrap().mithf,
            "m1",
            &ui.editing,
            "cal-ft"
        ));
        ui.toggle_edit("cal-ft");
        assert!(!needs_dropdown(
            &ui.state.as_ref().unwrap().mithf,
            "m1",
            &ui.editing,
            "cal-ft"
        ));
    }

    #[test]
    fn table_renders_suggested_ambiguous_and_excluded_rows() {
        let mut ui = SetupUi {
            state: Some(confirmation_state()),
            ..SetupUi::default()
        };
        let _ = ui.view(Section::Helpers);
        ui.toggle_edit("cal-ft");
        let _ = ui.view(Section::Helpers);
    }

    #[test]
    fn providers_render_sources_and_services_and_collapse() {
        let mut state = confirmation_state();
        state.arrangements = vec![choice("a1", "SPS")];
        state.arrangement = "a1".into();
        state.types = vec![choice("t1", "Almindelig")];
        state.registration_type = "t1".into();
        let mut ui = SetupUi {
            state: Some(state),
            ..SetupUi::default()
        };
        let _ = ui.view(Section::Integrations);
        ui.toggle_provider("duos");
        assert!(ui.collapsed.contains("duos"));
        let _ = ui.view(Section::Integrations);
        // Without a valid type and no ordinary one offered, the picker stays.
        let mut state = confirmation_state();
        state.types = vec![choice("t9", "Sygdom")];
        ui.state = Some(state);
        ui.collapsed.clear();
        let _ = ui.view(Section::Integrations);
    }

    #[test]
    fn the_ical_form_renders_both_layouts_and_builds_the_rule() {
        let mut state = confirmation_state();
        state.source = "ical".into();
        state.stage = "source".into();
        let mut ui = SetupUi {
            state: Some(state.clone()),
            ..SetupUi::default()
        };
        let _ = ui.view(Section::Integrations);
        assert_eq!(ui.ical_rule(), HelperRule::Feed);

        // One calendar per helper: several links, each removable.
        ui.update_ical(&Message::IcalLink(0, "https://a.example/a.ics".into()));
        ui.update_ical(&Message::AddIcalLink);
        ui.update_ical(&Message::IcalLink(1, "webcal://b.example/b.ics".into()));
        let _ = ui.view(Section::Integrations);
        assert_eq!(ui.ical_links_to_connect().len(), 2);

        // A shared calendar uses the first link and a separator, "-" by default.
        ui.update_ical(&Message::IcalShared(true));
        let _ = ui.view(Section::Integrations);
        assert_eq!(ui.ical_links_to_connect(), ["https://a.example/a.ics"]);
        assert_eq!(
            ui.ical_rule(),
            HelperRule::Title {
                part: TitlePart::Before,
                separator: "-".into()
            }
        );
        ui.update_ical(&Message::IcalPart(TitlePart::After));
        ui.update_ical(&Message::IcalSeparator(":".into()));
        ui.update_ical(&Message::RemoveIcalLink(0));
        assert_eq!(ui.ical_links_to_connect(), ["webcal://b.example/b.ics"]);

        // A saved rule comes back when the setup is shown again.
        state.ical_rule = Some(ui.ical_rule());
        let mut fresh = SetupUi::default();
        fresh.load_ical(&state);
        assert_eq!(fresh.ical_rule(), ui.ical_rule());
        // A connected source that is not active still says so.
        state.connected_sources = vec!["ical".into(), "teamup".into()];
        fresh.state = Some(state);
        let _ = fresh.view(Section::Integrations);
    }
}
