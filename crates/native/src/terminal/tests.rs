//! The terminal's scripted session, with no window: byte streams through the emulator and the
//! pump, checked on the screen the view would draw. Colours (16, 256, true colour) and every
//! attribute, cursor movement and erasing, wide characters, the alternate screen, scrolling
//! regions, a full-screen program's screen, resize and reflow, the program's questions answered,
//! titles and the bell, selection text, damage-based updates, and a million lines of output with
//! bounded memory and time (the numbers are printed: `cargo test -p hermes-native --lib
//! terminal::tests -- --nocapture`).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alacritty_terminal::index::Side;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::CursorShape;

use super::colors::{T3_DARK, TermColors, dim};
use super::emulator::{Emulator, SCROLLBACK, Screen, attrs};
use super::fixture::{FixturePty, colours_demo, numbered_lines, tui_demo};
use super::session::{Pty, Shared, pump};
use crate::design::Color;
use crate::theme::ThemeDef;

fn colors() -> TermColors {
    TermColors::of(&ThemeDef::default_dark())
}

fn emulator(cols: usize, rows: usize) -> Emulator {
    Emulator::new(cols, rows, colors())
}

fn fed(cols: usize, rows: usize, bytes: &[u8]) -> (Emulator, Screen) {
    let mut e = emulator(cols, rows);
    e.feed(bytes);
    let screen = e.screen();
    (e, screen)
}

/// The cell under the first occurrence of `needle` on the screen: `(row, col)`.
fn find(screen: &Screen, needle: &str) -> (usize, usize) {
    for row in 0..screen.rows {
        let text: String = screen.lines[row]
            .iter()
            .filter(|g| !g.has(attrs::SPACER))
            .map(|g| g.c)
            .collect();
        if let Some(byte) = text.find(needle) {
            let ix = text[..byte].chars().count();
            let mut seen = 0;
            for (col, g) in screen.lines[row].iter().enumerate() {
                if g.has(attrs::SPACER) {
                    continue;
                }
                if seen == ix {
                    return (row, col);
                }
                seen += 1;
            }
        }
    }
    panic!("{needle:?} is not on the screen:\n{}", screen.text());
}

#[test]
fn sgr_colours_sixteen_256_and_true_colour() {
    let c = colors();
    let (_, s) = fed(
        80,
        6,
        b"\x1b[31mred\x1b[0m \x1b[92mbright\x1b[0m \x1b[38;5;196mcube\x1b[0m \x1b[38;5;240mgrey\x1b[0m \
          \x1b[38;2;10;20;30mtrue\x1b[0m \x1b[44mbg\x1b[0m \x1b[48;5;21mbgcube\x1b[0m \x1b[48;2;1;2;3mbgrgb\x1b[0m",
    );
    let at = |needle| {
        let (r, col) = find(&s, needle);
        s.lines[r][col].clone()
    };
    assert_eq!(at("red").fg, Color::rgb(T3_DARK[1]));
    assert_eq!(at("bright").fg, Color::rgb(T3_DARK[10]));
    assert_eq!(at("cube").fg, Color::rgb(0xff0000));
    assert_eq!(at("grey").fg, Color::rgb(0x585858));
    assert_eq!(at("true").fg, Color::rgb(0x0a141e));
    assert_eq!(at("bg ").bg, Color::rgb(T3_DARK[4]));
    assert_eq!(at("bgcube").bg, Color::rgb(0x0000ff));
    assert_eq!(at("bgrgb").bg, Color::rgb(0x010203));
    // Plain text: the theme's foreground on the canvas.
    let (r, col) = find(&s, " bright");
    assert_eq!(s.lines[r][col].fg, c.foreground);
    assert_eq!(s.lines[r][col].bg, c.background);
}

