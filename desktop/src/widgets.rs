//! Shared Danish week widgets: the hour grid and the quiet button style.
//!
//! The grid renders the [`Week`] the core-based preview builds. The side
//! panel beside it (in `native.rs`) carries the week's state and the
//! selected shift's details, so the cells stay short.

use chrono::{Datelike, NaiveDate};
use iced::widget::{button, column, container, row, space, text, tooltip};
use iced::{Color, Element, Length};
use teamup_shift_sync_gui::protocol::{Block, Marker, Notice, Tone, Week};

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

/// Style for every low-emphasis action. Iced's `button::text` style draws
/// nothing but a label, which reads as running text rather than as something
/// to press; this keeps the same quiet weight but gives the button an outline
/// and a hover fill, so it is unmistakably clickable. Filled `primary` and
/// `secondary` buttons still carry the loud actions.
pub fn outlined(theme: &iced::Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let base = button::Style {
        background: Some(palette.background.weakest.color.into()),
        text_color: palette.background.base.text,
        border: iced::Border {
            color: palette.background.strong.color,
            width: 1.0,
            radius: 2.0.into(),
        },
        ..button::Style::default()
    };
    match status {
        button::Status::Active => base,
        button::Status::Hovered => button::Style {
            background: Some(palette.background.weak.color.into()),
            ..base
        },
        button::Status::Pressed => button::Style {
            background: Some(palette.background.strong.color.into()),
            ..base
        },
        // A disabled action stays legible as a button, just visibly inactive.
        button::Status::Disabled => button::Style {
            text_color: palette.background.base.text.scale_alpha(0.4),
            border: iced::Border {
                color: palette.background.strong.color.scale_alpha(0.4),
                ..base.border
            },
            ..base
        },
    }
}

/// An outlined choice: tabs, settings sections, list entries and choice
/// cards. The selected one gets a thicker accent border and a tint, so the
/// choice never depends on colour alone. Filled buttons stay for actions.
pub fn chosen(
    selected: bool,
    radius: f32,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |theme, status| {
        let mut style = outlined(theme, status);
        style.border.radius = radius.into();
        if selected {
            let accent = theme.extended_palette().primary.base.color;
            style.border.color = accent;
            style.border.width = 2.0;
            style.background = Some(accent.scale_alpha(0.2).into());
        }
        style
    }
}

/// Corner radius that turns a [`chosen`] button into a round chip.
pub const CHIP: f32 = 999.0;

/// A compact notification: tone icon, short plain outcome, an optional
/// detail line and a visible dismiss button. Every status and action result
/// uses this one shape, so the page body never repeats it as prose.
///
/// Without `dismiss` there is no button. Callers hide every notice after
/// `NativeApp::NOTICE_SECONDS` either way.
pub fn notice_card<'a, M: Clone + 'a>(notice: Notice, dismiss: Option<M>) -> Element<'a, M> {
    let tone = notice.tone;
    let icon = match tone {
        Tone::Info => "ℹ",
        Tone::Success => "✓",
        Tone::Warning => "!",
        Tone::Error => "✕",
    };
    let mut lines = column![text(notice.title).size(14)]
        .spacing(2)
        .width(Length::Fill);
    if !notice.detail.is_empty() {
        lines = lines.push(text(notice.detail).size(13).style(|theme: &iced::Theme| {
            text::Style {
                color: Some(
                    theme
                        .extended_palette()
                        .background
                        .base
                        .text
                        .scale_alpha(0.75),
                ),
            }
        }));
    }
    let mut content = row![
        text(icon)
            .size(16)
            .style(move |theme: &iced::Theme| text::Style {
                color: Some(tone_color(theme, tone)),
            }),
        lines,
    ]
    .spacing(10)
    .align_y(iced::alignment::Vertical::Top);
    if let Some(dismiss) = dismiss {
        content = content.push(tooltip(
            button(text("×").size(14))
                .style(|theme, status| {
                    let mut style = outlined(theme, status);
                    style.border.radius = 12.0.into();
                    style
                })
                .padding([0, 7])
                .on_press(dismiss),
            "Luk",
            tooltip::Position::Bottom,
        ));
    }
    container(content)
        .padding([10, 12])
        .max_width(560)
        .style(move |theme: &iced::Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(palette.background.weak.color.into()),
                border: iced::Border {
                    color: palette.background.strong.color,
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..container::Style::default()
            }
        })
        .into()
}

