//! Subscriptions: one connection worker per session, plus keyboard input.

use iced::keyboard::{self, key, Key, Modifiers};
use iced::{event, window, Subscription};
use iced::advanced::input_method;

use crate::connection::{worker, Event as ConnEvent};
use crate::message::Message;
use crate::session::{Phase, SessionKind};
use crate::App;

pub fn subscription(app: &App) -> Subscription<Message> {
    // One worker per session. The worker lives as long as the session exists
    // (across connect/disconnect cycles), so reconnect reuses the same channel.
    let workers = app
        .sessions
        .iter()
        .map(|session| {
            if session.kind == SessionKind::Local {
                #[cfg(unix)]
                return Subscription::run_with(session.id, local_worker_builder).map(Message::Conn);
                #[cfg(not(unix))]
                return Subscription::run_with(session.id, worker_builder).map(Message::Conn);
            }
            Subscription::run_with(session.id, worker_builder).map(Message::Conn)
        });
    let connections = Subscription::batch(workers);

    // Always track modifier state so SFTP clicks know about Cmd/Ctrl/Shift.
    let modifiers = keyboard::listen().filter_map(|event| match event {
        keyboard::Event::ModifiersChanged(m) => Some(Message::ModifiersChanged(m)),
        _ => None,
    });

    // Keyboard -> terminal bytes, only when the active session is connected.
    let active_connected = app
        .active_session()
        .map(|s| s.phase == Phase::Connected)
        .unwrap_or(false);
    // DECCKM: when the remote app (vim, less, …) requests application cursor
    // keys, arrows/Home/End must be encoded as ESC O x, not ESC [ x. Read it
    // here so the (non-capturing) filter_map can pick the right sequence.
    let app_cursor = app
        .active_session()
        .map(|s| s.terminal.mouse_protocol().app_cursor)
        .unwrap_or(false);
    let palette_open = app.palette_open;

    // Vault auto-lock heartbeat: only needed while the vault is enabled.
    let vault_tick = if app.vault_enabled {
        iced::time::every(std::time::Duration::from_secs(60))
            .map(|_| Message::VaultCheckLock)
    } else {
        Subscription::none()
    };

    // Toast heartbeat: toast progress is wall-clock derived, so during the
    // static hold phase a slow tick (instead of full-rate frames) is enough
    // to retire finished toasts and hand control back to `frames` when the
    // fade-out window approaches (see `Toast::animating`).
    let toast_tick = if !app.toasts.is_empty() {
        iced::time::every(std::time::Duration::from_millis(250))
            .map(|_| Message::Tick(std::time::Instant::now()))
    } else {
        Subscription::none()
    };

    // While the vault overlay is up it gates everything: only Enter (submit)
    // and Esc (manual lock when already unlockable) reach the app, and no
    // keystrokes leak to the terminal.
    if app.vault_overlay_active() {
        let keys = keyboard::listen().filter_map(|event| {
            let keyboard::Event::KeyPressed { key, .. } = event else {
                return None;
            };
            match key.as_ref() {
                Key::Named(key::Named::Enter) => Some(Message::VaultSubmit),
                _ => None,
            }
        });
        return Subscription::batch([connections, modifiers, keys, vault_tick, toast_tick]);
    }

    // While the settings overlay is open, Esc closes it.
    if app.settings_open {
        let keys = keyboard::listen().filter_map(|event| {
            let keyboard::Event::KeyPressed { key, .. } = event else {
                return None;
            };
            match key.as_ref() {
                Key::Named(key::Named::Escape) => Some(Message::CloseSettings),
                _ => None,
            }
        });
        return Subscription::batch([connections, modifiers, keys, vault_tick, toast_tick]);
    }

    // While terminal search is open, Esc closes it and app shortcuts still
    // work; typing flows to the focused search box (Enter jumps to the next
    // match), not the terminal.
    if app.terminal_search.is_some() {
        let search_keys = keyboard::listen().filter_map(|event| {
            let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
                return None;
            };
            // App shortcuts (Cmd+F again, copy, etc.) still work.
            if let Some(message) = app_shortcut(&key, modifiers) {
                return Some(message);
            }
            match key.as_ref() {
                Key::Named(key::Named::Escape) => Some(Message::TerminalSearchClose),
                _ => None,
            }
        });
        let frames = if app.any_animating() {
            window::frames().map(Message::Tick)
        } else {
            Subscription::none()
        };
        let metrics = if active_connected {
            let period = if app.rail_visible() { 2 } else { 10 };
            iced::time::every(std::time::Duration::from_secs(period)).map(|_| Message::MetricsTick)
        } else {
            Subscription::none()
        };
        return Subscription::batch([connections, modifiers, search_keys, frames, metrics, vault_tick, toast_tick]);
    }

    // While the palette is open it owns the keyboard (nav + run + close).
    if palette_open {
        let palette_keys = keyboard::listen().filter_map(|event| {
            let keyboard::Event::KeyPressed { key, .. } = event else {
                return None;
            };
            match key.as_ref() {
                Key::Named(key::Named::ArrowDown) => Some(Message::PaletteMove(1)),
                Key::Named(key::Named::ArrowUp) => Some(Message::PaletteMove(-1)),
                Key::Named(key::Named::Enter) => Some(Message::PaletteRunSelected),
                Key::Named(key::Named::Escape) => Some(Message::ClosePalette),
                _ => None,
            }
        });
    let frames = if app.any_animating() {
        window::frames().map(Message::Tick)
    } else {
        Subscription::none()
    };
    return Subscription::batch([connections, modifiers, palette_keys, frames, vault_tick, toast_tick]);
    }

    // The Ctrl+R history-search overlay owns the keyboard while open: nav + run
    // + close. Typing flows to its focused search box via on_input.
    if app.history_search_open {
        let keys = keyboard::listen().filter_map(|event| {
            let keyboard::Event::KeyPressed { key, .. } = event else {
                return None;
            };
            match key.as_ref() {
                Key::Named(key::Named::ArrowDown) => Some(Message::HistorySearchMove(1)),
                Key::Named(key::Named::ArrowUp) => Some(Message::HistorySearchMove(-1)),
                Key::Named(key::Named::Enter) => Some(Message::HistorySearchAccept),
                Key::Named(key::Named::Escape) => Some(Message::HistorySearchClose),
                _ => None,
            }
        });
        let frames = if app.any_animating() {
            window::frames().map(Message::Tick)
        } else {
            Subscription::none()
        };
        return Subscription::batch([connections, modifiers, keys, frames, vault_tick, toast_tick]);
    }

    // `with` carries the connected + app-cursor flags into the (non-capturing)
    // filter_map so the terminal-byte encoder can pick the right arrow sequence.
    let typing = keyboard::listen()
        .with((active_connected, app_cursor))
        .filter_map(|((connected, app_cursor), event)| {
            let keyboard::Event::KeyPressed {
                key,
                modifiers,
                text,
                repeat: _,
                ..
            } = event
            else {
                return None;
            };
            // App shortcuts take precedence and are not forwarded as bytes.
            if let Some(message) = app_shortcut(&key, modifiers) {
                return Some(message);
            }
            if connected {
                // Ctrl+R opens the history-search overlay instead of the shell's
                // own reverse-search (Sprint 3). Intercepted before encode_key
                // would turn it into 0x12.
                if modifiers.control() && !modifiers.shift() && !modifiers.alt() {
                    if let Key::Character(v) = key.as_ref() {
                        if v.eq_ignore_ascii_case("r") {
                            return Some(Message::HistorySearchOpen);
                        }
                    }
                }
                crate::keys::encode_key(key, modifiers, text.as_deref(), app_cursor)
                    .map(Message::TerminalInput)
            } else {
                None
            }
        });

    let resize = window::resize_events().map(|(_id, size)| Message::WindowResized(size));

    // Poll remote resource metrics while the active session is connected:
    // every 2s when the resource rail is visible (it drives the charts), and
    // at a relaxed 10s when the rail is collapsed — then only the footer's
    // coarse throughput label consumes samples. Zero overhead when
    // disconnected.
    let metrics = if active_connected {
        let period = if app.rail_visible() { 2 } else { 10 };
        iced::time::every(std::time::Duration::from_secs(period)).map(|_| Message::MetricsTick)
    } else {
        Subscription::none()
    };

    // Animation frames — only subscribed while an animation is in flight, so
    // idle CPU stays at zero.
    let frames = if app.any_animating() {
        window::frames().map(Message::Tick)
    } else {
        Subscription::none()
    };

    // Ping saved hosts every 30s to show latency in the sidebar.
    let ping_tick = iced::time::every(std::time::Duration::from_secs(30))
        .map(|_| Message::PingTick);

    // Connecting-dot pulse: slow heartbeat while any session is handshaking.
    let pulse = if app.sessions.iter().any(|s| s.phase == Phase::Connecting) {
        iced::time::every(std::time::Duration::from_millis(700)).map(|_| Message::PulseTick)
    } else {
        Subscription::none()
    };

    // IME commit events (Chinese/Japanese/Korean input confirmed).
    let ime = if active_connected {
        event::listen_with(|ev, _status, _id| {
            if let iced::Event::InputMethod(input_method::Event::Commit(text)) = ev {
                Some(Message::TerminalInput(text.into_bytes()))
            } else {
                None
            }
        })
    } else {
        Subscription::none()
    };

    Subscription::batch([connections, modifiers, typing, resize, metrics, frames, ping_tick, pulse, ime, vault_tick, toast_tick])
}

