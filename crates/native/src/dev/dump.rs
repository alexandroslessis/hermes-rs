//! The element dump: every named element of a window's last frame, as compact text. One line per
//! element: its path, bounds, background and text colour, state. Layout tests assert on it
//! (`crate::testing`), and `--dump <file>` / F12 write it from a running instance.
//!
//! What it reads, all from GPUI Kit's observation and GPUI's test support (no Kit or GPUI change):
//! - the elements: those registered with Kit's `.test_support()` (Kit's own controls register
//!   themselves; ours through `.probe()`), with their bounds, visibility, focus and disabled flag;
//! - colours: for probed elements, their computed style ([`super::probe`]); otherwise, and when the
//!   style has no solid background, the topmost painted quad with exactly the element's bounds
//!   (`Window::painted_quads`), which is how a Kit button's background is read;
//! - hover: the probe's hitbox, else the pointer being inside the bounds (ignores occlusion).

use std::fmt::{self, Write as _};

use gpui_kit::{Bounds, ElementId, Hsla, Pixels, Point, Rgba, Window};

use super::probe::facts;

/// A colour as 8-bit RGBA, printed `#rrggbbaa`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(pub [u8; 4]);

impl From<Hsla> for Color {
    fn from(color: Hsla) -> Self {
        let rgba = Rgba::from(color);
        let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
        Self([byte(rgba.r), byte(rgba.g), byte(rgba.b), byte(rgba.a)])
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [r, g, b, a] = self.0;
        write!(f, "#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

impl Color {
    /// Whether two colours are equal within `tolerance` per channel (colours mixed by GPUI Kit
    /// in OKLab round differently from the same mix done in a test).
    pub fn near(self, other: Color, tolerance: u8) -> bool {
        self.0
            .iter()
            .zip(other.0)
            .all(|(a, b)| a.abs_diff(b) <= tolerance)
    }
}

/// One named element of the frame.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The element's id path, outermost first (view and anonymous ids left out).
    pub path: Vec<String>,
    /// Window-local bounds in logical pixels.
    pub bounds: Bounds<Pixels>,
    /// Painted and not clipped away (Kit's notion: visibility, opacity, content mask).
    pub visible: bool,
    /// Background colour, when solid.
    pub background: Option<Color>,
    /// The text colour of its content, for probed elements.
    pub text_color: Option<Color>,
    /// Border colour, for probed elements that have one.
    pub border: Option<Color>,
    /// The width its one-line text needs, for `.probe_text(..)`.
    pub text_width: Option<Pixels>,
    /// The pointer is over it.
    pub hovered: bool,
    /// It holds keyboard focus (`None`: it tracks no focus handle).
    pub focused: Option<bool>,
    /// Its accessibility node says disabled.
    pub disabled: bool,
    /// Its accessible label, when it has one.
    pub label: Option<String>,
}

impl Entry {
    /// The path joined with `/`.
    pub fn name(&self) -> String {
        self.path.join("/")
    }

    /// Whether `name` (one id, or several joined with `/`) is the end of this element's path.
    pub fn is_named(&self, name: &str) -> bool {
        let wanted: Vec<&str> = name.split('/').collect();
        self.path.len() >= wanted.len()
            && self.path[self.path.len() - wanted.len()..]
                .iter()
                .zip(&wanted)
                .all(|(have, want)| have == want)
    }
}

/// Every named element of a window's last frame.
#[derive(Clone, Debug)]
pub struct Dump {
    /// The elements, top to bottom, then left to right.
    pub entries: Vec<Entry>,
    /// The window's size in logical pixels.
    pub viewport: gpui_kit::Size<Pixels>,
    /// Where the pointer is.
    pub mouse: Point<Pixels>,
}

impl Dump {
    /// Reads the window's last completed frame.
    pub fn capture(window: &Window) -> Self {
        let scale = window.scale_factor();
        let quads = window.painted_quads();
        let mouse = window.mouse_position();
        let mut snapshots = gpui_kit::base::test_support::snapshots(window);
        snapshots.sort_by_cached_key(|s| {
            (
                s.bounds().origin.y,
                s.bounds().origin.x,
                format!("{:?}", s.path()),
            )
        });
        let entries = snapshots
            .into_iter()
            .map(|snapshot| {
                let bounds = snapshot.bounds();
                let probe = facts(window, snapshot.path());
                // The last quad painted over exactly these bounds with a visible fill is the one
                // on top (a border is painted as a further quad with a transparent fill).
                let painted = quads
                    .iter()
                    .rev()
                    .filter(|quad| {
                        let b = quad.bounds;
                        let near = |a: f32, b: f32| (a - b * scale).abs() <= 1.;
                        near(b.origin.x.0, bounds.origin.x.as_f32())
                            && near(b.origin.y.0, bounds.origin.y.as_f32())
                            && near(b.size.width.0, bounds.size.width.as_f32())
                            && near(b.size.height.0, bounds.size.height.as_f32())
                    })
                    .filter_map(|quad| quad.background.as_solid())
                    .find(|color| color.a > 0.);
                let background = probe
                    .as_ref()
                    .and_then(|p| p.background)
                    .filter(|c| c.a > 0.)
                    .or(painted)
                    .map(Color::from);
                let hovered = match probe.as_ref().and_then(|p| p.hovered) {
                    Some(hovered) => hovered,
                    None => snapshot.visible() && bounds.contains(&mouse),
                };
                Entry {
                    path: snapshot.path().iter().filter_map(segment).collect(),
                    bounds,
                    visible: snapshot.visible(),
                    background,
                    text_color: probe.as_ref().map(|p| Color::from(p.text_color)),
                    border: probe
                        .as_ref()
                        .and_then(|p| p.border)
                        .filter(|c| c.a > 0.)
                        .map(Color::from),
                    text_width: probe.as_ref().and_then(|p| p.text_width),
                    hovered,
                    focused: focused(&snapshot),
                    disabled: snapshot.disabled() == Some(true),
                    label: snapshot.label().map(str::to_string),
                }
            })
            .collect();
        Self {
            entries,
            viewport: window.viewport_size(),
            mouse,
        }
    }

    /// The elements whose path ends with `name`.
    pub fn named(&self, name: &str) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.is_named(name)).collect()
    }