/// The theme's tone colour, moved away from the card background: the dark
/// theme's success green is otherwise too dark to read as an icon.
fn tone_color(theme: &iced::Theme, tone: Tone) -> Color {
    use iced::theme::palette::{darken, lighten};
    let palette = theme.extended_palette();
    let base = match tone {
        Tone::Info => palette.primary.base.color,
        Tone::Success => palette.success.base.color,
        Tone::Warning => palette.warning.base.color,
        Tone::Error => palette.danger.base.color,
    };
    if palette.is_dark {
        lighten(base, 0.25)
    } else {
        darken(base, 0.1)
    }
}

/// Vagtmøde participants share clock times, so overlapping blocks need
/// separate lanes. Later shifts reuse the first available lane. Each block
/// keeps its index in `blocks`, so the grid can say which one was clicked.
fn grid_lanes(blocks: &[Block]) -> Vec<Vec<(usize, &Block)>> {
    let mut sorted: Vec<_> = blocks.iter().enumerate().collect();
    sorted.sort_by_key(|(_, block)| block.minutes_from);
    let mut lanes: Vec<Vec<(usize, &Block)>> = vec![Vec::new()];
    for (index, block) in sorted {
        if let Some(lane) = lanes.iter_mut().find(|lane| {
            lane.last()
                .is_none_or(|(_, previous)| previous.minutes_to <= block.minutes_from)
        }) {
            lane.push((index, block));
        } else {
            lanes.push(vec![(index, block)]);
        }
    }
    lanes
}

/// One shift in a [`Week`]: its day and its index in that day's blocks. It
/// is only a view selection and never part of what is approved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShiftRef {
    pub day: usize,
    pub block: usize,
}

/// The shift at `at`, if the week still has it.
pub fn shift(week: &Week, at: ShiftRef) -> Option<&Block> {
    week.days.get(at.day)?.blocks.get(at.block)
}

/// The shift `by` steps from `from` in reading order: day by day, and by
/// start time within a day. Without a selection, a step forward picks the
/// first shift and a step back the last. It stops at either end.
pub fn step_shift(week: &Week, from: Option<ShiftRef>, by: isize) -> Option<ShiftRef> {
    let mut order: Vec<(ShiftRef, u32)> = Vec::new();
    for (day, shifts) in week.days.iter().enumerate() {
        for (block, shift) in shifts.blocks.iter().enumerate() {
            order.push((ShiftRef { day, block }, shift.minutes_from));
        }
    }
    order.sort_by_key(|(at, minutes)| (at.day, *minutes, at.block));
    let last = order.len().checked_sub(1)?;
    let target = match from.and_then(|from| order.iter().position(|(at, _)| *at == from)) {
        Some(index) => index.saturating_add_signed(by).min(last),
        None if by < 0 => last,
        None => 0,
    };
    Some(order[target].0)
}

/// One labelled settings block: a heading row above a bordered box holding
/// that section's controls. The heading is an element so a group can carry
/// an action beside its title.
pub fn group<'a, M: 'a>(
    heading: impl Into<Element<'a, M>>,
    content: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    column![
        heading.into(),
        container(content.into())
            .padding(12)
            .width(Length::Fill)
            .style(group_box),
    ]
    .spacing(6)
    .width(Length::Fill)
    .into()
}

/// A bordered card around one item in a list.
pub fn card<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    container(content.into())
        .padding([10, 12])
        .width(Length::Fill)
        .style(group_box)
        .into()
}

/// Width of one day in [`date_picker`].
const DAY_WIDTH: f32 = 40.0;