#[test]
fn attributes_bold_italic_underline_inverse_dim_strike_curly_hidden() {
    let c = colors();
    let (_, s) = fed(
        80,
        4,
        b"\x1b[1mB\x1b[0m \x1b[3mI\x1b[0m \x1b[4mU\x1b[0m \x1b[7mV\x1b[0m \x1b[2mD\x1b[0m \x1b[9mS\x1b[0m \
          \x1b[4:3mC\x1b[0m \x1b[4:2mW\x1b[0m \x1b[8mH\x1b[0m \x1b[31;7mR\x1b[0m",
    );
    let at = |needle| {
        let (r, col) = find(&s, needle);
        s.lines[r][col].clone()
    };
    assert!(at("B").has(attrs::BOLD));
    assert!(at("I").has(attrs::ITALIC));
    assert!(at("U").has(attrs::UNDERLINE));
    assert!(at("C").has(attrs::UNDERCURL));
    assert!(at("W").has(attrs::DOUBLE_UNDERLINE));
    assert!(at("S").has(attrs::STRIKE));
    // Inverse swaps; dim fades towards the canvas; hidden is the canvas colour.
    assert_eq!((at("V").fg, at("V").bg), (c.background, c.foreground));
    assert_eq!(
        (at("R").fg, at("R").bg),
        (c.background, Color::rgb(T3_DARK[1]))
    );
    assert!(at("D").has(attrs::DIM));
    assert_eq!(at("D").fg, dim(c.foreground, c.background));
    let h = at("H");
    assert_eq!(h.fg, h.bg);
}

#[test]
fn cursor_movement_and_erasing() {
    let mut e = emulator(20, 5);
    // CUP, then text; CUB and overwrite; EL erases to the end of the line.
    e.feed(b"\x1b[3;5Hhello\x1b[3D\x1b[1mXY\x1b[0m");
    let s = e.screen();
    assert_eq!(s.row_text(2).trim_end(), "    heXYo");
    assert_eq!(s.cursor.map(|c| (c.row, c.col)), Some((2, 8)));
    e.feed(b"\x1b[3;7H\x1b[K");
    assert_eq!(e.screen().row_text(2).trim_end(), "    he");
    // EL 1 erases to the start of the line.
    e.feed(b"\x1b[1;1Htop\x1b[1;2H\x1b[1K");
    assert_eq!(e.screen().row_text(0).trim_end(), "  p");
    // ED 2 the screen.
    e.feed(b"\x1b[2J\x1b[H");
    assert_eq!(e.text(), "");
    // Relative moves: CUD, CUF, then save, move home, restore.
    e.feed(b"a\x1b[2B\x1b[3Cb\x1b7\x1b[Hc\x1b8d");
    let s = e.screen();
    assert_eq!(s.row_text(0).trim_end(), "c");
    assert_eq!(s.row_text(2).trim_end(), "    bd");
    // Insert (ICH) and delete (DCH) characters.
    e.feed(b"\x1b[H\x1b[2Jabcdef\x1b[1;3H\x1b[2@\x1b[1;1H\x1b[P");
    assert_eq!(e.screen().row_text(0).trim_end(), "b  cdef");
}

#[test]
fn wide_and_emoji_characters_take_two_cells() {
    let (_, s) = fed(20, 3, "漢字🚀x".as_bytes());
    let row = &s.lines[0];
    assert!(row[0].has(attrs::WIDE) && row[1].has(attrs::SPACER));
    assert_eq!(row[0].c, '漢');
    assert_eq!(row[2].c, '字');
    assert!(row[4].has(attrs::WIDE) && row[4].c == '🚀');
    assert_eq!(row[6].c, 'x');
    assert_eq!(s.cursor.map(|c| c.col), Some(7));
    // A wide character that does not fit at the end of a line wraps whole.
    let (_, s) = fed(5, 3, "abcd漢".as_bytes());
    assert_eq!(s.row_text(0).trim_end(), "abcd");
    assert_eq!(s.lines[1][0].c, '漢');
}

#[test]
fn the_alternate_screen_comes_and_goes_with_the_primary_kept() {
    let mut e = emulator(30, 6);
    e.feed(b"primary text\r\n$ ");
    e.feed(b"\x1b[?1049h\x1b[H\x1b[2Jfull screen");
    assert!(e.mode().contains(TermMode::ALT_SCREEN));
    assert_eq!(e.text(), "full screen");
    e.feed(b"\x1b[?1049l");
    assert!(!e.mode().contains(TermMode::ALT_SCREEN));
    assert_eq!(e.text(), "primary text\n$");
}

