//! A terminal's state, with no GPUI in it: `alacritty_terminal`'s grid and parser behind one
//! type the session's pump feeds (off the UI thread) and the view reads (briefly, under the lock).
//!
//! - **Bytes in** ([`Emulator::feed`]): the parser updates the grid, the scrollback (bounded:
//!   [`SCROLLBACK`] lines), the modes (alternate screen, mouse reporting, bracketed paste,
//!   application cursor keys, focus reporting), the title (OSC 0/2) and the bell. What the emulator
//!   must answer (the pseudo-console's cursor-position question at start, colour and size queries)
//!   collects in [`Emulator::take_replies`] for the pump to write back. The replies are fixed
//!   formats with numbers in them (positions, sizes, colours, modes): nothing a program printed is
//!   ever echoed back as input (no title report), and OSC 52 (the clipboard) is refused both ways.
//! - **The screen out** ([`Emulator::update_screen`]): the visible rows as resolved cells (colours
//!   from [`TermColors`], attributes), rebuilt only where the grid says it changed (its damage), or
//!   whole after a scroll, a resize or a theme change. The view keeps the [`Screen`] between frames.
//! - **Selection** (Alacritty's: simple, word, line; extended by a later point) and its text.
//!
//! Nothing here blocks or allocates per byte; the view's lock on it is held for a screen update
//! (rows × columns cell copies at most).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Osc52, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, Processor, StdSyncHandler};

use super::colors::{TermColors, dim};
use crate::design::Color;

/// Lines kept above the screen. 10,000 (Alacritty's default) at 120 columns is ~29 MB of cells,
/// the bound a terminal's memory stays under however much a program prints.
pub const SCROLLBACK: usize = 10_000;

/// The grid size the emulator was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Collects what the grid tells its owner (titles, the bell, answers for the program).
#[derive(Clone, Default)]
pub struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if !matches!(event, Event::MouseCursorDirty | Event::Wakeup) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event);
        }
    }
}

/// A cell's attributes as drawn.
pub mod attrs {
    pub const BOLD: u16 = 1;
    pub const ITALIC: u16 = 1 << 1;
    pub const UNDERLINE: u16 = 1 << 2;
    pub const DOUBLE_UNDERLINE: u16 = 1 << 3;
    pub const UNDERCURL: u16 = 1 << 4;
    pub const STRIKE: u16 = 1 << 5;
    /// The first cell of a double-width character.
    pub const WIDE: u16 = 1 << 6;
    /// The second cell of a double-width character (nothing is drawn in it).
    pub const SPACER: u16 = 1 << 7;
    /// SGR 2 (the colour is already dimmed).
    pub const DIM: u16 = 1 << 8;
    /// An OSC 8 hyperlink.
    pub const LINK: u16 = 1 << 9;
}

/// One cell as drawn: colours resolved (inverse, dim and hidden applied).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub c: char,
    /// Combining marks after it, if any.
    pub extra: Option<Box<str>>,
    pub fg: Color,
    pub bg: Color,
    pub attrs: u16,
}

impl Glyph {
    pub fn has(&self, attr: u16) -> bool {
        self.attrs & attr != 0
    }
}

/// The cursor as drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorPaint {
    pub col: usize,
    pub row: usize,
    pub shape: CursorShape,
    pub blinking: bool,
    /// It sits on a double-width character.
    pub wide: bool,
}

/// The visible rows, kept by the view between frames and updated from the grid's damage.
#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    pub lines: Vec<Vec<Glyph>>,
    pub cursor: Option<CursorPaint>,
    /// Selected cells per visible row: `(row, first col, last col)`.
    pub selection: Vec<(usize, usize, usize)>,
    /// How many lines the view is scrolled up from the bottom.
    pub display_offset: usize,
    /// Lines in the scrollback.
    pub history: usize,
    pub mode: Option<TermMode>,
    /// The emulator's generation this screen shows.
    pub generation: u64,
    /// The rows rebuilt by the last update (what the damage said).
    pub rebuilt: Vec<usize>,
    /// The colours it was built with (a change rebuilds everything).
    colors: Option<TermColors>,
}

impl Screen {
    /// The text of visible row `row` (spacers left out, trailing blanks kept).
    pub fn row_text(&self, row: usize) -> String {
        self.lines.get(row).map_or_else(String::new, |cells| {
            cells
                .iter()
                .filter(|g| !g.has(attrs::SPACER))
                .map(|g| {
                    let mut s = g.c.to_string();
                    if let Some(extra) = &g.extra {
                        s.push_str(extra);
                    }
                    s
                })
                .collect()
        })
    }

