//! Connection card: shown in the workspace when the active session is idle.

use iced::widget::{button, column, container, row, text, Space};
use iced::{Border, Color, Element, Length};

use crate::message::Message;
use crate::session::{AuthMode, Phase, SessionConfig};
use crate::theme;
use crate::ui::widgets::{self, Tone};
use crate::App;

pub fn view(app: &App) -> Element<'_, Message> {
    let reveal = app.reveal_password;
    let Some(session) = app.active_session() else {
        return container(Space::new()).into();
    };
    let config = &session.config;

    let heading = column![
        text("Connect to a server").size(20).color(theme::text_high()),
        text("Enter connection details, then Connect.")
            .size(13)
            .color(theme::text_muted()),
    ]
    .spacing(5);

    let name = labeled(
        "Label (optional)",
        widgets::field("My server", &config.name, Message::NameChanged),
    );

    // Group + Tags row
    let group_tags = row![
        container(labeled(
            "Group",
            widgets::field(
                "Production / Staging...",
                &config.group,
                Message::GroupChanged,
            ),
        ))
        .width(Length::FillPortion(1)),
        container(labeled(
            "Tags",
            widgets::field("nginx, mysql...", &config.tags_str, Message::TagsChanged),
        ))
        .width(Length::FillPortion(1)),
    ]
    .spacing(12);

    let host = labeled(
        "Host",
        widgets::field(
            "example.com or 10.0.0.5",
            &config.host,
            Message::HostChanged,
        ),
    );
    let user = labeled(
        "Username",
        widgets::field("ubuntu", &config.user, Message::UserChanged),
    );
    let port = labeled(
        "Port",
        widgets::field("22", &config.port, Message::PortChanged),
    );

    let host_user = row![
        container(host).width(Length::FillPortion(2)),
        container(user).width(Length::FillPortion(2)),
        container(port).width(Length::FillPortion(1)),
    ]
    .spacing(12);

    let method_cards = row![
        auth_card(
            AuthMode::Password,
            config.auth,
            "Password",
            "Log in with account password.",
        ),
        auth_card(
            AuthMode::Key,
            config.auth,
            "SSH key",
            "Use a private key file.",
        ),
        auth_card(
            AuthMode::Agent,
            config.auth,
            "SSH agent",
            "Use keys in system agent.",
        ),
    ]
    .spacing(10);

    let auth_extra: Element<'_, Message> = match config.auth {
        AuthMode::Password => labeled(
            "Password",
            widgets::secure_field_toggle(
                "Your server password",
                &config.password,
                reveal,
                Message::PasswordChanged,
                Message::ToggleRevealPassword,
            ),
        ),
        AuthMode::Key => column![
            labeled(
                "Private key file",
                row![
                    widgets::field("~/.ssh/id_ed25519", &config.key_path, Message::KeyPathChanged),
                    browse_btn(),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center)
                .into(),
            ),
            labeled(
                "Key passphrase (leave empty if none)",
                widgets::secure_field_toggle(
                    "Passphrase",
                    &config.passphrase,
                    reveal,
                    Message::PassphraseChanged,
                    Message::ToggleRevealPassword,
                ),
            ),
        ]
        .spacing(12)
        .into(),
        AuthMode::Agent => container(
            text("Nothing to fill in — OpenTerm will try the keys in your SSH agent and ~/.ssh.")
                .size(12)
                .color(theme::text_dim()),
        )
        .padding([4, 0])
        .into(),
    };

    let auth_section = column![
        text("How do you want to sign in?")
            .size(12)
            .color(theme::text_muted()),
        method_cards,
    ]
    .spacing(8);

    let jump = jump_section(config);

    let connecting = session.phase == Phase::Connecting;
    let connect_label = if connecting { "Connecting…" } else { "Connect" };

    let buttons = row![
        container(widgets::action_button(
            connect_label,
            Tone::Primary,
            Message::Connect,
        ))
        .width(Length::FillPortion(2)),
        container(widgets::action_button(
            "Save host",
            Tone::Neutral,
            Message::SaveHost,
        ))
        .width(Length::FillPortion(1)),
        container(widgets::action_button(
            "Local shell",
            Tone::Neutral,
            Message::NewLocalShell,
        ))
        .width(Length::FillPortion(1)),
    ]
    .spacing(12);

    let mut form = column![
        heading,
        Space::new().height(Length::Fixed(6.0)),
        name,
        group_tags,
        host_user,
        auth_section,
        auth_extra,
        jump,
        Space::new().height(Length::Fixed(4.0)),
        buttons,
    ]
    .spacing(14);

    if let Phase::Failed(error) = &session.phase {
        form = form.push(
            container(text(error.clone()).size(13).color(theme::status_error()))
                .padding([8, 12])
                .style(|_| container::Style {
                    background: Some(theme::with_alpha(theme::status_error(), 0.10).into()),
                    border: iced::Border {
                        color: theme::with_alpha(theme::status_error(), 0.3),
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    ..Default::default()
                }),
        );
    }

    let card = widgets::card(form).max_width(560);

    let progress = app.card_progress();
    let nudge = ((1.0 - progress) * 26.0).max(0.0);

    let centered = column![
        Space::new().height(Length::Fill),
        Space::new().height(Length::Fixed(nudge)),
        card,
        Space::new().height(Length::Fill),
    ]
    .align_x(iced::Alignment::Center);

    container(centered)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .padding(28)
        .into()
}