#[test]
fn a_scrolling_region_scrolls_only_its_lines() {
    let mut e = emulator(20, 6);
    e.feed(b"\x1b[1;1Hheader\x1b[6;1Hfooter");
    // Rows 2 to 4 scroll; the header and footer stay.
    e.feed(b"\x1b[2;4r\x1b[2;1Ha\r\nb\r\nc\r\nd\r\ne");
    let s = e.screen();
    assert_eq!(s.row_text(0).trim_end(), "header");
    assert_eq!(s.row_text(1).trim_end(), "c");
    assert_eq!(s.row_text(2).trim_end(), "d");
    assert_eq!(s.row_text(3).trim_end(), "e");
    assert_eq!(s.row_text(5).trim_end(), "footer");
    // Reverse index at the region's top scrolls it down.
    e.feed(b"\x1b[2;1H\x1bM");
    assert_eq!(e.screen().row_text(1).trim_end(), "");
    assert_eq!(e.screen().row_text(2).trim_end(), "c");
}

#[test]
fn a_full_screen_programs_screen_with_box_drawing_and_mouse_reporting() {
    let mut e = emulator(40, 10);
    e.feed(b"$ tui\r\n");
    e.feed(&tui_demo(40, 10));
    let s = e.screen();
    let mode = e.mode();
    assert!(mode.contains(TermMode::ALT_SCREEN));
    assert!(mode.contains(TermMode::MOUSE_REPORT_CLICK) && mode.contains(TermMode::SGR_MOUSE));
    assert_eq!(s.lines[0][0].c, '┌');
    assert_eq!(s.lines[0][39].c, '┐');
    assert_eq!(s.lines[8][0].c, '└');
    assert!(s.row_text(0).contains("htop-ish"));
    assert!(s.row_text(2).contains("hermes-native"));
    // The status bar is inverse across the whole width.
    assert!(s.lines[9].iter().all(|g| g.bg == colors().foreground));
    assert_eq!(s.cursor.map(|c| (c.row, c.col)), Some((2, 1)));
    // Leaving brings back the shell's screen.
    e.feed(b"\x1b[?1000l\x1b[?1006l\x1b[?1049l");
    assert_eq!(e.text(), "$ tui");
}

#[test]
fn resizing_reflows_wrapped_lines_and_keeps_the_cursor() {
    let mut e = emulator(20, 5);
    e.feed(b"0123456789abcdefghij-tail\r\n$ ");
    assert_eq!(e.screen().row_text(1).trim_end(), "-tail");
    e.resize(10, 5);
    let s = e.screen();
    assert_eq!((s.cols, s.rows), (10, 5));
    // The wrapped line now takes three rows; the cursor stays on its row, so the first row went
    // into the scrollback.
    assert_eq!(s.row_text(0), "abcdefghij");
    assert_eq!(s.row_text(1).trim_end(), "-tail");
    assert_eq!(s.row_text(2).trim_end(), "$");
    assert_eq!(s.cursor.map(|c| (c.row, c.col)), Some((2, 2)));
    e.scroll(alacritty_terminal::grid::Scroll::Delta(1));
    assert_eq!(e.screen().row_text(0), "0123456789");
    e.scroll(alacritty_terminal::grid::Scroll::Bottom);
    e.resize(30, 5);
    assert_eq!(
        e.screen().row_text(0).trim_end(),
        "0123456789abcdefghij-tail"
    );
    // Fewer rows: the top goes into the scrollback, the cursor's line stays on screen.
    e.feed(b"\r\n1\r\n2\r\n3\r\n4");
    e.resize(30, 2);
    let s = e.screen();
    assert_eq!(s.row_text(1).trim_end(), "4");
    assert!(e.history_size() >= 3);
}

