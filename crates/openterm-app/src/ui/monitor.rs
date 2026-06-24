//! Resource rail — redesigned with per-metric colors, big numbers, taller
//! charts, a summary row, and a footer grid.

use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::widget::{button, column, container, mouse_area, row, scrollable, text, Space};
use iced::{Border, Color, Element, Length, Point, Rectangle, Renderer, Theme};

use crate::message::Message;
use crate::metrics::{PortInfo, ProcessInfo, SessionMetrics};
use crate::session::{MonitorPanel, Session};
use crate::theme;
use crate::ui::RAIL_WIDTH;
use crate::App;

// ── Per-metric accent colors ─────────────────────────────────────────────────
const CPU_COLOR: Color = Color::from_rgb(0.22, 0.65, 0.40);   // green
const MEM_COLOR: Color = Color::from_rgb(0.82, 0.52, 0.25);   // amber
const NET_COLOR: Color = Color::from_rgb(0.32, 0.52, 0.82);   // blue
const DISK_IO_COLOR: Color = Color::from_rgb(0.52, 0.47, 0.82); // purple
const DISK_COLOR: Color = Color::from_rgb(0.82, 0.52, 0.25);  // amber (same as mem)
const SWAP_COLOR: Color = Color::from_rgb(0.52, 0.52, 0.52);  // gray

// ── Top-level view ────────────────────────────────────────────────────────────

pub fn view(app: &App) -> Element<'_, Message> {
    let Some(session) = app.active_session() else {
        return container(Space::new())
            .width(Length::Fixed(RAIL_WIDTH))
            .into();
    };

    let body: Element<'_, Message> = match &session.metrics {
        None => text("Collecting metrics…")
            .size(12)
            .color(theme::text_muted())
            .into(),
        Some(m) => {
            let uptime_badge = container(
                text(format!("↑ {}", fmt_uptime(m.uptime_secs)))
                    .size(10)
                    .color(theme::text_high()),
            )
            .padding([3, 8])
            .style(|_| container::Style {
                background: Some(theme::surface_2().into()),
                border: Border { radius: 6.0.into(), color: theme::border_subtle(), width: 1.0 },
                ..Default::default()
            });

            let header = row![
                text("MONITOR").size(11).color(theme::text_dim()).width(Length::Fill),
                uptime_badge,
                hide_button(),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center);

            let blocks = column![
                header,
                divider(),
                pct_metric("CPU", m.cpu_percent, m.has_rates, &session.cpu_history, CPU_COLOR,
                    format!("{} cores", m.per_core.len().max(1))),
                divider(),
                pct_metric("Memory", m.mem_percent, true, &session.mem_history, MEM_COLOR,
                    format!("{} / {}", fmt_kb(m.mem_used_kb), fmt_kb(m.mem_total_kb))),
                divider(),
                rate_metric("Network", NET_COLOR,
                    format!("↓ {}", rate_str(m.net_rx_bps, m.has_rates)),
                    format!("↑ {}", rate_str(m.net_tx_bps, m.has_rates)),
                    &session.net_history),
                divider(),
                rate_metric("Disk IO", DISK_IO_COLOR,
                    format!("R {}", rate_str(m.disk_read_bps, m.has_rates)),
                    format!("W {}", rate_str(m.disk_write_bps, m.has_rates)),
                    &session.diskio_history),
                divider(),
                bar_metric("Disk (/)", m.disk_percent, m.disk_total_kb > 0, DISK_COLOR,
                    format!("{} / {}", fmt_kb(m.disk_used_kb), fmt_kb(m.disk_total_kb))),
                divider(),
                bar_metric("Swap", m.swap_percent, m.swap_total_kb > 0, SWAP_COLOR,
                    if m.swap_total_kb > 0 {
                        format!("{} / {}", fmt_kb(m.swap_used_kb), fmt_kb(m.swap_total_kb))
                    } else {
                        "none".to_string()
                    }),
                divider(),
                footer_row(m),
                divider(),
                process_section(session),
                divider(),
                ports_section(session),
            ]
            .spacing(10)
            .padding([14, 14]);
            scrollable(blocks).height(Length::Fill).into()
        }
    };

    let rail_w = app.rail_width_value();
    let content = container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(theme::surface_1().into()),
            border: Border { color: theme::border_subtle(), width: 1.0, radius: 0.0.into() },
            ..Default::default()
        });

    // Left-edge drag divider (6px wide, ResizingHorizontally cursor).
    let divider = mouse_area(
        container(Space::new())
            .width(Length::Fixed(6.0))
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(theme::border_subtle().into()),
                ..Default::default()
            }),
    )
    .interaction(iced::mouse::Interaction::ResizingHorizontally)
    .on_press(Message::RailDragStart)
    .on_release(Message::RailDragEnd);

    row![divider, content]
        .width(Length::Fixed(rail_w))
        .height(Length::Fill)
        .into()
}

