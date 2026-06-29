//! Terminal grid rendering on an iced canvas.

use iced::widget::canvas::{self, Action, Event, Frame, Geometry, Path, Text};
use iced::widget::text::LineHeight;
use iced::{alignment, mouse, Color, Font, Point, Rectangle, Renderer, Size, Theme};
use openterm_terminal::{TerminalCell, TerminalColor, TerminalSnapshot};

use crate::message::Message;
use crate::theme;

/// Fixed per-cell metrics for a given font size.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub cell_width: f32,
    pub line_height: f32,
}

pub fn metrics(font_size: u16) -> Metrics {
    let font_size = f32::from(font_size);
    Metrics {
        cell_width: (font_size * 0.62).max(6.0),
        line_height: (font_size * 1.32).max(14.0),
    }
}

pub fn grid_for_viewport(width: f32, height: f32, font_size: u16) -> (u16, u16) {
    let m = metrics(font_size);
    let cols = (width / m.cell_width).floor().max(20.0) as u16;
    let rows = (height / m.line_height).floor().max(4.0) as u16;
    (cols, rows)
}

fn cell_draw_width(cell: &TerminalCell, m: Metrics) -> f32 {
    m.cell_width * if cell.wide { 2.0 } else { 1.0 }
}

fn font_for(ch: char, bold: bool) -> Font {
    let base = if ch.is_ascii() { theme::TERMINAL_FONT } else { theme::UI_FONT };
    if bold { Font { weight: iced::font::Weight::Bold, ..base } } else { base }
}

fn color_to_iced(color: TerminalColor) -> Color {
    Color::from_rgb8(color.r, color.g, color.b)
}

/// Convert a canvas-local pixel position to a (col, row) cell coordinate.
fn pos_to_cell(pos: Point, m: Metrics, rows: usize) -> (usize, usize) {
    let col = (pos.x / m.cell_width).max(0.0) as usize;
    let row = (pos.y / m.line_height).max(0.0) as usize;
    (col, row.min(rows.saturating_sub(1)))
}

/// Return true if (col, row) falls within the selection [start..end] (order-independent).
fn in_selection(col: usize, row: usize, start: (usize, usize), end: (usize, usize)) -> bool {
    // Normalize to top-left / bottom-right.
    let (sc, sr, ec, er) = if start.1 < end.1 || (start.1 == end.1 && start.0 <= end.0) {
        (start.0, start.1, end.0, end.1)
    } else {
        (end.0, end.1, start.0, start.1)
    };
    if row < sr || row > er { return false; }
    if row == sr && col < sc { return false; }
    if row == er && col > ec { return false; }
    true
}

/// Extract text for the given cell-range selection.
pub fn selected_text(
    snapshot: &TerminalSnapshot,
    start: (usize, usize),
    end: (usize, usize),
) -> String {
    let (sc, sr, ec, er) = if start.1 < end.1 || (start.1 == end.1 && start.0 <= end.0) {
        (start.0, start.1, end.0, end.1)
    } else {
        (end.0, end.1, start.0, start.1)
    };
    let mut result = String::new();
    for row in sr..=er {
        let Some(cells) = snapshot.cells.get(row) else { break };
        let col_start = if row == sr { sc } else { 0 };
        let col_end = if row == er { ec } else { cells.len().saturating_sub(1) };
        if !result.is_empty() {
            result.push('\n');
        }
        let line: String = cells
            .get(col_start..=col_end.min(cells.len().saturating_sub(1)))
            .unwrap_or(&[])
            .iter()
            .filter(|c| !c.wide_spacer)
            .map(|c| c.ch)
            .collect();
        result.push_str(line.trim_end());
    }
    result
}

/// Per-canvas drag state for text selection.
#[derive(Debug, Default, Clone)]
pub struct SelectionState {
    dragging: bool,
    pub start: Option<(usize, usize)>,
    pub end: Option<(usize, usize)>,
}

/// A canvas program that paints one terminal snapshot.
#[derive(Debug)]
pub struct TerminalCanvas {
    pub snapshot: TerminalSnapshot,
    pub font_size: u16,
    /// Committed selection from the session (for cross-frame highlight when not dragging).
    pub selection: Option<(usize, usize, usize, usize)>,
    /// Lowercased search query; empty disables highlighting.
    pub search_query: String,
    /// Index of the "current" match to emphasize (wraps modulo match count).
    pub search_current: usize,
}

impl canvas::Program<Message> for TerminalCanvas {
    type State = SelectionState;