/// A date button that opens a Danish month calendar under it, Monday first.
/// `shown` is the month in view while the calendar is open, `None` while it
/// is closed; the caller closes it when a day is picked.
pub fn date_picker<'a, M: Clone + 'a>(
    selected: NaiveDate,
    shown: Option<NaiveDate>,
    toggle: M,
    on_month: impl Fn(NaiveDate) -> M,
    on_pick: impl Fn(NaiveDate) -> M,
) -> Element<'a, M> {
    let field = button(text(format!("{}  ▾", selected.format("%d.%m.%Y"))))
        .style(outlined)
        .padding([8, 12])
        .on_press(toggle);
    let Some(shown) = shown else {
        return field.into();
    };
    let first = shown.with_day(1).expect("the first of a month exists");
    let next = first + chrono::Months::new(1);
    let centered = |label: String| {
        text(label)
            .width(Length::Fixed(DAY_WIDTH))
            .align_x(iced::alignment::Horizontal::Center)
    };
    let mut calendar = column![row![
        button(text("‹"))
            .style(outlined)
            .padding([4, 12])
            .on_press(on_month(first - chrono::Months::new(1))),
        text(format!(
            "{} {}",
            DANISH_MONTHS[first.month0() as usize],
            first.year()
        ))
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Center),
        button(text("›"))
            .style(outlined)
            .padding([4, 12])
            .on_press(on_month(next)),
    ]
    .align_y(iced::alignment::Vertical::Center)
    .width(Length::Fixed(7.0 * DAY_WIDTH))]
    .spacing(4);
    let mut weekdays = row![];
    for label in ["ma", "ti", "on", "to", "fr", "lø", "sø"] {
        weekdays = weekdays.push(centered(label.into()).size(13));
    }
    calendar = calendar.push(weekdays);
    let mut day = first - chrono::Duration::days(first.weekday().num_days_from_monday().into());
    while day < next {
        let mut week = row![];
        for _ in 0..7 {
            let cell: Element<'a, M> = if day.month() == first.month() {
                let chosen = day == selected;
                button(centered(day.day().to_string()))
                    .width(Length::Fixed(DAY_WIDTH))
                    .padding([8, 0])
                    .style(move |theme, status| {
                        if chosen {
                            button::primary(theme, status)
                        } else {
                            button::text(theme, status)
                        }
                    })
                    .on_press(on_pick(day))
                    .into()
            } else {
                space().width(Length::Fixed(DAY_WIDTH)).into()
            };
            week = week.push(cell);
            day = day.succ_opt().expect("a later day exists");
        }
        calendar = calendar.push(week);
    }
    column![field, container(calendar).padding(8).style(group_box)]
        .spacing(6)
        .into()
}

/// A clock time as a button with a ▾, like [`date_picker`]'s field. Pressing
/// it opens a [`crate::clock::Dial`].
pub fn time_field<'a, M: Clone + 'a>((hour, minute): (u32, u32), open: M) -> Element<'a, M> {
    button(text(format!("{hour:02}:{minute:02}  ▾")))
        .style(outlined)
        .padding([8, 12])
        .on_press(open)
        .into()
}

fn group_box(theme: &iced::Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        border: iced::Border {
            color: palette.background.strong.color,
            width: 1.0,
            radius: 4.0.into(),
        },
        ..container::Style::default()
    }
}

/// Height of one marker chip above a day column, including its gap.
const MARKER_HEIGHT: f32 = 22.0;

