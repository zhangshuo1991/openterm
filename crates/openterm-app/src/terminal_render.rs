//! Terminal grid rendering on an iced canvas.

use iced::widget::canvas::{self, Action, Event, Frame, Geometry, Path, Text};
use iced::widget::text::LineHeight;
use iced::{alignment, mouse, Color, Font, Point, Rectangle, Renderer, Size, Theme};
use openterm_terminal::{MouseProtocol, TerminalCell, TerminalColor, TerminalSnapshot};

use crate::message::Message;
use crate::theme;

// Runtime line-height multiplier (font_size × this = row height) and extra
// letter spacing. Kept in thread-locals so `metrics()` — called from many
// places, including grid sizing — reads current values without threading them
// through every signature.
thread_local! {
    static LINE_HEIGHT_MULT: std::cell::Cell<f32> = const { std::cell::Cell::new(1.32) };
    static LETTER_SPACING: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

/// Install the line-height multiplier (clamped to a sane range).
pub fn set_line_height(mult: f32) {
    let m = if mult.is_finite() { mult.clamp(1.0, 2.0) } else { 1.32 };
    LINE_HEIGHT_MULT.with(|c| c.set(m));
}

/// Install extra per-cell horizontal spacing in pixels (clamped).
pub fn set_letter_spacing(px: f32) {
    let v = if px.is_finite() { px.clamp(0.0, 6.0) } else { 0.0 };
    LETTER_SPACING.with(|c| c.set(v));
}

/// Fixed per-cell metrics for a given font size.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub cell_width: f32,
    pub line_height: f32,
}

pub fn metrics(font_size: u16) -> Metrics {
    let font_size = f32::from(font_size);
    let mult = LINE_HEIGHT_MULT.with(|c| c.get());
    let spacing = LETTER_SPACING.with(|c| c.get());
    Metrics {
        cell_width: (font_size * 0.62).max(6.0) + spacing,
        line_height: (font_size * mult).max(14.0),
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

/// Paint an Underline or Beam cursor bar in accent color. Block is handled
/// inline (full-cell inverse), so this only covers the thin variants.
fn draw_cursor_bar(
    frame: &mut Frame,
    shape: crate::theme::CursorShape,
    x: f32,
    y: f32,
    width: f32,
    line_height: f32,
) {
    use crate::theme::CursorShape;
    let color = theme::accent_strong();
    match shape {
        CursorShape::Underline => {
            frame.fill_rectangle(
                Point::new(x, y + line_height - 2.0),
                Size::new(width, 2.0),
                color,
            );
        }
        CursorShape::Beam => {
            frame.fill_rectangle(Point::new(x, y), Size::new(2.0, line_height), color);
        }
        CursorShape::Block => {}
    }
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

/// One kind of mouse event we may forward to a remote app.
#[derive(Debug, Clone, Copy)]
enum MouseKind {
    Press,
    Release,
    Motion,
    WheelUp,
    WheelDown,
}

/// Encode a mouse event into the escape sequence the active protocol expects
/// (SGR mode 1006 or legacy X10). `col`/`row` are 0-based cell coordinates;
/// `base_button` is 0=left, 1=middle, 2=right. Returns `None` when the event
/// can't be represented (e.g. X10 coords past the 223-cell limit).
fn encode_mouse_report(
    proto: MouseProtocol,
    kind: MouseKind,
    base_button: u8,
    col: usize,
    row: usize,
    shift: bool,
    alt: bool,
    ctrl: bool,
) -> Option<Vec<u8>> {
    // Base button code per the xterm protocol.
    let mut cb: u32 = match kind {
        MouseKind::WheelUp => 64,
        MouseKind::WheelDown => 65,
        MouseKind::Release if !proto.sgr => 3, // X10 can't tell which button released
        MouseKind::Press | MouseKind::Release => u32::from(base_button),
        MouseKind::Motion => u32::from(base_button) + 32,
    };
    // Modifier bits.
    if shift { cb += 4; }
    if alt { cb += 8; }
    if ctrl { cb += 16; }

    // Coordinates are 1-based on the wire.
    let cx = col as u32 + 1;
    let cy = row as u32 + 1;

    if proto.sgr {
        // ESC [ < Cb ; Cx ; Cy (M for press/motion/wheel, m for release)
        let final_char = if matches!(kind, MouseKind::Release) { 'm' } else { 'M' };
        Some(format!("\x1b[<{cb};{cx};{cy}{final_char}").into_bytes())
    } else {
        // Legacy X10: ESC [ M  (Cb+32) (Cx+32) (Cy+32), each a single byte.
        // Cannot encode coordinates beyond 223 (255-32).
        if cx > 223 || cy > 223 {
            return None;
        }
        Some(vec![
            0x1b,
            b'[',
            b'M',
            (cb + 32) as u8,
            (cx + 32) as u8,
            (cy + 32) as u8,
        ])
    }
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
    /// Time + cell of the previous left-press, for double/triple-click detection.
    last_click: Option<(std::time::Instant, (usize, usize))>,
    /// Consecutive-click counter (1=single, 2=double, 3=triple), reset when the
    /// press lands on a different cell or after the double-click timeout.
    click_streak: u8,
    /// Latest keyboard modifiers seen by this canvas (for mouse-report bits and
    /// the Shift-to-override-mouse-mode gesture).
    modifiers: iced::keyboard::Modifiers,
    /// Cell of the last mouse-report we forwarded while a button is held, so we
    /// only emit a motion report when the cell actually changes.
    last_report_cell: Option<(usize, usize)>,
    /// Button currently held for mouse-motion reports (0=left,1=mid,2=right).
    held_button: Option<u8>,
    /// Fractional wheel-line accumulator, so macOS pixel-based trackpad deltas
    /// emit remote scroll events at line granularity instead of per-pixel.
    scroll_accum: f32,
}

/// True for characters that count as part of a "word" when double-clicking.
/// Mirrors common terminal behavior: alphanumerics plus a few path/URL glyphs.
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '~' | ':' | '@')
}

/// Given a clicked cell, expand left/right along the row to the word boundaries
/// and return the inclusive `(start_col, end_col)`. Returns `None` if the
/// clicked cell is whitespace/empty (nothing to select).
fn word_bounds_at(
    snapshot: &TerminalSnapshot,
    col: usize,
    row: usize,
) -> Option<(usize, usize)> {
    let cells = snapshot.cells.get(row)?;
    if cells.is_empty() {
        return None;
    }
    let col = col.min(cells.len() - 1);
    let ch = cells.get(col)?.ch;
    if !is_word_char(ch) {
        return None;
    }
    let mut start = col;
    while start > 0 {
        let prev = cells[start - 1].ch;
        if is_word_char(prev) {
            start -= 1;
        } else {
            break;
        }
    }
    let mut end = col;
    while end + 1 < cells.len() {
        let next = cells[end + 1].ch;
        if is_word_char(next) {
            end += 1;
        } else {
            break;
        }
    }
    Some((start, end))
}

/// Characters allowed inside a URL (a pragmatic subset of RFC 3986).
fn is_url_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '-' | '.' | '_' | '~' | ':' | '/' | '?' | '#' | '[' | ']' | '@'
                | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';'
                | '=' | '%'
        )
}

