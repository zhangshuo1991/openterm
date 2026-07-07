//! Right-column file viewer panel in the SFTP workspace.
//! Modes: Preview (syntax-highlighted read-only), Edit (editable + save), Log (paged + search).

use iced::widget::{
    button, column, container, row, scrollable, text, text_input, Space,
};
use iced::{Border, Color, Element, Length};

use crate::message::Message;
use crate::session::{FileViewerState, ViewerContent, ViewerMode};
use crate::theme;

pub fn view(state: &FileViewerState) -> Element<'_, Message> {
    let panel = column![toolbar(state), content_area(state), search_bar(state)]
        .height(Length::Fill);

    container(panel)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(theme::surface_1().into()),
            border: Border { color: theme::border_subtle(), width: 1.0, radius: 10.0.into() },
            ..Default::default()
        })
        .into()
}

fn toolbar(state: &FileViewerState) -> Element<'_, Message> {
    let name = state.path.rsplit('/').next().unwrap_or(&state.path);
    let title = text(name).size(13).color(theme::text_high()).width(Length::Fill);

    let mut actions: iced::widget::Row<'_, Message> =
        row![title].spacing(6).align_y(iced::Alignment::Center);

    match state.mode {
        ViewerMode::Preview => {
            actions = actions.push(small_btn("Edit", Message::FileViewerToggleEdit));
        }
        ViewerMode::Edit => {
            let label = if state.saving { "Saving…" } else { "Save" };
            actions = actions
                .push(small_btn("Preview", Message::FileViewerToggleEdit))
                .push(small_btn(label, Message::FileViewerSave));
        }
        ViewerMode::Log => {
            actions = actions
                .push(small_btn("◀ Prev", Message::FileViewerPrevPage))
                .push(small_btn("Next ▶", Message::FileViewerNextPage));
        }
    }
    actions = actions.push(small_btn("✕", Message::FileViewerClose));

    let dirty: Element<'_, Message> = if state.dirty {
        text("●").size(10).color(theme::accent()).into()
    } else {
        Space::new().into()
    };

    container(row![dirty, actions].spacing(4).align_y(iced::Alignment::Center))
        .width(Length::Fill)
        .padding([8, 12])
        .style(|_| container::Style {
            background: Some(theme::surface_2().into()),
            ..Default::default()
        })
        .into()
}

fn content_area(state: &FileViewerState) -> Element<'_, Message> {
    let inner: Element<'_, Message> = match &state.content {
        ViewerContent::Loading => container(
            text("Loading…").size(13).color(theme::text_muted()),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into(),

        ViewerContent::Error(e) => container(
            text(format!("Error: {e}")).size(13).color(Color::from_rgb(0.9, 0.3, 0.3)),
        )
        .padding(16)
        .into(),

        ViewerContent::Loaded(txt) => {
            if state.mode == ViewerMode::Edit {
                editor_view(state)
            } else {
                highlighted_view(txt, &state.highlight_cache)
            }
        }

        ViewerContent::Streaming { text: txt, total, page_offset } => {
            let pct = if *total > 0 {
                format!(" — {:.0}%", (*page_offset as f64 / *total as f64) * 100.0)
            } else {
                String::new()
            };
            column![
                highlighted_view(txt, &state.highlight_cache),
                container(
                    text(format!("{}  total{pct}", human_size(*total)))
                        .size(11)
                        .color(theme::text_muted()),
                )
                .padding([4, 12]),
            ]
            .height(Length::Fill)
            .into()
        }
    };

    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(4)
        .into()
}

