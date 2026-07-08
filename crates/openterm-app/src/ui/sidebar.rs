//! Left sidebar: brand, host search, and the saved-hosts list.
//! Hosts are grouped by the `group` field; each row shows a colored left bar,
//! name, first tag chip, user@host, and an offline status dot.

use iced::widget::{
    button, column, container, mouse_area, row, scrollable, text, text_input, Space,
};
use iced::{mouse, Border, Color, Element, Length};

use crate::message::Message;
use crate::theme;
use crate::App;

pub fn view(app: &App) -> Element<'_, Message> {
    let brand = container(
        row![
            container(Space::new())
                .width(Length::Fixed(4.0))
                .height(Length::Fixed(18.0))
                .style(|_| container::Style {
                    background: Some(theme::accent_strong().into()),
                    border: Border {
                        radius: 2.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            text("OpenTerm").size(17).color(theme::text_high()),
            Space::new().width(Length::Fill),
            collapse_button("‹", Message::ToggleSidebar),
            new_tab_button(),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    )
    .padding(iced::Padding {
        top: 30.0,
        right: 10.0,
        bottom: 10.0,
        left: 14.0,
    });

    let search = container(
        text_input("Search name, IP, tag...", &app.host_search)
            .on_input(Message::HostSearchChanged)
            .padding([8, 11])
            .size(13)
            .style(|_, status| {
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
                        radius: 8.0.into(),
                    },
                    icon: theme::text_muted(),
                    placeholder: theme::text_dim(),
                    value: theme::text_high(),
                    selection: theme::accent_soft(),
                }
            }),
    )
    .padding([0, 12]);

    let query = app.host_search.trim().to_lowercase();
    let hovered = app.hovered_host();

    // Collect indices that match the filter.
    let matching: Vec<usize> = app
        .hosts
        .iter()
        .enumerate()
        .filter(|(_, h)| {
            if query.is_empty() {
                return true;
            }
            h.name.to_lowercase().contains(&query)
                || h.host.to_lowercase().contains(&query)
                || h.tags.iter().any(|t| t.to_lowercase().contains(&query))
        })
        .map(|(i, _)| i)
        .collect();

    let list_section: Element<'_, Message> = if matching.is_empty() {
        container(
            text(if app.hosts.is_empty() {
                "No saved hosts yet.\nConnect once and Save to add one."
            } else {
                "No hosts match your search."
            })
            .size(12)
            .color(theme::text_dim()),
        )
        .padding(16)
        .into()
    } else {
        // Group matching indices by their group name (preserve insertion order).
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for &idx in &matching {
            let g = app.hosts[idx]
                .group
                .clone()
                .unwrap_or_default();
            if let Some(entry) = groups.iter_mut().find(|(k, _)| k == &g) {
                entry.1.push(idx);
            } else {
                groups.push((g, vec![idx]));
            }
        }
        // Sort: named groups first (alphabetical), then ungrouped last.
        groups.sort_by(|(a, _), (b, _)| match (a.is_empty(), b.is_empty()) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => a.cmp(b),
        });

        let multi_group = groups.len() > 1 || groups.first().map(|(g, _)| !g.is_empty()).unwrap_or(false);

        let mut list = column![].spacing(2);
        for (group_name, indices) in &groups {
            let collapsed = app.collapsed_groups.contains(group_name);
            if multi_group {
                let label = if group_name.is_empty() {
                    format!("UNGROUPED ({})", indices.len())
                } else {
                    format!("{} ({})", group_name.to_uppercase(), indices.len())
                };
                list = list.push(group_header(label, group_name.clone(), collapsed));
            }
            // A collapsed group hides its rows (the header stays as the toggle).
            if multi_group && collapsed {
                continue;
            }
            let color = group_color(group_name);
            for &idx in indices {
                list = list.push(host_row(idx, &app.hosts[idx], hovered == Some(idx), color, app.ping_results.get(&app.hosts[idx].id).copied().flatten()));
            }
        }
        scrollable(list.padding([4, 12]))
            .height(Length::Fill)
            .into()
    };

    let content = column![
        brand,
        search,
        Space::new().height(Length::Fixed(8.0)),
        list_section,
    ]
    .height(Length::Fill);

    container(content)
        .width(Length::Fixed(app.sidebar_visual_width()))
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(theme::surface_1().into()),
            ..Default::default()
        })
        .into()
}

