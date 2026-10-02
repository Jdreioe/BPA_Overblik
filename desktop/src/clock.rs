//! A time picker like Android's: the time in large digits over a round dial.
//! The dial shows the hours, 0 to 11 on the outer ring and 12 to 23 inside
//! it, and moves on to the minutes once an hour is picked. Pressing the
//! digits goes back to either. Digits can be typed too; Enter is OK and
//! Escape is Annuller.

use iced::widget::canvas::{self, Frame, Path, Stroke, Text};
use iced::widget::{button, column, container, row, space, text};
use iced::{keyboard, mouse, Element, Length, Point, Rectangle, Renderer, Theme, Vector};

/// The dial's radius. Material's dial is 256 across.
const RADIUS: f32 = 128.0;
/// Where the hours 0 to 11, and the minutes, sit.
const OUTER: f32 = 100.0;
/// Where the hours 12 to 23 sit.
const INNER: f32 = 60.0;

/// An open picker: the time so far and whether the dial shows the minutes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Dial {
    pub clock: (u32, u32),
    pub minutes: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialMessage {
    /// An hour, and whether the dial moves on to the minutes.
    Hour {
        hour: u32,
        next: bool,
    },
    Minute(u32),
    ShowHours,
    ShowMinutes,
    Cancel,
    Confirm,
}

/// How a closed picker ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Cancel,
    Pick((u32, u32)),
}

impl Dial {
    pub fn new(clock: (u32, u32)) -> Self {
        Dial {
            clock,
            minutes: false,
        }
    }

    /// Apply a message, and say how the picker ended if it closes.
    pub fn update(&mut self, message: DialMessage) -> Option<Outcome> {
        match message {
            DialMessage::Hour { hour, next } => {
                self.clock.0 = hour;
                self.minutes |= next;
            }
            DialMessage::Minute(minute) => self.clock.1 = minute,
            DialMessage::ShowHours => self.minutes = false,
            DialMessage::ShowMinutes => self.minutes = true,
            DialMessage::Cancel => return Some(Outcome::Cancel),
            DialMessage::Confirm => return Some(Outcome::Pick(self.clock)),
        }
        None
    }

    /// The dialog: »Vælg tid«, the digits, the dial, Annuller and OK.
    pub fn view(&self) -> Element<'_, DialMessage> {
        let digits = |value: u32, chosen: bool, message: DialMessage| {
            button(
                text(format!("{value:02}"))
                    .size(48)
                    .width(Length::Fill)
                    .align_x(iced::alignment::Horizontal::Center),
            )
            .width(Length::Fixed(96.0))
            .padding([8, 0])
            .style(move |theme: &Theme, _| {
                let palette = theme.extended_palette();
                let (background, color) = if chosen {
                    (palette.primary.weak.color, palette.primary.weak.text)
                } else {
                    (palette.background.weak.color, palette.background.weak.text)
                };
                button::Style {
                    background: Some(background.into()),
                    text_color: color,
                    border: iced::Border::default().rounded(8),
                    ..button::Style::default()
                }
            })
            .on_press(message)
        };
        let flat = |label: &'static str, message: DialMessage| {
            button(text(label))
                .padding([10, 12])
                .style(|theme: &Theme, status| {
                    let palette = theme.extended_palette();
                    button::Style {
                        background: matches!(
                            status,
                            button::Status::Hovered | button::Status::Pressed
                        )
                        .then(|| palette.primary.weak.color.scale_alpha(0.3).into()),
                        text_color: palette.primary.strong.color,
                        border: iced::Border::default().rounded(20),
                        ..button::Style::default()
                    }
                })
                .on_press(message)
        };
        let content = column![
            text("Vælg tid").size(12),
            container(
                row![
                    digits(self.clock.0, !self.minutes, DialMessage::ShowHours),
                    text(":").size(48),
                    digits(self.clock.1, self.minutes, DialMessage::ShowMinutes),
                ]
                .spacing(6)
                .align_y(iced::alignment::Vertical::Center),
            )
            .center_x(Length::Fill),
            container(
                canvas::Canvas::new(Face { dial: *self })
                    .width(Length::Fixed(2.0 * RADIUS))
                    .height(Length::Fixed(2.0 * RADIUS)),
            )
            .center_x(Length::Fill),
            row![
                space::horizontal(),
                flat("Annuller", DialMessage::Cancel),
                flat("OK", DialMessage::Confirm),
            ]
            .spacing(8),
        ]
        .spacing(20);
        container(content)
            .padding(24)
            .width(Length::Fixed(328.0))
            .style(|theme: &Theme| {
                let palette = theme.extended_palette();
                container::Style {
                    background: Some(palette.background.base.color.into()),
                    text_color: Some(palette.background.base.text),
                    border: iced::Border::default().rounded(28),
                    shadow: iced::Shadow {
                        color: iced::Color::BLACK.scale_alpha(0.3),
                        offset: Vector::new(0.0, 4.0),
                        blur_radius: 16.0,
                    },
                    ..container::Style::default()
                }
            })
            .into()
    }
}

