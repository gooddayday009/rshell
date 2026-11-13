pub use alacritty_terminal::index::Side;
pub use alacritty_terminal::term::{TermMode, cell::Flags};
use alacritty_terminal::{
    Term,
    event::{Event, EventListener, WindowSize},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point},
    selection::{Selection, SelectionType},
    term::{Config, Osc52},
    vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb},
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}
impl Dimensions for Size {
    fn columns(&self) -> usize {
        self.cols
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn total_lines(&self) -> usize {
        self.rows
    }
}

#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<Event>>>);
impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        // Wakeups and clipboard requests do not need to accumulate.
        if matches!(
            event,
            Event::PtyWrite(_) | Event::TextAreaSizeRequest(_) | Event::ColorRequest(_, _)
        ) {
            self.0.lock().unwrap().push(event);
        }
    }
}

pub struct Cell {
    pub row: usize,
    pub col: usize,
    pub text: String,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub fg_index: Option<usize>,
    pub bg_index: Option<usize>,
    pub flags: Flags,
    pub selected: bool,
}

pub struct Snapshot {
    pub cells: Vec<Cell>,
    pub cursor: Option<(usize, usize)>,
    pub cols: usize,
    pub rows: usize,
    pub mode: TermMode,
    pub scroll_offset: usize,
    pub history_size: usize,
}

pub struct Terminal {
    term: Term<Listener>,
    parser: Processor,
    listener: Listener,
    size: Size,
    history_limit: usize,
    snapshot_cache: std::sync::OnceLock<Arc<Snapshot>>,
}