/// The draggable divider between the sidebar and the workspace.
pub fn divider() -> Element<'static, Message> {
    mouse_area(
        container(Space::new())
            .width(Length::Fixed(theme::SIDEBAR_DIVIDER_WIDTH))
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(theme::border_subtle().into()),
                ..Default::default()
            }),
    )
    .interaction(mouse::Interaction::ResizingHorizontally)
    .on_press(Message::SidebarDragStart)
    .into()
}

fn new_tab_button() -> Element<'static, Message> {
    button(
        text("+")
            .size(16)
            .color(theme::text_muted())
            .align_x(iced::alignment::Horizontal::Center),
    )
    .width(Length::Fixed(28.0))
    .height(Length::Fixed(28.0))
    .padding([2, 0])
    .on_press(Message::NewTab)
    .style(|_, status| {
        let hovered = matches!(status, button::Status::Hovered);
        button::Style {
            background: Some(
                if hovered {
                    theme::surface_3()
                } else {
                    Color::TRANSPARENT
                }
                .into(),
            ),
            text_color: theme::text_high(),
            border: Border {
                color: theme::border_subtle(),
                width: 1.0,
                radius: 7.0.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

fn collapse_button(glyph: &str, on_press: Message) -> Element<'_, Message> {
    button(text(glyph.to_string()).size(15).color(theme::text_muted()))
        .padding([2, 8])
        .on_press(on_press)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(
                    if hovered {
                        theme::surface_3()
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

/// A clickable group header: a disclosure chevron + the section label. Clicking
/// toggles the group collapsed/expanded (persisted).
fn group_header(label: String, group_name: String, collapsed: bool) -> Element<'static, Message> {
    let chevron = if collapsed { "›" } else { "⌄" };
    button(
        row![
            text(chevron).size(11).color(theme::text_dim()),
            text(label).size(10).color(theme::text_dim()),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    )
    .width(Length::Fill)
    .padding(iced::Padding {
        top: 10.0,
        right: 4.0,
        bottom: 3.0,
        left: 4.0,
    })
    .on_press(Message::GroupToggle(group_name))
    .style(|_, status| {
        let hovered = matches!(status, button::Status::Hovered);
        button::Style {
            background: Some(
                if hovered { theme::surface_2() } else { Color::TRANSPARENT }.into(),
            ),
            text_color: theme::text_dim(),
            border: Border {
                radius: 5.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
    .into()
}

/// Deterministic color for a group name.
fn group_color(group: &str) -> Color {
    if group.is_empty() {
        return theme::border_subtle();
    }
    const COLORS: [Color; 5] = [
        Color::from_rgb(0.11, 0.62, 0.46), // teal
        Color::from_rgb(0.22, 0.47, 0.85), // blue
        Color::from_rgb(0.67, 0.45, 0.20), // amber
        Color::from_rgb(0.55, 0.30, 0.75), // purple
        Color::from_rgb(0.80, 0.25, 0.35), // red
    ];
    let idx = group
        .bytes()
        .fold(0usize, |a, b| a.wrapping_add(b as usize))
        % COLORS.len();
    COLORS[idx]
}

fn tag_chip(tag: &str) -> Element<'_, Message> {
    container(text(tag).size(10).color(theme::text_muted()))
        .padding([2, 6])
        .style(|_| container::Style {
            background: Some(theme::surface_3().into()),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// A circular monogram avatar: the host name's first letter on a hue derived
/// from the name, with a small status dot baked into the bottom-right.
fn host_avatar<'a>(host: &openterm_core::HostProfile, reachable: bool) -> Element<'a, Message> {
    let initial = host
        .name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "·".to_string());
    let hue = theme::tab_accent(if host.host.trim().is_empty() {
        host.name.as_str()
    } else {
        host.host.as_str()
    });
    let dot_color = if reachable {
        theme::status_ok()
    } else {
        theme::status_idle()
    };

    let circle = container(
        text(initial)
            .size(13)
            .color(Color::WHITE)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fixed(30.0))
    .height(Length::Fixed(30.0))
    .center_x(Length::Fixed(30.0))
    .center_y(Length::Fixed(30.0))
    .style(move |_| container::Style {
        background: Some(theme::with_alpha(hue, 0.85).into()),
        border: Border {
            radius: 15.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });

    // Overlay a status pip in the bottom-right corner.
    let pip = container(
        container(Space::new())
            .width(Length::Fixed(9.0))
            .height(Length::Fixed(9.0))
            .style(move |_| container::Style {
                background: Some(dot_color.into()),
                border: Border {
                    color: theme::surface_1(),
                    width: 1.5,
                    radius: 5.0.into(),
                },
                ..Default::default()
            }),
    )
    .width(Length::Fixed(30.0))
    .height(Length::Fixed(30.0))
    .align_x(iced::alignment::Horizontal::Right)
    .align_y(iced::alignment::Vertical::Bottom);

    iced::widget::stack![circle, pip].into()
}

/// Human-friendly "time since" from a stored `unix:<secs>` timestamp.
fn relative_time(ts: &str) -> Option<String> {
    let secs: u64 = ts.strip_prefix("unix:").and_then(|s| s.parse().ok())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let age = now.saturating_sub(secs);
    Some(match age {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", age / 60),
        3600..=86399 => format!("{}h ago", age / 3600),
        86400..=2591999 => format!("{}d ago", age / 86400),
        _ => format!("{}w ago", age / 604800),
    })
}

fn host_row<'a>(
    index: usize,
    host: &'a openterm_core::HostProfile,
    hovered: bool,
    group_color: Color,
    ping_ms: Option<u32>,  // None = not yet measured or unreachable
) -> Element<'a, Message> {
    let color_bar = container(Space::new())
        .width(Length::Fixed(3.0))
        .height(Length::Fixed(40.0))
        .style(move |_| container::Style {
            background: Some(group_color.into()),
            border: Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });

    let avatar = host_avatar(host, ping_ms.is_some());

    let meta = host.display_target();
    // Append a relative "last connected" hint when we have one. The primary
    // `meta` text takes `Fill` and clips (Wrapping::None) so a long
    // `user@host:port` shrinks within `info_col` at narrow sidebar widths
    // instead of overflowing into the status column and overlapping it.
    let meta_line: Element<'_, Message> =
        match host.last_connected_at.as_deref().and_then(relative_time) {
            Some(rel) => row![
                text(meta)
                    .size(11)
                    .color(theme::text_muted())
                    .wrapping(text::Wrapping::None)
                    .width(Length::Fill),
                text("·").size(11).color(theme::text_dim()),
                text(rel).size(11).color(theme::text_dim()),
            ]
            .spacing(5)
            .into(),
            None => text(meta)
                .size(11)
                .color(theme::text_muted())
                .wrapping(text::Wrapping::None)
                .width(Length::Fill)
                .into(),
        };
    let name_row = {
        let mut r = row![text(host.name.clone())
            .size(13)
            .color(theme::text_high())
            .wrapping(text::Wrapping::None)
            .width(Length::Fill)]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        if let Some(tag) = host.tags.first() {
            r = r.push(tag_chip(tag));
        }
        r
    };

    let dot_color = if ping_ms.is_some() {
        Color::from_rgb(0.13, 0.75, 0.42) // green = reachable
    } else {
        Color::from_rgb(0.42, 0.42, 0.42) // gray = offline/unknown
    };
    let status_dot = container(Space::new())
        .width(Length::Fixed(8.0))
        .height(Length::Fixed(8.0))
        .style(move |_| container::Style {
            background: Some(dot_color.into()),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });

    let ping_label: Element<'_, Message> = match ping_ms {
        Some(ms) => text(format!("{ms}ms")).size(10).color(theme::text_dim()).into(),
        None => text("Offline").size(10).color(theme::text_dim()).into(),
    };

    // Fixed-width slot so the dot + "142ms" / "Offline" label always has its
    // own reserved space; without this the Fill `info_col` expands over it at
    // narrow widths and the labels overlap.
    let status_col = column![status_dot, ping_label]
        .spacing(2)
        .width(Length::Fixed(44.0))
        .align_x(iced::Alignment::Center);

    let info_col = column![
        name_row,
        meta_line,
    ]
    .spacing(2)
    .width(Length::Fill);

    let info = button(
        row![color_bar, avatar, info_col, status_col]
            .spacing(8)
            .align_y(iced::Alignment::Center),
    )
    .width(Length::Fill)
    .padding([7, 9])
    .on_press(Message::ConnectSavedHost(index))
    .style(move |_, status| {
        let active = matches!(status, button::Status::Hovered) || hovered;
        button::Style {
            background: Some(
                if active {
                    theme::surface_3()
                } else {
                    theme::surface_2()
                }
                .into(),
            ),
            text_color: theme::text_high(),
            border: Border {
                color: if active {
                    theme::border_strong()
                } else {
                    theme::border_subtle()
                },
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        }
    });

    let actions: Element<'_, Message> = if hovered {
        row![
            icon_button("✎", Message::EditSavedHost(index), false),
            icon_button("✕", Message::DeleteSavedHost(index), true),
        ]
        .spacing(2)
        .align_y(iced::Alignment::Center)
        .into()
    } else {
        Space::new().width(Length::Fixed(2.0)).into()
    };

    mouse_area(
        row![info, actions]
            .spacing(4)
            .align_y(iced::Alignment::Center),
    )
    .on_enter(Message::HostHovered(Some(index)))
    .on_exit(Message::HostHovered(None))
    .into()
}

fn icon_button(glyph: &str, on_press: Message, danger: bool) -> Element<'_, Message> {
    button(text(glyph.to_string()).size(12).color(theme::text_muted()))
        .padding([6, 7])
        .on_press(on_press)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            let (bg, fg) = if hovered && danger {
                (
                    theme::with_alpha(theme::status_error(), 0.2),
                    theme::status_error(),
                )
            } else if hovered {
                (theme::surface_3(), theme::text_high())
            } else {
                (Color::TRANSPARENT, theme::text_muted())
            };
            button::Style {
                background: Some(bg.into()),
                text_color: fg,
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 7.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

/// Modal asking the user to confirm deleting a saved host.
pub fn delete_host_overlay(app: &App) -> Element<'_, Message> {
    let Some((_, name)) = app.pending_host_delete() else {
        return container(Space::new()).into();
    };

    let buttons = row![
        Space::new().width(Length::Fill),
        confirm_btn("Cancel", Message::CancelDeleteHost, false),
        confirm_btn("Delete", Message::ConfirmDeleteHost, true),
    ]
    .spacing(8);

    let card = container(
        column![
            text("Delete saved host?").size(16).color(theme::text_high()),
            text(name.to_string()).size(13).color(theme::text_high()),
            text("This removes it from your saved hosts. This cannot be undone.")
                .size(12)
                .color(theme::text_muted()),
            buttons,
        ]
        .spacing(12),
    )
    .padding(18)
    .width(Length::Fixed(360.0))
    .style(|_| container::Style {
        background: Some(theme::surface_1().into()),
        border: Border {
            color: theme::border_strong(),
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    });

    let backdrop = button(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::CancelDeleteHost)
        .style(|_, _| button::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.5).into()),
            ..Default::default()
        });

    iced::widget::stack![
        backdrop,
        container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    ]
    .into()
}

fn confirm_btn(label: &str, msg: Message, danger: bool) -> Element<'_, Message> {
    button(text(label.to_string()).size(12).color(if danger {
        Color::WHITE
    } else {
        theme::text_high()
    }))
    .padding([6, 11])
    .on_press(msg)
    .style(move |_, status| {
        let hovered = matches!(status, button::Status::Hovered);
        let bg = if danger {
            if hovered {
                Color::from_rgb(0.78, 0.22, 0.22)
            } else {
                Color::from_rgb(0.70, 0.18, 0.18)
            }
        } else if hovered {
            theme::surface_3()
        } else {
            theme::surface_2()
        };
        button::Style {
            background: Some(bg.into()),
            text_color: if danger { Color::WHITE } else { theme::text_high() },
            border: Border {
                color: if danger {
                    Color::TRANSPARENT
                } else {
                    theme::border_subtle()
                },
                width: 1.0,
                radius: 7.0.into(),
            },
            ..Default::default()
        }
    })
    .into()
}
