//! Settings overlay — multi-panel: Terminal, SSH, Keys, Appearance, Advanced.

use iced::widget::{button, column, container, row, scrollable, text, text_input, Space};
use iced::{Border, Color, Element, Length};

use crate::message::Message;
use crate::session::{OnDisconnect, SettingsPanel};
use crate::theme;
use crate::ui::widgets;
use crate::App;

pub fn view(app: &App) -> Element<'_, Message> {
    let progress = app.settings_progress();
    let nudge = ((1.0 - progress) * 20.0).max(0.0);
    let backdrop_alpha = progress * 0.5;

    let nav = nav_panel(app.settings_panel);
    let content = content_panel(app);

    let inner = row![
        nav,
        container(Space::new())
            .width(Length::Fixed(1.0))
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(theme::border_subtle().into()),
                ..Default::default()
            }),
        scrollable(content).height(Length::Fill),
    ]
    .height(Length::Fill);

    let card = container(inner)
        .width(Length::Fixed(600.0))
        .height(Length::Fixed(480.0))
        .style(|_| container::Style {
            background: Some(theme::surface_1().into()),
            border: Border {
                color: theme::border_strong(),
                width: 1.0,
                radius: 13.0.into(),
            },
            ..Default::default()
        });

    let backdrop = button(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::CloseSettings)
        .style(move |_, _| button::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, backdrop_alpha).into()),
            ..Default::default()
        });

    // Slide the card up from a small offset as it enters.
    let centered = container(
        column![
            Space::new().height(Length::Fill),
            Space::new().height(Length::Fixed(nudge)),
            card,
            Space::new().height(Length::Fill),
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill);

    iced::widget::stack![backdrop, centered].into()
}

fn nav_panel(active: SettingsPanel) -> Element<'static, Message> {
    let items = [
        (SettingsPanel::Terminal, "Terminal"),
        (SettingsPanel::Ssh, "SSH"),
        (SettingsPanel::Keys, "Keys"),
        (SettingsPanel::Appearance, "Appearance"),
        (SettingsPanel::Snippets, "Snippets"),
        (SettingsPanel::Advanced, "Advanced"),
    ];

    let mut col = column![
        container(text("Settings").size(13).color(theme::text_high()))
            .padding(iced::Padding { top: 18.0, right: 16.0, bottom: 12.0, left: 16.0 }),
    ];

    for (panel, label) in items {
        col = col.push(nav_item(label, panel == active, Message::SettingsPanelChanged(panel)));
    }

    container(col)
        .width(Length::Fixed(160.0))
        .height(Length::Fill)
        .into()
}

fn nav_item(label: &str, active: bool, on_press: Message) -> Element<'_, Message> {
    button(text(label).size(13).color(if active { theme::text_high() } else { theme::text_muted() }))
        .width(Length::Fill)
        .padding([8, 16])
        .on_press(on_press)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(
                    if active || hovered { theme::surface_2() } else { Color::TRANSPARENT }.into(),
                ),
                text_color: if active { theme::text_high() } else { theme::text_muted() },
                border: Border {
                    // 2px right accent bar for active item.
                    color: if active { theme::accent_strong() } else { Color::TRANSPARENT },
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

fn content_panel(app: &App) -> Element<'_, Message> {
    let body: Element<'_, Message> = match app.settings_panel {
        SettingsPanel::Terminal => terminal_panel(app),
        SettingsPanel::Ssh => ssh_panel(app),
        SettingsPanel::Keys => keys_panel(),
        SettingsPanel::Appearance => appearance_panel(app),
        SettingsPanel::Snippets => snippets_panel(app),
        SettingsPanel::Advanced => advanced_panel(app),
    };

    container(body)
        .padding(iced::Padding { top: 22.0, right: 24.0, bottom: 22.0, left: 24.0 })
        .width(Length::Fill)
        .into()
}

// ── Terminal ────────────────────────────────────────────────────────────────