#[test]
fn the_programs_questions_are_answered() {
    let mut e = emulator(80, 24);
    // ConPTY asks where the cursor is when it starts and waits for the answer.
    e.feed(b"ab\x1b[6n");
    assert_eq!(e.take_replies(), b"\x1b[1;3R");
    // Device attributes, and the foreground colour (OSC 10).
    e.feed(b"\x1b[c");
    assert!(e.take_replies().starts_with(b"\x1b[?"));
    e.feed(b"\x1b]10;?\x07");
    let reply = String::from_utf8(e.take_replies()).unwrap();
    assert!(reply.starts_with("\x1b]10;rgb:"), "{reply:?}");
    assert!(e.take_replies().is_empty());
}

#[test]
fn titles_and_the_bell() {
    let mut e = emulator(20, 3);
    assert_eq!(e.title(), None);
    e.feed(b"\x1b]0;build: cargo\x07");
    assert_eq!(e.title(), Some("build: cargo"));
    e.feed(b"\x1b]2;vim notes.md\x1b\\");
    assert_eq!(e.title(), Some("vim notes.md"));
    e.feed(b"\x07\x07");
    assert_eq!(e.bells(), 2);
    // OSC 52 is refused both ways: no clipboard read is answered (nor anything else).
    e.feed(b"\x1b]52;c;aGVsbG8=\x07");
    e.feed(b"\x1b]52;c;?\x07");
    assert!(e.take_replies().is_empty());
}

/// Nothing a program prints comes back as input except fixed-format answers: a title is never
/// reported (`CSI 21 t`, the classic injection), OSC 52 reads are not answered, and the answers
/// that are given carry numbers, not printed text.
#[test]
fn printed_text_cannot_type_into_the_shell() {
    let mut e = emulator(40, 5);
    e.feed(b"\x1b]2;\x1b[201~rm -rf ~\r\x07");
    e.feed(b"\x1b[21t\x1b[20t\x1b]l?\x1b\\\x1b]52;c;?\x07\x1b]52;p;?\x07");
    assert!(e.take_replies().is_empty());
    // The standard questions are answered, with numbers only.
    e.feed(b"\x1b[6n\x1b[c\x1b[18t\x1b[?2004$p");
    let replies = e.take_replies();
    assert!(!replies.is_empty());
    let text = String::from_utf8(replies).unwrap();
    assert!(!text.contains("rm"), "{text:?}");
    assert!(
        text.chars()
            .all(|c| c == '\x1b' || c.is_ascii_digit() || "[;?R$ytc".contains(c)),
        "{text:?}"
    );
}

#[test]
fn selection_text_joins_wrapped_lines_and_drops_trailing_blanks() {
    let mut e = emulator(10, 5);
    // A line that wraps, then a short line with trailing spaces, then wide characters.
    e.feed("0123456789abc\r\nshort     \r\n漢字ok".as_bytes());
    let (start, end) = (e.point_at(0, 0), e.point_at(9, 3));
    e.start_selection(SelectionType::Simple, start, Side::Left);
    e.extend_selection(end, Side::Right);
    assert_eq!(
        e.selection_text().as_deref(),
        Some("0123456789abc\nshort\n漢字ok")
    );
    // A word (double click) and a line (triple click).
    let at = e.point_at(1, 2);
    e.start_selection(SelectionType::Semantic, at, Side::Left);
    assert_eq!(e.selection_text().as_deref(), Some("short"));
    let at = e.point_at(3, 0);
    e.start_selection(SelectionType::Lines, at, Side::Left);
    assert_eq!(e.selection_text().as_deref(), Some("0123456789abc\n"));
    // Selecting half of a wide character takes all of it.
    let (a, b) = (e.point_at(1, 3), e.point_at(2, 3));
    e.start_selection(SelectionType::Simple, a, Side::Left);
    e.extend_selection(b, Side::Right);
    assert_eq!(e.selection_text().as_deref(), Some("漢字"));
    // The screen marks the selected cells.
    let s = e.screen();
    assert_eq!(s.selection, vec![(3, 0, 3)]);
    e.clear_selection();
    assert!(!e.has_selection() && e.selection_text().is_none());
}

