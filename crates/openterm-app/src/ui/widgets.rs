//! Small reusable styled widgets so the whole UI shares one visual language.

use iced::widget::{button, container, text, text_input};
use iced::{Border, Color, Element, Length, Shadow};

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

/// A colored status dot reflecting a session phase.
pub fn status_dot<'a>(phase: &Phase) -> Element<'a, Message> {
    let color = phase_color(phase);
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