fn terminal_panel(app: &App) -> Element<'_, Message> {
    column![
        panel_title("Terminal", "Font, cursor, and scrollback preferences."),
        group_label("Font"),
        setting_row(
            "Font size",
            "Affects all terminals.",
            row![
                stepper_btn("−", Message::SettingsFontSize(-1)),
                container(text(format!("{} px", app.font_size)).size(13).color(theme::text_high()))
                    .width(Length::Fixed(58.0))
                    .center_x(Length::Fixed(58.0)),
                stepper_btn("+", Message::SettingsFontSize(1)),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into(),
        ),
        group_label("Scrolling"),
        setting_row(
            "Scrollback lines",
            "Lines kept in memory per session.",
            cosmetic_badge("10,000"),
        ),
    ]
    .spacing(0)
    .into()
}

// ── SSH ─────────────────────────────────────────────────────────────────────

fn ssh_panel(app: &App) -> Element<'_, Message> {
    let on_disc_opts: &[(OnDisconnect, &str)] = &[
        (OnDisconnect::Alert, "Show alert"),
        (OnDisconnect::AutoReconnect, "Auto-reconnect"),
        (OnDisconnect::CloseTab, "Close tab"),
    ];

    let on_disc_row: Element<'_, Message> = row(
        on_disc_opts.iter().map(|(opt, label)| {
            let sel = app.on_disconnect == *opt;
            let opt = *opt;
            button(text(*label).size(12).color(if sel { theme::text_high() } else { theme::text_muted() }))
                .padding([5, 10])
                .on_press(Message::SettingsOnDisconnect(opt))
                .style(move |_, _| button::Style {
                    background: Some(if sel { theme::surface_3() } else { theme::surface_2() }.into()),
                    text_color: theme::text_high(),
                    border: Border {
                        color: if sel { theme::accent() } else { theme::border_subtle() },
                        width: if sel { 1.5 } else { 1.0 },
                        radius: 6.0.into(),
                    },
                    ..Default::default()
                })
                .into()
        })
        .collect::<Vec<_>>(),
    )
    .spacing(6)
    .into();

    column![
        panel_title("SSH", "Default values pre-filled for new connections."),
        group_label("Defaults"),
        setting_row(
            "Default username",
            "Pre-filled for new sessions.",
            text_input("e.g. ubuntu", &app.default_user)
                .on_input(Message::SettingsDefaultUserChanged)
                .padding([6, 10])
                .size(12)
                .style(input_style)
                .width(Length::Fixed(140.0))
                .into(),
        ),
        setting_row(
            "Default port",
            "Pre-filled for new sessions.",
            text_input("22", &app.default_port)
                .on_input(Message::SettingsDefaultPortChanged)
                .padding([6, 10])
                .size(12)
                .style(input_style)
                .width(Length::Fixed(70.0))
                .into(),
        ),
        group_label("Keep-alive"),
        setting_row(
            "ServerAliveInterval",
            "Send keepalive every N seconds. 0 = disabled.",
            text_input("60", &app.server_alive_interval)
                .on_input(Message::SettingsServerAliveInterval)
                .padding([6, 10])
                .size(12)
                .style(input_style)
                .width(Length::Fixed(70.0))
                .into(),
        ),
        group_label("Disconnect behavior"),
        setting_row("On disconnect", "What to do when a connection drops.", on_disc_row),
    ]
    .spacing(0)
    .into()
}

// ── Keys ────────────────────────────────────────────────────────────────────

fn keys_panel() -> Element<'static, Message> {
    column![
        panel_title("SSH Keys", "Keys used for authentication."),
        container(
            text("Keys are loaded automatically from ~/.ssh (id_ed25519, id_rsa, id_ecdsa).\nSpecify a custom key path in the connect form when needed.")
                .size(12)
                .color(theme::text_muted()),
        )
        .padding([8, 0]),
    ]
    .spacing(8)
    .into()
}

// ── Appearance ──────────────────────────────────────────────────────────────

