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
    let palette_open = app.palette_open;

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
        return Subscription::batch([connections, modifiers, keys]);
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
        let frames = if app.card_animating() {
            window::frames().map(Message::Tick)
        } else {
            Subscription::none()
        };
        return Subscription::batch([connections, modifiers, palette_keys, frames]);
    }

    // `with` carries the connected flag into the (non-capturing) filter_map.
    let typing = keyboard::listen()
        .with(active_connected)
        .filter_map(|(connected, event)| {
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
                crate::keys::encode_key(key, modifiers, text.as_deref()).map(Message::TerminalInput)
            } else {
                None
            }
        });

    let resize = window::resize_events().map(|(_id, size)| Message::WindowResized(size));

    // Poll remote resource metrics every 2s whenever the active session is
    // connected — the resource rail is always shown, so this drives it. Zero
    // overhead when disconnected.
    let metrics = if active_connected {
        iced::time::every(std::time::Duration::from_secs(2)).map(|_| Message::MetricsTick)
    } else {
        Subscription::none()
    };

    // Animation frames — only subscribed while an animation is in flight, so
    // idle CPU stays at zero.
    let frames = if app.card_animating() {
        window::frames().map(Message::Tick)
    } else {
        Subscription::none()
    };

    // Ping saved hosts every 30s to show latency in the sidebar.
    let ping_tick = iced::time::every(std::time::Duration::from_secs(30))
        .map(|_| Message::PingTick);

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

    Subscription::batch([connections, modifiers, typing, resize, metrics, frames, ping_tick, ime])
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
        Key::Character(v) if v.eq_ignore_ascii_case("c") => Some(Message::TerminalCopy),
        Key::Character(v) if v.eq_ignore_ascii_case("v") => Some(Message::PasteRequested),
        Key::Character(v) if v == "+" || v == "=" => Some(Message::FontSizeDelta(1)),
        Key::Character(v) if v == "-" => Some(Message::FontSizeDelta(-1)),
        Key::Named(key::Named::Enter) => None,
        _ => None,
    }
}
