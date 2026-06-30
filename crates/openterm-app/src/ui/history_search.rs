//! Ctrl+R history-search overlay (Sprint 3). A palette-style fuzzy list over
//! the persisted command history: type to filter, ↑/↓ to navigate, Enter to
//! insert the command onto the prompt, Esc to close. Reuses the palette's
//! overlay look so it feels native alongside Cmd+K.

use iced::widget::{button, column, container, row, text, text_input, Space};
use iced::{Border, Color, Element, Length};
use once_cell::sync::Lazy;

use crate::message::Message;
use crate::theme;
use crate::App;

/// Stable id for the search box, so we can focus it on open.
pub static INPUT_ID: Lazy<iced::advanced::widget::Id> =
    Lazy::new(|| iced::advanced::widget::Id::new("history-search"));

/// Max rows shown in the overlay.
const MAX_ROWS: usize = 40;

/// The deduplicated, newest-first list of history commands matching the current
/// query. Shared by the view (to render) and `update` (to navigate/accept), so
/// the selected index always lines up with what's drawn.
pub fn matches(app: &App) -> Vec<&str> {
    let q = app.history_search_query.trim().to_lowercase();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out: Vec<&str> = Vec::new();
    for entry in app.all_history.iter() {
        let cmd = entry.cmd.as_str();
        if cmd.is_empty() {
            continue;
        }
        if !q.is_empty() && !cmd.to_lowercase().contains(&q) {
            continue;
        }
        if seen.insert(cmd) {
            out.push(cmd);
            if out.len() >= MAX_ROWS {
                break;
            }
        }
    }
    out
}

pub fn view(app: &App) -> Element<'_, Message> {
    let input = text_input("Search history…", &app.history_search_query)
        .id(INPUT_ID.clone())
        .on_input(Message::HistorySearchQuery)
        .on_submit(Message::HistorySearchAccept)
        .padding([11, 14])
        .size(15)
        .style(|_, _| text_input::Style {
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
        });

    let rows = matches(app);
    let mut list = column![].spacing(2);
    if rows.is_empty() {
        list = list.push(
            container(text("No matching history").size(13).color(theme::text_dim()))
                .padding([10, 12]),
        );
    } else {
        for (index, cmd) in rows.iter().enumerate() {
            list = list.push(history_row(cmd, index == app.history_search_idx));
        }
    }

    let hint = text("↑↓ navigate · Enter insert · Esc close")
        .size(11)
        .color(theme::text_dim());

    let body = column![
        input,
        Space::new().height(Length::Fixed(10.0)),
        list,
        Space::new().height(Length::Fixed(8.0)),
        hint,
    ]
    .spacing(2);

    let card = container(body)
        .padding(14)
        .width(Length::Fixed(620.0))
        .style(|_| container::Style {
            background: Some(theme::surface_1().into()),
            border: Border {
                color: theme::border_strong(),
                width: 1.0,
                radius: 13.0.into(),
            },
            ..Default::default()
        });

    // Click outside the card closes it.
    let backdrop = button(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::HistorySearchClose)
        .style(|_, _| button::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.45).into()),
            ..Default::default()
        });

    iced::widget::stack![
        backdrop,
        container(
            column![Space::new().height(Length::Fixed(86.0)), card]
                .align_x(iced::Alignment::Center)
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill),
    ]
    .into()
}

fn history_row(cmd: &str, selected: bool) -> Element<'_, Message> {
    let body = row![text(cmd)
        .size(13)
        .font(theme::TERMINAL_FONT)
        .color(theme::text_high())
        .width(Length::Fill)]
    .align_y(iced::Alignment::Center);

    button(body)
        .width(Length::Fill)
        .padding([8, 12])
        .on_press(Message::HistorySearchAccept)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            let bg = if selected {
                theme::accent_soft()
            } else if hovered {
                theme::surface_2()
            } else {
                Color::TRANSPARENT
            };
            button::Style {
                background: Some(bg.into()),
                text_color: theme::text_high(),
                border: Border {
                    color: if selected {
                        theme::with_alpha(theme::accent(), 0.5)
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