fn appearance_panel(app: &App) -> Element<'_, Message> {
    use crate::theme::ColorScheme;

    let dot = |scheme: ColorScheme| -> Element<'_, Message> {
        let active = app.color_scheme() == scheme;
        let accent_color = match scheme {
            ColorScheme::DarkTeal => Color::from_rgb(0.235, 0.620, 0.560),
            ColorScheme::DarkBlue => Color::from_rgb(0.220, 0.530, 0.940),
            ColorScheme::Dracula  => Color::from_rgb(0.741, 0.576, 0.976),
            ColorScheme::Light    => Color::from_rgb(0.040, 0.480, 0.430),
        };
        button(
            column![
                container(Space::new())
                    .width(Length::Fixed(24.0))
                    .height(Length::Fixed(24.0))
                    .style(move |_| container::Style {
                        background: Some(accent_color.into()),
                        border: Border {
                            color: if active { Color::WHITE } else { Color::TRANSPARENT },
                            width: 2.0,
                            radius: 12.0.into(),
                        },
                        ..Default::default()
                    }),
                text(scheme.label()).size(10).color(if active { theme::accent() } else { theme::text_dim() }),
            ]
            .spacing(4)
            .align_x(iced::Alignment::Center)
        )
        .padding(4)
        .on_press(Message::SettingsColorScheme(scheme))
        .style(|_, _| button::Style { ..Default::default() })
        .into()
    };

    column![
        panel_title("Appearance", "Visual theme and window style."),
        group_label("Color scheme"),
        setting_row(
            "Theme",
            "Choose a color scheme for the app.",
            row(ColorScheme::ALL.iter().copied().map(dot).collect::<Vec<_>>()).spacing(10).into(),
        ),
        group_label("Accent"),
        setting_row(
            "Accent color",
            "Override the scheme's accent. Default keeps the theme's own.",
            accent_picker(app),
        ),
        group_label("Terminal"),
        setting_row(
            "Cursor shape",
            "How the terminal cursor is drawn.",
            cursor_shape_picker(app),
        ),
        setting_row(
            "Line height",
            "Vertical spacing between terminal rows.",
            row![
                stepper_btn("−", Message::SettingsLineHeight(-0.04)),
                container(
                    text(format!("{:.2}×", app.line_height))
                        .size(13)
                        .color(theme::text_high()),
                )
                .width(Length::Fixed(58.0))
                .center_x(Length::Fixed(58.0)),
                stepper_btn("+", Message::SettingsLineHeight(0.04)),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into(),
        ),
        setting_row(
            "Letter spacing",
            "Extra horizontal space between characters.",
            row![
                stepper_btn("−", Message::SettingsLetterSpacing(-0.5)),
                container(
                    text(format!("{:.1} px", app.letter_spacing()))
                        .size(13)
                        .color(theme::text_high()),
                )
                .width(Length::Fixed(58.0))
                .center_x(Length::Fixed(58.0)),
                stepper_btn("+", Message::SettingsLetterSpacing(0.5)),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into(),
        ),
    ]
    .spacing(0)
    .into()
}

/// Row of accent presets plus a "Default" (clear-override) chip.
fn accent_picker(app: &App) -> Element<'_, Message> {
    let mut r = row![].spacing(8).align_y(iced::Alignment::Center);

    // "Default" chip clears the override.
    let is_default = app.accent_hex.trim().is_empty();
    r = r.push(
        button(text("Default").size(11).color(if is_default {
            theme::accent()
        } else {
            theme::text_muted()
        }))
        .padding([4, 9])
        .on_press(Message::SettingsAccent(String::new()))
        .style(move |_, _| button::Style {
            background: Some(theme::surface_2().into()),
            text_color: theme::text_high(),
            border: Border {
                color: if is_default { theme::accent() } else { theme::border_subtle() },
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        }),
    );

    for (_, hex) in theme::ACCENT_PRESETS {
        let color = theme::parse_hex(hex).unwrap_or(theme::accent());
        let selected = app.accent_hex.eq_ignore_ascii_case(hex);
        r = r.push(
            button(Space::new().width(Length::Fixed(16.0)).height(Length::Fixed(16.0)))
                .padding(2)
                .on_press(Message::SettingsAccent(hex.to_string()))
                .style(move |_, _| button::Style {
                    background: Some(color.into()),
                    border: Border {
                        color: if selected { Color::WHITE } else { Color::TRANSPARENT },
                        width: 2.0,
                        radius: 10.0.into(),
                    },
                    ..Default::default()
                }),
        );
    }
    r.into()
}

/// Three-way cursor shape selector (Block / Underline / Beam).
fn cursor_shape_picker(app: &App) -> Element<'_, Message> {
    use crate::theme::CursorShape;
    let current = app.cursor_shape();
    row(CursorShape::ALL.iter().copied().map(|shape| {
        let sel = current == shape;
        button(text(shape.label()).size(12).color(if sel {
            theme::text_high()
        } else {
            theme::text_muted()
        }))
        .padding([5, 10])
        .on_press(Message::SettingsCursorShape(shape))
        .style(move |_, _| button::Style {
            background: Some(if sel { theme::surface_3() } else { theme::surface_2() }.into()),
            text_color: theme::text_high(),
            border: Border {
                color: if sel { theme::accent() } else { theme::border_subtle() },
                width: if sel { 1.5 } else { 1.0 },
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .into()
    }).collect::<Vec<_>>())
    .spacing(6)
    .into()
}

// ── Snippets ──────────────────────────────────────────────────────────────────

fn snippets_panel(app: &App) -> Element<'_, Message> {
    let mut col = column![
        panel_title(
            "Snippets",
            "Type an abbreviation then Space or Tab to expand it (e.g. gp → git push).",
        ),
        group_label("New"),
        row![
            text_input("abbr", &app.snippet_draft_abbr)
                .on_input(Message::SnippetDraftAbbr)
                .padding([6, 10])
                .size(12)
                .style(input_style)
                .width(Length::Fixed(90.0)),
            text_input("expands to…", &app.snippet_draft_expansion)
                .on_input(Message::SnippetDraftExpansion)
                .on_submit(Message::SnippetAdd)
                .padding([6, 10])
                .size(12)
                .style(input_style)
                .width(Length::Fill),
            widgets::small_button("Add", widgets::Tone::Primary, Message::SnippetAdd),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        group_label("Saved"),
    ]
    .spacing(0);

    if app.snippets.is_empty() {
        col = col.push(
            container(text("No snippets yet.").size(12).color(theme::text_dim()))
                .padding([8, 0]),
        );
    } else {
        for snip in &app.snippets {
            col = col.push(snippet_row(&snip.abbr, &snip.expansion));
        }
    }

    col.into()
}

fn snippet_row<'a>(abbr: &'a str, expansion: &'a str) -> Element<'a, Message> {
    column![
        row![
            container(
                text(abbr)
                    .size(13)
                    .font(theme::TERMINAL_FONT)
                    .color(theme::accent_strong()),
            )
            .width(Length::Fixed(90.0)),
            text(expansion)
                .size(12)
                .font(theme::TERMINAL_FONT)
                .color(theme::text_high())
                .width(Length::Fill),
            widgets::small_button(
                "Delete",
                widgets::Tone::Danger,
                Message::SnippetDelete(abbr.to_string()),
            ),
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center),
        container(Space::new().width(Length::Fill).height(Length::Fixed(1.0))).style(|_| {
            container::Style {
                background: Some(theme::border_subtle().into()),
                ..Default::default()
            }
        }),
    ]
    .spacing(0)
    .padding(iced::Padding { top: 8.0, right: 0.0, bottom: 0.0, left: 0.0 })
    .into()
}

// ── Advanced ────────────────────────────────────────────────────────────────

fn advanced_panel(app: &App) -> Element<'_, Message> {
    let vault_control: Element<'_, Message> = if app.vault_busy {
        container(text("Migrating…").size(12).color(theme::text_muted()))
            .padding([4, 10])
            .into()
    } else if app.vault_enabled {
        use iced::widget::row as irow;
        let lock_btn = if app.vault_locked() {
            container(text("Locked").size(12).color(theme::text_muted()))
                .padding([4, 10])
                .into()
        } else {
            widgets::small_button("Lock", widgets::Tone::Neutral, Message::VaultLock)
        };
        irow![
            lock_btn,
            widgets::small_button("Disable", widgets::Tone::Danger, Message::VaultDisableRequest),
        ]
        .spacing(6)
        .into()
    } else {
        widgets::small_button("Enable", widgets::Tone::Primary, Message::VaultEnableRequest)
    };

    let vault_hint = if app.vault_enabled {
        "Master-password encryption is ON. Saved SSH passwords auto-lock after 15 min."
    } else {
        "Disabled (default). Passwords use a built-in key. Enable to require a master password."
    };

    column![
        panel_title("Advanced", "Power-user options."),
        group_label("Security"),
        setting_row("Credential vault", vault_hint, vault_control),
        group_label("History"),
        danger_row(
            "Clear command history",
            "Remove all persisted command history.",
            widgets::small_button("Clear all", widgets::Tone::Danger, Message::HistoryClearAll),
        ),
    ]
    .spacing(0)
    .into()
}

// ── Shared helpers ───────────────────────────────────────────────────────────

fn panel_title<'a>(title: &'a str, sub: &'a str) -> Element<'a, Message> {
    container(
        column![
            text(title).size(16).color(theme::text_high()),
            text(sub).size(12).color(theme::text_dim()),
        ]
        .spacing(3),
    )
    .padding(iced::Padding { top: 0.0, right: 0.0, bottom: 16.0, left: 0.0 })
    .into()
}

fn group_label(label: &str) -> Element<'_, Message> {
    container(text(label).size(11).color(theme::text_dim()))
        .padding(iced::Padding { top: 12.0, right: 0.0, bottom: 4.0, left: 0.0 })
        .into()
}