/// Where on the dial a point is: the hour or minute under it, counted
/// clockwise from the top in `steps`, and whether it is on the inner ring.
fn position(point: Point, steps: u32) -> (u32, bool) {
    let (x, y) = (point.x - RADIUS, point.y - RADIUS);
    let turn = x.atan2(-y).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
    let step = (turn * steps as f32).round() as u32 % steps;
    (step, x.hypot(y) < (OUTER + INNER) / 2.0)
}

/// The hour or minute a press or drag at `point` picks. A press picks the
/// minutes in fives, so a click lands on a label; a drag picks any minute.
fn pick(dial: Dial, point: Point, dragging: bool) -> DialMessage {
    if dial.minutes {
        let minute = if dragging {
            position(point, 60).0
        } else {
            position(point, 12).0 * 5
        };
        DialMessage::Minute(minute)
    } else {
        let (hour, inner) = position(point, 12);
        DialMessage::Hour {
            hour: hour + if inner { 12 } else { 0 },
            next: false,
        }
    }
}

/// Typed digits so far for the shown hours or minutes.
#[derive(Default)]
struct Typed {
    minutes: bool,
    digits: String,
}

/// What a typed digit picks, given the digits typed before it. Two digits,
/// or one that no second digit could follow, finish the hours and move on.
fn typed(dial: Dial, digits: &str) -> Option<DialMessage> {
    let value: u32 = digits.parse().ok()?;
    let done = digits.len() == 2;
    if dial.minutes {
        (value < 60).then_some(DialMessage::Minute(value))
    } else {
        (value < 24).then_some(DialMessage::Hour {
            hour: value,
            next: done || value > 2,
        })
    }
}

#[derive(Default)]
struct FaceState {
    dragging: bool,
    typed: Typed,
}

struct Face {
    dial: Dial,
}

impl canvas::Program<DialMessage> for Face {
    type State = FaceState;

