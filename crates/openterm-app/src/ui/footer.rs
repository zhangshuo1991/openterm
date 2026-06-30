//! Footer status bar: connection status, target, and quick actions.

use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::widget::{button, canvas as canvas_widget, container, row, text, Space};
use iced::{mouse, Border, Color, Element, Length, Point, Rectangle, Renderer, Theme};

use crate::message::Message;
use crate::session::Phase;
use crate::theme;
use crate::ui::widgets;
use crate::App;

pub fn view(app: &App) -> Element<'_, Message> {
    let (status, phase_label, target) = match app.active_session() {
        Some(session) => (
            session.status.clone(),
            session.phase.clone(),
            session.config.target_label(),
        ),
        None => (app.status.clone(), Phase::Idle, String::new()),
    };

    let connected = phase_label == Phase::Connected;
    let has_host = app
        .active_session()
        .is_some_and(|s| !s.config.host.trim().is_empty());
    // Show Reconnect when disconnected (idle or failed) but a host is already set.
    let disconnected = !phase_label.is_active() && has_host;

    let pill_color = widgets::phase_color(&phase_label);
    let pill = container(
        row![
            widgets::status_dot(&phase_label, app.connecting_pulse),
            text(phase_word(&phase_label)).size(11).color(pill_color),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    )
    .padding([3, 9])
    .style(move |_| container::Style {
        background: Some(theme::surface_2().into()),
        border: Border {
            color: theme::with_alpha(pill_color, 0.30),
            width: 1.0,
            radius: 7.0.into(),
        },
        ..Default::default()
    });

    let mut left = row![pill].spacing(10).align_y(iced::Alignment::Center);
    if !target.is_empty() {
        left = left.push(text(target).size(12).color(theme::text_high()));
    }

    // Live telemetry shown only while connected: throughput, uptime, latency.
    if connected {
        if let Some(session) = app.active_session() {
            if let Some(m) = &session.metrics {
                if m.has_rates {
                    left = left.push(throughput_label(m.net_rx_bps, m.net_tx_bps));
                }
            }
            if let Some(since) = session.connected_at {
                let secs = std::time::Instant::now()
                    .saturating_duration_since(since)
                    .as_secs();
                left = left.push(
                    text(fmt_duration(secs))
                        .size(12)
                        .color(theme::text_muted()),
                );
            }
            // Latency sparkline for the connected host (when we have samples).
            if let Some(hist) = session
                .config
                .host_id
                .and_then(|id| app.ping_history.get(&id))
            {
                let pts: Vec<f32> = hist.iter().map(|&v| v as f32).collect();
                if pts.len() >= 2 {
                    let last = hist.back().copied().unwrap_or(0);
                    left = left.push(
                        row![
                            canvas_widget(LatencySparkline { points: pts })
                                .width(Length::Fixed(54.0))
                                .height(Length::Fixed(14.0)),
                            text(format!("{last}ms")).size(11).color(theme::text_dim()),
                        ]
                        .spacing(5)
                        .align_y(iced::Alignment::Center),
                    );
                }
            }
        }
    }

    if !status.is_empty() {
        left = left.push(text(status).size(12).color(theme::text_muted()));
    }
    if !connected {
        left = left.push(Space::new());
    }

    let mut actions = row![].spacing(6).align_y(iced::Alignment::Center);
    if disconnected {
        // Accent-colored button so it's immediately obvious when disconnected.
        actions = actions.push(accent_button("Reconnect", Message::Connect));
    }
    if connected {
        actions = actions
            .push(footer_button("History", Message::ToggleHistory))
            .push(footer_button("Files", Message::ToggleSftp))
            .push(footer_button("Monitor", Message::ToggleMonitor))
            .push(footer_button("Disconnect", Message::Disconnect));
    }
    if has_host {
        actions = actions.push(footer_button("Duplicate", Message::DuplicateTab));
    }
    actions = actions
        .push(footer_button("Settings", Message::OpenSettings))
        .push(footer_button(&format!("{}px", app.font_size), Message::OpenSettings));

    container(
        row![left, Space::new().width(Length::Fill), actions]
            .align_y(iced::Alignment::Center)
            .padding([0, 14]),
    )
    .width(Length::Fill)
    .height(Length::Fixed(crate::ui::FOOTER_HEIGHT))
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

fn phase_word(phase: &Phase) -> &'static str {
    match phase {
        Phase::Connected => "Connected",
        Phase::Connecting => "Connecting",
        Phase::Failed(_) => "Disconnected",
        Phase::Idle => "Idle",
    }
}

fn footer_button<'a>(label: &str, on_press: Message) -> Element<'a, Message> {
    button(text(label.to_string()).size(11).color(theme::text_muted()))
        .padding([3, 9])
        .on_press(on_press)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(if hovered { theme::surface_3() } else { Color::TRANSPARENT }.into()),
                text_color: theme::text_high(),
                border: Border { radius: 6.0.into(), ..Default::default() },
                ..Default::default()
            }
        })
        .into()
}

/// An accent-colored button for high-priority actions (e.g. Reconnect).
fn accent_button<'a>(label: &str, on_press: Message) -> Element<'a, Message> {
    button(text(label.to_string()).size(11).color(theme::surface_0()))
        .padding([3, 9])
        .on_press(on_press)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(if hovered { theme::accent_strong() } else { theme::accent() }.into()),
                text_color: theme::surface_0(),
                border: Border { radius: 6.0.into(), ..Default::default() },
                ..Default::default()
            }
        })
        .into()
}

/// "↓2.4K/s ↑0.1K/s" throughput chip from the latest network rates.
fn throughput_label<'a>(rx_bps: f64, tx_bps: f64) -> Element<'a, Message> {
    text(format!("↓{} ↑{}", fmt_rate(rx_bps), fmt_rate(tx_bps)))
        .size(11)
        .color(theme::text_muted())
        .into()
}

/// Bytes/s → compact "2.4K/s", "12M/s".
fn fmt_rate(bps: f64) -> String {
    const UNITS: [&str; 4] = ["B", "K", "M", "G"];
    let mut v = bps.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0}{}/s", UNITS[i])
    } else if v < 10.0 {
        format!("{v:.1}{}/s", UNITS[i])
    } else {
        format!("{v:.0}{}/s", UNITS[i])
    }
}

/// Seconds → "MM:SS" or "H:MM:SS".
fn fmt_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// A tiny line chart of recent ping samples, drawn in the footer.
struct LatencySparkline {
    points: Vec<f32>,
}

impl canvas::Program<Message> for LatencySparkline {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let h = bounds.height;
        if self.points.len() < 2 {
            return vec![frame.into_geometry()];
        }
        // Adaptive scale across the window's own range (small floor so a flat
        // line doesn't amplify noise).
        let (lo, hi) = self
            .points
            .iter()
            .copied()
            .fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let span = (hi - lo).max(5.0);
        let n = self.points.len();
        let dx = w / (n as f32 - 1.0);
        let y_for = |v: f32| h - ((v - lo) / span).clamp(0.0, 1.0) * (h - 2.0) - 1.0;

        let mut points = Vec::with_capacity(n);
        for (i, &v) in self.points.iter().enumerate() {
            points.push(Point::new(i as f32 * dx, y_for(v)));
        }
        let line = Path::new(|b| {
            b.move_to(points[0]);
            for p in &points[1..] {
                b.line_to(*p);
            }
        });
        frame.stroke(
            &line,
            Stroke::default()
                .with_color(theme::accent())
                .with_width(1.0),
        );
        vec![frame.into_geometry()]
    }
}
