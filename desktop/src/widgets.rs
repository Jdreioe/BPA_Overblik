//! Shared Danish week widgets: the hour grid and the day-by-day list.
//!
//! Both render the same [`Week`] the core-based preview builds, so the grid
//! and the text below it can never disagree about what the week contains.

use chrono::{Datelike, NaiveDate};
use iced::widget::{column, container, row, space, text};
use iced::{Color, Element, Length};
use teamup_shift_sync_gui::protocol::{Block, Week};

pub const DANISH_MONTHS: [&str; 12] = [
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
pub fn week_grid<'a, M: 'a>(week: &'a Week) -> Element<'a, M> {
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

fn block_body<'a, M: 'a>(block: &'a Block) -> Element<'a, M> {
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
pub fn day_details<'a, M: 'a>(week: &'a Week) -> Element<'a, M> {
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

pub fn format_week_da(start: NaiveDate, end: NaiveDate) -> String {
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

    fn test_week() -> Week {
        serde_json::from_value(serde_json::json!({
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
        }))
        .unwrap()
    }

    #[test]
    fn week_widgets_render_without_panicking() {
        let week = test_week();
        let _ = week_grid::<()>(&week);
        let _ = day_details::<()>(&week);
        let empty = Week::default();
        let _ = week_grid::<()>(&empty);
        let _ = day_details::<()>(&empty);
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
        let template = test_week().days[0].blocks[0].clone();
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
        let mut block = test_week().days[0].blocks[0].clone();
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
