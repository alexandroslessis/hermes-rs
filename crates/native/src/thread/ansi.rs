//! ANSI output as styled lines, with no GPUI in it: a command's output (colours, bold, dim,
//! underline, italic; every other escape sequence dropped) split into [`Line`]s whose styled runs
//! are byte ranges. Parsed once when the text arrives (a progress tail, a finished result), never
//! per frame; the view draws the lines it has.
//!
//! - SGR: reset, bold, dim, italic, underline, their off codes, the 16 colours (`30-37`, `90-97`),
//!   256 colours (`38;5;n`) and true colour (`38;2;r;g;b`, also with `:` separators); backgrounds
//!   are read and dropped (a terminal's canvas is the card's).
//! - A bare `\r` returns to the start of the line (a progress bar overwrites itself: only its last
//!   state stays); `\r\n` is a newline; a tab is four spaces; other control characters, CSI
//!   sequences that are not SGR, OSC strings (`ESC ]` ... `BEL` / `ESC \`) and two-character
//!   escapes are dropped.
//! - The style carries across lines until reset, as in a terminal.
//!
//! t3code: `packages/shared/src/terminalOutput.ts` (`terminalOutputSpans`, `TERMINAL_PALETTES`,
//! the colours below), used by `apps/web/src/components/chat/CommandOutputPanel.tsx`.

use std::ops::Range;

use crate::design::Color as Rgb;

/// A foreground colour: a palette index (0-255) or a true colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Index(u8),
    Rgb(u8, u8, u8),
}

/// What an ANSI run looks like.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Style {
    fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// A styled run: bytes `range` of its line's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Style,
}

/// One line: its text and the runs that are not plain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub spans: Vec<Span>,
}

/// t3code's terminal palettes (`TERMINAL_PALETTES`): the 16 ANSI colours, dark and light.
pub const DARK: [u32; 16] = [
    0x3b4048, 0xf47067, 0x57ab5a, 0xc69026, 0x539bf5, 0xb083f0, 0x39c5cf, 0xadbac7, 0x768390,
    0xff938a, 0x6bc46d, 0xdaaa3f, 0x6cb6ff, 0xdcbdfb, 0x56d4dd, 0xcdd9e5,
];
pub const LIGHT: [u32; 16] = [
    0x24292f, 0xcf222e, 0x116329, 0x7d4e00, 0x0550ae, 0x8250df, 0x1b7c83, 0x6e7781, 0x57606a,
    0xa40e26, 0x1a7f37, 0x633c01, 0x0969da, 0x6639ba, 0x3192aa, 0x8c959f,
];

/// The colour of `color` under the dark or light palette (xterm's cube and gray ramp above 15).
pub fn resolve(color: Color, dark: bool) -> Rgb {
    match color {
        Color::Rgb(r, g, b) => Rgb::rgb(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)),
        Color::Index(i) if i < 16 => Rgb::rgb(if dark { DARK } else { LIGHT }[usize::from(i)]),
        Color::Index(i) if i < 232 => {
            let n = i - 16;
            let level = |v: u8| if v == 0 { 0u32 } else { 55 + 40 * u32::from(v) };
            Rgb::rgb(level(n / 36) << 16 | level(n / 6 % 6) << 8 | level(n % 6))
        }
        Color::Index(i) => {
            let v = 8 + 10 * u32::from(i - 232);
            Rgb::rgb(v << 16 | v << 8 | v)
        }
    }
}

/// Parses `input` into lines. An empty input has no lines; a final newline does not add an empty
/// line.
pub fn parse(input: &str) -> Vec<Line> {
    let mut parser = Parser::default();
    parser.feed(input);
    parser.finish()
}

#[derive(Default)]
struct Parser {
    lines: Vec<Line>,
    line: Line,
    style: Style,
    /// Where the current styled run began in `line.text`.
    run_start: usize,
    /// A line is open (text or a newline was seen since the last one was pushed).
    open: bool,
}