    /// The dump as text: a header line, then the elements as a tree: each under the nearest named
    /// element whose path contains it, indented, with its path written relative to that one;
    /// siblings top to bottom, then left to right.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# window {}x{}  pointer {},{}",
            num(self.viewport.width),
            num(self.viewport.height),
            num(self.mouse.x),
            num(self.mouse.y)
        );
        // The nearest named ancestor of each entry (entries are already in reading order).
        let parent: Vec<Option<usize>> = self
            .entries
            .iter()
            .map(|entry| {
                self.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, other)| {
                        other.path.len() < entry.path.len() && entry.path.starts_with(&other.path)
                    })
                    .max_by_key(|(_, other)| other.path.len())
                    .map(|(ix, _)| ix)
            })
            .collect();
        let mut stack: Vec<(usize, usize)> = (0..self.entries.len())
            .rev()
            .filter(|&ix| parent[ix].is_none())
            .map(|ix| (ix, 0))
            .collect();
        while let Some((ix, depth)) = stack.pop() {
            let entry = &self.entries[ix];
            let parent_len = parent[ix].map_or(0, |p| self.entries[p].path.len());
            let _ = writeln!(
                out,
                "{}{}",
                "  ".repeat(depth),
                line(entry, &entry.path[parent_len..])
            );
            stack.extend(
                (0..self.entries.len())
                    .rev()
                    .filter(|&child| parent[child] == Some(ix))
                    .map(|child| (child, depth + 1)),
            );
        }
        out
    }
}

