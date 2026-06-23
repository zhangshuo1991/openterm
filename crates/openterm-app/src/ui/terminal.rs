//! Terminal workspace: the active session's grid painted on a canvas that
//! fills the available area. The grid is sized to this area (see
//! `terminal_render::grid_for_viewport` + `terminal_area`), so it never
//! overflows vertically.

use iced::widget::{canvas, container, mouse_area};
use iced::{mouse, Element, Length};
use openterm_terminal::TerminalEngine;

use crate::message::Message;
use crate::terminal_render::TerminalCanvas;
use crate::theme;
use crate::App;

pub fn view(app: &App) -> Element<'_, Message> {
    let Some(session) = app.active_session() else {
        return container(iced::widget::Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    };

    let program = TerminalCanvas {
        snapshot: session.terminal.snapshot(),
        font_size: app.font_size,
        selection: session.selection,
    };

    let surface = canvas(program).width(Length::Fill).height(Length::Fill);

    let scrollable_surface = mouse_area(surface).on_scroll(|delta| {
        let lines = match delta {
            mouse::ScrollDelta::Lines { y, .. } => y,
            mouse::ScrollDelta::Pixels { y, .. } => y / 16.0,
        };
        Message::TerminalScroll(lines)
    });

    // Wrap in ImeEnabled so winit activates the OS input method (Chinese/Japanese/Korean).
    let ime_surface: Element<'_, Message> = super::ime::ImeEnabled::new(scrollable_surface).into();

    container(ime_surface)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding([crate::ui::TERMINAL_V_PADDING as u16, 14])
        .style(|_| container::Style {
            background: Some(theme::surface_0().into()),
            ..Default::default()
        })
        .into()
}