/// Edit mode: a line-number gutter beside a syntax-highlighted `text_editor`.
/// The editor is `Shrink`-height so it lays out at full content height; the
/// gutter shares the same outer scrollable, so the two columns scroll together
/// and never drift (the same trick the read-only preview uses).
fn editor_view(state: &FileViewerState) -> Element<'_, Message> {
    let line_count = state.editor.line_count().max(1);
    let gutter_width = line_count.to_string().len();

    // Line-number gutter. Uses the same font/size/line-height as the editor so
    // rows line up. Wrapping is disabled on the editor (below), so one logical
    // line = one visual row and the numbers stay aligned.
    let mut gutter_text = String::with_capacity(line_count * (gutter_width + 1));
    for n in 1..=line_count {
        gutter_text.push_str(&format!("{n:>gutter_width$}\n"));
    }
    let gutter = container(
        text(gutter_text)
            .size(13)
            .font(iced::Font::MONOSPACE)
            .line_height(iced::widget::text::LineHeight::default())
            .color(theme::text_dim()),
    )
    .padding(iced::Padding::from([12.0, 8.0]));

    let editor = iced::widget::text_editor(&state.editor)
        .on_action(Message::FileViewerAction)
        .size(13)
        .font(iced::Font::MONOSPACE)
        .padding(12)
        .wrapping(iced::widget::text::Wrapping::None)
        .height(Length::Shrink)
        .highlight(&state.ext(), iced::highlighter::Theme::Base16Ocean);

    scrollable(
        row![gutter, editor].width(Length::Fill),
    )
    .direction(iced::widget::scrollable::Direction::Both {
        vertical: iced::widget::scrollable::Scrollbar::default(),
        horizontal: iced::widget::scrollable::Scrollbar::default(),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn highlighted_view<'a>(content: &'a str, spans: &[(iced::Color, String)]) -> Element<'a, Message> {
    // Count '\n' chars so trailing newline and \r\n files are handled correctly.
    let line_count = (content.chars().filter(|&c| c == '\n').count() + 1).max(1);
    let gutter_width = line_count.to_string().len();
    let gutter_color = theme::text_dim();

    // Source spans: highlight cache, or one plain span if highlighting is off.
    let src: Vec<(iced::Color, &str)> = if spans.is_empty() {
        vec![(theme::text_high(), content)]
    } else {
        spans.iter().map(|(c, t)| (*c, t.as_str())).collect()
    };

    // Build a single rich_text where the line-number gutter is part of the
    // same text flow as the code. This makes drift between the two columns
    // physically impossible — they share one layout.
    let make_span = |s: String, color: Option<iced::Color>| {
        let sp = iced::widget::span(s).font(iced::Font::MONOSPACE).size(13.0);
        match color {
            Some(c) => sp.color(c),
            None => sp,
        }
    };
    let gutter = |n: usize| make_span(format!("{n:>gutter_width$}  "), Some(gutter_color));

    let mut out: Vec<iced::widget::text::Span<'static, ()>> = Vec::new();
    let mut line_no = 1usize;
    out.push(gutter(line_no));
    for (color, text) in src {
        let mut parts = text.split('\n').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                out.push(make_span(part.to_string(), Some(color)));
            }
            if parts.peek().is_some() {
                out.push(make_span("\n".to_string(), None));
                line_no += 1;
                out.push(gutter(line_no));
            }
        }
    }

    scrollable(
        container(iced::widget::rich_text(out).width(Length::Shrink)).padding(12),
    )
    .direction(iced::widget::scrollable::Direction::Both {
        vertical: iced::widget::scrollable::Scrollbar::default(),
        horizontal: iced::widget::scrollable::Scrollbar::default(),
    })
    .into()
}

fn search_bar(state: &FileViewerState) -> Element<'_, Message> {
    let match_info = if state.search.is_empty() {
        String::new()
    } else if state.matches.is_empty() {
        "No matches".to_string()
    } else {
        format!("{}/{}", state.match_idx + 1, state.matches.len())
    };

    let search_row = row![
        text("Search:").size(12).color(theme::text_muted()),
        text_input("", &state.search)
            .on_input(Message::FileViewerSearchChanged)
            .size(12)
            .padding([3, 8])
            .width(Length::Fixed(160.0)),
        small_btn("▲", Message::FileViewerSearchPrev),
        small_btn("▼", Message::FileViewerSearchNext),
        text(match_info).size(11).color(theme::text_muted()).width(Length::Fixed(60.0)),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let body: Element<'_, Message> = if state.mode == ViewerMode::Edit {
        let replace_row = row![
            text("Replace:").size(12).color(theme::text_muted()),
            text_input("", &state.replace)
                .on_input(Message::FileViewerReplaceChanged)
                .size(12)
                .padding([3, 8])
                .width(Length::Fixed(160.0)),
            small_btn("One", Message::FileViewerReplaceOne),
            small_btn("All", Message::FileViewerReplaceAll),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        column![search_row, replace_row].spacing(4).into()
    } else {
        search_row.into()
    };

    container(body)
        .width(Length::Fill)
        .padding([6, 12])
        .style(|_| container::Style {
            background: Some(theme::surface_2().into()),
            border: Border { color: theme::border_subtle(), width: 1.0, radius: 0.0.into() },
            ..Default::default()
        })
        .into()
}

fn small_btn(label: &str, msg: Message) -> Element<'_, Message> {
    button(text(label.to_string()).size(12).color(theme::text_muted()))
        .padding([3, 8])
        .on_press(msg)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(
                    if hovered { theme::surface_3() } else { Color::TRANSPARENT }.into(),
                ),
                border: Border { radius: 5.0.into(), ..Default::default() },
                text_color: theme::text_high(),
                ..Default::default()
            }
        })
        .into()
}

fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0}KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}