impl Parser {
    fn feed(&mut self, input: &str) {
        let mut chars = input.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\n' => self.newline(),
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        continue;
                    }
                    // Back to the start of the line: what was there is overwritten.
                    self.line = Line::default();
                    self.run_start = 0;
                    self.open = true;
                }
                '\t' => self.text("    "),
                '\u{1b}' => self.escape(&mut chars),
                c if c.is_control() => {}
                c => {
                    let mut buf = [0u8; 4];
                    self.text(c.encode_utf8(&mut buf));
                }
            }
        }
    }

    fn finish(mut self) -> Vec<Line> {
        if self.open {
            self.close_run();
            self.lines.push(self.line);
        }
        self.lines
    }

    fn text(&mut self, s: &str) {
        self.open = true;
        self.line.text.push_str(s);
    }

    fn newline(&mut self) {
        self.close_run();
        self.lines.push(std::mem::take(&mut self.line));
        self.run_start = 0;
        self.open = false;
    }

    /// Ends the styled run being written (the style may change next).
    fn close_run(&mut self) {
        let end = self.line.text.len();
        if self.run_start < end && !self.style.is_plain() {
            match self.line.spans.last_mut() {
                Some(last) if last.range.end == self.run_start && last.style == self.style => {
                    last.range.end = end;
                }
                _ => self.line.spans.push(Span {
                    range: self.run_start..end,
                    style: self.style,
                }),
            }
        }
        self.run_start = end;
    }

    fn escape(&mut self, chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
        match chars.next() {
            Some('[') => {
                let mut params = String::new();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        if c == 'm' {
                            self.close_run();
                            self.sgr(&params);
                        }
                        return;
                    }
                    params.push(c);
                }
            }
            // OSC: up to BEL or ESC \.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        return;
                    }
                    if c == '\u{1b}' {
                        chars.next();
                        return;
                    }
                }
            }
            // A two-character escape (`ESC c`, `ESC (B`'s first byte): dropped.
            _ => {}
        }
    }

    fn sgr(&mut self, params: &str) {
        let numbers: Vec<Option<u32>> = if params.is_empty() {
            vec![Some(0)]
        } else {
            params
                .split([';', ':'])
                .map(|p| {
                    if p.is_empty() {
                        Some(0)
                    } else {
                        p.parse().ok()
                    }
                })
                .collect()
        };
        let mut i = 0;
        while i < numbers.len() {
            let n = numbers[i].unwrap_or(u32::MAX);
            i += 1;
            match n {
                0 => self.style = Style::default(),
                1 => self.style.bold = true,
                2 => self.style.dim = true,
                3 => self.style.italic = true,
                4 => self.style.underline = true,
                22 => {
                    self.style.bold = false;
                    self.style.dim = false;
                }
                23 => self.style.italic = false,
                24 => self.style.underline = false,
                30..=37 => self.style.fg = Some(Color::Index((n - 30) as u8)),
                39 => self.style.fg = None,
                90..=97 => self.style.fg = Some(Color::Index((n - 90 + 8) as u8)),
                38 | 48 => {
                    let color = match numbers.get(i).copied().flatten() {
                        Some(5) => {
                            let c = numbers.get(i + 1).copied().flatten();
                            i += 2;
                            c.map(|c| Color::Index(c.min(255) as u8))
                        }
                        Some(2) => {
                            // `38:2::r:g:b` has an empty colour-space slot (it parses as 0).
                            let skip = usize::from(params.contains("::"));
                            let channel = |k: usize| {
                                numbers
                                    .get(i + k + skip)
                                    .copied()
                                    .flatten()
                                    .unwrap_or(0)
                                    .min(255) as u8
                            };
                            let c = Color::Rgb(channel(1), channel(2), channel(3));
                            i += 4 + skip;
                            Some(c)
                        }
                        _ => None,
                    };
                    if n == 38 {
                        self.style.fg = color;
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<&str> {
        lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn colours_and_weight_become_runs_over_the_visible_text() {
        let lines = parse("a \u{1b}[1;31mbad\u{1b}[0m b\n\u{1b}[32mok\u{1b}[m");
        assert_eq!(plain(&lines), ["a bad b", "ok"]);
        assert_eq!(
            lines[0].spans,
            vec![Span {
                range: 2..5,
                style: Style {
                    fg: Some(Color::Index(1)),
                    bold: true,
                    ..Style::default()
                }
            }]
        );
        assert_eq!(lines[1].spans[0].range, 0..2);
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::Index(2)));
    }

    #[test]
    fn a_style_carries_across_lines_until_it_is_reset() {
        let lines = parse("\u{1b}[34mone\ntwo\u{1b}[0m\nthree");
        assert_eq!(plain(&lines), ["one", "two", "three"]);
        assert_eq!(lines[1].spans.len(), 1, "still blue on the next line");
        assert!(lines[2].spans.is_empty());
    }

    #[test]
    fn bright_256_and_true_colours() {
        let lines = parse("\u{1b}[91ma\u{1b}[38;5;196mb\u{1b}[38;2;1;2;3mc\u{1b}[38:2::9:8:7md");
        let colors: Vec<Option<Color>> = lines[0].spans.iter().map(|s| s.style.fg).collect();
        assert_eq!(
            colors,
            [
                Some(Color::Index(9)),
                Some(Color::Index(196)),
                Some(Color::Rgb(1, 2, 3)),
                Some(Color::Rgb(9, 8, 7)),
            ]
        );
        assert_eq!(resolve(Color::Index(196), true), Rgb::rgb(0xff0000));
        assert_eq!(resolve(Color::Index(232), true), Rgb::rgb(0x080808));
        assert_eq!(resolve(Color::Index(1), true), Rgb::rgb(DARK[1]));
        assert_eq!(resolve(Color::Index(1), false), Rgb::rgb(LIGHT[1]));
    }

    #[test]
    fn other_sequences_are_dropped_and_carriage_returns_overwrite() {
        let lines = parse(
            "\u{1b}[2K\u{1b}[1Gdone\u{1b}]0;title\u{7}!\r\nprogress 10%\rprogress 90%\r\nx\ty\u{8}",
        );
        assert_eq!(plain(&lines), ["done!", "progress 90%", "x    y"]);
    }

    #[test]
    fn dim_underline_italic_and_their_off_codes() {
        let lines = parse("\u{1b}[2;3;4mq\u{1b}[22;23;24mr");
        let s = lines[0].spans[0].style;
        assert!(s.dim && s.italic && s.underline && !s.bold);
        assert_eq!(
            lines[0].spans.len(),
            1,
            "the plain run after it has no span"
        );
    }

    #[test]
    fn empty_input_and_a_final_newline() {
        assert!(parse("").is_empty());
        assert_eq!(plain(&parse("a\n")), ["a"]);
        assert_eq!(plain(&parse("a\n\nb")), ["a", "", "b"]);
    }

    #[test]
    fn ten_thousand_lines_parse_in_milliseconds_and_keep_every_line() {
        let text: String = (0..10_000)
            .map(|i| format!("\u{1b}[32mline {i}\u{1b}[0m some more words to make it long\n"))
            .collect();
        let start = std::time::Instant::now();
        let lines = parse(&text);
        let took = start.elapsed();
        assert_eq!(lines.len(), 10_000);
        assert_eq!(
            lines[9_999].text,
            "line 9999 some more words to make it long"
        );
        assert_eq!(lines[9_999].spans[0].range, 0..9);
        // Debug build, generous: the point is that it is one pass, nothing per frame.
        assert!(took.as_millis() < 500, "10,000 lines took {took:?}");
    }
}
