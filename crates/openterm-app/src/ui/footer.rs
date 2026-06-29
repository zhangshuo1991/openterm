//! Footer status bar: connection status, target, and quick actions.

use iced::widget::{button, container, row, text, Space};
use iced::{Border, Color, Element, Length};

use crate::message::Message;
use crate::session::Phase;
use crate::theme;
use crate::ui::widgets;
use crate::App;

pub fn view(app: &App) -> Element<'_, Message> {
    let (status, phase_label, target) = match app.active_session() {
        Some(session) => (
            session.status.clone(),
            session.phase.clone(),
            session.config.target_label(),
        ),
        None => (app.status.clone(), Phase::Idle, String::new()),
    };

    let connected = phase_label == Phase::Connected;
    let has_host = app
        .active_session()
        .is_some_and(|s| !s.config.host.trim().is_empty());
    // Show Reconnect when disconnected (idle or failed) but a host is already set.
    let disconnected = !phase_label.is_active() && has_host;

    let pill_color = widgets::phase_color(&phase_label);
    let pill = container(
        row![
            widgets::status_dot(&phase_label, app.connecting_pulse),
            text(phase_word(&phase_label)).size(11).color(pill_color),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    )
    .padding([3, 9])
    .style(move |_| container::Style {
        background: Some(theme::surface_2().into()),
        border: Border {
            color: theme::with_alpha(pill_color, 0.30),
            width: 1.0,
            radius: 7.0.into(),
        },
        ..Default::default()
    });

    let mut left = row![pill].spacing(10).align_y(iced::Alignment::Center);
    if !target.is_empty() {
        left = left.push(text(target).size(12).color(theme::text_high()));
    }
    if !status.is_empty() {
        left = left.push(text(status).size(12).color(theme::text_muted()));
    }
    if !connected {
        left = left.push(Space::new());
    }

    let mut actions = row![].spacing(6).align_y(iced::Alignment::Center);
    if disconnected {
        // Accent-colored button so it's immediately obvious when disconnected.
        actions = actions.push(accent_button("Reconnect", Message::Connect));
    }
    if connected {
        actions = actions
            .push(footer_button("History", Message::ToggleHistory))
            .push(footer_button("Files", Message::ToggleSftp))
            .push(footer_button("Monitor", Message::ToggleMonitor))
            .push(footer_button("Disconnect", Message::Disconnect));
    }
    if has_host {
        actions = actions.push(footer_button("Duplicate", Message::DuplicateTab));
    }
    actions = actions
        .push(footer_button("Settings", Message::OpenSettings))
        .push(footer_button(&format!("{}px", app.font_size), Message::OpenSettings));

    container(
        row![left, Space::new().width(Length::Fill), actions]
            .align_y(iced::Alignment::Center)
            .padding([0, 14]),
    )
    .width(Length::Fill)
    .height(Length::Fixed(crate::ui::FOOTER_HEIGHT))
    .style(|_| container::Style {
        background: Some(theme::surface_1().into()),
        border: Border {
            color: theme::border_subtle(),
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn phase_word(phase: &Phase) -> &'static str {
    match phase {
        Phase::Connected => "Connected",
        Phase::Connecting => "Connecting",
        Phase::Failed(_) => "Disconnected",
        Phase::Idle => "Idle",
    }
}

fn footer_button<'a>(label: &str, on_press: Message) -> Element<'a, Message> {
    button(text(label.to_string()).size(11).color(theme::text_muted()))
        .padding([3, 9])
        .on_press(on_press)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(if hovered { theme::surface_3() } else { Color::TRANSPARENT }.into()),
                text_color: theme::text_high(),
                border: Border { radius: 6.0.into(), ..Default::default() },
                ..Default::default()
            }
        })
        .into()
}

/// An accent-colored button for high-priority actions (e.g. Reconnect).
fn accent_button<'a>(label: &str, on_press: Message) -> Element<'a, Message> {
    button(text(label.to_string()).size(11).color(theme::surface_0()))
        .padding([3, 9])
        .on_press(on_press)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(if hovered { theme::accent_strong() } else { theme::accent() }.into()),
                text_color: theme::surface_0(),
                border: Border { radius: 6.0.into(), ..Default::default() },
                ..Default::default()
            }
        })
        .into()
}
