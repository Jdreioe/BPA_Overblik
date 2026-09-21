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
    pub continues_before: bool,
    pub continues_after: bool,
    #[serde(default)]
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Day {
    pub date: String,
    pub label: String,
    #[serde(default)]
    pub blocks: Vec<Block>,
}

/// An item the user must resolve, with its cause and next action.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Attention {
    pub when: String,
    pub who: String,
    pub explanation: String,
    pub action: String,
}

/// The readable week: grid, attention items and the exact approval summary.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Week {
    #[serde(default)]
    pub days: Vec<Day>,
    #[serde(default)]
    pub attention: Vec<Attention>,
    #[serde(default)]
    pub headline: String,
    #[serde(default)]
    pub notice: String,
    #[serde(default)]
    pub summary: Vec<String>,
    #[serde(default)]
    pub apply_summary: String,
    #[serde(default)]
    pub can_apply: bool,
    #[serde(default)]
    pub blocked_reason: String,
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
                           "explanation": "…", "action": "…"}],
            "headline": "Ugen er klar til overførsel.", "notice": "",
            "summary": ["1 ny vagt i MitHF"], "apply_summary": "Overfører …",
            "can_apply": true, "blocked_reason": "", "destination_read": true,
        }))
        .unwrap();
        assert!(week.can_apply);
        assert_eq!(week.days[0].blocks[0].part_label, "Del 1 af 2");
        assert_eq!(week.days[0].blocks[0].helper_color, "#4770d8");
        assert_eq!(week.attention.len(), 1);
    }
}