/// If the clicked cell falls inside an `http://`/`https://` URL on its row,
/// return that URL. Scans the row's contiguous run of URL characters around the
/// click and checks it begins with a supported scheme.
fn url_at(snapshot: &TerminalSnapshot, col: usize, row: usize) -> Option<String> {
    let cells = snapshot.cells.get(row)?;
    if cells.is_empty() {
        return None;
    }
    let col = col.min(cells.len() - 1);
    if !is_url_char(cells.get(col)?.ch) {
        return None;
    }
    // Expand to the maximal run of URL characters (skip wide spacers).
    let mut start = col;
    while start > 0 && is_url_char(cells[start - 1].ch) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < cells.len() && is_url_char(cells[end + 1].ch) {
        end += 1;
    }
    let run: String = cells[start..=end]
        .iter()
        .filter(|c| !c.wide_spacer)
        .map(|c| c.ch)
        .collect();
    // Find a scheme within the run (the run may start mid-word before it).
    let lower = run.to_ascii_lowercase();
    let pos = lower.find("https://").or_else(|| lower.find("http://"))?;
    let url = run[pos..].trim_end_matches(|c| matches!(c, '.' | ',' | ')' | ']' | ';' | ':'));
    if url.len() > 8 {
        Some(url.to_string())
    } else {
        None
    }
}

