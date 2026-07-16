use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Color, NamedColor, Rgb};

/// Mouse-reporting state a remote app has requested via DECSET private modes.
/// When `report` is set, the UI must forward mouse events to the PTY as escape
/// sequences instead of doing local text selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseProtocol {
    /// Any button-event reporting is on (mode 1000/1002/1003).
    pub report: bool,
    /// Report motion while a button is held (mode 1002).
    pub button_motion: bool,
    /// Report all motion, even with no button held (mode 1003).
    pub any_motion: bool,
    /// SGR extended encoding (mode 1006). When false, use legacy X10.
    pub sgr: bool,
    /// Alternate-scroll: wheel becomes arrow keys on the alt screen (mode 1007).
    pub alternate_scroll: bool,
    /// The terminal is currently on the alternate screen.
    pub alt_screen: bool,
    /// Application cursor keys (DECCKM): arrows are ESC O A/B instead of ESC [ A/B.
    pub app_cursor: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: 100,
            rows: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalLine {
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCell {
    pub row: usize,
    pub col: usize,
    pub ch: char,
    pub wide: bool,
    pub wide_spacer: bool,
    pub inverse: bool,
    pub bold: bool,
    pub underline: bool,
    pub foreground: Option<TerminalColor>,
    pub background: Option<TerminalColor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCursor {
    pub row: usize,
    pub col: usize,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSnapshot {
    pub size: TerminalSize,
    pub cursor: TerminalCursor,
    pub cells: Vec<Vec<TerminalCell>>,
}

impl TerminalSnapshot {
    pub fn render_plain_text(&self) -> String {
        self.cells
            .iter()
            .enumerate()
            .map(|(row_index, row)| {
                let mut line = String::with_capacity(row.len());
                for cell in row {
                    if cell.wide_spacer {
                        continue;
                    }
                    if self.cursor.visible
                        && self.cursor.row == row_index
                        && self.cursor.col == cell.col
                    {
                        line.push(if cell.ch == ' ' { '█' } else { cell.ch });
                    } else {
                        line.push(cell.ch);
                    }
                }
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("terminal renderer is not implemented yet")]
    NotImplemented,
}

pub trait TerminalEngine {
    fn resize(&mut self, size: TerminalSize);
    fn write_remote_output(&mut self, bytes: &[u8]) -> Result<(), TerminalError>;
    fn snapshot(&self) -> TerminalSnapshot;
}

pub struct AlacrittyTerminalBuffer {
    size: TerminalSize,
    term: Term<VoidListener>,
    parser: ansi::Processor,
    /// Bumped on every mutation that changes what a snapshot would render
    /// (output, resize, scroll). Consumers cache the last snapshot keyed on
    /// this so an idle frame doesn't re-materialize the whole grid.
    generation: u64,
}

impl AlacrittyTerminalBuffer {
    pub fn new(size: TerminalSize) -> Self {
        let term_size = TermSize::new(usize::from(size.cols), usize::from(size.rows));
        let term = Term::new(Config::default(), &term_size, VoidListener);
        Self {
            size,
            term,
            parser: ansi::Processor::new(),
            generation: 0,
        }
    }

    /// Monotonic counter identifying the current grid state. See `generation`.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Scroll the viewport through scrollback history by `lines` (positive =
    /// toward older output, negative = toward newer). Used by the mouse wheel.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
        self.generation += 1;
    }

    /// Jump back to the live bottom of the buffer.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
        self.generation += 1;
    }

    /// How many lines the viewport is currently scrolled above the bottom
    /// (0 means pinned to the latest output).
    pub fn scroll_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// The mouse-reporting protocol the remote app has requested. Used by the
    /// UI to decide whether a click/drag/wheel should be forwarded to the PTY
    /// (vim, tmux, htop, less …) instead of driving local selection/scroll.
    pub fn mouse_protocol(&self) -> MouseProtocol {
        let mode = self.term.mode();
        MouseProtocol {
            report: mode.intersects(TermMode::MOUSE_MODE),
            button_motion: mode.contains(TermMode::MOUSE_DRAG),
            any_motion: mode.contains(TermMode::MOUSE_MOTION),
            sgr: mode.contains(TermMode::SGR_MOUSE),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            app_cursor: mode.contains(TermMode::APP_CURSOR),
        }
    }

    /// Returns the trimmed text of the line at the current cursor row.
    /// Used to capture the fully tab-completed command before Enter is sent.
    pub fn cursor_line_text(&self) -> String {
        // Read the single cursor row straight from the grid — no full-grid
        // snapshot needed. Wide-char spacers are skipped so CJK text doesn't
        // pick up a phantom space per glyph.
        let grid = self.term.grid();
        let row = &grid[grid.cursor.point.line];
        let cols = usize::from(self.size.cols);
        let mut text = String::with_capacity(cols);
        for col in 0..cols {
            let cell = &row[alacritty_terminal::index::Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            text.push(cell.c);
        }
        text.truncate(text.trim_end().len());
        text
    }
}

impl TerminalEngine for AlacrittyTerminalBuffer {
    fn resize(&mut self, size: TerminalSize) {
        self.size = size;
        self.term.resize(TermSize::new(
            usize::from(size.cols),
            usize::from(size.rows),
        ));
        self.generation += 1;
    }

    fn write_remote_output(&mut self, bytes: &[u8]) -> Result<(), TerminalError> {
        self.parser.advance(&mut self.term, bytes);
        self.generation += 1;
        Ok(())
    }

    fn snapshot(&self) -> TerminalSnapshot {
        let cols = usize::from(self.size.cols);
        let rows_count = usize::from(self.size.rows);
        let mut rows = (0..rows_count)
            .map(|row| {
                (0..cols)
                    .map(|col| TerminalCell {
                        row,
                        col,
                        ch: ' ',
                        wide: false,
                        wide_spacer: false,
                        inverse: false,
                        bold: false,
                        underline: false,
                        foreground: None,
                        background: None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        let colors = self.term.colors();
        // When scrolled into history, display lines are negative grid indices.
        // Shift by the display offset so line -offset maps to viewport row 0.
        let display_offset = self.term.grid().display_offset() as i32;
        for indexed in self.term.grid().display_iter() {
            let row = indexed.point.line.0 + display_offset;
            if row < 0 {
                continue;
            }
            let row = row as usize;
            let col = indexed.point.column.0;
            if row >= rows.len() || col >= cols {
                continue;
            }

            rows[row][col] = TerminalCell {
                row,
                col,
                ch: if indexed.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    ' '
                } else {
                    indexed.c
                },
                wide: indexed.flags.contains(Flags::WIDE_CHAR),
                wide_spacer: indexed.flags.contains(Flags::WIDE_CHAR_SPACER),
                inverse: indexed.flags.contains(Flags::INVERSE),
                bold: indexed.flags.contains(Flags::BOLD),
                underline: indexed.flags.intersects(Flags::ALL_UNDERLINES),
                foreground: resolve_terminal_color(indexed.fg, colors),
                background: resolve_terminal_color(indexed.bg, colors),
            };
        }

        let cursor_point = self.term.grid().cursor.point;
        // Apply the same offset; when scrolled into history the cursor falls
        // below the viewport and is hidden.
        let cursor_line = cursor_point.line.0 + display_offset;
        let cursor_visible = cursor_line >= 0 && (cursor_line as usize) < rows_count;
        let cursor_row = usize::try_from(cursor_line)
            .ok()
            .filter(|row| *row < rows_count)
            .unwrap_or(rows_count.saturating_sub(1));
        let cursor_col = cursor_point.column.0.min(cols.saturating_sub(1));

        TerminalSnapshot {
            size: self.size,
            cursor: TerminalCursor {
                row: cursor_row,
                col: cursor_col,
                visible: cursor_visible,
            },
            cells: rows,
        }
    }
}

#[derive(Debug)]
pub struct PlainTerminalBuffer {
    size: TerminalSize,
    lines: Vec<TerminalLine>,
    scrollback_limit: usize,
}

impl PlainTerminalBuffer {
    pub fn new(scrollback_limit: usize) -> Self {
        Self {
            size: TerminalSize::default(),
            lines: Vec::new(),
            scrollback_limit,
        }
    }

    pub fn visible_lines(&self) -> &[TerminalLine] {
        let rows = usize::from(self.size.rows);
        let start = self.lines.len().saturating_sub(rows);
        &self.lines[start..]
    }
}

impl TerminalEngine for PlainTerminalBuffer {
    fn resize(&mut self, size: TerminalSize) {
        self.size = size;
    }

    fn write_remote_output(&mut self, bytes: &[u8]) -> Result<(), TerminalError> {
        let text = String::from_utf8_lossy(bytes);
        for line in text.lines() {
            self.lines.push(TerminalLine {
                text: line.to_string(),
            });
        }
        if self.lines.len() > self.scrollback_limit {
            let excess = self.lines.len() - self.scrollback_limit;
            self.lines.drain(0..excess);
        }
        Ok(())
    }

    fn snapshot(&self) -> TerminalSnapshot {
        let rows_count = usize::from(self.size.rows);
        let cols = usize::from(self.size.cols);
        let visible = self.visible_lines();
        let mut cells = Vec::with_capacity(rows_count);

        for row in 0..rows_count {
            let text = visible
                .get(row)
                .map(|line| line.text.as_str())
                .unwrap_or("");
            let mut chars = text.chars();
            cells.push(
                (0..cols)
                    .map(|col| TerminalCell {
                        row,
                        col,
                        ch: chars.next().unwrap_or(' '),
                        wide: false,
                        wide_spacer: false,
                        inverse: false,
                        bold: false,
                        underline: false,
                        foreground: None,
                        background: None,
                    })
                    .collect(),
            );
        }

        TerminalSnapshot {
            size: self.size,
            cursor: TerminalCursor {
                row: visible
                    .len()
                    .saturating_sub(1)
                    .min(rows_count.saturating_sub(1)),
                col: 0,
                visible: false,
            },
            cells,
        }
    }
}

fn resolve_terminal_color(
    color: Color,
    palette: &alacritty_terminal::term::color::Colors,
) -> Option<TerminalColor> {
    match color {
        Color::Spec(rgb) => Some(terminal_color_from_rgb(rgb)),
        Color::Indexed(index) => Some(indexed_terminal_color(index)),
        Color::Named(NamedColor::Foreground)
        | Color::Named(NamedColor::Background)
        | Color::Named(NamedColor::Cursor)
        | Color::Named(NamedColor::BrightForeground)
        | Color::Named(NamedColor::DimForeground) => {
            palette[color_index(color)].map(terminal_color_from_rgb)
        }
        Color::Named(named) => palette[named]
            .map(terminal_color_from_rgb)
            .or_else(|| named_terminal_color(named)),
    }
}

fn color_index(color: Color) -> usize {
    match color {
        Color::Named(named) => named as usize,
        Color::Indexed(index) => usize::from(index),
        Color::Spec(_) => 0,
    }
}

fn terminal_color_from_rgb(rgb: Rgb) -> TerminalColor {
    TerminalColor {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    }
}

fn named_terminal_color(named: NamedColor) -> Option<TerminalColor> {
    let (r, g, b) = match named {
        NamedColor::Black => (0x1f, 0x23, 0x2b),
        NamedColor::Red => (0xf7, 0x76, 0x8e),
        NamedColor::Green => (0x9e, 0xce, 0x6a),
        NamedColor::Yellow => (0xe0, 0xaf, 0x68),
        NamedColor::Blue => (0x7a, 0xa2, 0xf7),
        NamedColor::Magenta => (0xbb, 0x9a, 0xf7),
        NamedColor::Cyan => (0x7d, 0xcf, 0xf7),
        NamedColor::White => (0xc0, 0xca, 0xf5),
        NamedColor::BrightBlack | NamedColor::DimBlack => (0x56, 0x5f, 0x89),
        NamedColor::BrightRed | NamedColor::DimRed => (0xff, 0x7a, 0x93),
        NamedColor::BrightGreen | NamedColor::DimGreen => (0xb9, 0xf2, 0x7c),
        NamedColor::BrightYellow | NamedColor::DimYellow => (0xff, 0xc7, 0x77),
        NamedColor::BrightBlue | NamedColor::DimBlue => (0x7d, 0xa6, 0xff),
        NamedColor::BrightMagenta | NamedColor::DimMagenta => (0xc0, 0xa0, 0xff),
        NamedColor::BrightCyan | NamedColor::DimCyan => (0x86, 0xe1, 0xfc),
        NamedColor::BrightWhite | NamedColor::DimWhite => (0xff, 0xff, 0xff),
        _ => return None,
    };
    Some(TerminalColor { r, g, b })
}

fn indexed_terminal_color(index: u8) -> TerminalColor {
    match index {
        0..=15 => named_terminal_color(match index {
            0 => NamedColor::Black,
            1 => NamedColor::Red,
            2 => NamedColor::Green,
            3 => NamedColor::Yellow,
            4 => NamedColor::Blue,
            5 => NamedColor::Magenta,
            6 => NamedColor::Cyan,
            7 => NamedColor::White,
            8 => NamedColor::BrightBlack,
            9 => NamedColor::BrightRed,
            10 => NamedColor::BrightGreen,
            11 => NamedColor::BrightYellow,
            12 => NamedColor::BrightBlue,
            13 => NamedColor::BrightMagenta,
            14 => NamedColor::BrightCyan,
            _ => NamedColor::BrightWhite,
        })
        .unwrap_or(TerminalColor {
            r: 0xff,
            g: 0xff,
            b: 0xff,
        }),
        16..=231 => {
            let value = index - 16;
            let channel = |level: u8| if level == 0 { 0 } else { 55 + level * 40 };
            TerminalColor {
                r: channel(value / 36),
                g: channel((value % 36) / 6),
                b: channel(value % 6),
            }
        }
        232..=255 => {
            let level = 8 + (index - 232) * 10;
            TerminalColor {
                r: level,
                g: level,
                b: level,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_scrollback() {
        let mut buffer = PlainTerminalBuffer::new(2);
        buffer.write_remote_output(b"one\ntwo\nthree\n").unwrap();

        let lines: Vec<_> = buffer
            .visible_lines()
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(lines, ["two", "three"]);
    }

    #[test]
    fn alacritty_scrollback_reveals_older_lines() {
        // A 4-row viewport; write 12 numbered lines so 8 scroll into history.
        let mut buffer = AlacrittyTerminalBuffer::new(TerminalSize { cols: 20, rows: 4 });
        for i in 1..=12 {
            buffer
                .write_remote_output(format!("line{i}\r\n").as_bytes())
                .unwrap();
        }

        // At the bottom, the latest lines are visible and offset is zero.
        assert_eq!(buffer.scroll_offset(), 0);
        let bottom = buffer.snapshot().render_plain_text();
        assert!(bottom.contains("line12"), "bottom should show newest line");

        // Scroll up into history; an older line that was off-screen appears.
        buffer.scroll(6);
        assert!(buffer.scroll_offset() > 0, "offset should track scroll-up");
        let scrolled = buffer.snapshot().render_plain_text();
        assert!(
            scrolled.contains("line5") || scrolled.contains("line6"),
            "scrolled view should reveal older lines, got: {scrolled:?}"
        );

        // Back to bottom restores the live tail.
        buffer.scroll_to_bottom();
        assert_eq!(buffer.scroll_offset(), 0);
        assert!(buffer.snapshot().render_plain_text().contains("line12"));
    }

    #[test]
    fn alacritty_buffer_handles_carriage_return_and_ansi_clear() {
        let mut buffer = AlacrittyTerminalBuffer::new(TerminalSize { cols: 20, rows: 3 });

        buffer
            .write_remote_output(b"hello\rworld\x1b[2K\nnext")
            .unwrap();

        let text = buffer.snapshot().render_plain_text();

        assert!(text.contains("next"));
        assert!(!text.contains("\x1b"));
    }

    #[test]
    fn generation_tracks_mutations() {
        let mut buffer = AlacrittyTerminalBuffer::new(TerminalSize { cols: 20, rows: 4 });
        let g0 = buffer.generation();

        buffer.write_remote_output(b"hi").unwrap();
        let g1 = buffer.generation();
        assert!(g1 > g0, "output must bump the generation");

        buffer.scroll(2);
        assert!(buffer.generation() > g1, "scroll must bump the generation");

        let g2 = buffer.generation();
        buffer.resize(TerminalSize { cols: 30, rows: 5 });
        assert!(buffer.generation() > g2, "resize must bump the generation");
    }

    #[test]
    fn cursor_line_text_reads_cursor_row_and_skips_wide_spacers() {
        let mut buffer = AlacrittyTerminalBuffer::new(TerminalSize { cols: 20, rows: 4 });
        buffer
            .write_remote_output("first\r\n$ vim 中文.txt".as_bytes())
            .unwrap();
        assert_eq!(buffer.cursor_line_text(), "$ vim 中文.txt");
    }

    #[test]
    fn alacritty_snapshot_preserves_columns_after_cursor_movement() {
        let mut buffer = AlacrittyTerminalBuffer::new(TerminalSize { cols: 8, rows: 2 });

        buffer.write_remote_output(b"ab\x1b[1;5Hxy").unwrap();

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.cells[0][0].ch, 'a');
        assert_eq!(snapshot.cells[0][1].ch, 'b');
        assert_eq!(snapshot.cells[0][2].ch, ' ');
        assert_eq!(snapshot.cells[0][4].ch, 'x');
        assert_eq!(snapshot.cells[0][5].ch, 'y');
    }

    #[test]
    fn alacritty_snapshot_tracks_cursor_position() {
        let mut buffer = AlacrittyTerminalBuffer::new(TerminalSize { cols: 10, rows: 3 });

        buffer.write_remote_output(b"\x1b[2;4H").unwrap();

        let cursor = buffer.snapshot().cursor;
        assert_eq!(cursor.row, 1);
        assert_eq!(cursor.col, 3);
        assert!(cursor.visible);
    }

    #[test]
    fn alacritty_snapshot_marks_wide_spacer_and_plain_text_skips_it() {
        let mut buffer = AlacrittyTerminalBuffer::new(TerminalSize { cols: 10, rows: 2 });

        buffer.write_remote_output("中a\r\n".as_bytes()).unwrap();

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.cells[0][0].ch, '中');
        assert!(snapshot.cells[0][0].wide);
        assert!(!snapshot.cells[0][0].wide_spacer);
        assert_eq!(snapshot.cells[0][1].ch, ' ');
        assert!(snapshot.cells[0][1].wide_spacer);
        assert_eq!(snapshot.cells[0][2].ch, 'a');
        assert_eq!(snapshot.render_plain_text(), "中a\n█");
    }

    #[test]
    fn mouse_protocol_tracks_private_modes() {
        let mut buffer = AlacrittyTerminalBuffer::new(TerminalSize { cols: 20, rows: 4 });

        // Defaults: no reporting, alternate-scroll on, primary screen.
        let p = buffer.mouse_protocol();
        assert!(!p.report);
        assert!(p.alternate_scroll, "alternate scroll should default on");
        assert!(!p.alt_screen);

        // vim-like startup: alt screen + app cursor + SGR mouse drag reporting.
        buffer
            .write_remote_output(b"\x1b[?1049h\x1b[?1h\x1b[?1002h\x1b[?1006h")
            .unwrap();
        let p = buffer.mouse_protocol();
        assert!(p.report, "1002 should enable reporting");
        assert!(p.button_motion);
        assert!(p.sgr, "1006 should select SGR encoding");
        assert!(p.alt_screen, "1049 should switch to alt screen");
        assert!(p.app_cursor, "DECCKM should set app cursor");

        // Teardown restores the primary screen and disables reporting.
        buffer
            .write_remote_output(b"\x1b[?1002l\x1b[?1006l\x1b[?1l\x1b[?1049l")
            .unwrap();
        let p = buffer.mouse_protocol();
        assert!(!p.report);
        assert!(!p.alt_screen);
        assert!(!p.app_cursor);
    }

    #[test]
    fn alacritty_snapshot_preserves_ansi_colors() {
        let mut buffer = AlacrittyTerminalBuffer::new(TerminalSize { cols: 10, rows: 2 });

        buffer
            .write_remote_output(b"\x1b[31;44mA\x1b[0m \x1b[38;2;1;2;3mB")
            .unwrap();

        let snapshot = buffer.snapshot();
        assert_eq!(
            snapshot.cells[0][0].foreground,
            named_terminal_color(NamedColor::Red)
        );
        assert_eq!(
            snapshot.cells[0][0].background,
            named_terminal_color(NamedColor::Blue)
        );
        assert_eq!(
            snapshot.cells[0][2].foreground,
            Some(TerminalColor { r: 1, g: 2, b: 3 })
        );
    }
}
