//! Horizontal session tabs across the top of the workspace, Termius-style.
//! Each tab shows a status dot, the session title, and a close affordance.

use iced::widget::{button, container, mouse_area, row, text, tooltip, Space};
use iced::{Border, Color, Element, Length};

use crate::message::Message;
use crate::theme;
use crate::ui::widgets;
use crate::App;

pub fn view(app: &App) -> Element<'_, Message> {
    let mut tabs = row![].spacing(2).align_y(iced::Alignment::Center);

    // When the sidebar is collapsed, the tab bar reaches the window's left edge,
    // so the floating traffic lights would overlap it. Reserve their space and
    // offer an expand affordance in that reclaimed strip.
    if app.sidebar_collapsed() && !app.sidebar_animating() {
        tabs = tabs
            .push(Space::new().width(Length::Fixed(theme::TRAFFIC_LIGHT_INSET)))
            .push(expand_button());
    }

    let pulse = app.connecting_pulse;
    for (index, session) in app.sessions.iter().enumerate() {
        let active = index == app.active;
        // A tab mid-close animates its width down to 0 (interpolated each frame).
        let closing = app
            .closing_tabs
            .get(&session.id)
            .map(|a| a.interpolate(0.0_f32, 1.0_f32, app.now()));
        tabs = tabs.push(tab(index, session, active, pulse, closing));
    }

    // New-tab button.
    tabs = tabs.push(
        button(text("+").size(16).color(theme::text_muted()))
            .padding([6, 12])
            .on_press(Message::NewTab)
            .style(|_, status| {
                let hovered = matches!(status, button::Status::Hovered);
                button::Style {
                    background: Some(
                        if hovered {
                            theme::surface_2()
                        } else {
                            Color::TRANSPARENT
                        }
                        .into(),
                    ),
                    text_color: theme::text_high(),
                    border: Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: 6.0.into(),
                    },
                    ..Default::default()
                }
            }),
    );

    // The empty stretch to the right of the tabs doubles as the window's drag
    // region, since the native titlebar is fused away.
    let drag_region = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_press(Message::StartWindowDrag);

    container(
        row![tabs, drag_region]
            .align_y(iced::Alignment::Center)
            .padding([0, 8]),
    )
    .width(Length::Fill)
    .height(Length::Fixed(theme::TAB_BAR_HEIGHT))
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