/// Seven day columns with each shift drawn over the hours it covers. Markers
/// sit above the clock, never on it: they are not shifts. Every column gets
/// the same marker area, so the hours stay aligned across the week.
///
/// Every block is a button: pressing it sends `on_select` with that shift,
/// and the `selected` one gets a ring in the text colour around its status
/// outline.
pub fn week_grid<'a, M: Clone + 'a>(
    week: &'a Week,
    selected: Option<ShiftRef>,
    on_select: impl Fn(ShiftRef) -> M,
) -> Element<'a, M> {
    let marker_rows = week.days.iter().map(|d| d.markers.len()).max().unwrap_or(0);
    let marker_area = MARKER_HEIGHT * marker_rows as f32;
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
        space::vertical().height(Length::Fixed(marker_area)),
        container(hours).height(Length::Fixed(GRID_HEIGHT)),
    ]
    .spacing(4)]
    .spacing(4);
    for (day_index, day) in week.days.iter().enumerate() {
        let mut lanes = row![].spacing(2).width(Length::Fill);
        for blocks in grid_lanes(&day.blocks) {
            let mut lane = column![].width(Length::Fill);
            let mut cursor = 0;
            for (index, block) in blocks {
                let from = block.minutes_from.min(1439);
                let to = block.minutes_to.clamp(from + 1, 1440);
                if from > cursor {
                    lane = lane.push(
                        space::vertical()
                            .height(Length::Fixed(GRID_HEIGHT * (from - cursor) as f32 / 1440.0)),
                    );
                }
                let at = ShiftRef {
                    day: day_index,
                    block: index,
                };
                let chosen = selected == Some(at);
                lane = lane.push(
                    button(
                        container(block_body(block))
                            .height(Length::Fill)
                            .width(Length::Fill)
                            .padding(3)
                            .clip(true)
                            .style(block_style(&block.status, &block.helper_color)),
                    )
                    .padding(2)
                    .height(Length::Fixed(GRID_HEIGHT * (to - from) as f32 / 1440.0))
                    .width(Length::Fill)
                    .style(move |theme, status| selection_ring(theme, status, chosen))
                    .on_press(on_select(at)),
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
        let mut markers = column![].spacing(2);
        for marker in &day.markers {
            markers = markers.push(tooltip(
                container(
                    text(format!("⚑ {}: {}", marker.helper, marker.title))
                        .size(10)
                        .wrapping(text::Wrapping::None),
                )
                .height(Length::Fixed(MARKER_HEIGHT - 2.0))
                .width(Length::Fill)
                .padding([2, 4])
                .clip(true)
                .style(marker_style(&marker.helper_color)),
                marker_details(marker),
                tooltip::Position::FollowCursor,
            ));
        }
        columns = columns.push(
            column![
                text(&day.label).size(11),
                container(markers).height(Length::Fixed(marker_area)),
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

fn marker_details<'a, M: 'a>(marker: &'a Marker) -> Element<'a, M> {
    container(
        column![
            text(&marker.title).size(13).font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..iced::Font::DEFAULT
            }),
            text(format!(
                "{} · {} · overføres ikke",
                marker.helper, marker.time_label
            ))
            .size(12),
        ]
        .spacing(2),
    )
    .padding(8)
    .max_width(320)
    .style(container::bordered_box)
    .into()
}

/// A marker is a note, not a shift: a thin grey outline around the helper's
/// tint, with the ⚑ sign in its text so it never relies on colour.
fn marker_style(helper_color: &str) -> impl Fn(&iced::Theme) -> container::Style + use<> {
    let fill = parse_hex(helper_color)
        .map(|color| tint(color, 0.85))
        .unwrap_or(Color::from_rgb8(0xF0, 0xF0, 0xF0));
    move |_theme: &iced::Theme| container::Style {
        background: Some(fill.into()),
        text_color: Some(Color::from_rgb8(0x1A, 0x1A, 0x1A)),
        border: iced::Border {
            color: Color::from_rgb8(0x75, 0x75, 0x75),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..container::Style::default()
    }
}

fn block_body<'a, M: 'a>(block: &'a Block) -> Element<'a, M> {
    // The cell carries who and what-state. Time is the block's position on
    // the clock; continuation and SPS are marks. Their exact values are in
    // the block's tooltip.
    // The name moves under the label when a narrow lane has no room beside it.
    let mut body = column![
        row![status_badge(&block.status), text(&block.helper).size(11)]
            .spacing(3)
            .wrap()
            .vertical_spacing(1)
    ]
    .spacing(0);
    let mut marks: Vec<&str> = Vec::new();
    if block.continues_before {
        marks.push("▲");
    }
    if block.continues_after {
        marks.push("▼");
    }
    if !block.sps_label.is_empty() {
        marks.push("◆ SPS");
    }
    if !block.absence.is_empty() {
        marks.push("✚ fravær");
    }
    if block.standard_time {
        marks.push("uden tid");
    }
    if !block.part_label.is_empty() {
        marks.push(&block.part_label);
    }
    if !marks.is_empty() {
        body = body.push(text(marks.join(" ")).size(9));
    }
    body.into()
}

/// Around a block: a ring in the text colour when it is the selected shift,
/// a faint one on hover, nothing otherwise. The ring sits outside the status
/// outline, so selecting never hides a shift's status.
fn selection_ring(theme: &iced::Theme, status: button::Status, selected: bool) -> button::Style {
    let ink = theme.extended_palette().background.base.text;
    let color = if selected {
        ink
    } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        ink.scale_alpha(0.4)
    } else {
        Color::TRANSPARENT
    };
    button::Style {
        border: iced::Border {
            color,
            width: 2.0,
            radius: 6.0.into(),
        },
        ..button::Style::default()
    }
}

/// Everything the small cell leaves out: the exact time, the status in words
/// and each change the transfer makes to this shift. The side panel shows it
/// for the selected shift.
pub fn shift_details<'a, M: 'a>(block: &'a Block) -> Element<'a, M> {
    let mut facts = vec![block.time_label.as_str(), block.status_label.as_str()];
    if !block.absence.is_empty() {
        facts.push(&block.absence);
    }
    if !block.part_label.is_empty() {
        facts.push(&block.part_label);
    }
    let mut lines = column![
        text(&block.helper).size(16).font(iced::Font {
            weight: iced::font::Weight::Bold,
            ..iced::Font::DEFAULT
        }),
        text(facts.join(" · ")).size(13),
    ]
    .spacing(4);
    if block.standard_time {
        lines = lines.push(text("Tid fra Vagter uden tid.").size(13));
    }
    if !block.sps_label.is_empty() {
        lines = lines.push(text(format!("SPS {}", block.sps_label)).size(13));
    }
    for detail in &block.details {
        lines = lines.push(text(detail).size(13));
    }
    lines.into()
}

/// Text cue beside every colour, so status never depends on colour alone.
fn status_marker(status: &str) -> &'static str {
    match status {
        "create" => "NY",
        "update" => "ÆNDRET",
        "matched" => "OK",
        "attention" => "!",
        _ => "?",
    }
}

/// The status as a small filled label in the outline's colour. White on
/// every status colour passes contrast, and the word itself carries the state.
fn status_badge<'a, M: 'a>(status: &str) -> Element<'a, M> {
    let color = status_color(status);
    container(
        text(status_marker(status))
            .size(9)
            .font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..iced::Font::DEFAULT
            })
            .wrapping(text::Wrapping::None),
    )
    .padding([0, 3])
    .style(move |_theme: &iced::Theme| container::Style {
        background: Some(color.into()),
        text_color: Some(Color::WHITE),
        border: iced::Border {
            radius: 3.0.into(),
            ..iced::Border::default()
        },
        ..container::Style::default()
    })
    .into()
}

/// Fill the block with the helper's own Teamup colour, tinted so the text
/// stays readable, and outline it in the status colour. Colour therefore
/// carries who is on the shift, exactly as in Teamup, while status keeps the
/// outline plus its text marker.
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
            "summary": ["1 ny vagt i MitHF"],
            "apply_summary": "Overfører 1 ny vagt til MitHF.",
            "can_apply": true, "destination_read": true
        }))
        .unwrap()
    }

    #[test]
    fn week_widgets_render_without_panicking() {
        let week = test_week();
        let _ = week_grid(&week, None, |_| ());
        let _ = week_grid(&week, Some(ShiftRef { day: 0, block: 1 }), |_| ());
        let _ = shift_details::<()>(&week.days[0].blocks[0]);
        let empty = Week::default();
        let _ = week_grid(&empty, None, |_| ());
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
        assert_eq!(lanes[0][0].1.minutes_from, lanes[1][0].1.minutes_from);
        assert_eq!(lanes[0][0].1.minutes_to, lanes[1][0].1.minutes_to);
        assert_eq!(lanes[0][1].1.minutes_from, 720);
        // Each block keeps its own index, so a click selects the right one.
        assert_eq!((lanes[0][1].0, lanes[1][0].0), (2, 1));
        for lane in lanes {
            assert!(lane
                .windows(2)
                .all(|pair| pair[0].1.minutes_to <= pair[1].1.minutes_from));
        }
    }

    /// Arrow keys walk the week in reading order, even when a day's blocks
    /// are stored out of time order, and stop at either end.
    #[test]
    fn stepping_walks_shifts_day_by_day_and_by_start_time() {
        let mut week = test_week();
        week.days[0].blocks.reverse();
        let at = |day, block| Some(ShiftRef { day, block });
        assert_eq!(step_shift(&week, None, 1), at(0, 1));
        assert_eq!(step_shift(&week, at(0, 1), 1), at(0, 0));
        assert_eq!(step_shift(&week, at(0, 0), 1), at(1, 0));
        assert_eq!(step_shift(&week, at(1, 0), 1), at(1, 0));
        assert_eq!(step_shift(&week, at(0, 1), -1), at(0, 1));
        assert_eq!(step_shift(&week, None, -1), at(1, 0));
        assert_eq!(step_shift(&Week::default(), None, 1), None);
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
