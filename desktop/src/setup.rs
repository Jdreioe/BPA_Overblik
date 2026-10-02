use iced::widget::{
    button, column, container, image, pick_list, radio, row, space, text, text_input, toggler,
};
use iced::{Element, Length};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use teamup_shift_sync_core::ical::{HelperRule, TitlePart};
use teamup_shift_sync_core::live::Service;
use teamup_shift_sync_core::sheets::SheetLayout;
use teamup_shift_sync_core::standard_time::StandardTimes;
use teamup_shift_sync_core::{AbsenceMarking, AbsenceReason, DuosAbsence};
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
    /// The tabs of the last workbook read, when it has any.
    #[serde(default)]
    pub sheet_tabs: Vec<String>,
    #[serde(default)]
    pub ical_rule: Option<HelperRule>,
    #[serde(default)]
    pub standard_times: StandardTimes,
    /// Titles shown as markers instead of planned as shifts.
    #[serde(default = "default_markers")]
    pub markers: Vec<String>,
    #[serde(default = "teamup_shift_sync_core::default_absences")]
    pub absences: Vec<AbsenceMarking>,
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

fn default_markers() -> Vec<String> {
    teamup_shift_sync_core::live::DEFAULT_MARKERS
        .map(String::from)
        .to_vec()
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
    MarkerDraft(String),
    AddMarker,
    RemoveMarker(usize),
    AbsenceWords(AbsenceReason, String),
    AbsenceDuos(AbsenceReason, DuosChoice),
    SaveAbsences,
    Connect,
    ConnectSheets,
    IcalLink(usize, String),
    AddIcalLink,
    RemoveIcalLink(usize),
    IcalShared(bool),
    IcalSeparator(String),
    ConnectIcal,
    /// Open the OS file dialog for a local spreadsheet.
    ChooseFile,
    FileChosen(Option<String>),
    /// Go back from a chosen file to pasting a link.
    ClearFile,
    SheetTab(String),
    OpenTeamupKeys,
    CopyOrganization,
    CopyPurpose,
    /// Show this calendar's mapping on Hjælpere.
    SelectHelper(String),
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
const MARKER_DRAFT_ID: &str = "marker-draft";
const ABSENCE_WORD_IDS: [&str; 4] = [
    "absence-own-illness",
    "absence-child-illness",
    "absence-work-injury",
    "absence-other",
];
const TEAMUP_KEY_ID: &str = "teamup-key";

#[derive(Default)]
pub struct SetupUi {
    pub state: Option<SetupState>,
    pub link: String,
    pub key: String,
    pub template: Template,
    /// A local spreadsheet chosen instead of a link. Its path is shown only
    /// as a file name and stored only in the OS keyring.
    pub sheet_file: Option<String>,
    /// The workbook tab picked from [`SetupState::sheet_tabs`].
    pub sheet_tab: Option<String>,
    /// Why the typed standard times cannot be saved. Shown beside the fields;
    /// action results and errors go to the app's status notice instead.
    pub error: Option<String>,
    pub standard_default: String,
    pub standard_days: [String; 7],
    /// A marker title being typed, not saved until added.
    pub marker_draft: String,
    /// Each absence reason's words as typed, comma separated, in
    /// [`AbsenceReason::ALL`] order. Saved on Enter or **Gem ord**.
    pub absence_words: [String; 4],
    /// The calendar picked in the Hjælpere list. Without one, or when it is
    /// gone, the page shows [`shown_mapping`]'s choice.
    pub selected_helper: Option<String>,
    /// Providers collapsed on the providers tab. Everything starts open.
    pub collapsed: std::collections::BTreeSet<String>,
    /// iCal links being entered. Like `link`, only kept until stored.
    pub ical_links: Vec<String>,
    /// One shared calendar naming helpers in titles, not one per helper.
    pub ical_shared: bool,
    pub ical_part: TitlePart,
    pub ical_separator: String,
    /// The blocked-helpers notice has had its time on screen. The app resets
    /// this whenever the reason changes.
    pub blocked_hidden: bool,
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
        column![
            text("Standardtider").size(20),
            text("Bruges når en vagt ikke har egne tider.").size(13),
            self.standard_fields(),
        ]
        .spacing(10)
        .into()
    }

    /// The standard time fields without a heading, for the guide's own.
    pub fn standard_fields(&self) -> Element<'_, Message> {
        let mut content = column![].spacing(10);
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

    /// Show the absence words of a state about to replace `self.state`,
    /// except in a field the person is still editing: one whose text is
    /// neither what was saved before nor what was just saved.
    pub fn load_absences(&mut self, after: &SetupState) {
        for (index, reason) in AbsenceReason::ALL.iter().enumerate() {
            let saved = |state: &SetupState| words_text(&state.absences, *reason);
            let typed = &self.absence_words[index];
            let tidy = typed
                .split(',')
                .map(|w| w.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            if tidy == saved(after) || self.state.as_ref().is_none_or(|b| *typed == saved(b)) {
                self.absence_words[index] = saved(after);
            }
        }
    }

    /// The saved markings with the typed words and, if given, one changed
    /// DUOS choice. The engine validates and cleans them.
    pub fn absences(&self, changed: Option<(AbsenceReason, DuosAbsence)>) -> Vec<AbsenceMarking> {
        let saved = self
            .state
            .as_ref()
            .map_or_else(teamup_shift_sync_core::default_absences, |s| {
                s.absences.clone()
            });
        AbsenceReason::ALL
            .iter()
            .enumerate()
            .map(|(index, reason)| {
                let duos = match &changed {
                    Some((which, duos)) if which == reason => duos.clone(),
                    _ => saved
                        .iter()
                        .find(|m| m.reason == *reason)
                        .map(|m| m.duos.clone())
                        .unwrap_or_default(),
                };
                AbsenceMarking {
                    reason: *reason,
                    words: self.absence_words[index]
                        .split(',')
                        .map(str::to_owned)
                        .collect(),
                    duos,
                }
            })
            .collect()
    }

    /// One row per MitHF absence reason: the words that start its lines and,
    /// with DUOS on, how the absent helper's SPS hours are registered.
    pub fn absences_view(&self) -> Element<'_, Message> {
        let Some(state) = &self.state else {
            return column![].into();
        };
        let edited = AbsenceReason::ALL
            .iter()
            .enumerate()
            .any(|(index, reason)| {
                self.absence_words[index] != words_text(&state.absences, *reason)
            });
        let mut content = column![
            text("Fravær").size(20),
            text("Skriv en linje i vagtens noter eller kommentarer, når en anden tog vagten. »SYG: Anna« gælder hele vagten, »SYG 8-12: Anna« kun det tidsrum. Den planlagte hjælper meldes syg i MitHF, og Anna sættes på vagten.").size(13),
        ]
        .spacing(12);
        let duos_choices: Vec<DuosChoice> = std::iter::once(DuosChoice::skip())
            .chain(state.types.iter().map(DuosChoice::of))
            .collect();
        for (index, reason) in AbsenceReason::ALL.iter().enumerate() {
            let reason = *reason;
            let mut row_content = column![
                text(format!("MitHF: {}", reason.label()))
                    .size(14)
                    .font(iced::Font {
                        weight: iced::font::Weight::Bold,
                        ..iced::Font::DEFAULT
                    }),
                text_input("Ingen ord – slået fra", &self.absence_words[index])
                    .id(iced::widget::Id::new(ABSENCE_WORD_IDS[index]))
                    .on_input(move |value| Message::AbsenceWords(reason, value))
                    .on_submit(Message::SaveAbsences)
                    .padding(10)
                    .width(Length::Fixed(360.0)),
            ]
            .spacing(6);
            if state.duos_enabled && !state.types.is_empty() {
                let current = state
                    .absences
                    .iter()
                    .find(|m| m.reason == reason)
                    .map(|m| &m.duos);
                let chosen = duos_choices
                    .iter()
                    .find(|c| Some(&c.value) == current)
                    .cloned();
                row_content = row_content.push(
                    row![
                        text("DUOS-type for den syges SPS-timer")
                            .size(13)
                            .width(Length::Fixed(230.0)),
                        pick_list(duos_choices.clone(), chosen, move |choice| {
                            Message::AbsenceDuos(reason, choice)
                        })
                        .placeholder("Vælg type"),
                    ]
                    .spacing(10)
                    .align_y(iced::alignment::Vertical::Center),
                );
            }
            content = content.push(row_content);
        }
        content
            .push(
                text("Adskil flere ord med komma. Store og små bogstaver er ligegyldige.").size(12),
            )
            .push(
                quiet_button("Gem ord", Message::SaveAbsences)
                    .on_press_maybe(edited.then_some(Message::SaveAbsences)),
            )
            .into()
    }

    /// The marker titles with a way to add and remove them. Saving happens
    /// per click; there is nothing half-typed to autosave.
    pub fn markers_view(&self) -> Element<'_, Message> {
        let markers = self
            .state
            .as_ref()
            .map_or(&[][..], |state| state.markers.as_slice());
        let mut list = column![].spacing(6);
        for (index, title) in markers.iter().enumerate() {
            list = list.push(
                row![
                    text(title).width(Length::Fill),
                    quiet_button("Fjern", Message::RemoveMarker(index)),
                ]
                .spacing(10)
                .align_y(iced::alignment::Vertical::Center),
            );
        }
        let draft = self.marker_draft.trim();
        column![
            text("Markeringer").size(20),
            list,
            row![
                text_input("Fx Ferie", &self.marker_draft)
                    .id(iced::widget::Id::new(MARKER_DRAFT_ID))
                    .on_input(Message::MarkerDraft)
                    .on_submit_maybe((!draft.is_empty()).then_some(Message::AddMarker))
                    .padding(10)
                    .width(Length::Fixed(300.0)),
                quiet_button("Tilføj", Message::AddMarker)
                    .on_press_maybe((!draft.is_empty()).then_some(Message::AddMarker)),
            ]
            .spacing(10)
            .align_y(iced::alignment::Vertical::Center),
        ]
        .spacing(10)
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
            return content
                .push(text(
                    "Hjælperne hentes automatisk, når forbindelserne er klar.",
                ))
                .into();
        }
        if let Some(blocked) = state.blocked.as_ref().filter(|_| !self.blocked_hidden) {
            content = content.push(crate::widgets::notice_card(
                Notice::from_message(Tone::Warning, blocked),
                None,
            ));
        }
        let shown = shown_mapping(state, self.selected_helper.as_deref());
        let mut list = column![text("Vælg hjælper").size(13)].spacing(6);
        // Excluded calendars last: they are not helpers.
        let (kept, excluded): (Vec<&Mapping>, Vec<&Mapping>) =
            state.mappings.iter().partition(|m| !m.excluded);
        for mapping in kept.into_iter().chain(excluded) {
            list = list.push(self.helper_entry(state, mapping, shown));
        }
        let mut page = row![list.width(Length::Fixed(260.0))].spacing(24);
        if let Some(mapping) = shown {
            page = page.push(self.helper_detail(state, mapping));
        }
        content.push(page).into()
    }

    /// One calendar in the Hjælpere list: its name, its group when the
    /// TeamUp name has one (»Deltid > Anna«), and whether it is ready, in
    /// words beside the colour.
    fn helper_entry<'a>(
        &'a self,
        state: &'a SetupState,
        mapping: &'a Mapping,
        shown: Option<&Mapping>,
    ) -> Element<'a, Message> {
        let (group, name) = split_calendar(calendar_name(state, mapping));
        let mut label = column![text(name).size(15)].width(Length::Fill);
        if let Some(group) = group {
            label = label.push(text(group).size(12));
        }
        let mark = if mapping.excluded {
            "Udeladt"
        } else if needs_choice(state, mapping) {
            "! Vælg"
        } else {
            "✓"
        };
        let selected = shown.is_some_and(|shown| shown.source == mapping.source);
        button(
            row![label, text(mark).size(13)]
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center),
        )
        .style(crate::widgets::chosen(selected, 6.0))
        .padding([8, 12])
        .width(Length::Fill)
        .on_press(Message::SelectHelper(mapping.source.clone()))
        .into()
    }

    /// The shown calendar's mapping: TeamUp → MitHF → DUOS in one row, what
    /// that means, and excluding or including the calendar. A choice saves
    /// as soon as it is picked, as it always has.
    fn helper_detail<'a>(
        &'a self,
        state: &'a SetupState,
        mapping: &'a Mapping,
    ) -> Element<'a, Message> {
        let calendar = calendar_name(state, mapping);
        let source_id = mapping.source.clone();
        let heading = text(split_calendar(calendar).1).size(18);
        if mapping.excluded {
            return column![
                heading,
                text("Kalenderen er udeladt, så dens vagter overføres ikke."),
                row![quiet_button(
                    "Medtag igen",
                    Message::Action("edit", json!({"source": source_id, "excluded": false})),
                )],
            ]
            .spacing(12)
            .width(Length::Fill)
            .into();
        }
        let field = |label: &'a str, value: Element<'a, Message>| {
            column![text(label).size(13), value]
                .spacing(4)
                .width(Length::FillPortion(1))
        };
        // Level with the fields' middle, under their labels.
        let arrow = || container(text("→").size(16)).padding(iced::Padding::ZERO.bottom(10));
        let source_label = match state.source.as_str() {
            "sheets" => "Regneark",
            "ical" => "Kalender",
            _ => "TeamUp",
        };
        let mut flow = row![
            field(
                source_label,
                crate::widgets::card(text(calendar).wrapping(text::Wrapping::None)),
            ),
            arrow(),
            field(
                "MitHF",
                self.choice(
                    &state.mithf,
                    &mapping.mithf,
                    &mapping.source,
                    "mithf",
                    "Vælg hjælper"
                ),
            ),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Bottom);
        if state.duos_enabled {
            flow = flow.push(arrow()).push(field(
                "DUOS",
                self.choice(
                    &state.duos,
                    &mapping.duos,
                    &mapping.source,
                    "duos",
                    "Vælg aktiv hjælper",
                ),
            ));
        }
        column![
            heading,
            flow,
            text(format!(
                "Vagter i »{calendar}« overføres til disse personer. Vælg en anden, hvis et navn er forkert."
            ))
            .size(13),
            row![quiet_button(
                "Udelad kalenderen",
                Message::Action("edit", json!({"source": source_id, "excluded": true})),
            )],
        ]
        .spacing(12)
        .width(Length::Fill)
        .into()
    }

    /// A MitHF or DUOS person for one calendar. Picking saves at once.
    fn choice<'a>(
        &'a self,
        choices: &'a [Choice],
        selected: &str,
        source: &str,
        service: &'static str,
        placeholder: &'static str,
    ) -> Element<'a, Message> {
        let options = disambiguate(choices);
        let current = find(&options, selected);
        let source_id = source.to_owned();
        pick_list(options, current, move |c: Choice| {
            let mut params = json!({"source": source_id});
            params[service] = json!(c.id);
            Message::Action("edit", params)
        })
        .placeholder(placeholder)
        .padding(10)
        .width(Length::Fill)
        .into()
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
            self.source_row(state, "sheets", "Regneark"),
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
    pub fn source_connection<'a>(&'a self, state: &'a SetupState) -> Element<'a, Message> {
        let mut content = column![].spacing(8);
        if state.source == "ical" {
            return self.ical_connection();
        }
        if state.source == "sheets" {
            // One step at a time: where the sheet is, what a shift looks
            // like, then connect. A step shows once the one before is done.
            let step = |title: &'static str| text(title).size(16);
            content = content.push(step("1. Hvor ligger dit regneark?"));
            content = match &self.sheet_file {
                Some(path) => content.push(
                    row![
                        text(file_name(path)).width(Length::Fill),
                        quiet_button("Vælg en anden fil", Message::ChooseFile),
                        quiet_button("Brug et link", Message::ClearFile),
                    ]
                    .spacing(8)
                    .align_y(iced::alignment::Vertical::Center),
                ),
                None => content.push(
                    row![
                        text_input("Indsæt link", &self.link)
                            .id(iced::widget::Id::new(SOURCE_LINK_ID))
                            .on_input(Message::Link)
                            .secure(true)
                            .padding(12),
                        quiet_button("Vælg fil …", Message::ChooseFile),
                    ]
                    .spacing(8)
                    .align_y(iced::alignment::Vertical::Center),
                ),
            };
            if self.sheet_file.is_none() && self.link.trim().is_empty() {
                return content.into();
            }
            content = content
                .push(step("2. Hvordan ser en vagt ud for dig?"))
                .push(
                    self.template
                        .view(state.sheet_layout.is_some())
                        .map(Message::Template),
                );
            if self.sheet_layout().is_err() {
                return content.into();
            }
            content = content.push(step("3. Tilslut"));
            if state.sheet_tabs.len() > 1 {
                content = content.push(
                    pick_list(
                        state.sheet_tabs.as_slice(),
                        self.sheet_tab.as_ref(),
                        Message::SheetTab,
                    )
                    .placeholder("Vælg fanen med vagtplanen"),
                );
            }
            content = content.push(primary_button("Tilslut regneark", Message::ConnectSheets));
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
            // The name comes before the separator, or is the whole title
            // when there is none. A saved "after" rule still works.
            content = content.push(
                row![
                    text("Tegn efter navnet").size(13),
                    text_input("-", &self.ical_separator)
                        .id(iced::widget::Id::new(ICAL_SEPARATOR_ID))
                        .on_input(Message::IcalSeparator)
                        .padding(10)
                        .width(Length::Fixed(80.0)),
                ]
                .spacing(10)
                .align_y(iced::alignment::Vertical::Center),
            );
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
        if let Some(picker) = arrangement_picker(state) {
            content = content.push(text("DUOS SPS-ordning").size(13)).push(picker);
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

    pub fn toggle_provider(&mut self, id: &str) {
        if !self.collapsed.remove(id) {
            self.collapsed.insert(id.to_owned());
        }
    }
}

/// The DUOS arrangements found, once there are any. Picking one reads it in
/// full, like a fresh discovery.
pub fn arrangement_picker(state: &SetupState) -> Option<Element<'_, Message>> {
    if state.arrangements.is_empty() {
        return None;
    }
    Some(
        pick_list(
            state.arrangements.clone(),
            find(&state.arrangements, &state.arrangement),
            |c: Choice| Message::Action("discover", json!({"arrangement": c.id})),
        )
        .placeholder("Vælg ordning")
        .padding(8)
        .width(Length::Fixed(280.0))
        .into(),
    )
}

/// The name of the chosen DUOS arrangement, if one is chosen.
pub fn arrangement_name(state: &SetupState) -> Option<&str> {
    state
        .arrangements
        .iter()
        .find(|choice| choice.id == state.arrangement)
        .map(|choice| choice.name.as_str())
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

/// Only the file's own name: the folders around it can name the user.
fn file_name(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
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

fn words_text(markings: &[AbsenceMarking], reason: AbsenceReason) -> String {
    markings
        .iter()
        .find(|m| m.reason == reason)
        .map(|m| m.words.join(", "))
        .unwrap_or_default()
}

/// A DUOS absence choice in the dropdown: a type of the arrangement, or not
/// registering the absent helper's hours at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuosChoice {
    label: String,
    pub value: DuosAbsence,
}
impl DuosChoice {
    fn skip() -> Self {
        Self {
            label: "Registreres ikke".into(),
            value: DuosAbsence::Skip,
        }
    }
    fn of(choice: &Choice) -> Self {
        Self {
            label: choice.name.clone(),
            value: DuosAbsence::Type(choice.id.clone()),
        }
    }
}
impl std::fmt::Display for DuosChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

fn find(choices: &[Choice], id: &str) -> Option<Choice> {
    choices.iter().find(|c| c.id == id).cloned()
}

fn calendar_name<'a>(state: &'a SetupState, mapping: &Mapping) -> &'a str {
    state
        .calendars
        .iter()
        .find(|c| c.id == mapping.source)
        .map_or("Ukendt kalender", |c| c.name.as_str())
}