// ── Summary row ───────────────────────────────────────────────────────────────

fn summary_card<'a>(
    label: &'a str,
    pct: f32,
    available: bool,
    color: Color,
    sub: String,
) -> Element<'a, Message> {
    let value_color = if available { metric_color(pct, color) } else { theme::text_dim() };
    container(
        column![
            text(label).size(10).color(theme::text_dim()),
            if available {
                text(format!("{pct:.0}%")).size(22).color(value_color)
            } else {
                text("—").size(22).color(theme::text_dim())
            },
            text(sub).size(10).color(theme::text_muted()),
        ]
        .spacing(2),
    )
    .padding([8, 10])
    .width(Length::FillPortion(1))
    .style(move |_| container::Style {
        background: Some(theme::surface_2().into()),
        border: Border { radius: 8.0.into(), color: theme::border_subtle(), width: 1.0 },
        ..Default::default()
    })
    .into()
}

// ── Metric blocks ─────────────────────────────────────────────────────────────

/// CPU / Memory: colored dot + label + big %, progress bar, sparkline, subtitle.
fn pct_metric<'a>(
    title: &'a str,
    pct: f32,
    available: bool,
    history: &std::collections::VecDeque<f32>,
    color: Color,
    subtitle: String,
) -> Element<'a, Message> {
    let pct = pct.clamp(0.0, 100.0);
    let value_color = metric_color(pct, color);
    let head = row![
        dot(color),
        text(title).size(12).color(theme::text_high()).width(Length::Fill),
        column![
            if available {
                text(format!("{pct:.0}%")).size(16).color(value_color)
            } else {
                text("—").size(16).color(theme::text_dim())
            },
            text(subtitle.clone()).size(10).color(theme::text_muted()),
        ]
        .spacing(0)
        .align_x(iced::Alignment::End),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    column![
        head,
        meter(pct, available, color),
        chart(history, color, 100.0, 44.0),
    ]
    .spacing(6)
    .into()
}

/// Network / Disk IO: colored dot + label + two rate values, sparkline.
fn rate_metric<'a>(
    title: &'a str,
    color: Color,
    v1: String,
    v2: String,
    history: &std::collections::VecDeque<f32>,
) -> Element<'a, Message> {
    let head = row![
        dot(color),
        text(title).size(12).color(theme::text_high()).width(Length::Fill),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let rates = row![
        text(v1).font(theme::TERMINAL_FONT).size(11).color(color).width(Length::FillPortion(1)),
        text(v2).font(theme::TERMINAL_FONT).size(11).color(theme::with_alpha(color, 0.65)).width(Length::FillPortion(1)),
    ]
    .spacing(4);

    column![head, rates, chart(history, color, 0.0, 44.0)]
        .spacing(6)
        .into()
}

/// Disk / Swap: colored dot + label + % + progress bar + subtitle, no sparkline.
fn bar_metric<'a>(
    title: &'a str,
    pct: f32,
    available: bool,
    color: Color,
    subtitle: String,
) -> Element<'a, Message> {
    let pct = pct.clamp(0.0, 100.0);
    let value_color = metric_color(pct, color);
    let head = row![
        dot(color),
        text(title).size(12).color(theme::text_high()).width(Length::Fill),
        column![
            if available {
                text(format!("{pct:.0}%")).size(14).color(value_color)
            } else {
                text("—").size(14).color(theme::text_dim())
            },
            text(subtitle).size(10).color(theme::text_muted()),
        ]
        .spacing(0)
        .align_x(iced::Alignment::End),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    column![head, meter(pct, available, color)]
        .spacing(6)
        .into()
}

/// Footer: Load / Tasks / Uptime in three equal columns.
fn footer_row(m: &SessionMetrics) -> Element<'_, Message> {
    let cell = |label: &str, value: String| -> Element<'_, Message> {
        container(
            column![
                text(label.to_string()).size(10).color(theme::text_dim()),
                text(value).size(13).color(theme::text_high()),
            ]
            .spacing(2)
            .align_x(iced::Alignment::Center),
        )
        .width(Length::FillPortion(1))
        .center_x(Length::Fill)
        .into()
    };
    row![
        cell("Load", format!("{:.2}", m.load1)),
        cell("Tasks", format!("{} {}", m.tasks_total, if m.tasks_running > 1 { format!("{} run", m.tasks_running) } else { String::new() })),
        cell("Uptime", fmt_uptime(m.uptime_secs)),
    ]
    .into()
}

// ── Process expander ──────────────────────────────────────────────────────────

const MAX_ROWS: usize = 8;

