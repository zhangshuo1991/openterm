//! Terminal workspace: the active session's grid painted on a canvas that
//! fills the available area. The grid is sized to this area (see
//! `terminal_render::grid_for_viewport` + `terminal_area`), so it never
//! overflows vertically.

use iced::advanced::widget;
use iced::widget::{button, canvas, container, row, stack, text, text_input};
use iced::{Alignment, Border, Color, Element, Length};
use once_cell::sync::Lazy;
use openterm_terminal::TerminalEngine;

use crate::message::Message;
use crate::terminal_render::TerminalCanvas;
use crate::theme;
use crate::App;

/// Stable id for the terminal search box, so we can focus it on open.
pub static SEARCH_INPUT_ID: Lazy<widget::Id> =
    Lazy::new(|| widget::Id::new("terminal-search"));

pub fn view(app: &App) -> Element<'_, Message> {
    let Some(session) = app.active_session() else {
        return container(iced::widget::Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    };

    let query = app.terminal_search.as_deref().unwrap_or("");

    let program = TerminalCanvas {
        snapshot: session.terminal.snapshot(),
        font_size: app.font_size,
        selection: session.selection,
        mouse: session.terminal.mouse_protocol(),
        search_query: query.to_lowercase(),
        search_current: app.terminal_search_idx,
        cursor_shape: app.cursor_shape(),
        copy_flash: app.copy_flash(),
        inline_suggestion: session.inline_suggestion.clone().unwrap_or_default(),
    };

    // Wheel handling lives inside the canvas `update` (so mouse-reporting apps
    // like vim/tmux receive wheel escape sequences, and local scroll still
    // works otherwise), so no `mouse_area` scroll wrapper is needed here.
    let surface = canvas(program).width(Length::Fill).height(Length::Fill);

    // Wrap in ImeEnabled so winit activates the OS input method (Chinese/Japanese/Korean).
    let ime_surface: Element<'_, Message> = super::ime::ImeEnabled::new(surface).into();

    let body = container(ime_surface)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding([crate::ui::TERMINAL_V_PADDING as u16, 14])
        .style(|_| container::Style {
            background: Some(theme::surface_0().into()),
            ..Default::default()
        });

    // Overlays: search bar (top-right) and/or the right-click context menu.
    let mut layers: Vec<Element<'_, Message>> = vec![body.into()];

    if app.terminal_search.is_some() {
        let bar = search_bar(query);
        let overlay = container(bar)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::End)
            .align_y(Alignment::Start)
            .padding([10, 18]);
        layers.push(overlay.into());
    }

    if let Some((mx, my)) = app.terminal_menu {
        let has_selection = session.selection.is_some();
        // Position the menu near the click using left/top padding. Clamp so it
        // stays on-screen-ish even near the right/bottom edges.
        let left = (mx + 14.0).clamp(0.0, 2000.0);
        let top = (my + 6.0).clamp(0.0, 2000.0);
        let menu = container(context_menu(has_selection))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Start)
            .align_y(Alignment::Start)
            .padding(iced::Padding { top, right: 0.0, bottom: 0.0, left });
        layers.push(menu.into());
    }

    if layers.len() == 1 {
        layers.pop().unwrap()
    } else {
        stack(layers).into()
    }
}

/// The terminal right-click context menu: Copy (when a selection exists),
/// Paste, Select All, Clear.
fn context_menu<'a>(has_selection: bool) -> Element<'a, Message> {
    let mut col = iced::widget::column![].spacing(1);
    if has_selection {
        col = col.push(menu_item("Copy", Message::TerminalCopy));
    }
    col = col
        .push(menu_item("Paste", Message::PasteRequested))
        .push(menu_item("Select All", Message::TerminalSelectAll))
        .push(menu_item("Clear", Message::ClearTerminal));

    container(col)
        .padding(4)
        .style(|_| container::Style {
            background: Some(theme::surface_2().into()),
            border: Border {
                color: theme::border_strong(),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn menu_item<'a>(label: &'a str, msg: Message) -> Element<'a, Message> {
    button(text(label).size(12).color(theme::text_high()))
        .width(Length::Fixed(140.0))
        .padding([5, 10])
        .on_press(msg)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(if hovered { theme::surface_3() } else { Color::TRANSPARENT }.into()),
                text_color: theme::text_high(),
                border: Border { radius: 5.0.into(), ..Default::default() },
                ..Default::default()
            }
        })
        .into()
}

fn search_bar(query: &str) -> Element<'_, Message> {
    let input = text_input("Search output…", query)
        .id(SEARCH_INPUT_ID.clone())
        .on_input(Message::TerminalSearchQuery)
        .on_submit(Message::TerminalSearchNext)
        .padding([7, 11])
        .size(14)
        .width(Length::Fixed(220.0))
        .style(|_, _| text_input::Style {
            background: theme::surface_2().into(),
            border: Border {
                color: theme::accent(),
                width: 1.0,
                radius: 8.0.into(),
            },
            icon: theme::text_muted(),
            placeholder: theme::text_dim(),
            value: theme::text_high(),
            selection: theme::accent_soft(),
        });

    let nav_btn = |glyph: &'static str, msg: Message| {
        button(text(glyph).size(15).color(theme::text_high()))
            .on_press(msg)
            .padding([4, 9])
            .style(|_, status| nav_button_style(status))
    };

    let bar = row![
        input,
        nav_btn("‹", Message::TerminalSearchPrev),
        nav_btn("›", Message::TerminalSearchNext),
        nav_btn("✕", Message::TerminalSearchClose),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    container(bar)
        .padding(8)
        .style(|_| container::Style {
            background: Some(theme::surface_1().into()),
            border: Border {
                color: theme::border_subtle(),
                width: 1.0,
                radius: 11.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn nav_button_style(status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => theme::surface_3(),
        _ => theme::surface_2(),
    };
    button::Style {
        background: Some(bg.into()),
        text_color: theme::text_high(),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 7.0.into(),
        },
        ..Default::default()
    }
}
