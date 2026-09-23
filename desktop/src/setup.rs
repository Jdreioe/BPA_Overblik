use iced::widget::{button, column, pick_list, row, text, text_input};
use iced::{Element, Length};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
    pub calendars: Vec<Choice>,
    pub arrangements: Vec<Choice>,
    pub types: Vec<Choice>,
    pub mithf: Vec<Choice>,
    pub duos: Vec<Choice>,
    pub mappings: Vec<Mapping>,
    pub arrangement: String,
    pub registration_type: String,
    pub account: String,
    pub notice: String,
    pub can_import: bool,
    pub has_credentials: bool,
}

// These inputs contain credentials. Do not derive Debug with their contents.
#[derive(Clone)]
pub enum Message {
    Link(String),
    Key(String),
    Connect,
    OpenTeamupKeys,
    CopyOrganization,
    CopyPurpose,
    ToggleEdit(String),
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

#[derive(Default)]
pub struct SetupUi {
    pub state: Option<SetupState>,
    pub link: String,
    pub key: String,
    pub busy: bool,
    pub error: Option<String>,
    /// Calendars the person reopened for editing. A row shows dropdowns
    /// while it is ambiguous (no match yet) or reopened here; a unique
    /// suggestion otherwise renders as plain text. Nothing is confirmed
    /// until the joint confirmation below.
    pub editing: std::collections::BTreeSet<String>,
}
impl SetupUi {
    pub fn view(&self) -> Element<'_, Message> {
        let mut content = column![].spacing(8);
        if let Some(error) = &self.error {
            content = content.push(text(error));
        }
        if self.busy {
            return content.push(text("Kontrollerer og gemmer …")).into();
        }
        let Some(state) = &self.state else {
            return content
                .push(primary_button(
                    "Hent opsætning",
                    Message::Action("status", json!({})),
                ))
                .into();
        };
        if !state.notice.is_empty() {
            content = content.push(text(&state.notice));
        }
        if state.stage == "source" {
            content = content
                .push(
                    text_input("TeamUp-kalenderlink", &self.link)
                        .on_input(Message::Link)
                        .secure(true)
                        .padding(12),
                )
                .push(
                    text_input("TeamUp API-nøgle", &self.key)
                        .on_input(Message::Key)
                        .secure(true)
                        .padding(12),
                )
                .push(
                    text("Brug din egen mail hos TeamUp. Organisation skal være over 5 bogstaver. Nøglen gemmes i nøgleringen.").size(12),
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
            if state.has_credentials || state.can_import {
                let mut extra = row![].spacing(8);
                if state.has_credentials {
                    extra = extra.push(quiet_button(
                        "Prøv den gemte forbindelse igen",
                        Message::Action("retry_source", json!({})),
                    ));
                }
                if state.can_import {
                    extra = extra.push(quiet_button(
                        "Importér tidligere opsætning",
                        Message::Action("import", json!({})),
                    ));
                }
                content = content.push(extra);
            }
        } else {
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
            if !state.types.is_empty() {
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
            if state.mappings.is_empty() {
                content = content.push(primary_button(
                    "Hent hjælpere og ordninger",
                    Message::Action("discover", json!({})),
                ));
            } else {
                content = content.push(
                    row![
                        text("TeamUp").width(Length::FillPortion(2)),
                        text("MitHF").width(Length::FillPortion(2)),
                        text("DUOS").width(Length::FillPortion(2)),
                    ]
                    .spacing(8),
                );
                for mapping in &state.mappings {
                    content = content.push(self.mapping_row(state, mapping));
                }
                content = content.push(primary_button(
                    "Bekræft og se denne uge",
                    Message::Action("confirm", json!({})),
                ));
            }
            let mut extra = row![quiet_button(
                "Ret kalenderforbindelsen",
                Message::Action("source", json!({})),
            )]
            .spacing(8);
            if !state.mappings.is_empty() {
                extra = extra.push(quiet_button(
                    "Hent hjælpere og ordninger",
                    Message::Action("discover", json!({})),
                ));
            }
            content = content.push(extra);
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
        row![
            teamup,
            self.choice_cell(
                &state.mithf,
                &mapping.mithf,
                &mapping.source,
                "mithf",
                "Vælg hjælper"
            ),
            self.choice_cell(
                &state.duos,
                &mapping.duos,
                &mapping.source,
                "duos",
                "Vælg aktiv hjælper"
            ),
        ]
        .spacing(8)
        .into()
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
            notice: String::new(),
            can_import: false,
            has_credentials: true,
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
        let _ = ui.view();
        ui.toggle_edit("cal-ft");
        let _ = ui.view();
    }
}