fn process_section(session: &Session) -> Element<'_, Message> {
    let open = session.monitor_panel.is_some();
    let panel = session.monitor_panel.unwrap_or(MonitorPanel::Cpu);

    let header_row = row![
        text("TOP PROCESSES").size(10).color(theme::text_dim()).width(Length::Fill),
        sort_toggle("CPU", panel == MonitorPanel::Cpu, MonitorPanel::Cpu),
        sort_toggle("MEM", panel == MonitorPanel::Memory, MonitorPanel::Memory),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let toggle_btn = button(header_row)
        .width(Length::Fill)
        .padding([5, 0])
        .on_press(if open {
            Message::MonitorCloseDetail
        } else {
            Message::MonitorSelect(MonitorPanel::Cpu)
        })
        .style(|_, _| button::Style {
            background: Some(Color::TRANSPARENT.into()),
            ..Default::default()
        });

    if !open {
        return toggle_btn.into();
    }

    let mut list = column![].spacing(2);
    if session.processes.is_empty() {
        list = list.push(text("unavailable").size(10).color(theme::text_dim()));
    } else {
        for p in session.processes.iter().take(MAX_ROWS) {
            list = list.push(process_row(p, panel));
        }
    }

    column![toggle_btn, list].spacing(6).into()
}

fn sort_toggle(label: &str, active: bool, panel: MonitorPanel) -> Element<'_, Message> {
    button(
        text(label.to_string())
            .size(10)
            .color(if active { Color::WHITE } else { theme::text_muted() }),
    )
    .padding([4, 10])
    .on_press(Message::MonitorSelect(panel))
    .style(move |_, status| {
        let hovered = matches!(status, button::Status::Hovered);
        button::Style {
            background: Some(
                if active { theme::accent_strong() }
                else if hovered { theme::surface_3() }
                else { theme::surface_2() }
                .into(),
            ),
            text_color: if active { Color::WHITE } else { theme::text_high() },
            border: Border { radius: 6.0.into(), ..Default::default() },
            ..Default::default()
        }
    })
    .into()
}

fn process_row(p: &ProcessInfo, panel: MonitorPanel) -> Element<'_, Message> {
    let value = match panel {
        MonitorPanel::Cpu => p.cpu,
        MonitorPanel::Memory => p.mem,
    };
    let name = if p.command.chars().count() > 14 {
        format!("{}…", p.command.chars().take(13).collect::<String>())
    } else {
        p.command.clone()
    };
    row![
        text(format!("{}", p.pid)).font(theme::TERMINAL_FONT).size(10)
            .color(theme::text_dim()).width(Length::Fixed(44.0)),
        text(name).font(theme::TERMINAL_FONT).size(11)
            .color(theme::text_high()).width(Length::Fill),
        text(format!("{value:.1}%")).font(theme::TERMINAL_FONT).size(11)
            .color(theme::accent_strong()),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center)
    .into()
}

// ── Ports section ─────────────────────────────────────────────────────────────

const MAX_PORTS: usize = 12;

fn ports_section(session: &Session) -> Element<'_, Message> {
    let header = text("LISTENING PORTS").size(10).color(theme::text_dim());
    let mut list = column![header].spacing(2);
    if session.ports.is_empty() || session.monitor_panel.is_none() {
        return list.into();
    }
    for p in session.ports.iter().take(MAX_PORTS) {
        list = list.push(port_row(p));
    }
    list.into()
}

fn port_row(p: &PortInfo) -> Element<'_, Message> {
    let proto_color = if p.proto == "udp" { theme::text_muted() } else { theme::accent() };
    let proc = if p.process.is_empty() {
        p.pid.map(|id| id.to_string()).unwrap_or_else(|| "—".to_string())
    } else if p.process.chars().count() > 12 {
        format!("{}…", p.process.chars().take(11).collect::<String>())
    } else {
        p.process.clone()
    };
    row![
        text(p.proto.to_uppercase()).font(theme::TERMINAL_FONT).size(9)
            .color(proto_color).width(Length::Fixed(30.0)),
        text(format!(":{}", p.port)).font(theme::TERMINAL_FONT).size(11)
            .color(theme::text_high()).width(Length::Fixed(52.0)),
        text(proc).font(theme::TERMINAL_FONT).size(10)
            .color(theme::text_muted()).width(Length::Fill),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center)
    .into()
}

// ── Shared primitives ─────────────────────────────────────────────────────────

fn dot(color: Color) -> Element<'static, Message> {
    container(Space::new())
        .width(Length::Fixed(7.0))
        .height(Length::Fixed(7.0))
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border { radius: 4.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

fn divider() -> Element<'static, Message> {
    container(Space::new().width(Length::Fill).height(Length::Fixed(1.0)))
        .style(|_| container::Style {
            background: Some(theme::border_subtle().into()),
            ..Default::default()
        })
        .into()
}

