//! View layer. The workspace is terminal-first: a hosts sidebar on the left,
//! horizontal session tabs across the top, and the terminal (or a connection
//! card when idle) filling the rest. Overlays (host-key confirm, palette) float
//! on top.

mod connect_card;
mod file_viewer;
mod footer;
mod history;
pub mod history_search;
mod hostkey;
mod ime;
mod monitor;
mod palette;
mod settings;
mod sftp;
mod sidebar;
mod tabs;
pub mod terminal;
pub mod toasts;
pub mod vault;
mod widgets;

use iced::widget::{column, container, mouse_area, row, stack};
use iced::{Border, Color, Element, Length};

use crate::message::Message;
use crate::session::Phase;
use crate::theme;
use crate::App;

// Layout constants shared with `terminal_area` in main.rs so the grid size
// matches the painted area exactly.
pub const WORKSPACE_H_PADDING: f32 = 0.0;
pub const TERMINAL_V_PADDING: f32 = 10.0;
pub const FOOTER_HEIGHT: f32 = 26.0;
pub const HISTORY_DIVIDER_WIDTH: f32 = 6.0;
/// Height of the Terminal | Files sub-tab bar (shown when connected).
pub const SUBTAB_HEIGHT: f32 = 40.0;
/// Width of the always-on resource rail (when shown).
pub const RAIL_WIDTH: f32 = 250.0;

pub fn view(app: &App) -> Element<'_, Message> {
    let workspace = column![tabs::view(app), workspace_body(app), footer::view(app),]
        .width(Length::Fill)
        .height(Length::Fill);

    // The sidebar is collapsible. When expanded it sits left of a thin draggable
    // divider; when collapsed it disappears entirely and an expand affordance
    // lives in the tab bar. The resource rail docks at the far right whenever the
    // active session is connected (unless the user collapsed it).
    let mut main = if app.sidebar_collapsed() && !app.sidebar_animating() {
        row![workspace]
    } else {
        row![sidebar::view(app), sidebar::divider(), workspace]
    };
    if app.rail_rendered() {
        main = main.push(monitor::view(app));
    }
    let base = main.width(Length::Fill).height(Length::Fill);

    let base = container(base)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(theme::surface_0().into()),
            ..Default::default()
        });

    // Overlays.
    let mut layers = stack![base];

    if let Some(session) = app.active_session() {
        if session.host_key.is_some() {
            layers = layers.push(hostkey::view(app));
        }
    }
    if app.palette_open {
        layers = layers.push(palette::view(app));
    }
    if app.history_search_open {
        layers = layers.push(history_search::view(app));
    }
    if app.settings_open {
        layers = layers.push(settings::view(app));
    }
    if app.sftp_prompt_active() {
        layers = layers.push(sftp::prompt_overlay(app));
    }
    if app.sftp_confirm().is_some() {
        layers = layers.push(sftp::confirm_delete_overlay(app));
    }
    if app.pending_host_delete().is_some() {
        layers = layers.push(sidebar::delete_host_overlay(app));
    }

    // The vault overlay sits above everything else: it gates credential access.
    if app.vault_overlay_active() {
        layers = layers.push(vault::view(app));
    }

    // While dragging the history divider, a full-window transparent capture
    // layer keeps mouse-move events flowing even as the cursor leaves the thin
    // divider (mouse_area only reports moves while hovered).
    if app.history_dragging {
        let overlay: Element<'_, Message> = mouse_area(
            container(iced::widget::Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .interaction(iced::mouse::Interaction::ResizingHorizontally)
        .on_move(Message::HistoryDragMove)
        .on_release(Message::HistoryDragEnd)
        .into();
        layers = layers.push(overlay);
    }

    // Same capture trick for the sidebar resize divider.
    if app.sidebar_dragging() {
        let overlay: Element<'_, Message> = mouse_area(
            container(iced::widget::Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .interaction(iced::mouse::Interaction::ResizingHorizontally)
        .on_move(Message::SidebarDragMove)
        .on_release(Message::SidebarDragEnd)
        .into();
        layers = layers.push(overlay);
    }

    // Same capture trick for the rail resize divider.
    if app.rail_dragging() {
        let overlay: Element<'_, Message> = mouse_area(
            container(iced::widget::Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .interaction(iced::mouse::Interaction::ResizingHorizontally)
        .on_move(Message::RailDragMove)
        .on_release(Message::RailDragEnd)
        .into();
        layers = layers.push(overlay);
    }

    // Toast notifications float above everything (but below the modal vault
    // gate, which is pushed last). Only present when there are active toasts.
    if !app.toasts.is_empty() {
        layers = layers.push(toasts::view(&app.toasts, app.now()));
    }

    layers.width(Length::Fill).height(Length::Fill).into()
}

/// The main body: terminal when connected/active, else the connection card.
/// When SFTP is open it takes the whole workspace; otherwise the terminal can
/// share the width with the command-history panel.
fn workspace_body(app: &App) -> Element<'_, Message> {
    let Some(session) = app.active_session() else {
        return container(iced::widget::Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    };

    let show_terminal = matches!(session.phase, Phase::Connected | Phase::Connecting)
        || session.terminal_has_content();
    let connected = session.phase == Phase::Connected;

    // When SFTP is open on a connected session, it takes the whole workspace.
    // The terminal grid is preserved underneath and returns intact when toggled
    // off. (Resource metrics live in the always-on right rail, not here.)
    let inner: Element<'_, Message> = if session.sftp_open && connected {
        sftp::view(app)
    } else if show_terminal {
        // Optionally split: terminal | divider | command-history panel.
        if app.history_open || app.history_animating() {
            row![
                container(terminal::view(app)).width(Length::Fill),
                history::divider(),
                history::view(app),
            ]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        } else {
            terminal::view(app)
        }
    } else {
        connect_card::view(app)
    };

    // A connected SSH session gets an explicit Terminal | Files switcher so
    // toggling views never spawns a new SSH session (the old failure mode).
    // Local sessions only have a terminal — no remote SFTP.
    let body: Element<'_, Message> = if connected && session.kind == crate::session::SessionKind::Ssh {
        column![sub_tabs(session.sftp_open), inner]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        inner
    };

    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(theme::surface_0().into()),
            ..Default::default()
        })
        .into()
}

/// The Terminal | Files switcher shown above a connected session. (Resource
/// metrics live in the always-on right rail, not as a tab.)
fn sub_tabs(files_active: bool) -> Element<'static, Message> {
    use iced::widget::{button, text};
    let terminal_active = !files_active;
    let tab = |label: &str, active: bool, msg: Message| {
        button(text(label.to_string()).size(13).color(if active {
            theme::text_high()
        } else {
            theme::text_muted()
        }))
        .padding([6, 16])
        .on_press(msg)
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
                        theme::accent()
                    } else {
                        Color::TRANSPARENT
                    },
                    width: 0.0,
                    radius: 7.0.into(),
                },
                ..Default::default()
            }
        })
    };

    container(
        row![
            tab("Terminal", terminal_active, Message::ShowFiles(false)),
            tab("Files", files_active, Message::ShowFiles(true)),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(SUBTAB_HEIGHT))
    .padding([0, 12])
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