fn setting_row<'a>(
    title: &'a str,
    hint: &'a str,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    column![
        row![
            column![
                text(title).size(13).color(theme::text_high()),
                text(hint).size(11).color(theme::text_dim()),
            ]
            .spacing(2)
            .width(Length::Fill),
            container(control).align_x(iced::alignment::Horizontal::Right),
        ]
        .spacing(16)
        .align_y(iced::Alignment::Center),
        container(Space::new().width(Length::Fill).height(Length::Fixed(1.0)))
            .style(|_| container::Style {
                background: Some(theme::border_subtle().into()),
                ..Default::default()
            }),
    ]
    .spacing(0)
    .padding(iced::Padding { top: 10.0, right: 0.0, bottom: 0.0, left: 0.0 })
    .into()
}

fn danger_row<'a>(
    title: &'a str,
    hint: &'a str,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    setting_row(title, hint, control)
}

fn cosmetic_badge(label: &'static str) -> Element<'static, Message> {
    container(text(label).size(12).color(theme::text_muted()))
        .padding([4, 10])
        .style(|_| container::Style {
            background: Some(theme::surface_2().into()),
            border: Border {
                color: theme::border_subtle(),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn stepper_btn(label: &str, on_press: Message) -> Element<'_, Message> {
    button(
        text(label.to_string())
            .size(15)
            .color(theme::text_high())
            .align_x(iced::alignment::Horizontal::Center)
            .width(Length::Fill),
    )
    .width(Length::Fixed(30.0))
    .padding([3, 0])
    .on_press(on_press)
    .style(|_, status| {
        let hovered = matches!(status, button::Status::Hovered);
        button::Style {
            background: Some(
                if hovered { theme::surface_3() } else { theme::surface_2() }.into(),
            ),
            text_color: theme::text_high(),
            border: Border {
                color: theme::border_subtle(),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

fn input_style(_: &iced::Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: theme::surface_2().into(),
        border: Border {
            color: if focused { theme::accent() } else { theme::border_subtle() },
            width: 1.0,
            radius: 6.0.into(),
        },
        icon: theme::text_muted(),
        placeholder: theme::text_dim(),
        value: theme::text_high(),
        selection: theme::accent_soft(),
    }
}
