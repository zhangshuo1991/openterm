//! Lightweight toast notifications: transient messages that slide in at the
//! top-right, then fade out on their own. iced has no transform, so the slide
//! is faked with a left-padding interpolation and the fade with alpha.

use iced::widget::{button, column, container, row, text, Space};
use iced::{Border, Color, Element, Length};

use crate::message::Message;
use crate::theme;

/// Severity of a toast, which picks its accent color and glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
    Info,
    Warning,
}

impl ToastKind {
    fn color(self) -> Color {
        match self {
            ToastKind::Success => theme::status_ok(),
            ToastKind::Error => theme::status_error(),
            ToastKind::Info => theme::accent(),
            ToastKind::Warning => theme::status_warn(),
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            ToastKind::Success => "✓",
            ToastKind::Error => "✕",
            ToastKind::Info => "ℹ",
            ToastKind::Warning => "!",
        }
    }

    /// Short heading shown above the message body.
    fn title(self) -> &'static str {
        match self {
            ToastKind::Success => "Success",
            ToastKind::Error => "Error",
            ToastKind::Info => "Info",
            ToastKind::Warning => "Warning",
        }
    }
}

/// One toast. Progress is derived from wall-clock age (see [`Toast::progress`])
/// and runs 0.0→~1.3 over its lifetime: it slides/fades in over the first
/// ~12%, holds, then fades out past 1.0. `dismissed` forces an early retire
/// on the next tick.
///
/// Deriving progress from time (instead of accumulating per-frame deltas)
/// means the hold phase needs no animation frames at all — a slow heartbeat
/// tick retires the toast and catches the fade-out window, and full-rate
/// frames only run during the actual fade transitions.
#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub msg: String,
    pub created: std::time::Instant,
    pub dismissed: bool,
}

/// Seconds per unit of progress (total lifetime = 1.3 × this ≈ 3.9 s).
const LIFE_UNIT_SECS: f32 = 3.0;

impl Toast {
    pub fn new(id: u64, kind: ToastKind, msg: impl Into<String>) -> Self {
        Self {
            id,
            kind,
            msg: msg.into(),
            created: std::time::Instant::now(),
            dismissed: false,
        }
    }

    /// Lifecycle progress at `now`: fade in over [0, 0.12], hold over
    /// [0.12, 1.0], fade out over [1.0, 1.3].
    pub fn progress(&self, now: std::time::Instant) -> f32 {
        now.saturating_duration_since(self.created).as_secs_f32() / LIFE_UNIT_SECS
    }

    /// Finished (fully faded out, or manually dismissed) — ready to retire.
    pub fn done(&self, now: std::time::Instant) -> bool {
        self.dismissed || self.progress(now) >= 1.3
    }

    /// True while an entrance/exit transition is running — the only phases
    /// that need per-frame redraws; the hold phase is visually static. The
    /// margins are wider than the exact fade windows so the ~250 ms heartbeat
    /// hands over to full-rate frames before a fade visibly starts.
    pub fn animating(&self, now: std::time::Instant) -> bool {
        if self.dismissed {
            return false;
        }
        let p = self.progress(now);
        p < 0.13 || p > 0.95
    }

    /// Entrance/exit eased opacity in 0.0..=1.0.
    fn opacity(&self, now: std::time::Instant) -> f32 {
        if self.dismissed {
            return 0.0;
        }
        let progress = self.progress(now);
        if progress < 0.12 {
            (progress / 0.12).clamp(0.0, 1.0)
        } else if progress <= 1.0 {
            1.0
        } else {
            (1.0 - (progress - 1.0) / 0.3).clamp(0.0, 1.0)
        }
    }
}

/// Stack of toasts, anchored top-right. Returns an overlay element to push onto
/// the view's top `stack![]` layer.
pub fn view(toasts: &[Toast], now: std::time::Instant) -> Element<'_, Message> {
    let mut col = column![].spacing(8).align_x(iced::Alignment::End);
    for t in toasts {
        col = col.push(toast_card(t, now));
    }

    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .align_y(iced::alignment::Vertical::Top)
        .padding(iced::Padding {
            top: 46.0, // clear the fused titlebar / tab bar
            right: 16.0,
            bottom: 0.0,
            left: 0.0,
        })
        .into()
}

fn toast_card(t: &Toast, now: std::time::Instant) -> Element<'_, Message> {
    let alpha = t.opacity(now);
    let accent = t.kind.color();
    // Slide in ~24px from the right as it appears.
    let slide = ((0.12 - t.progress(now).min(0.12)) / 0.12 * 24.0).max(0.0);

    // Circular glyph badge tinted with the toast's accent.
    let badge = container(
        text(t.kind.glyph())
            .size(13)
            .color(theme::with_alpha(accent, alpha)),
    )
    .width(Length::Fixed(24.0))
    .height(Length::Fixed(24.0))
    .center_x(Length::Fixed(24.0))
    .center_y(Length::Fixed(24.0))
    .style(move |_| container::Style {
        background: Some(theme::with_alpha(accent, alpha * 0.16).into()),
        border: Border {
            color: theme::with_alpha(accent, alpha * 0.45),
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    });

    // Title + message stacked.
    let texts = column![
        text(t.kind.title())
            .size(11)
            .color(theme::with_alpha(accent, alpha)),
        text(t.msg.clone())
            .size(13)
            .color(theme::with_alpha(theme::text_high(), alpha)),
    ]
    .spacing(1)
    .width(Length::Shrink);

    // Colored left accent bar (fixed height so it never stretches the card).
    let bar = container(Space::new())
        .width(Length::Fixed(3.0))
        .height(Length::Fixed(30.0))
        .style(move |_| container::Style {
            background: Some(theme::with_alpha(accent, alpha * 0.9).into()),
            border: Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });

    let body = row![
        bar,
        badge,
        texts,
        Space::new().width(Length::Fixed(10.0)),
        dismiss_button(t.id, alpha),
    ]
    .spacing(10)
    .align_y(iced::Alignment::Center);

    let card = container(body)
        .padding([10, 13])
        .max_width(380.0)
        .style(move |_| container::Style {
            background: Some(theme::with_alpha(theme::surface_2(), alpha * 0.98).into()),
            border: Border {
                color: theme::with_alpha(accent, alpha * 0.5),
                width: 1.0,
                radius: 11.0.into(),
            },
            shadow: iced::Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.32 * alpha),
                offset: iced::Vector::new(0.0, 4.0),
                blur_radius: 18.0,
            },
            ..Default::default()
        });

    // The right-padding spacer implements the horizontal slide.
    row![card, Space::new().width(Length::Fixed(slide))].into()
}

fn dismiss_button(id: u64, alpha: f32) -> Element<'static, Message> {
    button(text("✕").size(11).color(theme::with_alpha(theme::text_dim(), alpha)))
        .padding([1, 4])
        .on_press(Message::ToastDismiss(id))
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(
                    if hovered {
                        theme::with_alpha(theme::surface_3(), alpha)
                    } else {
                        Color::TRANSPARENT
                    }
                    .into(),
                ),
                text_color: theme::with_alpha(theme::text_high(), alpha),
                border: Border {
                    radius: 5.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .into()
}
