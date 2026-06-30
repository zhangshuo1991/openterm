//! Small reusable styled widgets so the whole UI shares one visual language.

use iced::widget::{button, container, svg, text, text_input};
use iced::{Border, Color, Element, Length, Shadow};

// ── Eye icons (Feather-style line art, stroke="currentColor") ─────────────

/// Eye-open SVG: lens outline + iris circle.
static EYE_OPEN_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
  <path d="M1 10s3.5-6.5 9-6.5S19 10 19 10s-3.5 6.5-9 6.5S1 10 1 10z"/>
  <circle cx="10" cy="10" r="2.5"/>
</svg>"#;

/// Eye-off SVG: partial arcs + diagonal slash, same weight.
static EYE_OFF_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
  <path d="M14.12 14.12A7.84 7.84 0 0110 15.5C4.5 15.5 1 10 1 10a17.4 17.4 0 014.42-4.88"/>
  <path d="M8.24 4.63A7.3 7.3 0 0110 4.5c5.5 0 9 5.5 9 5.5a17.4 17.4 0 01-2.1 2.97"/>
  <line x1="2" y1="2" x2="18" y2="18"/>
</svg>"#;

use crate::message::Message;
use crate::session::Phase;
use crate::theme;

/// Tone of an action button.
#[derive(Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum Tone {
    Primary,
    Neutral,
    Danger,
    Ghost,
}

pub fn action_button<'a>(label: &'a str, tone: Tone, on_press: Message) -> Element<'a, Message> {
    button(
        text(label)
            .size(13)
            .align_x(iced::alignment::Horizontal::Center)
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .padding([9, 14])
    .on_press(on_press)
    .style(move |_, status| button_style(tone, status))
    .into()
}

#[allow(dead_code)]
pub fn small_button<'a>(label: &'a str, tone: Tone, on_press: Message) -> Element<'a, Message> {
    button(text(label).size(12))
        .padding([6, 11])
        .on_press(on_press)
        .style(move |_, status| button_style(tone, status))
        .into()
}

fn button_style(tone: Tone, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    let (bg, fg, border) = match tone {
        Tone::Primary => {
            let base = theme::accent();
            (
                if hovered { theme::accent_strong() } else { base },
                theme::surface_0(),
                Color::TRANSPARENT,
            )
        }
        Tone::Neutral => (
            if hovered {
                theme::surface_3()
            } else {
                theme::surface_2()
            },
            theme::text_high(),
            theme::border_subtle(),
        ),
        Tone::Danger => (
            if hovered {
                theme::status_error()
            } else {
                theme::with_alpha(theme::status_error(), 0.18)
            },
            if hovered {
                theme::surface_0()
            } else {
                theme::status_error()
            },
            theme::with_alpha(theme::status_error(), 0.35),
        ),
        Tone::Ghost => (
            if hovered {
                theme::with_alpha(theme::text_high(), 0.06)
            } else {
                Color::TRANSPARENT
            },
            theme::text_muted(),
            Color::TRANSPARENT,
        ),
    };
    button::Style {
        background: Some(bg.into()),
        text_color: fg,
        border: Border {
            color: border,
            width: 1.0,
            radius: 7.0.into(),
        },
        shadow: Shadow::default(),
        ..Default::default()
    }
}

pub fn field<'a>(
    placeholder: &str,
    value: &str,
    on_input: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    text_input(placeholder, value)
        .on_input(on_input)
        .padding([9, 12])
        .size(14)
        .style(input_style)
        .into()
}

#[allow(dead_code)]
pub fn secure_field<'a>(
    placeholder: &str,
    value: &str,
    on_input: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    text_input(placeholder, value)
        .on_input(on_input)
        .secure(true)
        .padding([9, 12])
        .size(14)
        .style(input_style)
        .into()
}

/// A secure text field with a trailing eye-icon button that toggles plain-text
/// visibility. `revealed` = true means the text is currently visible.
/// Pressing the eye emits `on_toggle`.
pub fn secure_field_toggle<'a>(
    placeholder: &str,
    value: &str,
    revealed: bool,
    on_input: impl Fn(String) -> Message + 'a,
    on_toggle: Message,
) -> Element<'a, Message> {
    use iced::widget::row;

    let input = text_input(placeholder, value)
        .on_input(on_input)
        .secure(!revealed)
        .padding([9, 12])
        .size(14)
        .style(input_style)
        .width(Length::Fill);

    // Eye-open = password is currently visible (click to hide).
    // Eye-off  = password is currently hidden (click to show).
    let icon_bytes: &'static [u8] = if revealed { EYE_OPEN_SVG } else { EYE_OFF_SVG };
    let icon = svg(svg::Handle::from_memory(icon_bytes))
        .width(Length::Fixed(16.0))
        .height(Length::Fixed(16.0))
        .style(|_, status| {
            // Brighten the icon on hover so the interaction is obvious even
            // though iced doesn't let us change nested widget color via button
            // hover state directly — we rely on the SVG's own hover status.
            let active = matches!(status, svg::Status::Hovered);
            svg::Style {
                color: Some(if active {
                    theme::text_high()
                } else {
                    theme::text_muted()
                }),
            }
        });

    // Wrap in a square button that highlights on hover.
    let eye = button(
        container(icon)
            .width(Length::Fixed(34.0))
            .height(Length::Fixed(34.0))
            .center_x(Length::Fixed(34.0))
            .center_y(Length::Fixed(34.0)),
    )
    .padding(0)
    .on_press(on_toggle)
    .style(|_, status| {
        let hovered = matches!(status, button::Status::Hovered);
        button::Style {
            background: Some(
                if hovered { theme::surface_3() } else { theme::surface_2() }.into(),
            ),
            border: Border {
                color: theme::border_subtle(),
                width: 1.0,
                radius: 7.0.into(),
            },
            ..Default::default()
        }
    });

    row![input, eye]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .into()
}

fn input_style(_theme: &iced::Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: theme::surface_2().into(),
        border: Border {
            color: if focused {
                theme::accent()
            } else {
                theme::border_subtle()
            },
            width: 1.0,
            radius: 7.0.into(),
        },
        icon: theme::text_muted(),
        placeholder: theme::text_dim(),
        value: theme::text_high(),
        selection: theme::accent_soft(),
    }
}

/// A colored status dot reflecting a session phase. `pulse` dims the dot on
/// alternate ticks while the phase is Connecting, giving a breathing effect.
pub fn status_dot<'a>(phase: &Phase, pulse: bool) -> Element<'a, Message> {
    let base = phase_color(phase);
    let color = if matches!(phase, Phase::Connecting) && pulse {
        theme::with_alpha(base, 0.3)
    } else {
        base
    };
    container(text("●").size(11).color(color)).into()
}

pub fn phase_color(phase: &Phase) -> Color {
    match phase {
        Phase::Connected => theme::status_ok(),
        Phase::Connecting => theme::status_warn(),
        Phase::Failed(_) => theme::status_error(),
        Phase::Idle => theme::status_idle(),
    }
}

/// A card-like container.
pub fn card<'a>(content: impl Into<Element<'a, Message>>) -> container::Container<'a, Message> {
    container(content).padding(18).style(|_| container::Style {
        background: Some(theme::surface_1().into()),
        border: Border {
            color: theme::border_subtle(),
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    })
}