/// »Deltid > Anna« as its group and the helper's name, for the list. A name
/// without a group is only a name.
fn split_calendar(name: &str) -> (Option<&str>, &str) {
    match name.rsplit_once(" > ") {
        Some((group, helper)) => (Some(group), helper),
        None => (None, name),
    }
}

/// A kept calendar without a known MitHF person, or DUOS person when DUOS
/// is on, still needs someone chosen.
fn needs_choice(state: &SetupState, mapping: &Mapping) -> bool {
    !mapping.excluded
        && (find(&state.mithf, &mapping.mithf).is_none()
            || (state.duos_enabled && find(&state.duos, &mapping.duos).is_none()))
}

/// The mapping Hjælpere shows: the picked one while it exists, otherwise the
/// first that still needs a choice, otherwise the first.
fn shown_mapping<'a>(state: &'a SetupState, picked: Option<&str>) -> Option<&'a Mapping> {
    let mappings = &state.mappings;
    picked
        .and_then(|picked| mappings.iter().find(|m| m.source == picked))
        .or_else(|| mappings.iter().find(|m| needs_choice(state, m)))
        .or_else(|| mappings.first())
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
            sheet_tabs: vec![],
            standard_times: Default::default(),
            markers: vec![],
            absences: teamup_shift_sync_core::default_absences(),
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

    /// Hjælpere opens on what still needs a choice, keeps a picked calendar
    /// while it exists, and falls back when it is gone.
    #[test]
    fn helpers_show_the_picked_calendar_or_the_first_needing_a_choice() {
        let state = confirmation_state();
        let shown = |picked| shown_mapping(&state, picked).map(|m| m.source.as_str());
        assert_eq!(shown(None), Some("cal-vikar"));
        assert_eq!(shown(Some("cal-møde")), Some("cal-møde"));
        assert_eq!(shown(Some("gone")), Some("cal-vikar"));
        assert!(!needs_choice(&state, &state.mappings[0]));
        assert!(!needs_choice(&state, &state.mappings[2]), "excluded");
        assert_eq!(split_calendar("FT > Zain"), (Some("FT"), "Zain"));
        assert_eq!(split_calendar("Vikar"), (None, "Vikar"));
    }

    #[test]
    fn helpers_render_the_list_and_each_kind_of_detail() {
        let mut ui = SetupUi {
            state: Some(confirmation_state()),
            ..SetupUi::default()
        };
        for picked in [None, Some("cal-ft"), Some("cal-møde")] {
            ui.selected_helper = picked.map(str::to_owned);
            let _ = ui.view(Section::Helpers);
        }
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