/// A canvas program that paints one terminal snapshot.
///
/// Drawing is split into two layers:
/// - the **grid layer** (cells, colors, cursor, search highlights) is stored
///   in `cache` and only re-tessellated when the render key changes (see
///   `TerminalRenderCache::sync_key`);
/// - a cheap **overlay layer** (selection tint, copy flash, ghost suggestion)
///   is rebuilt every frame, since it changes during drags/animations.
#[derive(Debug)]
pub struct TerminalCanvas<'a> {
    /// Persistent geometry cache owned by the session.
    pub cache: &'a canvas::Cache,
    pub snapshot: std::sync::Arc<TerminalSnapshot>,
    pub font_size: u16,
    /// Committed selection from the session (for cross-frame highlight when not dragging).
    pub selection: Option<(usize, usize, usize, usize)>,
    /// Mouse-reporting protocol the remote app requested. When `report` is set,
    /// clicks/drags/wheel are forwarded to the PTY instead of selecting locally.
    pub mouse: MouseProtocol,
    /// Lowercased search query; empty disables highlighting.
    pub search_query: String,
    /// Index of the "current" match to emphasize (wraps modulo match count).
    pub search_current: usize,
    /// Shape used to paint the cursor cell.
    pub cursor_shape: crate::theme::CursorShape,
    /// Copy-confirmation flash intensity (0 = none, 1 = just copied).
    pub copy_flash: f32,
    /// Inline ghost-text suggestion drawn at the cursor (Sprint 3). Empty = none.
    pub inline_suggestion: String,
}

