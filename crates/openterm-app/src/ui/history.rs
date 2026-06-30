//! Persistent command-history panel: filter by keyword/time, one-click copy or
//! insert, and a clear-all button. History is saved to the redb database and
//! survives app restarts.

use iced::widget::{button, column, container, mouse_area, row, scrollable, text, text_input, Space};
use iced::{mouse, Border, Color, Element, Length};

use crate::message::Message;
use crate::theme;
use crate::App;

pub fn divider() -> Element<'static, Message> {
    mouse_area(
        container(Space::new())
            .width(Length::Fixed(crate::ui::HISTORY_DIVIDER_WIDTH))
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(theme::border_subtle().into()),
                ..Default::default()
            }),
    )
    .interaction(mouse::Interaction::ResizingHorizontally)
    .on_press(Message::HistoryDragStart)
    .into()
}

pub fn view(app: &App) -> Element<'_, Message> {
    // --- header row ---
    let header = row![
        text("History")
            .size(13)
            .color(theme::text_high())
            .width(Length::Fill),
        action_btn("Clear", Message::HistoryClearAll, true),
    ]
    .align_y(iced::Alignment::Center);

    // --- keyword filter ---
    let filter = text_input("Filter commands…", &app.history_filter)
        .on_input(Message::HistoryFilterChanged)
        .padding([6, 10])
        .size(12)
        .style(|_, status| {
            let focused = matches!(status, text_input::Status::Focused { .. });
            text_input::Style {
                background: theme::surface_2().into(),
                border: Border {
                    color: if focused { theme::accent() } else { theme::border_subtle() },
                    width: 1.0,
                    radius: 7.0.into(),
                },
                icon: theme::text_muted(),
                placeholder: theme::text_dim(),
                value: theme::text_high(),
                selection: theme::accent_soft(),
            }
        });

    // --- filtered list ---
    let q = app.history_filter.to_lowercase();
    let filtered: Vec<_> = app
        .all_history
        .iter()
        .filter(|e| {
            if q.is_empty() {
                true
            } else {
                e.cmd.to_lowercase().contains(&q)
                || e.host.to_lowercase().contains(&q)
                || e.output.to_lowercase().contains(&q)
            }
        })
        .collect();

    let mut list = column![].spacing(1);
    if filtered.is_empty() {
        list = list.push(
            container(
                text(if app.all_history.is_empty() {
                    "Commands you type will appear here."
                } else {
                    "No matches."
                })
                .size(12)
                .color(theme::text_dim()),
            )
            .padding([8, 6]),
        );
    } else {
        // Group rows under Today / Yesterday / This Week / Older headers. Entries
        // are newest-first, so buckets appear in chronological order already.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut last_bucket: Option<&'static str> = None;
        for entry in &filtered {
            let bucket = time_bucket(entry.ts_ms, now_ms);
            if last_bucket != Some(bucket) {
                list = list.push(timeline_header(bucket));
                last_bucket = Some(bucket);
            }
            let expanded = app.expanded_history.contains(&entry.ts_ms);
            list = list.push(history_row(entry, expanded));
        }
    }

    let count_label = if filtered.len() == app.all_history.len() {
        format!("{} commands", app.all_history.len())
    } else {
        format!("{} / {}", filtered.len(), app.all_history.len())
    };

    let body = column![
        header,
        filter,
        scrollable(list).height(Length::Fill),
        text(count_label).size(10).color(theme::text_dim()),
    ]
    .spacing(10)
    .padding(12)
    .height(Length::Fill);

    container(body)
        .width(Length::Fixed(app.history_visual_width()))
        .height(Length::Fill)
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