    fn update(
        &self,
        state: &mut FaceState,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<DialMessage>> {
        use iced::Event::{Keyboard, Mouse};
        match event {
            Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor.position_in(bounds)?;
                if point.distance(Point::new(RADIUS, RADIUS)) > RADIUS {
                    return None;
                }
                state.dragging = true;
                Some(canvas::Action::publish(pick(self.dial, point, false)).and_capture())
            }
            Mouse(mouse::Event::CursorMoved { .. }) if state.dragging => {
                let point = cursor.position_from(bounds.position())?;
                Some(canvas::Action::publish(pick(self.dial, point, true)).and_capture())
            }
            Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.dragging => {
                state.dragging = false;
                // Letting go of an hour moves on to the minutes. The hour came with
                // the press or the drag, which may not be drawn yet.
                (!self.dial.minutes)
                    .then(|| canvas::Action::publish(DialMessage::ShowMinutes).and_capture())
            }
            Keyboard(keyboard::Event::KeyPressed { key, text, .. }) => {
                let message = match key {
                    keyboard::Key::Named(keyboard::key::Named::Escape) => DialMessage::Cancel,
                    keyboard::Key::Named(keyboard::key::Named::Enter) => DialMessage::Confirm,
                    _ => {
                        let digit = text
                            .as_deref()
                            .filter(|t| t.len() == 1 && t.chars().all(|c| c.is_ascii_digit()))?;
                        if state.typed.minutes != self.dial.minutes || state.typed.digits.len() == 2
                        {
                            state.typed = Typed {
                                minutes: self.dial.minutes,
                                digits: String::new(),
                            };
                        }
                        state.typed.digits.push_str(digit);
                        let message = typed(self.dial, &state.typed.digits).or_else(|| {
                            // A digit that makes no time starts over.
                            state.typed.digits = digit.to_owned();
                            typed(self.dial, digit)
                        })?;
                        if let DialMessage::Hour { next: true, .. } = message {
                            state.typed = Typed::default();
                        }
                        message
                    }
                };
                Some(canvas::Action::publish(message).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &FaceState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let palette = theme.extended_palette();
        let mut frame = Frame::new(renderer, bounds.size());
        let center = Point::new(RADIUS, RADIUS);
        let at = |step: u32, steps: u32, radius: f32| {
            let angle = step as f32 / steps as f32 * std::f32::consts::TAU;
            Point::new(
                center.x + radius * angle.sin(),
                center.y - radius * angle.cos(),
            )
        };
        frame.fill(&Path::circle(center, RADIUS), palette.background.weak.color);

        // The hand and the round mark at the picked hour or minute.
        let (hand, mark) = if self.dial.minutes {
            (at(self.dial.clock.1, 60, OUTER), 18.0)
        } else if self.dial.clock.0 >= 12 {
            (at(self.dial.clock.0 - 12, 12, INNER), 15.0)
        } else {
            (at(self.dial.clock.0, 12, OUTER), 18.0)
        };
        let primary = palette.primary.strong;
        frame.stroke(
            &Path::line(center, hand),
            Stroke::default().with_color(primary.color).with_width(2.0),
        );
        frame.fill(&Path::circle(center, 4.0), primary.color);
        frame.fill(&Path::circle(hand, mark), primary.color);
        if self.dial.minutes && !self.dial.clock.1.is_multiple_of(5) {
            frame.fill(&Path::circle(hand, 2.0), primary.text);
        }

        let mut label = |content: String, position: Point, size: f32, chosen: bool| {
            frame.fill_text(Text {
                content,
                position,
                color: if chosen {
                    primary.text
                } else {
                    palette.background.weak.text
                },
                size: size.into(),
                align_x: iced::widget::text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                ..Text::default()
            });
        };
        if self.dial.minutes {
            for step in 0..12 {
                let minute = step * 5;
                label(
                    format!("{minute:02}"),
                    at(step, 12, OUTER),
                    15.0,
                    minute == self.dial.clock.1,
                );
            }
        } else {
            for hour in 0..24 {
                let (radius, size) = if hour < 12 {
                    (OUTER, 15.0)
                } else {
                    (INNER, 13.0)
                };
                let text = if hour == 0 {
                    "00".into()
                } else {
                    hour.to_string()
                };
                label(
                    text,
                    at(hour % 12, 12, radius),
                    size,
                    hour == self.dial.clock.0,
                );
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &FaceState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let over = cursor
            .position_in(bounds)
            .is_some_and(|point| point.distance(Point::new(RADIUS, RADIUS)) <= RADIUS);
        if state.dragging {
            mouse::Interaction::Grabbing
        } else if over {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dial_picks_the_hour_under_the_cursor_on_either_ring() {
        let dial = Dial::new((8, 0));
        let top = |radius: f32| Point::new(RADIUS, RADIUS - radius);
        let right = |radius: f32| Point::new(RADIUS + radius, RADIUS);
        let hour = |point| match pick(dial, point, false) {
            DialMessage::Hour { hour, .. } => hour,
            _ => panic!("expected an hour"),
        };
        assert_eq!(hour(top(OUTER)), 0);
        assert_eq!(hour(top(INNER)), 12);
        assert_eq!(hour(right(OUTER)), 3);
        assert_eq!(hour(right(INNER)), 15);
        // Bottom left on the inner ring is 22, as on Android's clock.
        let angle = 10.0 / 12.0 * std::f32::consts::TAU;
        assert_eq!(
            hour(Point::new(
                RADIUS + INNER * angle.sin(),
                RADIUS - INNER * angle.cos()
            )),
            22
        );
    }

    #[test]
    fn a_press_picks_minutes_in_fives_and_a_drag_any_minute() {
        let dial = Dial {
            clock: (8, 0),
            minutes: true,
        };
        // Seven minutes past, a little past the 05.
        let angle = 7.0 / 60.0 * std::f32::consts::TAU;
        let point = Point::new(RADIUS + OUTER * angle.sin(), RADIUS - OUTER * angle.cos());
        assert_eq!(pick(dial, point, false), DialMessage::Minute(5));
        assert_eq!(pick(dial, point, true), DialMessage::Minute(7));
    }

    #[test]
    fn an_hour_moves_on_to_the_minutes_and_ok_picks_the_time() {
        let mut dial = Dial::new((6, 0));
        assert_eq!(
            dial.update(DialMessage::Hour {
                hour: 22,
                next: false
            }),
            None
        );
        assert!(!dial.minutes);
        dial.update(DialMessage::Hour {
            hour: 22,
            next: true,
        });
        assert!(dial.minutes);
        dial.update(DialMessage::Minute(30));
        dial.update(DialMessage::ShowHours);
        assert!(!dial.minutes);
        assert_eq!(
            dial.update(DialMessage::Confirm),
            Some(Outcome::Pick((22, 30)))
        );
        assert_eq!(dial.update(DialMessage::Cancel), Some(Outcome::Cancel));
    }

    #[test]
    fn typed_digits_finish_an_hour_when_no_second_digit_can_follow() {
        let hours = Dial::new((6, 0));
        assert_eq!(
            typed(hours, "2"),
            Some(DialMessage::Hour {
                hour: 2,
                next: false
            })
        );
        assert_eq!(
            typed(hours, "22"),
            Some(DialMessage::Hour {
                hour: 22,
                next: true
            })
        );
        assert_eq!(
            typed(hours, "7"),
            Some(DialMessage::Hour {
                hour: 7,
                next: true
            })
        );
        assert_eq!(typed(hours, "25"), None);
        let minutes = Dial {
            clock: (6, 0),
            minutes: true,
        };
        assert_eq!(typed(minutes, "45"), Some(DialMessage::Minute(45)));
        assert_eq!(typed(minutes, "75"), None);
    }
}
