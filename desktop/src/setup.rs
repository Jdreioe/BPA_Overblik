use iced::widget::{button, checkbox, column, pick_list, row, text, text_input};
use iced::Element;
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
    Action(&'static str, Value),
}
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetupMessage")
    }
}

#[derive(Default)]
pub struct SetupUi {
    pub state: Option<SetupState>,
    pub link: String,
    pub key: String,
    pub busy: bool,
    pub error: Option<String>,
}
impl SetupUi {
    pub fn view(&self) -> Element<'_, Message> {
        let mut content = column![text("Opsætning").size(24)].spacing(12);
        if let Some(error) = &self.error {
            content = content.push(text(error));
        }
        if self.busy {
            return content.push(text("Kontrollerer og gemmer …")).into();
        }
        let Some(state) = &self.state else {
            return content
                .push(
                    button("Hent opsætning")
                        .padding(14)
                        .on_press(Message::Action("status", json!({}))),
                )
                .into();
        };
        if !state.notice.is_empty() {
            content = content.push(text(&state.notice));
        }
        if state.stage == "source" {
            content = content.push(text("1. Tilslut TeamUp"))
                .push(text_input("TeamUp-kalenderlink", &self.link).on_input(Message::Link).secure(true).padding(12))
                .push(text("TeamUp kræver en API-nøgle én gang. Bed appens vedligeholder om hjælp, eller anmod om din egen på teamup.com/api-keys/request. Nøglen gemmes i computerens nøglering."))
                .push(text_input("TeamUp API-nøgle", &self.key).on_input(Message::Key).secure(true).padding(12))
                .push(button("Tilslut kalender").padding(14).on_press(Message::Connect));
            if state.has_credentials {
                content = content.push(
                    button("Prøv den gemte forbindelse igen")
                        .padding(12)
                        .on_press(Message::Action("retry_source", json!({}))),
                );
            }
            if state.can_import {
                content = content.push(
                    button("Importér tidligere opsætning til gennemgang")
                        .padding(12)
                        .on_press(Message::Action("import", json!({}))),
                );
            }
        } else {
            content = content
                .push(text(
                    "2. Log ind i MitHF og DUOS nedenfor, og hent mulighederne.",
                ))
                .push(
                    button("Hent hjælpere og ordninger")
                        .padding(14)
                        .on_press(Message::Action("discover", json!({}))),
                );
            if !state.account.is_empty() {
                content = content.push(text(&state.account));
            }
            if !state.arrangements.is_empty() {
                content = content.push(text("DUOS SPS-ordning")).push(
                    pick_list(
                        state.arrangements.clone(),
                        find(&state.arrangements, &state.arrangement),
                        |c: Choice| Message::Action("discover", json!({"arrangement":c.id})),
                    )
                    .placeholder("Vælg ordning")
                    .padding(12),
                );
            }
            if !state.types.is_empty() {
                content = content.push(text("Registreringstype")).push(
                    pick_list(
                        state.types.clone(),
                        find(&state.types, &state.registration_type),
                        |c: Choice| Message::Action("edit", json!({"registration_type":c.id})),
                    )
                    .placeholder("Vælg registreringstype")
                    .padding(12),
                );
            }
            if !state.mappings.is_empty() {
                content = content.push(text("3. Bekræft hjælpere").size(20))
                    .push(text("Kontrollér alle tre navne. Forslag er ikke bekræftede. Udelad kalendere, der ikke indeholder hjælpervagter."));
                for mapping in &state.mappings {
                    let source = state.calendars.iter().find(|c| c.id == mapping.source);
                    let source_id = mapping.source.clone();
                    let mut helper = column![
                        text(format!(
                            "TeamUp: {}",
                            source.map_or("Ukendt kalender", |c| c.name.as_str())
                        )),
                        checkbox(mapping.excluded)
                            .label("Udelad kalender")
                            .on_toggle(move |excluded| Message::Action(
                                "edit",
                                json!({"source":source_id,"excluded":excluded})
                            ))
                    ]
                    .spacing(8);
                    if !mapping.excluded {
                        let source_id = mapping.source.clone();
                        let duos_source = mapping.source.clone();
                        let mit = disambiguate(&state.mithf);
                        let duos = disambiguate(&state.duos);
                        helper = helper
                            .push(
                                row![
                                    text("MitHF"),
                                    pick_list(
                                        mit.clone(),
                                        find(&mit, &mapping.mithf),
                                        move |c: Choice| Message::Action(
                                            "edit",
                                            json!({"source":source_id,"mithf":c.id})
                                        )
                                    )
                                    .placeholder("Vælg hjælper")
                                    .padding(12)
                                ]
                                .spacing(12),
                            )
                            .push(
                                row![
                                    text("DUOS"),
                                    pick_list(
                                        duos.clone(),
                                        find(&duos, &mapping.duos),
                                        move |c: Choice| Message::Action(
                                            "edit",
                                            json!({"source":duos_source,"duos":c.id})
                                        )
                                    )
                                    .placeholder("Vælg aktiv hjælper")
                                    .padding(12)
                                ]
                                .spacing(12),
                            );
                    }
                    content = content.push(helper);
                }
                content = content.push(text("Bekræft, at kunden, bevillingen, SPS-ordningen, registreringstypen og alle hjælpernavne er korrekte."))
                    .push(button("Bekræft og se denne uge").padding(14)
                        .on_press(Message::Action("confirm", json!({}))));
            }
        }
        if state.stage != "source" {
            content = content.push(
                button("Ret kalenderforbindelsen")
                    .padding(12)
                    .on_press(Message::Action("source", json!({}))),
            );
        }
        content.into()
    }
}

fn find(choices: &[Choice], id: &str) -> Option<Choice> {
    choices.iter().find(|c| c.id == id).cloned()
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
