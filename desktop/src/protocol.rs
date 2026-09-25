//! Danish week view models shared by the preview builder and the widgets.
//!
//! Everything here is already Danish and safe to display as-is.

use serde::{Deserialize, Serialize};

/// One MitHF shift as drawn in a single day column of the week grid.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Block {
    pub helper: String,
    /// The helper's Teamup calendar colour as `#rrggbb`, or empty when
    /// unknown. Tinted for the block fill so the week reads like Teamup.
    #[serde(default)]
    pub helper_color: String,
    pub status: String,
    pub status_label: String,
    /// Minutes from local midnight, for the block's position and height.
    pub minutes_from: u32,
    pub minutes_to: u32,
    pub time_label: String,
    pub sps_label: String,
    pub part_label: String,
    /// MitHF's reason when this is the planned helper's shift reported
    /// absent, such as "Egen sygdom"; empty otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub absence: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub standard_time: bool,
    pub continues_before: bool,
    pub continues_after: bool,
    #[serde(default)]
    pub details: Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// A calendar marker, such as a day-off wish, on each day it touches. It is
/// never transferred, so it has no status and no position on the clock.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Marker {
    pub helper: String,
    #[serde(default)]
    pub helper_color: String,
    pub title: String,
    /// "hele dagen", or the marker's own clock times.
    pub time_label: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Day {
    pub date: String,
    pub label: String,
    #[serde(default)]
    pub blocks: Vec<Block>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
}

/// An item the user must resolve, with its cause and next action.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Attention {
    pub when: String,
    pub who: String,
    pub explanation: String,
    pub action: String,
    /// The settings page that fixes this item, when the fix is in setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<SettingsLink>,
}

/// A settings page an attention item can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsLink {
    Helpers,
    Integrations,
    Absences,
}

/// How a notice reads at a glance. The card shows it as an icon and text as
/// well as a colour, so it never depends on colour alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

/// One status or action result: a short outcome and, only when needed, the
/// next step. The detail never restates the title.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Notice {
    pub tone: Tone,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl Notice {
    pub fn new(tone: Tone, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            tone,
            title: title.into(),
            detail: detail.into(),
        }
    }

    /// A Danish message from the core or the engine. They are written as
    /// "Outcome. Next step.", so the first sentence is the title and the rest
    /// the detail. A sentence ends where the next word starts with a capital
    /// or a count ("… 3 hjælpere. 1 vagt …"), but not after a day number, so
    /// "23. sep" stays whole.
    pub fn from_message(tone: Tone, message: &str) -> Self {
        let message = message.trim();
        let end = message.match_indices(". ").map(|(at, _)| at).find(|&at| {
            !message[..at].ends_with(|c: char| c.is_ascii_digit())
                && message[at + 2..].starts_with(|c: char| c.is_uppercase() || c.is_ascii_digit())
        });
        match end {
            Some(at) => Self::new(tone, &message[..at], message[at + 2..].trim()),
            None => Self::new(tone, message.strip_suffix('.').unwrap_or(message), ""),
        }
    }
}

/// The readable week: grid, attention items and the exact approval summary.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Week {
    #[serde(default)]
    pub days: Vec<Day>,
    #[serde(default)]
    pub attention: Vec<Attention>,
    /// The week's one status, shown as a notice above the grid.
    #[serde(default)]
    pub status: Notice,
    #[serde(default)]
    pub summary: Vec<String>,
    #[serde(default)]
    pub apply_summary: String,
    #[serde(default)]
    pub can_apply: bool,
    #[serde(default)]
    pub destination_read: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn week_payload_deserializes() {
        let week: Week = serde_json::from_value(serde_json::json!({
            "days": [{"date": "2026-09-14", "label": "man 14. sep", "blocks": [{
                "helper": "Zain Alnemr", "helper_color": "#4770d8",
                "status": "create", "status_label": "Oprettes",
                "minutes_from": 450, "minutes_to": 780, "time_label": "07:30–13:00",
                "sps_label": "08:00–10:00", "part_label": "Del 1 af 2",
                "continues_before": false, "continues_after": false,
                "details": ["Vagten oprettes i MitHF."]
            }]}],
            "attention": [{"when": "man 14. sep 07:30", "who": "Zain Alnemr",
                           "explanation": "…", "action": "…",
                           "settings": "helpers"}],
            "status": {"tone": "info", "title": "Ugen er klar til godkendelse",
                       "detail": "Overfører …"},
            "summary": ["1 ny vagt i MitHF"], "apply_summary": "Overfører …",
            "can_apply": true, "destination_read": true,
        }))
        .unwrap();
        assert!(week.can_apply);
        assert_eq!(week.days[0].blocks[0].part_label, "Del 1 af 2");
        assert_eq!(week.days[0].blocks[0].helper_color, "#4770d8");
        assert_eq!(week.attention.len(), 1);
        assert_eq!(
            week.attention[0].settings,
            Some(super::SettingsLink::Helpers)
        );
    }

    #[test]
    fn a_message_splits_into_outcome_and_next_step() {
        let notice = Notice::from_message(
            Tone::Error,
            "MitHF kunne ikke læses. Log ind i MitHF igen, og prøv igen.",
        );
        assert_eq!(notice.title, "MitHF kunne ikke læses");
        assert_eq!(notice.detail, "Log ind i MitHF igen, og prøv igen.");
        let single = Notice::from_message(Tone::Info, "Tilslut vagtkilden først.");
        assert_eq!(
            (single.title.as_str(), single.detail.as_str()),
            ("Tilslut vagtkilden først", "")
        );
        let dated = Notice::from_message(Tone::Info, "Hentet 23. sep. Intet ændret.");
        assert_eq!(dated.title, "Hentet 23. sep");
        let counted = Notice::from_message(
            Tone::Info,
            "Regnearket er læst: 12 vagter og 3 hjælpere. 1 vagt skal rettes, før deres uge kan overføres.",
        );
        assert_eq!(counted.title, "Regnearket er læst: 12 vagter og 3 hjælpere");
    }
}