impl Terminal {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_history(cols, rows, 5000)
    }

    pub fn with_history(cols: u16, rows: u16, lines: usize) -> Self {
        let lines = lines.min(100_000);
        let size = Size {
            cols: cols.max(2) as usize,
            rows: rows.max(1) as usize,
        };
        let listener = Listener::default();
        let config = Config {
            scrolling_history: lines,
            osc52: Osc52::Disabled,
            ..Default::default()
        };
        Self {
            term: Term::new(config, &size, listener.clone()),
            parser: Processor::new(),
            listener,
            size,
            history_limit: lines,
            snapshot_cache: std::sync::OnceLock::new(),
        }
    }

    pub fn set_history_limit(&mut self, lines: usize) {
        self.snapshot_cache.take();
        let lines = lines.min(100_000);
        if self.history_limit == lines {
            return;
        }
        if lines < self.history_limit {
            self.term.selection = None;
        }
        self.term.set_options(Config {
            scrolling_history: lines,
            osc52: Osc52::Disabled,
            ..Default::default()
        });
        self.history_limit = lines;
    }

    /// Leave remote input modes without clearing the normal screen or its history.
    pub fn resume_prompt(&mut self) {
        self.parser = Processor::new();
        self.feed(b"\x1b[?1049l\x1b7\x1b[r\x1b8\x1b[0m\x1b[?1l\x1b[?2004l\x1b[?25h");
    }

    /// Save the current screen in bounded scrollback before a new connection can clear it.
    pub fn archive_screen(&mut self) {
        self.resume_prompt();
        self.feed(format!("\x1b[{};1H", self.size.rows).as_bytes());
        for _ in 0..self.size.rows {
            self.feed(b"\r\n");
        }
        self.feed(b"\x1b[H");
        self.bottom();
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.snapshot_cache.take();
        let mut replies = Vec::new();
        for chunk in bytes.chunks(8192) {
            self.parser.advance(&mut self.term, chunk);
            for event in self.listener.0.lock().unwrap().drain(..) {
                let text = match event {
                    Event::PtyWrite(text) => text,
                    Event::TextAreaSizeRequest(format) => format(WindowSize {
                        num_lines: self.size.rows as u16,
                        num_cols: self.size.cols as u16,
                        cell_width: 0,
                        cell_height: 0,
                    }),
                    Event::ColorRequest(index, format) => {
                        let [r, g, b] = palette(index);
                        format(Rgb { r, g, b })
                    }
                    _ => continue,
                };
                replies.extend_from_slice(text.as_bytes());
            }
        }
        replies
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.snapshot_cache.take();
        self.size = Size {
            cols: cols.max(2) as usize,
            rows: rows.max(1) as usize,
        };
        self.term.resize(self.size);
    }

    pub fn scroll(&mut self, lines: i32) {
        self.snapshot_cache.take();
        self.term.scroll_display(Scroll::Delta(lines));
    }
    pub fn bottom(&mut self) {
        self.snapshot_cache.take();
        self.term.scroll_display(Scroll::Bottom);
    }

    pub fn clear_screen(&mut self) {
        self.term.selection = None;
        // Clear the local display and scrollback without sending bytes to the session.
        self.feed(b"\x1b[2J\x1b[3J\x1b[H");
        self.bottom();
    }

    fn point(&self, row: usize, col: usize) -> Point {
        Point::new(
            Line(row.min(self.size.rows - 1) as i32 - self.term.grid().display_offset() as i32),
            Column(col.min(self.size.cols - 1)),
        )
    }

    pub fn start_selection(&mut self, row: usize, col: usize, side: Side) {
        self.snapshot_cache.take();
        self.term.selection = Some(Selection::new(
            SelectionType::Simple,
            self.point(row, col),
            side,
        ));
    }
    pub fn extend_selection(&mut self, row: usize, col: usize, side: Side) {
        self.snapshot_cache.take();
        let point = self.point(row, col);
        if let Some(selection) = &mut self.term.selection {
            selection.update(point, side);
        }
    }
    pub fn selection_text(&self) -> String {
        self.term.selection_to_string().unwrap_or_default()
    }

    pub fn clear_selection(&mut self) {
        self.snapshot_cache.take();
        self.term.selection = None;
    }

    /// Search complete logical lines across the bounded scrollback, including soft wraps.
    pub fn find(
        &mut self,
        query: &str,
        after: Option<(i32, usize)>,
        backwards: bool,
    ) -> Option<(i32, usize)> {
        if query.is_empty() {
            return None;
        }
        let mut matches = Vec::new();
        let mut text = String::new();
        let mut positions = Vec::new();
        for line in self.term.grid().topmost_line().0..=self.term.grid().bottommost_line().0 {
            for col in 0..self.size.cols {
                let cell = &self.term.grid()[Point::new(Line(line), Column(col))];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                positions.push((text.len(), (line, col)));
                text.push(cell.c);
                if let Some(extra) = cell.zerowidth() {
                    text.extend(extra);
                }
            }
            let wrapped = self.term.grid()[Point::new(Line(line), Column(self.size.cols - 1))]
                .flags
                .contains(Flags::WRAPLINE);
            if !wrapped || line == self.term.grid().bottommost_line().0 {
                for (offset, _) in text.match_indices(query) {
                    if let Some((_, point)) =
                        positions.iter().rev().find(|(start, _)| *start <= offset)
                    {
                        matches.push(*point);
                    }
                }
                text.clear();
                positions.clear();
            }
        }
        let point = if backwards {
            matches
                .iter()
                .rev()
                .find(|p| after.is_none_or(|anchor| **p < anchor))
                .or_else(|| matches.last())
        } else {
            matches
                .iter()
                .find(|p| after.is_none_or(|anchor| **p > anchor))
                .or_else(|| matches.first())
        }
        .copied()?;
        self.snapshot_cache.take();
        self.term
            .scroll_to_point(Point::new(Line(point.0), Column(point.1)));
        Some(point)
    }

    /// Reuse the immutable visible grid until terminal state changes.
    pub fn cached_snapshot(&self) -> Arc<Snapshot> {
        self.snapshot_cache
            .get_or_init(|| Arc::new(self.snapshot()))
            .clone()
    }

    pub fn snapshot(&self) -> Snapshot {
        let content = self.term.renderable_content();
        let offset = content.display_offset;
        let cursor_row = content.cursor.point.line.0 + offset as i32;
        let cursor = if content.cursor.shape != CursorShape::Hidden
            && (0..self.size.rows as i32).contains(&cursor_row)
        {
            Some((cursor_row as usize, content.cursor.point.column.0))
        } else {
            None
        };
        let cells = content
            .display_iter
            .map(|indexed| {
                let cell = indexed.cell;
                let mut fg = resolve(cell.fg);
                let mut bg = resolve(cell.bg);
                let mut fg_index = color_index(cell.fg);
                let mut bg_index = color_index(cell.bg);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                    std::mem::swap(&mut fg_index, &mut bg_index);
                }
                if cell.flags.contains(Flags::DIM) {
                    fg = fg.map(|v| (v as f32 * 0.65) as u8);
                }
                let mut text = cell.c.to_string();
                if let Some(extra) = cell.zerowidth() {
                    text.extend(extra);
                }
                Cell {
                    row: (indexed.point.line.0 + offset as i32) as usize,
                    col: indexed.point.column.0,
                    text,
                    fg,
                    bg,
                    fg_index,
                    bg_index,
                    flags: cell.flags,
                    selected: content
                        .selection
                        .as_ref()
                        .is_some_and(|s| s.contains(indexed.point)),
                }
            })
            .collect();
        Snapshot {
            cells,
            cursor,
            cols: self.size.cols,
            rows: self.size.rows,
            mode: content.mode,
            scroll_offset: offset,
            history_size: self.term.grid().history_size(),
        }
    }
}

