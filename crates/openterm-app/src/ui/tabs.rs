//! Horizontal session tabs across the top of the workspace, Termius-style.
//! Each tab shows a status dot, the session title, and a close affordance.

use iced::widget::{button, container, mouse_area, row, text, Space};
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
    if app.sidebar_collapsed() {
        tabs = tabs
            .push(Space::new().width(Length::Fixed(theme::TRAFFIC_LIGHT_INSET)))
            .push(expand_button());
    }

    for (index, session) in app.sessions.iter().enumerate() {
        let active = index == app.active;
        tabs = tabs.push(tab(index, session, active));
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
) -> Element<'a, Message> {
    let title = session.title();
    let title = if title.chars().count() > 22 {
        format!("{}…", title.chars().take(21).collect::<String>())
    } else {
        title
    };

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

    let body = row![
        widgets::status_dot(&session.phase),
        text(title).size(13).color(if active {
            theme::text_high()
        } else {
            theme::text_muted()
        }),
        close,
    ]
    .spacing(7)
    .align_y(iced::Alignment::Center);

    button(body)
        .padding([7, 10])
        .on_press(Message::SelectTab(index))
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            let bg = if active {
                theme::surface_0()
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
                        theme::border_subtle()
                    } else {
                        Color::TRANSPARENT
                    },
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            }
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