fn labeled<'a>(label: &'a str, control: Element<'a, Message>) -> Element<'a, Message> {
    column![text(label).size(12).color(theme::text_muted()), control]
        .spacing(5)
        .into()
}

fn browse_btn() -> Element<'static, Message> {
    button(text("浏览").size(12).color(theme::text_high()))
        .padding([8, 12])
        .on_press(Message::BrowseKeyFile)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(
                    if hovered {
                        theme::surface_3()
                    } else {
                        theme::surface_2()
                    }
                    .into(),
                ),
                text_color: theme::text_high(),
                border: Border {
                    color: theme::border_subtle(),
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

fn jump_section(config: &SessionConfig) -> Element<'_, Message> {
    let arrow = if config.show_jump { "▼" } else { "▶" };
    let header = button(
        text(format!("{arrow} 跳板机（可选）"))
            .size(12)
            .color(theme::text_muted()),
    )
    .padding([4, 0])
    .on_press(Message::ToggleJump)
    .style(|_, _| button::Style {
        background: Some(Color::TRANSPARENT.into()),
        text_color: theme::text_muted(),
        ..Default::default()
    });

    if config.show_jump {
        column![
            header,
            labeled(
                "跳板机主机",
                widgets::field(
                    "bastion.example.com",
                    &config.jump_host,
                    Message::JumpHostChanged,
                ),
            ),
        ]
        .spacing(6)
        .into()
    } else {
        column![
            header,
            container(
                text("通过 bastion 主机连接 → 选择已保存的主机...")
                    .size(11)
                    .color(theme::text_dim()),
            )
            .padding([2, 4]),
        ]
        .spacing(2)
        .into()
    }
}

fn auth_card<'a>(
    mode: AuthMode,
    current: AuthMode,
    title: &'a str,
    description: &'a str,
) -> Element<'a, Message> {
    let selected = mode == current;
    let title_color = if selected {
        theme::accent_strong()
    } else {
        theme::text_high()
    };
    let body = column![
        text(title).size(14).color(title_color),
        text(description).size(11).color(theme::text_muted()),
    ]
    .spacing(5);

    button(body)
        .width(Length::FillPortion(1))
        .padding([12, 13])
        .on_press(Message::AuthModeChanged(mode))
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            let bg = if selected {
                theme::accent_soft()
            } else if hovered {
                theme::surface_2()
            } else {
                theme::surface_1()
            };
            button::Style {
                background: Some(bg.into()),
                text_color: theme::text_high(),
                border: Border {
                    color: if selected {
                        theme::accent()
                    } else {
                        theme::border_subtle()
                    },
                    width: if selected { 1.5 } else { 1.0 },
                    radius: 9.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}