fn resolve(color: Color) -> [u8; 3] {
    match color {
        Color::Spec(Rgb { r, g, b }) => [r, g, b],
        Color::Indexed(index) => palette(index as usize),
        Color::Named(name) => palette(name as usize),
    }
}

fn color_index(color: Color) -> Option<usize> {
    match color {
        Color::Spec(_) => None,
        Color::Indexed(index) => Some(index as usize),
        Color::Named(name) => Some(name as usize),
    }
}

fn palette(index: usize) -> [u8; 3] {
    const ANSI: [[u8; 3]; 16] = [
        [0, 0, 0],       // black
        [205, 0, 0],     // red3
        [0, 205, 0],     // green3
        [205, 205, 0],   // yellow3
        [0, 0, 238],     // blue2
        [205, 0, 205],   // magenta3
        [0, 205, 205],   // cyan3
        [229, 229, 229], // gray90
        [127, 127, 127], // gray50
        [255, 0, 0],     // red
        [0, 255, 0],     // green
        [255, 255, 0],   // yellow
        [92, 92, 255],   // bright blue
        [255, 0, 255],   // magenta
        [0, 255, 255],   // cyan
        [255, 255, 255], // white
    ];
    match index {
        0..=15 => ANSI[index],
        16..=231 => {
            let i = index - 16;
            let value = |n: usize| if n == 0 { 0 } else { 55 + n as u8 * 40 };
            [value(i / 36), value(i / 6 % 6), value(i % 6)]
        }
        232..=255 => [8 + (index as u8 - 232) * 10; 3],
        n if n == NamedColor::Background as usize => [0, 0, 0],
        _ => [229, 229, 229],
    }
}

pub fn xterm_color(index: usize) -> [u8; 3] {
    palette(index)
}

pub fn background_color_index() -> usize {
    NamedColor::Background as usize
}

pub fn paste(text: &str, mode: TermMode) -> Vec<u8> {
    // Clipboard text must not send invisible terminal commands or escape paste mode.
    let text: String = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(*c, '\n' | '\t'))
        .collect();
    if mode.contains(TermMode::BRACKETED_PASTE) {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.replace('\n', "\r").into_bytes()
    }
}