fn tab<'a>(
    index: usize,
    session: &'a crate::session::Session,
    active: bool,
    pulse: bool,
    closing: Option<f32>,
) -> Element<'a, Message> {
    let full_title = session.title();
    let truncated = full_title.chars().count() > 22;
    let title = if truncated {
        format!("{}…", full_title.chars().take(21).collect::<String>())
    } else {
        full_title.clone()
    };

    // Stable per-tab hue derived from the host (falls back to the title for
    // local shells / unsaved sessions so each tab still gets a distinct color).
    let accent_key = if session.config.host.trim().is_empty() {
        full_title.as_str()
    } else {
        session.config.host.trim()
    };
    let accent = theme::tab_accent(accent_key);

    // Amber dot when the session's open file has unsaved edits.
    let dirty = session
        .file_viewer
        .as_ref()
        .map(|fv| fv.dirty)
        .unwrap_or(false);

    // While closing, render a non-interactive pill that collapses horizontally.
    if let Some(factor) = closing {
        return closing_tab(&title, accent, dirty, factor);
    }

    let close = button(text("✕").size(11).color(theme::text_dim()))
        .padding([2, 5])
        .on_press(Message::CloseTab(index))
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(
                    if hovered {
                        theme::with_alpha(theme::status_error(), 0.2)
                    } else {
                        Color::TRANSPARENT
                    }
                    .into(),
                ),
                text_color: if hovered {
                    theme::status_error()
                } else {
                    theme::text_dim()
                },
                border: Border {
                    radius: 5.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        });

    let mut body = row![widgets::status_dot(&session.phase, pulse)]
        .spacing(7)
        .align_y(iced::Alignment::Center);
    if dirty {
        body = body.push(unsaved_dot());
    }
    body = body
        .push(text(title).size(13).color(if active {
            theme::text_high()
        } else {
            theme::text_muted()
        }))
        .push(close);

    let tab_button = button(body)
        .padding([7, 10])
        .on_press(Message::SelectTab(index))
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            // Tint the active/hovered tab with its own accent at low alpha so
            // the bar reads as a row of colored pills without shouting.
            let bg = if active {
                theme::with_alpha(accent, 0.16)
            } else if hovered {
                theme::surface_2()
            } else {
                Color::TRANSPARENT
            };
            button::Style {
                background: Some(bg.into()),
                text_color: theme::text_high(),
                border: Border {
                    color: if active {
                        theme::with_alpha(accent, 0.6)
                    } else {
                        Color::TRANSPARENT
                    },
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            }
        });

    // Only attach a tooltip when the title was actually truncated, so it shows
    // the full host name on hover.
    if truncated {
        tooltip(
            tab_button,
            container(text(full_title).size(12).color(theme::text_high()))
                .padding([4, 8])
                .style(|_| container::Style {
                    background: Some(theme::surface_3().into()),
                    border: Border {
                        color: theme::border_subtle(),
                        width: 1.0,
                        radius: 6.0.into(),
                    },
                    ..Default::default()
                }),
            tooltip::Position::Bottom,
        )
        .gap(4)
        .into()
    } else {
        tab_button.into()
    }
}

/// A non-interactive pill that mirrors a tab's look but collapses its width to
/// zero as `factor` runs 1.0 → 0.0. The content is clipped, so the pill appears
/// to slide shut. Width is estimated from the title length (iced gives no
/// layout query), which is fine for a brief 120 ms collapse.
fn closing_tab<'a>(title: &str, accent: Color, dirty: bool, factor: f32) -> Element<'a, Message> {
    let chars = title.chars().count().min(22) as f32;
    let full = 68.0 + chars * 7.0 + if dirty { 14.0 } else { 0.0 };
    let width = (full * factor.clamp(0.0, 1.0)).max(0.0);

    let mut body = row![widgets_phase_idle_dot()]
        .spacing(7)
        .align_y(iced::Alignment::Center);
    if dirty {
        body = body.push(unsaved_dot());
    }
    body = body.push(text(title.to_string()).size(13).color(theme::text_muted()));

    container(body)
        .padding([7, 10])
        .width(Length::Fixed(width))
        .height(Length::Fixed(theme::TAB_BAR_HEIGHT - 8.0))
        .clip(true)
        .style(move |_| container::Style {
            background: Some(theme::with_alpha(accent, 0.16 * factor).into()),
            border: Border {
                color: theme::with_alpha(accent, 0.6 * factor),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// A dim status dot used by the closing pill (the live phase no longer matters).
fn widgets_phase_idle_dot() -> Element<'static, Message> {
    container(text("●").size(11).color(theme::text_dim())).into()
}

/// A small amber dot indicating the session's open file has unsaved edits.
fn unsaved_dot() -> Element<'static, Message> {
    container(Space::new())
        .width(Length::Fixed(7.0))
        .height(Length::Fixed(7.0))
        .style(|_| container::Style {
            background: Some(theme::status_warn().into()),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// Chevron shown in the tab bar when the sidebar is collapsed; expands it.
fn expand_button() -> Element<'static, Message> {
    button(text("›").size(15).color(theme::text_muted()))
        .padding([4, 9])
        .on_press(Message::ToggleSidebar)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(
                    if hovered {
                        theme::surface_2()
                    } else {
                        Color::TRANSPARENT
                    }
                    .into(),
                ),
                text_color: theme::text_high(),
                border: Border {
                    radius: 6.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .into()
}