fn meter<'a>(pct: f32, available: bool, color: Color) -> Element<'a, Message> {
    let frac = if available { (pct / 100.0).clamp(0.0, 1.0) } else { 0.0 };
    let fill_color = metric_color(pct, color);
    let fill_w = (frac * 1000.0).round() as u16;
    let rest_w = 1000u16.saturating_sub(fill_w);

    let fill = container(Space::new())
        .height(Length::Fixed(6.0))
        .width(Length::FillPortion(fill_w.max(1)))
        .style(move |_| container::Style {
            background: Some(fill_color.into()),
            border: Border { radius: 3.0.into(), ..Default::default() },
            ..Default::default()
        });
    let rest = Space::new().width(Length::FillPortion(rest_w.max(1)));
    let inner: Element<'_, Message> = if fill_w == 0 {
        rest.into()
    } else if rest_w == 0 {
        fill.into()
    } else {
        row![fill, rest].into()
    };
    container(inner)
        .width(Length::Fill)
        .height(Length::Fixed(6.0))
        .style(|_| container::Style {
            background: Some(theme::surface_3().into()),
            border: Border { radius: 3.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

fn hide_button() -> Element<'static, Message> {
    button(text("›").size(14).color(theme::text_muted()))
        .padding([1, 6])
        .on_press(Message::ToggleMonitor)
        .style(|_, status| button::Style {
            background: Some(
                if matches!(status, button::Status::Hovered) { theme::surface_3() }
                else { Color::TRANSPARENT }
                .into(),
            ),
            border: Border { radius: 6.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

// ── Line chart (canvas) ───────────────────────────────────────────────────────

fn chart<'a>(
    history: &std::collections::VecDeque<f32>,
    color: Color,
    max: f32,
    height: f32,
) -> Element<'a, Message> {
    let points: Vec<f32> = history.iter().copied().collect();
    canvas::Canvas::new(Sparkline { points, color, max })
        .width(Length::Fill)
        .height(Length::Fixed(height))
        .into()
}

struct Sparkline {
    points: Vec<f32>,
    color: Color,
    max: f32,
}

impl canvas::Program<Message> for Sparkline {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let h = bounds.height;
        if self.points.len() < 2 {
            return vec![frame.into_geometry()];
        }
        let scale = if self.max > 0.0 {
            self.max
        } else {
            self.points.iter().copied().fold(0.0_f32, f32::max).max(1.0)
        };
        let n = self.points.len();
        let dx = w / (n as f32 - 1.0);
        let y_for = |v: f32| h - (v / scale).clamp(0.0, 1.0) * (h - 2.0) - 1.0;
        let xy: Vec<Point> = self
            .points
            .iter()
            .enumerate()
            .map(|(i, &v)| Point::new(i as f32 * dx, y_for(v)))
            .collect();

        let fill = Path::new(|b| {
            b.move_to(Point::new(0.0, h));
            for p in &xy { b.line_to(*p); }
            b.line_to(Point::new(w, h));
            b.close();
        });
        frame.fill(&fill, theme::with_alpha(self.color, 0.18));

        let line = Path::new(|b| {
            b.move_to(xy[0]);
            for p in &xy[1..] { b.line_to(*p); }
        });
        frame.stroke(&line, Stroke::default().with_width(1.5).with_color(self.color));

        if let Some(last) = xy.last() {
            frame.fill(&Path::circle(*last, 2.5), self.color);
        }
        vec![frame.into_geometry()]
    }
}

// ── Formatters ────────────────────────────────────────────────────────────────

/// Return the accent color for a metric, escalating to warn/error at high load.
/// For non-percentage metrics (Network, DiskIO) always returns the base color.
fn metric_color(pct: f32, base: Color) -> Color {
    if pct >= 90.0 { theme::status_error() }
    else if pct >= 70.0 { theme::status_warn() }
    else { base }
}

fn rate_str(bps: f64, has_rates: bool) -> String {
    if !has_rates { "—".to_string() } else { format!("{}/s", fmt_bytes(bps)) }
}

fn fmt_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 { v /= 1024.0; i += 1; }
    if i == 0 { format!("{v:.0} {}", UNITS[i]) } else { format!("{v:.1} {}", UNITS[i]) }
}

fn fmt_kb(kb: u64) -> String { fmt_bytes(kb as f64 * 1024.0) }

fn fmt_uptime(secs: f64) -> String {
    let s = secs as u64;
    let days = s / 86_400;
    let hours = (s % 86_400) / 3_600;
    let mins = (s % 3_600) / 60;
    if days > 0 { format!("{days}d {hours}h") }
    else if hours > 0 { format!("{hours}h {mins}m") }
    else { format!("{mins}m") }
}