fn line(entry: &Entry, path: &[String]) -> String {
    let b = entry.bounds;
    let mut out = format!(
        "{}  {},{} {}x{}",
        path.join("/"),
        num(b.origin.x),
        num(b.origin.y),
        num(b.size.width),
        num(b.size.height)
    );
    if let Some(bg) = entry.background {
        let _ = write!(out, " bg={bg}");
    }
    if let Some(fg) = entry.text_color {
        let _ = write!(out, " fg={fg}");
    }
    if let Some(border) = entry.border {
        let _ = write!(out, " border={border}");
    }
    if let Some(width) = entry.text_width {
        let _ = write!(out, " text_w={}", num(width));
    }
    for (on, word) in [
        (entry.hovered, "hovered"),
        (entry.focused == Some(true), "focused"),
        (entry.disabled, "disabled"),
        (!entry.visible, "hidden"),
    ] {
        if on {
            let _ = write!(out, " {word}");
        }
    }
    if let Some(label) = &entry.label {
        let _ = write!(out, " label={label:?}");
    }
    out
}

/// A pixel value with at most one decimal.
fn num(value: Pixels) -> String {
    let v = (value.as_f32() * 10.).round() / 10.;
    if v.fract() == 0. {
        format!("{}", v as i64)
    } else {
        format!("{v:.1}")
    }
}

/// An id as it reads in a path; views, focus handles and code locations carry no name, and the
/// ids GPUI gives components after their Rust type (`gpui_component::button::button::Button`)
/// are left out too, so a path reads as the ids we wrote.
fn segment(id: &ElementId) -> Option<String> {
    match id {
        ElementId::Name(name) if name.contains("::") => None,
        ElementId::Name(name) => Some(name.to_string()),
        ElementId::Integer(n) => Some(n.to_string()),
        ElementId::NamedInteger(name, n) => Some(format!("{name}:{n}")),
        ElementId::NamedChild(_, name) => Some(name.to_string()),
        _ => None,
    }
}

/// Kit's `focused()` panics when an element can take focus but its focus binding was not
/// observed (a wiring mistake it wants tests to see); a dump reports that as unknown instead. Its
/// `Debug` output prints the same case as `<binding missed>`, so it is read from there.
fn focused(snapshot: &gpui_kit::base::test_support::ElementSnapshot) -> Option<bool> {
    let debug = format!("{snapshot:?}");
    if debug.contains("focused: Some(true)") {
        Some(true)
    } else if debug.contains("focused: Some(false)") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gallery::{all, open_story};
    use crate::testing::headless_app;
    use crate::theme::ThemeChoice;

    #[core::prelude::v1::test]
    fn the_dump_names_each_element_with_bounds_colours_and_state() {
        let mut cx = headless_app();
        let story = all()[0];
        // The `hover` state: the pointer rests on the button.
        let window = open_story(&mut cx, story, 1, ThemeChoice::DefaultLight);
        let text = cx
            .update_window(window, |_, window, _| Dump::capture(window).to_text())
            .unwrap();
        // Shown with --nocapture: what a dump looks like.
        println!("{text}");
        let line = |name: &str| {
            text.lines()
                .find(|l| l.trim_start().starts_with(name))
                .unwrap_or_else(|| panic!("no line for {name} in\n{text}"))
                .to_string()
        };
        assert!(text.starts_with("# window 160x60"), "{text}");
        let story_line = line("story ");
        assert!(
            story_line.contains(" bg=#") && story_line.contains(" fg=#"),
            "{story_line}"
        );
        let button = line("button ");
        assert!(button.starts_with("  button  12,12 "), "{button}");
        assert!(
            button.contains(" bg=#") && button.contains(" hovered"),
            "{button}"
        );
        assert!(button.contains("label=\"Save changes\""), "{button}");
        let caption = line("label/caption ");
        assert!(
            caption.starts_with("    "),
            "nested under the button: {caption}"
        );
        assert!(
            caption.contains(" fg=#") && caption.contains(" text_w="),
            "{caption}"
        );
    }
}