    /// Every visible row's text, trailing blanks trimmed, trailing empty rows dropped.
    pub fn text(&self) -> String {
        let mut rows: Vec<String> = (0..self.rows)
            .map(|r| self.row_text(r).trim_end().to_string())
            .collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows.join("\n")
    }
}

/// The terminal state.
pub struct Emulator {
    term: Term<Listener>,
    parser: Processor<StdSyncHandler>,
    events: Listener,
    colors: TermColors,
    size: GridSize,
    title: Option<String>,
    bells: u64,
    generation: u64,
    replies: Vec<u8>,
    /// The cell size in pixels, for a program that asks for the text area's size.
    cell: (u16, u16),
    /// Everything the view's screen shows must be rebuilt (a scroll, a resize, new colours).
    full: bool,
    bytes: u64,
}

impl Emulator {
    /// A `cols` × `rows` terminal drawn with `colors`.
    pub fn new(cols: usize, rows: usize, colors: TermColors) -> Self {
        Self::with_scrollback(cols, rows, SCROLLBACK, colors)
    }

    pub fn with_scrollback(
        cols: usize,
        rows: usize,
        scrollback: usize,
        colors: TermColors,
    ) -> Self {
        let size = GridSize {
            cols: cols.max(2),
            rows: rows.max(1),
        };
        let events = Listener::default();
        let config = Config {
            scrolling_history: scrollback,
            // OSC 52 is refused both ways: reading would hand the clipboard to any program (or a
            // file `cat`ed) that asks, and writing would let printed text replace what the user
            // copied with a command they then paste elsewhere (CP6.3 review).
            osc52: Osc52::Disabled,
            ..Config::default()
        };
        let term = Term::new(config, &size, events.clone());
        Self {
            term,
            parser: Processor::new(),
            events,
            colors,
            size,
            title: None,
            bells: 0,
            generation: 1,
            replies: Vec::new(),
            cell: (8, 16),
            full: true,
            bytes: 0,
        }
    }