/// Builder for `Subscription::run_with` — must be a non-capturing fn pointer.
fn worker_builder(session_id: &u64) -> impl iced::futures::Stream<Item = ConnEvent> {
    worker(*session_id)
}

#[cfg(unix)]
fn local_worker_builder(session_id: &u64) -> impl iced::futures::Stream<Item = ConnEvent> {
    crate::connection::local_worker(*session_id)
}

/// App-level keyboard shortcuts (Cmd/Ctrl based).
fn app_shortcut(key: &Key, modifiers: Modifiers) -> Option<Message> {
    let cmd = modifiers.command() || modifiers.logo();
    if !cmd {
        return None;
    }
    match key.as_ref() {
        Key::Character(v) if v.eq_ignore_ascii_case("t") => Some(Message::NewTab),
        Key::Character(v) if v.eq_ignore_ascii_case("w") => Some(Message::CloseTab(usize::MAX)),
        Key::Character(v) if v.eq_ignore_ascii_case("k") => Some(Message::TogglePalette),
        Key::Character(v) if v.eq_ignore_ascii_case("f") => Some(Message::TerminalSearchOpen),
        Key::Character(v) if v.eq_ignore_ascii_case("c") => Some(Message::TerminalCopy),
        Key::Character(v) if v.eq_ignore_ascii_case("a") => Some(Message::TerminalSelectAll),
        Key::Character(v) if v.eq_ignore_ascii_case("v") => Some(Message::PasteRequested),
        Key::Character(v) if v.eq_ignore_ascii_case("b") => Some(Message::ToggleSidebar),
        Key::Character(v) if v == "," => Some(Message::OpenSettings),
        // Cmd+1..=9 jumps to that tab (1-based → 0-based index).
        Key::Character(v) if matches!(v.as_ref(), "1"|"2"|"3"|"4"|"5"|"6"|"7"|"8"|"9") => {
            let n = v.parse::<usize>().unwrap_or(1).saturating_sub(1);
            Some(Message::SelectTab(n))
        }
        Key::Character(v) if v == "+" || v == "=" => Some(Message::FontSizeDelta(1)),
        Key::Character(v) if v == "-" => Some(Message::FontSizeDelta(-1)),
        Key::Named(key::Named::Enter) => None,
        _ => None,
    }
}