#[test]
fn only_damaged_rows_are_rebuilt() {
    let mut e = emulator(20, 10);
    let mut screen = Screen::default();
    e.update_screen(&mut screen);
    assert_eq!(
        screen.rebuilt.len(),
        10,
        "the first update builds every row"
    );
    e.feed(b"\x1b[4;1Hfour");
    e.update_screen(&mut screen);
    assert!(screen.rebuilt.contains(&3));
    assert!(screen.rebuilt.len() <= 2, "{:?}", screen.rebuilt);
    assert_eq!(screen.row_text(3).trim_end(), "four");
    // Nothing changed: nothing rebuilt but the cursor's row at most.
    e.update_screen(&mut screen);
    assert!(screen.rebuilt.len() <= 1, "{:?}", screen.rebuilt);
    // A scroll of the view rebuilds everything.
    e.feed(&numbered_lines(30));
    e.update_screen(&mut screen);
    e.scroll(alacritty_terminal::grid::Scroll::Delta(3));
    e.update_screen(&mut screen);
    assert_eq!(screen.rebuilt.len(), 10);
    assert_eq!(screen.display_offset, 3);
}

#[test]
fn a_theme_change_recolours_everything() {
    let mut e = emulator(20, 3);
    e.feed(b"\x1b[32mgreen");
    let dark = e.screen();
    e.set_colors(TermColors::of(&ThemeDef::default_light()));
    let light = e.screen();
    assert_ne!(dark.lines[0][0].fg, light.lines[0][0].fg);
    assert_eq!(light.lines[0][0].fg, Color::rgb(super::colors::T3_LIGHT[2]));
}

#[test]
fn cursor_shapes_from_the_program() {
    let mut e = emulator(20, 3);
    assert_eq!(e.screen().cursor.unwrap().shape, CursorShape::Block);
    e.feed(b"\x1b[6 q");
    let c = e.screen().cursor.unwrap();
    assert_eq!((c.shape, c.blinking), (CursorShape::Beam, false));
    e.feed(b"\x1b[3 q");
    let c = e.screen().cursor.unwrap();
    assert_eq!((c.shape, c.blinking), (CursorShape::Underline, true));
    e.feed(b"\x1b[?25l");
    assert!(e.screen().cursor.is_none(), "hidden");
}

#[test]
fn the_colours_demo_is_parsed_whole() {
    let (_, s) = fed(60, 20, &colours_demo());
    let text = s.text();
    assert!(text.contains("16 colours"));
    assert!(text.contains("漢字かな"));
    assert!(text.contains("┌──────┬──────┐"));
    assert!(text.contains("█▓▒░"));
}

// --- volume ------------------------------------------------------------------------------------------

/// A million lines through the emulator: time, and memory bounded by the scrollback.
#[test]
fn a_million_lines_keep_memory_bounded() {
    let mut e = emulator(120, 40);
    let bytes = numbered_lines(1_000_000);
    let started = Instant::now();
    for slice in bytes.chunks(super::session::SLICE) {
        e.feed(slice);
    }
    let parse = started.elapsed();
    let text = e.text();
    assert!(
        text.ends_with("line 1000000"),
        "{}",
        &text[text.len().saturating_sub(40)..]
    );
    assert_eq!(e.history_size(), SCROLLBACK);
    let cells = e.approx_bytes();
    let bound =
        (SCROLLBACK + 40) * 120 * std::mem::size_of::<alacritty_terminal::term::cell::Cell>();
    assert!(cells <= bound);
    println!(
        "1,000,000 lines ({:.1} MB) parsed in {:.2} s ({:.1} MB/s); scrollback {} lines, grid {:.1} MB",
        bytes.len() as f64 / 1e6,
        parse.as_secs_f64(),
        bytes.len() as f64 / 1e6 / parse.as_secs_f64(),
        e.history_size(),
        cells as f64 / 1e6
    );
    // A debug build parses at a few MB/s; the bound is generous so a loaded machine passes.
    assert!(parse < Duration::from_secs(240), "{parse:?}");
}