impl canvas::Program<Message> for TerminalCanvas<'_> {
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

        // Track modifiers so mouse reports carry the right bits and Shift can
        // override mouse mode (the standard "select locally anyway" gesture).
        if let Event::Keyboard(iced::keyboard::Event::ModifiersChanged(mods)) = event {
            state.modifiers = *mods;
            return None;
        }

        let shift = state.modifiers.shift();
        let alt = state.modifiers.alt();
        let ctrl = state.modifiers.control();
        // When the remote app is reading the mouse and Shift is NOT held, we
        // forward events to the PTY instead of doing local selection.
        let report_to_remote = self.mouse.report && !shift;

        // --- Mouse reporting path (vim / tmux / htop / less …) ---
        if report_to_remote {
            let btn = |b: &mouse::Button| -> Option<u8> {
                match b {
                    mouse::Button::Left => Some(0),
                    mouse::Button::Middle => Some(1),
                    mouse::Button::Right => Some(2),
                    _ => None,
                }
            };
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(b)) => {
                    if let (Some(base), Some(pos)) = (btn(b), cursor.position_in(bounds)) {
                        let (col, row) = pos_to_cell(pos, m, rows);
                        state.held_button = Some(base);
                        state.last_report_cell = Some((col, row));
                        if let Some(seq) = encode_mouse_report(
                            self.mouse, MouseKind::Press, base, col, row, shift, alt, ctrl,
                        ) {
                            return Some(Action::publish(Message::TerminalWriteRaw(seq)).and_capture());
                        }
                    }
                }
                Event::Mouse(mouse::Event::ButtonReleased(b)) => {
                    if let (Some(base), Some(pos)) = (btn(b), cursor.position_in(bounds)) {
                        let (col, row) = pos_to_cell(pos, m, rows);
                        state.held_button = None;
                        state.last_report_cell = None;
                        if let Some(seq) = encode_mouse_report(
                            self.mouse, MouseKind::Release, base, col, row, shift, alt, ctrl,
                        ) {
                            return Some(Action::publish(Message::TerminalWriteRaw(seq)).and_capture());
                        }
                    }
                }
                Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    // Motion reports: button-drag (1002) needs a held button;
                    // any-motion (1003) reports even with none.
                    let want_motion = (self.mouse.button_motion && state.held_button.is_some())
                        || self.mouse.any_motion;
                    if want_motion {
                        if let Some(pos) = cursor.position_in(bounds) {
                            let (col, row) = pos_to_cell(pos, m, rows);
                            if state.last_report_cell != Some((col, row)) {
                                state.last_report_cell = Some((col, row));
                                let base = state.held_button.unwrap_or(3); // 3 = no button
                                if let Some(seq) = encode_mouse_report(
                                    self.mouse, MouseKind::Motion, base, col, row, shift, alt, ctrl,
                                ) {
                                    return Some(
                                        Action::publish(Message::TerminalWriteRaw(seq)).and_capture(),
                                    );
                                }
                            }
                        }
                    }
                }
                Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                    let dy = match delta {
                        mouse::ScrollDelta::Lines { y, .. } => *y,
                        mouse::ScrollDelta::Pixels { y, .. } => *y / 16.0,
                    };
                    if dy.abs() >= 0.01 {
                        if let Some(pos) = cursor.position_in(bounds) {
                            let (col, row) = pos_to_cell(pos, m, rows);
                            let kind = if dy > 0.0 { MouseKind::WheelUp } else { MouseKind::WheelDown };
                            if let Some(seq) = encode_mouse_report(
                                self.mouse, kind, 0, col, row, shift, alt, ctrl,
                            ) {
                                return Some(Action::publish(Message::TerminalWriteRaw(seq)).and_capture());
                            }
                        }
                    }
                }
                _ => {}
            }
            return None;
        }

        // --- Local selection path (no remote mouse reporting) ---
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(pos) = cursor.position_in(bounds) {
                    let cell = pos_to_cell(pos, m, rows);

                    // Cmd/Ctrl+click on a URL opens it in the system browser.
                    if state.modifiers.command() || ctrl {
                        if let Some(url) = url_at(&self.snapshot, cell.0, cell.1) {
                            return Some(Action::publish(Message::OpenUrl(url)).and_capture());
                        }
                    }

                    let now = std::time::Instant::now();
                    // Track consecutive clicks on the same cell within 400ms:
                    // 2 = word select, 3 = line select.
                    let within = state.last_click.is_some_and(|(t, c)| {
                        c == cell && now.duration_since(t) <= std::time::Duration::from_millis(400)
                    });
                    state.click_streak = if within { state.click_streak + 1 } else { 1 };
                    state.last_click = Some((now, cell));

                    if state.click_streak >= 3 {
                        // Triple-click: select the whole line (all columns of the row).
                        state.dragging = false;
                        state.click_streak = 0; // reset so a 4th click starts over
                        let last_col = self
                            .snapshot
                            .cells
                            .get(cell.1)
                            .map(|r| r.len().saturating_sub(1))
                            .unwrap_or(0);
                        state.start = Some((0, cell.1));
                        state.end = Some((last_col, cell.1));
                        return Some(
                            Action::publish(Message::SelectionChanged(Some((
                                0, cell.1, last_col, cell.1,
                            ))))
                            .and_capture(),
                        );
                    }

                    if state.click_streak == 2 {
                        // Double-click: select the whole word under the cursor.
                        // Consume it so the pending single-click drag doesn't
                        // collapse it back to an empty selection.
                        state.dragging = false;
                        if let Some((sc, ec)) = word_bounds_at(&self.snapshot, cell.0, cell.1) {
                            state.start = Some((sc, cell.1));
                            state.end = Some((ec, cell.1));
                            return Some(
                                Action::publish(Message::SelectionChanged(Some((
                                    sc, cell.1, ec, cell.1,
                                ))))
                                .and_capture(),
                            );
                        }
                    }

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
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                // Only act on wheel when the pointer is over this canvas.
                if cursor.position_in(bounds).is_some() {
                    let lines = match delta {
                        mouse::ScrollDelta::Lines { y, .. } => *y,
                        mouse::ScrollDelta::Pixels { y, .. } => *y / 16.0,
                    };
                    // Alternate-scroll: on the alt screen (vim/less/man/htop
                    // without `set mouse=a`) there is no local scrollback, so a
                    // wheel would do nothing. Translate it into arrow-key presses
                    // the remote app scrolls with — exactly what xterm/iTerm do.
                    if self.mouse.alt_screen && self.mouse.alternate_scroll {
                        // Accumulate fractional (trackpad) deltas; ~3 lines/notch.
                        state.scroll_accum += lines * 3.0;
                        let steps = state.scroll_accum.trunc() as i32;
                        if steps != 0 {
                            state.scroll_accum -= steps as f32;
                            let up = steps > 0;
                            // DECCKM (app cursor) → ESC O A/B, else ESC [ A/B.
                            let arrow: &[u8] = match (self.mouse.app_cursor, up) {
                                (true, true) => b"\x1bOA",
                                (true, false) => b"\x1bOB",
                                (false, true) => b"\x1b[A",
                                (false, false) => b"\x1b[B",
                            };
                            let n = steps.unsigned_abs() as usize;
                            let mut seq = Vec::with_capacity(arrow.len() * n);
                            for _ in 0..n {
                                seq.extend_from_slice(arrow);
                            }
                            return Some(
                                Action::publish(Message::TerminalWriteRaw(seq)).and_capture(),
                            );
                        }
                        return None;
                    }
                    return Some(Action::publish(Message::TerminalScroll(lines)));
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                if let Some(pos) = cursor.position_in(bounds) {
                    return Some(
                        Action::publish(Message::TerminalOpenMenu(pos.x, pos.y)).and_capture(),
                    );
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

        // Grid layer: geometry is reused verbatim from the cache unless the
        // session cleared it (content/font/theme/search changed).
        let grid = self.cache.draw(renderer, bounds.size(), |frame| {
            self.draw_grid(frame, m);
        });

        // Overlay layer: selection tint / copy flash / ghost suggestion —
        // cheap to rebuild and changes during drags and animations.
        // Active in-progress drag takes priority; otherwise use the committed selection.
        let sel: Option<((usize, usize), (usize, usize))> = if state.start.is_some()
            && state.end.is_some()
            && (state.dragging || state.start != state.end)
        {
            state.start.zip(state.end)
        } else {
            self.selection.map(|(c1, r1, c2, r2)| ((c1, r1), (c2, r2)))
        };
        let want_ghost = !self.inline_suggestion.is_empty() && self.snapshot.cursor.visible;
        if sel.is_none() && !want_ghost {
            return vec![grid];
        }

        let mut overlay = Frame::new(renderer, bounds.size());

        if let Some((s, e)) = sel {
            // Translucent accent tint over the selected cells; the glyphs keep
            // their own colors underneath. Flash brighter right after a copy.
            let alpha = 0.32 + 0.38 * self.copy_flash;
            let tint = theme::with_alpha(theme::accent(), alpha);
            let (sr, er) = if s.1 <= e.1 { (s.1, e.1) } else { (e.1, s.1) };
            for row_index in sr..=er {
                let Some(row) = self.snapshot.cells.get(row_index) else { break };
                for cell in row {
                    if cell.wide_spacer {
                        continue;
                    }
                    if in_selection(cell.col, row_index, s, e) {
                        overlay.fill_rectangle(
                            Point::new(
                                cell.col as f32 * m.cell_width,
                                row_index as f32 * m.line_height,
                            ),
                            Size::new(cell_draw_width(cell, m), m.line_height),
                            tint,
                        );
                    }
                }
            }
        }

        // Inline ghost suggestion: draw the suffix starting at the cursor, in a
        // dimmed accent so it reads as "not yet typed". Painted after the grid
        // so it overlays the (blank) cells to the right of the cursor, and never
        // injected into the PTY. Clipped to the grid width.
        if want_ghost {
            let cols = self.snapshot.size.cols as usize;
            let row = self.snapshot.cursor.row;
            let mut col = self.snapshot.cursor.col;
            let y = row as f32 * m.line_height;
            let ghost = theme::with_alpha(theme::text_muted(), 0.55);
            for ch in self.inline_suggestion.chars() {
                if col >= cols {
                    break;
                }
                let x = col as f32 * m.cell_width;
                if ch != ' ' {
                    overlay.fill_text(Text {
                        content: ch.to_string(),
                        position: Point::new(x, y),
                        max_width: m.cell_width,
                        color: ghost,
                        size: f32::from(self.font_size).into(),
                        line_height: LineHeight::Absolute(m.line_height.into()),
                        font: font_for(ch, false),
                        align_x: alignment::Horizontal::Left.into(),
                        align_y: alignment::Vertical::Top,
                        shaping: iced::widget::text::Shaping::Advanced,
                    });
                }
                col += 1;
            }
        }

        vec![grid, overlay.into_geometry()]
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

impl TerminalCanvas<'_> {
    /// Paint the full cell grid — backgrounds, cursor, search highlights,
    /// glyphs, underlines — into `frame`. Selection is deliberately absent:
    /// it is drawn in the per-frame overlay so drag updates don't invalidate
    /// this (cached) geometry.
    fn draw_grid(&self, frame: &mut Frame, m: Metrics) {
        // Compute search-match highlights: a set of matched cells, plus the
        // cells belonging to the "current" match (emphasized differently).
        // This only runs on a cache rebuild, never on an idle redraw.
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
                // Only a Block cursor inverts the whole cell; Underline/Beam
                // leave the glyph untouched and add a thin bar instead.
                let block_cursor =
                    cursor_here && self.cursor_shape == crate::theme::CursorShape::Block;
                let inverse = cell.inverse || block_cursor;
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

                // Underline / Beam cursor bars (drawn whether or not the cell is
                // blank, so the cursor is visible on an empty line).
                if cursor_here && !block_cursor {
                    draw_cursor_bar(frame, self.cursor_shape, x, y, cell_draw_width(cell, m), m.line_height);
                }

                if cell.ch == ' ' {
                    // A space under a Block cursor still needs the inverse fill.
                    if block_cursor {
                        frame.fill_rectangle(
                            Point::new(x, y),
                            Size::new(cell_draw_width(cell, m), m.line_height),
                            theme::accent_strong(),
                        );
                    }
                    continue;
                }

                let fg = if is_match {
                    Color::from_rgb(0.05, 0.05, 0.05)
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
