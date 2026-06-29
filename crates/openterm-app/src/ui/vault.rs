//! Vault unlock / first-time master-password overlay.
//!
//! Saved SSH passwords are encrypted with a key derived from this master
//! password (Argon2id). On first run the user creates one; afterwards they
//! enter it to unlock. The vault auto-locks after inactivity, on system
//! sleep, or manually — see `update::VaultCheckLock`.

use iced::advanced::widget;
use iced::widget::{button, column, container, row, text, text_input, Space};
use iced::{Alignment, Border, Color, Element, Length};
use once_cell::sync::Lazy;

use crate::message::Message;
use crate::theme;
use crate::App;

/// Stable id for the master-password field, so we can focus it when the
/// overlay opens.
pub static PW_INPUT_ID: Lazy<widget::Id> =
    Lazy::new(|| widget::Id::new("vault-password"));

pub fn view(app: &App) -> Element<'_, Message> {
    let first_time = !app.vault_has_canary;

    let title = if first_time {
        "Create master password"
    } else {
        "Unlock vault"
    };
    let blurb = if first_time {
        "This password encrypts your saved SSH credentials. There is no recovery if you forget it."
    } else {
        "Enter your master password to decrypt saved SSH credentials."
    };

    let mut fields = column![
        text(title).size(20).color(theme::text_high()),
        text(blurb).size(13).color(theme::text_dim()),
    ]
    .spacing(10)
    .width(Length::Fill);

    let pw_input = text_input("Master password", &app.vault_pw)
        .id(PW_INPUT_ID.clone())
        .on_input(Message::VaultPasswordInput)
        .on_submit(Message::VaultSubmit)
        .secure(true)
        .padding([11, 14])
        .size(15)
        .style(field_style);
    fields = fields.push(pw_input);

    if first_time {
        let confirm = text_input("Confirm password", &app.vault_confirm)
            .on_input(Message::VaultConfirmInput)
            .on_submit(Message::VaultSubmit)
            .secure(true)
            .padding([11, 14])
            .size(15)
            .style(field_style);
        fields = fields.push(confirm);
    }

    if let Some(err) = &app.vault_err {
        fields = fields.push(text(err.clone()).size(13).color(Color::from_rgb(0.9, 0.35, 0.35)));
    }

    let submit_label = if first_time { "Create & unlock" } else { "Unlock" };
    let submit = button(text(submit_label).size(15).color(theme::surface_0()))
        .on_press(Message::VaultSubmit)
        .padding([10, 20])
        .style(|_, status| {
            let bg = match status {
                button::Status::Hovered | button::Status::Pressed => theme::accent_strong(),
                _ => theme::accent(),
            };
            button::Style {
                background: Some(bg.into()),
                text_color: theme::surface_0(),
                border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 9.0.into() },
                ..Default::default()
            }
        });

    fields = fields.push(
        row![Space::new().width(Length::Fill), submit].align_y(Alignment::Center),
    );

    let card = container(fields.spacing(14))
        .width(Length::Fixed(420.0))
        .padding(28)
        .style(|_| container::Style {
            background: Some(theme::surface_1().into()),
            border: Border {
                color: theme::border_strong(),
                width: 1.0,
                radius: 14.0.into(),
            },
            ..Default::default()
        });

    // Opaque backdrop: the vault gates credential access, so block the UI
    // behind it. It is not click-dismissable.
    let backdrop = container(Space::new())
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.75).into()),
            ..Default::default()
        });

    let centered = container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);

    iced::widget::stack![backdrop, centered].into()
}

fn field_style(_: &iced::Theme, _: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: theme::surface_2().into(),
        border: Border {
            color: theme::accent(),
            width: 1.0,
            radius: 9.0.into(),
        },
        icon: theme::text_muted(),
        placeholder: theme::text_dim(),
        value: theme::text_high(),
        selection: theme::accent_soft(),
    }
}
