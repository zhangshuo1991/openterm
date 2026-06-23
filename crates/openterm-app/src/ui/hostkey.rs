//! Host-key confirmation overlay. Surfaces the server fingerprint and lets the
//! user trust it (writing to known_hosts) or reject the connection. This is the
//! proper state model — not a passive banner.

use iced::widget::{column, container, row, text, Space};
use iced::{Color, Element, Length};

use crate::message::Message;
use crate::theme;
use crate::ui::widgets::{self, Tone};
use crate::App;

pub fn view(app: &App) -> Element<'_, Message> {
    let Some(challenge) = app.active_session().and_then(|s| s.host_key.as_ref()) else {
        return container(Space::new()).into();
    };

    let body = column![
        text("Verify host key").size(18).color(theme::text_high()),
        text(format!("{}:{}", challenge.host, challenge.port))
            .size(13)
            .color(theme::text_muted()),
        Space::new().height(Length::Fixed(6.0)),
        field_row("Algorithm", &challenge.algorithm),
        field_row("Fingerprint", &challenge.fingerprint),
        Space::new().height(Length::Fixed(6.0)),
        text("Only accept if you recognize this server. The key will be saved to known_hosts.")
            .size(12)
            .color(theme::text_dim()),
        Space::new().height(Length::Fixed(8.0)),
        row![
            container(widgets::action_button(
                "Trust & connect",
                Tone::Primary,
                Message::AcceptHostKey
            ))
            .width(Length::FillPortion(1)),
            container(widgets::action_button(
                "Reject",
                Tone::Danger,
                Message::RejectHostKey
            ))
            .width(Length::FillPortion(1)),
        ]
        .spacing(12),
    ]
    .spacing(8);

    let card = widgets::card(body).max_width(460);

    container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.55).into()),
            ..Default::default()
        })
        .into()
}

fn field_row<'a>(label: &'a str, value: &'a str) -> Element<'a, Message> {
    row![
        text(label)
            .size(12)
            .color(theme::text_muted())
            .width(Length::Fixed(96.0)),
        text(value.to_string()).size(12).color(theme::text_high()),
    ]
    .spacing(8)
    .into()
}