    fn update(
        &self,
        state: &mut SelectionState,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let m = metrics(self.font_size);
        let rows = self.snapshot.cells.len();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(pos) = cursor.position_in(bounds) {
                    let cell = pos_to_cell(pos, m, rows);
                    state.dragging = true;
                    state.start = Some(cell);
                    state.end = Some(cell);
                    // Clear committed selection immediately.
                    return Some(Action::publish(Message::SelectionChanged(None)).and_capture());
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if state.dragging {
                    if let Some(pos) = cursor.position_in(bounds) {
                        state.end = Some(pos_to_cell(pos, m, rows));
                        return Some(Action::request_redraw());
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if state.dragging {
                    state.dragging = false;
                    if let (Some(s), Some(e)) = (state.start, state.end) {
                        let msg = if s == e {
                            Message::SelectionChanged(None)
                        } else {
                            Message::SelectionChanged(Some((s.0, s.1, e.0, e.1)))
                        };
                        return Some(Action::publish(msg).and_capture());
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn draw(
        &self,
        state: &SelectionState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let m = metrics(self.font_size);
        let mut frame = Frame::new(renderer, bounds.size());

        // Active in-progress drag takes priority; otherwise use the committed selection.
        let sel: Option<((usize, usize), (usize, usize))> = if state.start.is_some() && state.end.is_some() && (state.dragging || state.start != state.end) {
            state.start.zip(state.end)
        } else {
            self.selection.map(|(c1, r1, c2, r2)| ((c1, r1), (c2, r2)))
        };

        // Compute search-match highlights: a set of matched cells, plus the
        // cells belonging to the "current" match (emphasized differently).
        let mut match_cells: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
        let mut current_cells: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
        if !self.search_query.is_empty() {
            let mut matches: Vec<Vec<(usize, usize)>> = Vec::new();
            for (row_index, row) in self.snapshot.cells.iter().enumerate() {
                // Build the row's lowercased text and a parallel map back to columns.
                let mut text = String::new();
                let mut cols: Vec<usize> = Vec::new();
                for cell in row {
                    if cell.wide_spacer { continue; }
                    for lc in cell.ch.to_lowercase() {
                        text.push(lc);
                        cols.push(cell.col);
                    }
                }
                let qlen = self.search_query.chars().count();
                let mut from = 0;
                while let Some(byte_pos) = text[from..].find(&self.search_query) {
                    let abs = from + byte_pos;
                    let char_start = text[..abs].chars().count();
                    let cells: Vec<(usize, usize)> = cols
                        .iter()
                        .skip(char_start)
                        .take(qlen)
                        .map(|&c| (row_index, c))
                        .collect();
                    if !cells.is_empty() {
                        matches.push(cells);
                    }
                    from = abs + self.search_query.len();
                }
            }
            if !matches.is_empty() {
                let cur = self.search_current % matches.len();
                for (i, cells) in matches.iter().enumerate() {
                    for &c in cells {
                        match_cells.insert(c);
                        if i == cur {
                            current_cells.insert(c);
                        }
                    }
                }
            }
        }

        for (row_index, row) in self.snapshot.cells.iter().enumerate() {
            for cell in row {
                if cell.wide_spacer { continue; }
                let x = cell.col as f32 * m.cell_width;
                let y = row_index as f32 * m.line_height;
                let cursor_here = self.snapshot.cursor.visible
                    && self.snapshot.cursor.row == row_index
                    && self.snapshot.cursor.col == cell.col;
                let selected = sel.is_some_and(|(s, e)| in_selection(cell.col, row_index, s, e));
                let inverse = cell.inverse || cursor_here;
                let is_match = match_cells.contains(&(row_index, cell.col));
                let is_current = current_cells.contains(&(row_index, cell.col));

                if is_match {
                    // Search highlight: orange for the current match, yellow otherwise.
                    let hl = if is_current {
                        Color::from_rgb(0.95, 0.55, 0.1)
                    } else {
                        Color::from_rgb(0.85, 0.78, 0.2)
                    };
                    frame.fill_rectangle(
                        Point::new(x, y),
                        Size::new(cell_draw_width(cell, m), m.line_height),
                        hl,
                    );
                } else if selected {
                    frame.fill_rectangle(
                        Point::new(x, y),
                        Size::new(cell_draw_width(cell, m), m.line_height),
                        theme::with_alpha(theme::accent(), 0.4),
                    );
                } else if inverse || cell.background.is_some() {
                    let bg = if inverse {
                        theme::accent_strong()
                    } else {
                        cell.background.map(color_to_iced).unwrap_or(Color::TRANSPARENT)
                    };
                    frame.fill_rectangle(
                        Point::new(x, y),
                        Size::new(cell_draw_width(cell, m), m.line_height),
                        bg,
                    );
                }

                if cursor_here && cell.ch == ' ' {
                    if !selected {
                        frame.fill_rectangle(
                            Point::new(x, y),
                            Size::new(cell_draw_width(cell, m), m.line_height),
                            theme::accent_strong(),
                        );
                    }
                    continue;
                }
                if cell.ch == ' ' { continue; }

                let fg = if is_match {
                    Color::from_rgb(0.05, 0.05, 0.05)
                } else if selected {
                    theme::text_high()
                } else if inverse {
                    theme::surface_0()
                } else {
                    cell.foreground.map(color_to_iced).unwrap_or(theme::text_high())
                };

                frame.fill_text(Text {
                    content: cell.ch.to_string(),
                    position: Point::new(x, y),
                    max_width: cell_draw_width(cell, m),
                    color: fg,
                    size: f32::from(self.font_size).into(),
                    line_height: LineHeight::Absolute(m.line_height.into()),
                    font: font_for(cell.ch, cell.bold),
                    align_x: alignment::Horizontal::Left.into(),
                    align_y: alignment::Vertical::Top,
                    shaping: iced::widget::text::Shaping::Advanced,
                });

                if cell.underline {
                    let underline = Path::rectangle(
                        Point::new(x, y + m.line_height - 1.5),
                        Size::new(cell_draw_width(cell, m), 1.0),
                    );
                    frame.fill(&underline, fg);
                }
            }
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &SelectionState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_grows_with_viewport() {
        let (cols, rows) = grid_for_viewport(800.0, 600.0, theme::DEFAULT_FONT_SIZE);
        assert!(cols > 40);
        assert!(rows > 20);
    }

    #[test]
    fn grid_has_sane_minimums() {
        let (cols, rows) = grid_for_viewport(10.0, 10.0, theme::DEFAULT_FONT_SIZE);
        assert_eq!(cols, 20);
        assert_eq!(rows, 4);
    }

    #[test]
    fn grid_never_overflows_viewport() {
        let height = 600.0;
        let (_, rows) = grid_for_viewport(800.0, height, theme::DEFAULT_FONT_SIZE);
        let drawn = f32::from(rows) * metrics(theme::DEFAULT_FONT_SIZE).line_height;
        assert!(drawn <= height, "drawn {drawn} > viewport {height}");
    }
}