fn history_row(entry: &openterm_storage::HistoryEntry, expanded: bool) -> Element<'_, Message> {
    let ts = fmt_ts(entry.ts_ms);
    let cmd = entry.cmd.clone();
    let cmd_copy = cmd.clone();
    let cmd_run = cmd.clone();
    let has_output = !entry.output.is_empty();

    let mut meta = row![
        text(ts).size(10).color(theme::text_dim()),
        Space::new().width(Length::Fixed(6.0)),
        text(entry.host.clone())
            .size(10)
            .color(theme::accent())
            .width(Length::Fill),
        action_btn("Run", Message::HistoryRun(cmd_run), false),
        action_btn("Copy", Message::HistoryCopyCmd(cmd_copy), false),
    ]
    .align_y(iced::Alignment::Center);
    // Expand/collapse toggle only when there's captured output to show.
    if has_output {
        let glyph = if expanded { "⌄" } else { "›" };
        meta = meta.push(action_btn(glyph, Message::HistoryToggleExpand(entry.ts_ms), false));
    }

    let cmd_label = text(cmd.clone())
        .font(theme::TERMINAL_FONT)
        .size(12)
        .color(theme::text_high())
        .wrapping(iced::widget::text::Wrapping::None);

    let mut inner = column![meta, cmd_label].spacing(2);

    if has_output {
        if expanded {
            // Full (capped) output block, monospace, in a subtle panel.
            let full: String = entry.output.chars().take(1500).collect();
            let block = container(
                text(full)
                    .font(theme::TERMINAL_FONT)
                    .size(11)
                    .color(theme::text_muted()),
            )
            .width(Length::Fill)
            .padding([5, 7])
            .style(|_| container::Style {
                background: Some(theme::surface_2().into()),
                border: Border {
                    radius: 5.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
            inner = inner.push(block);
        } else {
            // Collapsed: 2-line preview.
            let preview: String = entry
                .output
                .lines()
                .take(2)
                .collect::<Vec<_>>()
                .join("\n");
            let output_preview = text(preview)
                .font(theme::TERMINAL_FONT)
                .size(11)
                .color(theme::text_dim())
                .wrapping(iced::widget::text::Wrapping::None);
            inner = inner.push(output_preview);
        }
    }

    button(inner)
        .width(Length::Fill)
        .padding([6, 8])
        .on_press(Message::HistoryInsert(cmd))
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(
                    if hovered { theme::surface_2() } else { Color::TRANSPARENT }.into(),
                ),
                text_color: theme::text_high(),
                border: Border { radius: 6.0.into(), ..Default::default() },
                ..Default::default()
            }
        })
        .into()
}

/// A timeline section header (Today / Yesterday / This Week / Older).
fn timeline_header(label: &str) -> Element<'_, Message> {
    container(text(label.to_string()).size(10).color(theme::text_dim()))
        .padding(iced::Padding { top: 8.0, right: 4.0, bottom: 2.0, left: 2.0 })
        .into()
}

/// Bucket a timestamp relative to now into a coarse timeline label.
fn time_bucket(ts_ms: u64, now_ms: u64) -> &'static str {
    let age_secs = now_ms.saturating_sub(ts_ms) / 1000;
    let day = (ts_ms / 1000) / 86400;
    let today = (now_ms / 1000) / 86400;
    if day == today {
        "Today"
    } else if today.saturating_sub(day) == 1 {
        "Yesterday"
    } else if age_secs < 7 * 86400 {
        "This Week"
    } else {
        "Older"
    }
}

fn action_btn(label: &str, msg: Message, danger: bool) -> Element<'_, Message> {
    let fg = if danger { theme::status_error() } else { theme::text_muted() };
    button(text(label.to_string()).size(10).color(fg))
        .padding([2, 7])
        .on_press(msg)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(
                    if hovered {
                        if danger {
                            theme::with_alpha(theme::status_error(), 0.15)
                        } else {
                            theme::surface_3()
                        }
                    } else {
                        Color::TRANSPARENT
                    }
                    .into(),
                ),
                text_color: fg,
                border: Border { radius: 5.0.into(), ..Default::default() },
                ..Default::default()
            }
        })
        .into()
}

/// Format a Unix-ms timestamp as "Jun 23 10:04" (local time, no year if current year).
fn fmt_ts(ts_ms: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    let secs = ts_ms / 1000;
    // Use chrono if available; otherwise fall back to a simple approximation.
    // We use std only — compute days since epoch to get month/day.
    let dt = UNIX_EPOCH + Duration::from_secs(secs);
    // Convert to local time via chrono-free approach: just show HH:MM:SS of today
    // using the offset from UTC (not timezone-aware, but readable for recent commands).
    // Simple: show date as YYYY-MM-DD HH:MM.
    let total_secs = secs;
    let hh = (total_secs % 86400) / 3600;
    let mm = (total_secs % 3600) / 60;
    // Approximate days since epoch → date.
    let days = total_secs / 86400;
    let (y, mo, d) = days_to_ymd(days);
    let _ = dt; // suppress warning
    let months = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
    let mo_name = months.get(mo.saturating_sub(1) as usize).unwrap_or(&"?");
    format!("{mo_name} {d:02} {hh:02}:{mm:02}")
}

/// Gregorian calendar conversion from days-since-Unix-epoch (UTC) to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m as u64, d as u64)
}