pub fn key(name: &str, ctrl: bool, alt: bool, shift: bool, mode: TermMode) -> Option<Vec<u8>> {
    let name = match name {
        "Up" => "ArrowUp",
        "Down" => "ArrowDown",
        "Left" => "ArrowLeft",
        "Right" => "ArrowRight",
        other => other,
    };
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let bytes = match name {
        "Enter" => vec![b'\r'],
        "Backspace" => vec![127],
        "Escape" => vec![27],
        "Tab" if shift => b"\x1b[Z".to_vec(),
        "Tab" => vec![b'\t'],
        "ArrowUp" | "ArrowDown" | "ArrowRight" | "ArrowLeft" | "Home" | "End" => {
            let code = match name {
                "ArrowUp" => 'A',
                "ArrowDown" => 'B',
                "ArrowRight" => 'C',
                "ArrowLeft" => 'D',
                "Home" => 'H',
                _ => 'F',
            };
            if modifier > 1 {
                format!("\x1b[1;{modifier}{code}").into_bytes()
            } else {
                format!(
                    "\x1b{}{code}",
                    if mode.contains(TermMode::APP_CURSOR) {
                        'O'
                    } else {
                        '['
                    }
                )
                .into_bytes()
            }
        }
        "Insert" | "Delete" | "PageUp" | "PageDown" | "F5" | "F6" | "F7" | "F8" | "F9" | "F10"
        | "F11" | "F12" => {
            let n = match name {
                "Insert" => 2,
                "Delete" => 3,
                "PageUp" => 5,
                "PageDown" => 6,
                "F5" => 15,
                "F6" => 17,
                "F7" => 18,
                "F8" => 19,
                "F9" => 20,
                "F10" => 21,
                "F11" => 23,
                _ => 24,
            };
            if modifier > 1 {
                format!("\x1b[{n};{modifier}~").into_bytes()
            } else {
                format!("\x1b[{n}~").into_bytes()
            }
        }
        "F1" | "F2" | "F3" | "F4" => {
            let c = match name {
                "F1" => 'P',
                "F2" => 'Q',
                "F3" => 'R',
                _ => 'S',
            };
            if modifier > 1 {
                format!("\x1b[1;{modifier}{c}").into_bytes()
            } else {
                format!("\x1bO{c}").into_bytes()
            }
        }
        "Space" | "2" if ctrl => vec![0],
        "OpenBracket" if ctrl => vec![27],
        "Backslash" if ctrl => vec![28],
        "CloseBracket" if ctrl => vec![29],
        "6" if ctrl => vec![30],
        "Minus" if ctrl => vec![31],
        s if ctrl && s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() => {
            vec![s.as_bytes()[0].to_ascii_uppercase() - b'A' + 1]
        }
        s if alt && !ctrl && s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() => {
            vec![if shift {
                s.as_bytes()[0].to_ascii_uppercase()
            } else {
                s.as_bytes()[0].to_ascii_lowercase()
            }]
        }
        s if alt && !ctrl && !shift && s.len() == 1 && s.as_bytes()[0].is_ascii_digit() => {
            vec![s.as_bytes()[0]]
        }
        "Period" if alt && !ctrl => vec![if shift { b'>' } else { b'.' }],
        "Minus" if alt && !ctrl => vec![if shift { b'_' } else { b'-' }],
        _ => return None,
    };
    if alt && !bytes.starts_with(&[27]) {
        let mut out = vec![27];
        out.extend(bytes);
        Some(out)
    } else {
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_snapshot_tracks_mutations_and_preserves_old_frame() {
        let mut t = Terminal::new(20, 3);
        let first = t.cached_snapshot();
        assert!(Arc::ptr_eq(&first, &t.cached_snapshot()));
        t.feed("abc中e\u{301}".as_bytes());
        let written = t.cached_snapshot();
        assert!(!Arc::ptr_eq(&first, &written));
        assert_eq!(first.cells[0].text, " ");
        assert_eq!(written.cells[3].text, "中");
        t.start_selection(0, 0, Side::Left);
        t.extend_selection(0, 2, Side::Right);
        assert!(t.cached_snapshot().cells[0].selected);
        t.clear_selection();
        assert!(!t.cached_snapshot().cells[0].selected);
        t.feed(b"\r\n1\r\n2\r\n3\r\n4");
        t.scroll(1);
        assert_eq!(t.cached_snapshot().scroll_offset, 1);
        t.bottom();
        assert_eq!(t.cached_snapshot().scroll_offset, 0);
        t.set_history_limit(0);
        assert_eq!(t.cached_snapshot().history_size, 0);
        t.resize(30, 5);
        assert_eq!(t.cached_snapshot().cols, 30);
        t.clear_screen();
        assert!(t.cached_snapshot().cells.iter().all(|c| c.text == " "));
        assert!(Arc::ptr_eq(&t.cached_snapshot(), &t.cached_snapshot()));
    }

    #[test]
    fn split_utf8_and_ansi_survive_network_boundaries() {
        let mut t = Terminal::new(20, 4);
        for byte in "\x1b[31m你好\x1b[0m".as_bytes() {
            t.feed(&[*byte]);
        }
        let s = t.snapshot();
        assert_eq!(s.cells[0].text, "你");
        assert_eq!(s.cells[2].text, "好");
        assert_eq!(s.cells[0].fg, palette(1));
    }
    #[test]
    fn xterm_palette_applies_to_ansi_and_indexed_colors() {
        assert_eq!(palette(1), [205, 0, 0]);
        assert_eq!(palette(12), [92, 92, 255]);
        assert_eq!(palette(196), [255, 0, 0]);
        let mut terminal = Terminal::new(20, 2);
        terminal.feed(b"\x1b[31mR\x1b[94mB\x1b[0mN");
        let cells = terminal.snapshot().cells;
        assert_eq!(cells[0].fg, [205, 0, 0]);
        assert_eq!(cells[1].fg, [92, 92, 255]);
        assert_eq!(cells[2].fg, [229, 229, 229]);
    }
    #[test]
    fn snapshot_keeps_explicit_rgb_distinct_from_ansi_color() {
        let mut terminal = Terminal::new(20, 2);
        terminal.feed(b"\x1b[31mA\x1b[38;2;205;0;0mB\x1b[0m");
        let cells = terminal.snapshot().cells;
        assert_eq!(cells[0].fg, cells[1].fg);
        assert_eq!(cells[0].fg_index, Some(1));
        assert_eq!(cells[1].fg_index, None);
    }
    #[test]
    fn device_query_replies_and_alternate_screen_restore() {
        let mut t = Terminal::new(20, 4);
        t.feed(b"hello");
        assert_eq!(t.feed(b"\x1b[6n"), b"\x1b[1;6R");
        t.feed(b"\x1b[?1049hother\x1b[?1049l");
        assert_eq!(t.snapshot().cells[0].text, "h");
    }
    #[test]
    fn application_cursor_and_control_keys() {
        assert_eq!(
            key("ArrowUp", false, false, false, TermMode::APP_CURSOR).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            key("C", true, false, false, TermMode::empty()).unwrap(),
            vec![3]
        );
    }
    #[test]
    fn clipboard_cannot_inject_bracketed_paste_terminator() {
        assert_eq!(
            paste("a\x1b[201~\nb", TermMode::BRACKETED_PASTE),
            b"\x1b[200~a[201~\nb\x1b[201~"
        );
    }
    #[test]
    fn clipboard_control_characters_cannot_change_terminal_input() {
        let input = "echo safe\u{8}\u{7}\u{3}\u{4}\u{1b}[201~\u{7f}\rnext";
        assert_eq!(paste(input, TermMode::empty()), b"echo safe[201~\rnext");
        assert_eq!(
            paste(input, TermMode::BRACKETED_PASTE),
            b"\x1b[200~echo safe[201~\nnext\x1b[201~"
        );
    }
    #[test]
    fn search_finds_history_and_wraps_across_terminal_rows() {
        let mut t = Terminal::new(8, 2);
        t.feed(b"abcdefghijk\r\nsecond\r\nthird\r\nfourth\r\n");
        let found = t.find("ghijk", None, false).unwrap();
        assert!(found.0 < 0);
        assert!(t.snapshot().scroll_offset > 0);
        assert!(t.find("not-present", None, false).is_none());
    }
    #[test]
    fn clear_screen_removes_visible_text_and_scrollback() {
        let mut t = Terminal::new(8, 2);
        t.feed(b"first\r\nsecond\r\nthird");
        assert!(t.snapshot().history_size > 0);
        t.clear_screen();
        let snapshot = t.snapshot();
        assert_eq!(snapshot.history_size, 0);
        assert_eq!(snapshot.scroll_offset, 0);
        assert!(snapshot.cells.iter().all(|cell| cell.text == " "));
        t.feed(b"new");
        assert!(t.snapshot().cells.iter().any(|cell| cell.text == "n"));
    }
    #[test]
    fn history_limit_resizes_live_and_alternate_screen_without_resetting_display() {
        let mut terminal = Terminal::with_history(20, 2, 8);
        for _ in 0..30 {
            terminal.feed(b"line\r\n");
        }
        assert_eq!(terminal.snapshot().history_size, 8);
        terminal.set_history_limit(16);
        for _ in 0..30 {
            terminal.feed(b"new\r\n");
        }
        assert_eq!(terminal.snapshot().history_size, 16);
        terminal.feed(b"keep");
        terminal.set_history_limit(3);
        assert_eq!(terminal.snapshot().history_size, 3);
        assert!(
            terminal
                .snapshot()
                .cells
                .iter()
                .any(|cell| cell.text == "k")
        );
        terminal.feed(b"\x1b[?1049h");
        terminal.set_history_limit(1);
        assert_eq!(terminal.snapshot().history_size, 0);
        terminal.feed(b"\x1b[?1049l");
        assert_eq!(terminal.snapshot().history_size, 1);
        terminal.set_history_limit(0);
        for _ in 0..30 {
            terminal.feed(b"line\r\n");
        }
        assert_eq!(terminal.snapshot().history_size, 0);
        terminal.set_history_limit(usize::MAX);
        assert_eq!(terminal.history_limit, 100_000);
    }
}