    /// Output of the program.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.parser.advance(&mut self.term, bytes);
        self.bytes += bytes.len() as u64;
        self.after_change();
    }

    /// Ends a synchronized update (`CSI ? 2026 h`) whose time ran out with no more output.
    pub fn check_sync_timeout(&mut self) {
        let expired = self
            .parser
            .sync_timeout()
            .sync_timeout()
            .is_some_and(|at| at <= Instant::now());
        if expired {
            self.parser.stop_sync(&mut self.term);
            self.after_change();
        }
    }

    /// Whether a synchronized update is waiting (the pump then checks its time again).
    pub fn sync_pending(&self) -> bool {
        self.parser.sync_timeout().sync_timeout().is_some()
    }

    fn after_change(&mut self) {
        self.generation += 1;
        let events: Vec<Event> =
            std::mem::take(&mut *self.events.0.lock().unwrap_or_else(|e| e.into_inner()));
        for event in events {
            match event {
                Event::Title(title) => self.title = Some(title),
                Event::ResetTitle => self.title = None,
                Event::Bell => self.bells += 1,
                Event::PtyWrite(text) => self.replies.extend_from_slice(text.as_bytes()),
                Event::ColorRequest(ix, format) => {
                    let rgb = self.colors.rgb_of_slot(ix, self.term.colors());
                    self.replies.extend_from_slice(format(rgb).as_bytes());
                }
                Event::TextAreaSizeRequest(format) => {
                    let size = WindowSize {
                        num_lines: self.size.rows as u16,
                        num_cols: self.size.cols as u16,
                        cell_width: self.cell.0,
                        cell_height: self.cell.1,
                    };
                    self.replies.extend_from_slice(format(size).as_bytes());
                }
                // (No `ClipboardStore` / `ClipboardLoad`: OSC 52 is off. No title report either:
                // `alacritty_terminal` never echoes a title back, so printed text cannot type.)
                _ => {}
            }
        }
    }

    /// What the emulator answers the program (cursor position, colours, size), to write to it.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    /// Bumps on every change the view must draw.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Bytes fed so far.
    pub fn bytes_fed(&self) -> u64 {
        self.bytes
    }

    pub fn size(&self) -> GridSize {
        self.size
    }

    /// The cell size in pixels (answers a program's size query).
    pub fn set_cell_size(&mut self, width: u16, height: u16) {
        self.cell = (width.max(1), height.max(1));
    }

    /// New dimensions; the grid reflows its lines.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let size = GridSize {
            cols: cols.max(2),
            rows: rows.max(1),
        };
        if size != self.size {
            self.size = size;
            self.term.resize(size);
            self.full = true;
            self.generation += 1;
        }
    }

    /// New colours (a theme change).
    pub fn set_colors(&mut self, colors: TermColors) {
        if colors != self.colors {
            self.colors = colors;
            self.full = true;
            self.generation += 1;
        }
    }

    pub fn colors(&self) -> &TermColors {
        &self.colors
    }

    /// The title a program set (OSC 0 / 2), if any.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref().filter(|t| !t.trim().is_empty())
    }

    /// How many times the bell rang.
    pub fn bells(&self) -> u64 {
        self.bells
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// Scrolls the view (not the program).
    pub fn scroll(&mut self, scroll: Scroll) {
        let before = self.term.grid().display_offset();
        self.term.scroll_display(scroll);
        if self.term.grid().display_offset() != before {
            self.full = true;
            self.generation += 1;
        }
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    /// The grid point under visible cell (`col`, `row`).
    pub fn point_at(&self, col: usize, row: usize) -> Point {
        let col = col.min(self.size.cols - 1);
        let row = row.min(self.size.rows - 1);
        Point::new(Line(row as i32 - self.display_offset() as i32), Column(col))
    }

    // --- selection -------------------------------------------------------------------------------

    /// Starts a selection of `kind` at `point` (`side`: which half of the cell was pressed).
    pub fn start_selection(&mut self, kind: SelectionType, point: Point, side: Side) {
        self.term.selection = Some(Selection::new(kind, point, side));
        self.generation += 1;
    }

    /// Moves the selection's free end to `point` (a drag, a shift-click).
    pub fn extend_selection(&mut self, point: Point, side: Side) {
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
            self.generation += 1;
        }
    }

    pub fn clear_selection(&mut self) {
        if self.term.selection.take().is_some() {
            self.generation += 1;
        }
    }

    /// Whether a selection was started (a click leaves an empty one that Shift+click extends).
    pub fn has_anchor(&self) -> bool {
        self.term.selection.is_some()
    }

    /// Whether something (not nothing) is selected.
    pub fn has_selection(&self) -> bool {
        self.term.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// The selected text (wrapped lines joined, line ends as `\n`, trailing blanks of a line left out).
    pub fn selection_text(&self) -> Option<String> {
        if !self.has_selection() {
            return None;
        }
        self.term.selection_to_string().filter(|t| !t.is_empty())
    }

    // --- reading ---------------------------------------------------------------------------------

    /// The text of grid line `line` (a visible row is `row - display_offset`), with the column of
    /// each character (wide characters' spacers left out): for link detection.
    pub fn line_text(&self, row: usize) -> (String, Vec<usize>) {
        let point = self.point_at(0, row);
        let grid = self.term.grid();
        let line = &grid[point.line];
        let mut text = String::new();
        let mut columns = Vec::new();
        for col in 0..self.size.cols {
            let cell = &line[Column(col)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            text.push(cell.c);
            columns.push(col);
        }
        (text, columns)
    }

    /// The OSC 8 link of visible cell (`col`, `row`), if a program set one.
    pub fn hyperlink_at(&self, col: usize, row: usize) -> Option<String> {
        let point = self.point_at(col, row);
        self.term.grid()[point]
            .hyperlink()
            .map(|link| link.uri().to_string())
    }

    /// Rough memory of the grid's cells (scrollback and screen), in bytes.
    pub fn approx_bytes(&self) -> usize {
        (self.history_size() + self.size.rows)
            * self.size.cols
            * std::mem::size_of::<alacritty_terminal::term::cell::Cell>()
    }

    /// Brings `screen` up to date: rows the grid changed (or all of them after a scroll, a resize,
    /// a theme change), the cursor, the selection.
    pub fn update_screen(&mut self, screen: &mut Screen) {
        screen.rebuilt.clear();
        let rows = self.size.rows;
        let cols = self.size.cols;
        let offset = self.display_offset();
        let mut full = std::mem::take(&mut self.full)
            || screen.rows != rows
            || screen.cols != cols
            || screen.display_offset != offset
            || screen.colors.as_ref() != Some(&self.colors);
        let mut damaged: Vec<usize> = Vec::new();
        match self.term.damage() {
            TermDamage::Full => full = true,
            TermDamage::Partial(lines) => {
                if !full {
                    damaged.extend(lines.filter(|d| d.is_damaged()).map(|d| d.line));
                }
            }
        }
        self.term.reset_damage();
        if full {
            screen.rows = rows;
            screen.cols = cols;
            screen.lines = vec![Vec::new(); rows];
            screen.colors = Some(self.colors.clone());
            damaged = (0..rows).collect();
        }
        for row in damaged.iter().copied().filter(|r| *r < rows) {
            screen.lines[row] = self.build_row(row);
        }
        screen.rebuilt = damaged;
        screen.display_offset = offset;
        screen.history = self.history_size();
        screen.mode = Some(self.mode());
        screen.generation = self.generation;
        // The cursor.
        let content = self.term.renderable_content();
        let cursor = content.cursor;
        let style = self.term.cursor_style();
        let viewport_row = cursor.point.line.0 + offset as i32;
        screen.cursor = (cursor.shape != CursorShape::Hidden
            && viewport_row >= 0
            && (viewport_row as usize) < rows)
            .then(|| CursorPaint {
                col: cursor.point.column.0.min(cols - 1),
                row: viewport_row as usize,
                shape: cursor.shape,
                blinking: style.blinking,
                wide: self.term.grid()[cursor.point]
                    .flags
                    .contains(Flags::WIDE_CHAR),
            });
        // The selection, per visible row.
        screen.selection.clear();
        if let Some(range) = content.selection {
            for row in 0..rows {
                let line = Line(row as i32 - offset as i32);
                if line < range.start.line || line > range.end.line {
                    continue;
                }
                let (first, last) = if range.is_block {
                    (range.start.column.0, range.end.column.0)
                } else {
                    let first = if line == range.start.line {
                        range.start.column.0
                    } else {
                        0
                    };
                    let last = if line == range.end.line {
                        range.end.column.0
                    } else {
                        cols - 1
                    };
                    (first, last)
                };
                // A double-width character is selected whole (both of its cells are drawn).
                let cells = &screen.lines[row];
                let mut first = first.min(cols - 1);
                let mut last = last.min(cols - 1);
                if first > 0 && cells.get(first).is_some_and(|g| g.has(attrs::SPACER)) {
                    first -= 1;
                }
                if cells.get(last).is_some_and(|g| g.has(attrs::WIDE)) && last + 1 < cols {
                    last += 1;
                }
                if first <= last {
                    screen.selection.push((row, first, last));
                }
            }
        }
    }

    fn build_row(&self, row: usize) -> Vec<Glyph> {
        let point = self.point_at(0, row);
        let grid = self.term.grid();
        let line = &grid[point.line];
        let overrides = self.term.colors();
        let mut cells = Vec::with_capacity(self.size.cols);
        for col in 0..self.size.cols {
            let cell = &line[Column(col)];
            let flags = cell.flags;
            let mut fg = self.colors.resolve(cell.fg, overrides);
            let mut bg = self.colors.resolve(cell.bg, overrides);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut a = 0;
            if flags.contains(Flags::DIM) {
                fg = dim(fg, bg);
                a |= attrs::DIM;
            }
            if flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            if flags.contains(Flags::BOLD) {
                a |= attrs::BOLD;
            }
            if flags.contains(Flags::ITALIC) {
                a |= attrs::ITALIC;
            }
            if flags.contains(Flags::UNDERLINE)
                || flags.intersects(Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE)
            {
                a |= attrs::UNDERLINE;
            }
            if flags.contains(Flags::DOUBLE_UNDERLINE) {
                a |= attrs::DOUBLE_UNDERLINE;
            }
            if flags.contains(Flags::UNDERCURL) {
                a |= attrs::UNDERCURL;
            }
            if flags.contains(Flags::STRIKEOUT) {
                a |= attrs::STRIKE;
            }
            if flags.contains(Flags::WIDE_CHAR) {
                a |= attrs::WIDE;
            }
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                a |= attrs::SPACER;
            }
            if cell.hyperlink().is_some() {
                a |= attrs::LINK;
            }
            let extra = cell
                .zerowidth()
                .filter(|z| !z.is_empty())
                .map(|z| z.iter().collect::<String>().into_boxed_str());
            let c = if cell.c == '\t' || cell.c.is_control() {
                ' '
            } else {
                cell.c
            };
            cells.push(Glyph {
                c,
                extra,
                fg,
                bg,
                attrs: a,
            });
        }
        cells
    }

    /// The visible text, as [`Screen::text`] (tests and the fixture's checks).
    pub fn text(&mut self) -> String {
        self.screen().text()
    }

    /// A fresh screen (stories and tests). The view's own screen is rebuilt whole at its next
    /// update (this read used up the grid's damage).
    pub fn screen(&mut self) -> Screen {
        let mut screen = Screen::default();
        self.full = true;
        self.update_screen(&mut screen);
        self.full = true;
        screen
    }
}