/// Runs the pump on its own thread (a current-thread executor: it needs no runtime of its own).
fn run_pump(shared: Arc<Shared>, pty: Arc<dyn Pty>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(pump(shared, pty));
    })
}

/// The pump consumes everything the shell prints while a "frame" takes the lock every 16 ms:
/// a frame never waits long (one slice), and the emulator sees every line.
#[test]
fn the_pump_feeds_everything_while_frames_take_the_lock() {
    let pty = FixturePty::scripted(120, 40);
    let shared = Shared::new(Emulator::new(120, 40, colors()));
    let pumping = run_pump(shared.clone(), pty.clone());
    let lines = 200_000;
    let bytes = numbered_lines(lines);
    let total_bytes = bytes.len() as u64;
    let producer = {
        let pty = pty.clone();
        std::thread::spawn(move || {
            // Wait for the pump to attach.
            while pty.receivers() == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
            for chunk in bytes.chunks(16 * 1024) {
                // The core's reader sends 16 KiB chunks; wait when the channel is nearly full
                // (as a slow pipe would), so nothing is skipped in this test.
                while pty.pending() > 900 {
                    std::thread::sleep(Duration::from_micros(200));
                }
                pty.emit(chunk);
            }
        })
    };
    let started = Instant::now();
    let mut longest = Duration::ZERO;
    let mut frames = 0;
    let mut screen = Screen::default();
    loop {
        let asked = Instant::now();
        let fed = {
            let mut emulator = shared.emulator.lock();
            emulator.update_screen(&mut screen);
            emulator.bytes_fed()
        };
        longest = longest.max(asked.elapsed());
        frames += 1;
        if fed == total_bytes {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "the pump did not keep up"
        );
        std::thread::sleep(Duration::from_millis(16));
    }
    producer.join().unwrap();
    let total = started.elapsed();
    let text = shared.emulator.lock().text();
    assert!(
        text.ends_with(&format!("line {lines}")),
        "{}",
        &text[text.len().saturating_sub(40)..]
    );
    assert_eq!(shared.resyncs.load(Ordering::Relaxed), 0);
    println!(
        "pump: {lines} lines ({:.1} MB) in {:.2} s, {frames} frames, longest wait for the lock {:.1} ms, {} wake-ups",
        total_bytes as f64 / 1e6,
        total.as_secs_f64(),
        longest.as_secs_f64() * 1e3,
        shared.batches.load(Ordering::Relaxed)
    );
    // A frame waits at most for one slice (64 KiB) to be parsed, plus a screen update.
    assert!(longest < Duration::from_millis(500), "{longest:?}");
    pty.end();
    pumping.join().unwrap();
    assert!(shared.exited.load(Ordering::Acquire));
}

/// A flood the emulator cannot keep up with (more than the channel holds before it reads): the
/// pump starts again from the shell's history and shows the latest output.
#[test]
fn falling_behind_the_channel_replays_the_history() {
    let pty = FixturePty::scripted(80, 10);
    let shared = Shared::new(Emulator::new(80, 10, colors()));
    let pumping = {
        let (shared, pty) = (shared.clone(), pty.clone());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            rt.block_on(async move {
                let task = pump(shared, pty.clone() as Arc<dyn Pty>);
                tokio::pin!(task);
                // Start the pump (it attaches and waits), then flood before it reads.
                tokio::select! {
                    biased;
                    () = &mut task => {}
                    () = async {
                        for i in 0..3000 {
                            pty.emit(format!("chunk {i}\r\n").as_bytes());
                        }
                        pty.end();
                    } => {}
                }
                task.await;
            });
        })
    };
    pumping.join().unwrap();
    let text = shared.emulator.lock().text();
    assert!(text.ends_with("chunk 2999"), "{text}");
    assert!(shared.resyncs.load(Ordering::Relaxed) >= 1);
    println!(
        "resyncs after a 3,000-chunk flood: {}",
        shared.resyncs.load(Ordering::Relaxed)
    );
}
