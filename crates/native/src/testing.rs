//! Layout checks for tests: assertions over the element dump ([`crate::dev::Dump`]) of a window's
//! last frame, by element name, plus a headless app with real text measurement to render in.
//!
//! ```ignore
//! let layout = Layout::of(window);
//! let mut check = layout.check();
//! check.same_row("save", "cancel");
//! check.same_height(&["save", "cancel"]);
//! check.inside("save/label", "save");
//! check.no_overflow("save/label");
//! check.background("save", cx.theme().tokens.button_hover);
//! check.assert();          // panics with every failure and the dump
//! ```
//!
//! A name is an element id, or the last few ids of its path joined with `/` (`"toolbar/save"`);
//! it must match exactly one element. Elements are found when they are probed (`.probe()`) or are
//! GPUI Kit controls (which register themselves). The checks are usable from `#[gpui_kit::test]`
//! tests (a `TestAppContext`, whose text system is a fixed-width fake) and from
//! [`headless_app`] (real DirectWrite text, needed when a check depends on text width).

use std::{cell::RefCell, sync::Arc};

use gpui_kit::{
    AnyWindowHandle, App, Background, Bounds, Entity, HeadlessAppContext, Pixels,
    PlatformTextSystem, Render, Size, Window, WindowHandle, px,
};

use crate::dev::{Color, Dump, Entry};

/// How far two edges may be apart and still count as aligned (sub-pixel layout rounding).
const SLACK: f32 = 0.5;

/// A window's last frame, for layout checks.
pub struct Layout {
    dump: Dump,
}

impl Layout {
    /// Reads the window's last completed frame (call after drawing it).
    pub fn of(window: &Window) -> Self {
        Self {
            dump: Dump::capture(window),
        }
    }

    /// The dump the checks read, e.g. to print it.
    pub fn dump(&self) -> &Dump {
        &self.dump
    }

    /// The one element called `name`.
    pub fn get(&self, name: &str) -> Result<&Entry, String> {
        match self.dump.named(name).as_slice() {
            [one] => Ok(one),
            [] => Err(format!("no element named {name:?}")),
            many => Err(format!(
                "{} elements named {name:?}: {}",
                many.len(),
                many.iter().map(|e| e.name()).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    /// The bounds of the element called `name`; panics when there is not exactly one.
    #[track_caller]
    pub fn bounds(&self, name: &str) -> Bounds<Pixels> {
        match self.get(name) {
            Ok(entry) => entry.bounds,
            Err(error) => panic!("{error}\n{}", self.dump.to_text()),
        }
    }

    /// The painted background of the element called `name`.
    pub fn color_of(&self, name: &str) -> Option<Color> {
        self.get(name).ok().and_then(|e| e.background)
    }

    /// The text colour of the element called `name` (probed elements only).
    pub fn text_color_of(&self, name: &str) -> Option<Color> {
        self.get(name).ok().and_then(|e| e.text_color)
    }

    /// Starts a list of checks; each failure is collected, [`Checks::assert`] reports them all.
    pub fn check(&self) -> Checks<'_> {
        Checks {
            layout: self,
            failures: Vec::new(),
        }
    }
}

/// Checks over one [`Layout`], collecting failures instead of stopping at the first.
pub struct Checks<'a> {
    layout: &'a Layout,
    failures: Vec<String>,
}

impl Checks<'_> {
    fn entry(&mut self, name: &str) -> Option<Entry> {
        match self.layout.get(name) {
            Ok(entry) => Some(entry.clone()),
            Err(error) => {
                self.failures.push(error);
                None
            }
        }
    }

    fn fail(&mut self, message: String) {
        self.failures.push(message);
    }

    /// `a` and `b` sit on one row: their vertical centres line up.
    pub fn same_row(&mut self, a: &str, b: &str) -> &mut Self {
        if let (Some(ea), Some(eb)) = (self.entry(a), self.entry(b)) {
            let (ca, cb) = (ea.bounds.center().y, eb.bounds.center().y);
            if (ca - cb).abs() > px(SLACK) {
                self.fail(format!(
                    "same_row({a}, {b}): centres at y={} and y={}",
                    ca.as_f32(),
                    cb.as_f32()
                ));
            }
        }
        self
    }

    /// Every element in `names` has the same height.
    pub fn same_height(&mut self, names: &[&str]) -> &mut Self {
        let entries: Vec<_> = names.iter().filter_map(|n| self.entry(n)).collect();
        if let Some(first) = entries.first() {
            let h = first.bounds.size.height;
            if entries
                .iter()
                .any(|e| (e.bounds.size.height - h).abs() > px(SLACK))
            {
                let heights: Vec<String> = entries
                    .iter()
                    .map(|e| format!("{}={}", e.name(), e.bounds.size.height.as_f32()))
                    .collect();
                self.fail(format!("same_height: {}", heights.join(", ")));
            }
        }
        self
    }

    /// `child` lies within `parent`'s bounds.
    pub fn inside(&mut self, child: &str, parent: &str) -> &mut Self {
        if let (Some(c), Some(p)) = (self.entry(child), self.entry(parent))
            && !contains(&p.bounds, &c.bounds)
        {
            self.fail(format!(
                "inside({child}, {parent}): {} is not within {}",
                show(&c.bounds),
                show(&p.bounds)
            ));
        }
        self
    }

    /// Nothing of `name` is cut off: it lies within the window, every named element inside its
    /// path lies within it, and for `.probe_text(..)` its text fits its width.
    pub fn no_overflow(&mut self, name: &str) -> &mut Self {
        let Some(entry) = self.entry(name) else {
            return self;
        };
        let window = Bounds {
            origin: Default::default(),
            size: self.layout.dump.viewport,
        };
        if !contains(&window, &entry.bounds) {
            self.fail(format!(
                "no_overflow({name}): {} leaves the {}x{} window",
                show(&entry.bounds),
                window.size.width.as_f32(),
                window.size.height.as_f32()
            ));
        }
        let children: Vec<Entry> = self
            .layout
            .dump
            .entries
            .iter()
            .filter(|e| e.path.len() > entry.path.len() && e.path.starts_with(&entry.path))
            .cloned()
            .collect();
        for child in children {
            if !contains(&entry.bounds, &child.bounds) {
                self.fail(format!(
                    "no_overflow({name}): {} at {} sticks out of {}",
                    child.name(),
                    show(&child.bounds),
                    show(&entry.bounds)
                ));
            }
        }
        if let Some(needed) = entry.text_width
            && needed > entry.bounds.size.width + px(SLACK)
        {
            self.fail(format!(
                "no_overflow({name}): its text needs {} px, it has {}",
                needed.as_f32(),
                entry.bounds.size.width.as_f32()
            ));
        }
        self
    }

    /// The painted background of `name` is `expected` (a theme token).
    pub fn background(&mut self, name: &str, expected: impl Into<Background>) -> &mut Self {
        let expected = expected.into().as_solid().map(Color::from);
        if let Some(entry) = self.entry(name) {
            let ok = match (entry.background, expected) {
                (Some(have), Some(want)) => have.near(want, 1),
                (None, Some(want)) => want.0[3] == 0,
                _ => false,
            };
            if !ok {
                self.fail(format!(
                    "background({name}): {} instead of {}",
                    opt(entry.background),
                    opt(expected)
                ));
            }
        }
        self
    }

    /// The text colour of `name` (a probed element) is `expected`.
    pub fn text_color(&mut self, name: &str, expected: gpui_kit::Hsla) -> &mut Self {
        let expected = Color::from(expected);
        if let Some(entry) = self.entry(name)
            && !entry.text_color.is_some_and(|c| c.near(expected, 1))
        {
            self.fail(format!(
                "text_color({name}): {} instead of {expected}",
                opt(entry.text_color)
            ));
        }
        self
    }

    /// `name`'s width is within `range` (inclusive).
    pub fn width_within(&mut self, name: &str, min: f32, max: f32) -> &mut Self {
        if let Some(entry) = self.entry(name) {
            let w = entry.bounds.size.width.as_f32();
            if w < min - SLACK || w > max + SLACK {
                self.fail(format!("width_within({name}): {w} not in {min}..={max}"));
            }
        }
        self
    }

    /// `name` is hovered, focused or disabled as given (`None`: not checked).
    pub fn state(
        &mut self,
        name: &str,
        hovered: Option<bool>,
        focused: Option<bool>,
        disabled: Option<bool>,
    ) -> &mut Self {
        if let Some(entry) = self.entry(name) {
            if hovered.is_some_and(|h| h != entry.hovered) {
                self.fail(format!("state({name}): hovered is {}", entry.hovered));
            }
            if focused.is_some_and(|f| Some(f) != entry.focused) {
                self.fail(format!("state({name}): focused is {:?}", entry.focused));
            }
            if disabled.is_some_and(|d| d != entry.disabled) {
                self.fail(format!("state({name}): disabled is {}", entry.disabled));
            }
        }
        self
    }

    /// The failures so far.
    pub fn failures(&self) -> &[String] {
        &self.failures
    }

    /// Panics listing every failure and the dump, if anything failed.
    #[track_caller]
    pub fn assert(&self) {
        if !self.failures.is_empty() {
            panic!(
                "{} layout check(s) failed:\n  {}\n\nelement dump:\n{}",
                self.failures.len(),
                self.failures.join("\n  "),
                self.layout.dump.to_text()
            );
        }
    }
}

fn contains(outer: &Bounds<Pixels>, inner: &Bounds<Pixels>) -> bool {
    let s = px(SLACK);
    inner.origin.x >= outer.origin.x - s
        && inner.origin.y >= outer.origin.y - s
        && inner.bottom_right().x <= outer.bottom_right().x + s
        && inner.bottom_right().y <= outer.bottom_right().y + s
}

fn show(b: &Bounds<Pixels>) -> String {
    format!(
        "({},{} {}x{})",
        b.origin.x.as_f32(),
        b.origin.y.as_f32(),
        b.size.width.as_f32(),
        b.size.height.as_f32()
    )
}

fn opt(color: Option<Color>) -> String {
    color.map_or("none".into(), |c| c.to_string())
}

thread_local! {
    // Building a Windows platform for its text system takes a while; one per test thread.
    static TEXT_SYSTEM: RefCell<Option<Arc<dyn PlatformTextSystem>>> = const { RefCell::new(None) };
}

/// The real DirectWrite text system. `current_platform(true)` (headless) on Windows gives GPUI's
/// fixed-width fake instead, which is what `TestAppContext` uses too (docs/PITFALLS.md).
pub fn text_system() -> Arc<dyn PlatformTextSystem> {
    TEXT_SYSTEM.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(|| gpui_kit::platform::current_platform(false).text_system())
            .clone()
    })
}

/// A headless app (GPUI's test platform) with real text measurement and GPUI Kit's assets,
/// initialised, with no renderer: for layout checks.
pub fn headless_app() -> HeadlessAppContext {
    let mut cx =
        HeadlessAppContext::with_asset_source(text_system(), Arc::new(gpui_kit::assets::Assets));
    cx.update(gpui_kit::init);
    cx
}

/// Opens a window of `size` (logical pixels) on `cx` with `build`'s view, draws it, and returns
/// it. Test windows have a scale factor of 2.
pub fn open_window<V: Render + 'static>(
    cx: &mut HeadlessAppContext,
    size: Size<Pixels>,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> WindowHandle<V> {
    let handle = cx.open_window(size, build).expect("open a test window");
    cx.run_until_parked();
    draw(cx, handle.into());
    handle
}

/// Draws the window's next frame (after a change made outside a window update).
pub fn draw(cx: &mut HeadlessAppContext, window: AnyWindowHandle) {
    cx.update_window(window, |_, window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    })
    .expect("draw the test window");
}

/// The layout of the window's last frame.
pub fn layout(cx: &mut HeadlessAppContext, window: AnyWindowHandle) -> Layout {
    cx.update_window(window, |_, window, _| Layout::of(window))
        .expect("read the test window")
}

#[cfg(test)]
mod tests {
    //! Each helper on a hand-made dump: it passes on a good layout and names the problem on a bad
    //! one (the button story tests them on a rendered frame).

    use gpui_kit::{Hsla, hsla, point, size};

    use super::*;

    fn entry(path: &str, (x, y, w, h): (f32, f32, f32, f32)) -> Entry {
        Entry {
            path: path.split('/').map(str::to_string).collect(),
            bounds: Bounds::new(point(px(x), px(y)), size(px(w), px(h))),
            visible: true,
            background: None,
            text_color: None,
            border: None,
            text_width: None,
            hovered: false,
            focused: None,
            disabled: false,
            label: None,
        }
    }

    fn layout_of(entries: Vec<Entry>) -> Layout {
        Layout {
            dump: Dump {
                entries,
                viewport: size(px(200.), px(100.)),
                mouse: point(px(0.), px(0.)),
            },
        }
    }

    fn red() -> Hsla {
        hsla(0., 1., 0.5, 1.)
    }

    fn blue() -> Hsla {
        hsla(0.66, 1., 0.5, 1.)
    }

    /// A toolbar with two buttons; `save` has a caption.
    fn toolbar() -> Vec<Entry> {
        let mut save = entry("toolbar/save", (10., 5., 60., 30.));
        save.background = Some(Color::from(red()));
        save.hovered = true;
        save.focused = Some(true);
        let mut caption = entry("toolbar/save/caption", (15., 10., 50., 20.));
        caption.text_color = Some(Color::from(blue()));
        caption.text_width = Some(px(40.));
        let mut cancel = entry("toolbar/cancel", (80., 5., 60., 30.));
        cancel.disabled = true;
        vec![entry("toolbar", (0., 0., 200., 40.)), save, caption, cancel]
    }

    #[core::prelude::v1::test]
    fn every_check_passes_on_a_good_layout() {
        let layout = layout_of(toolbar());
        let mut check = layout.check();
        check
            .same_row("save", "cancel")
            .same_height(&["save", "cancel"])
            .inside("save/caption", "save")
            .inside("save", "toolbar")
            .no_overflow("toolbar")
            .no_overflow("caption")
            .background("save", red())
            .text_color("caption", blue())
            .width_within("save", 60., 60.)
            .state("save", Some(true), Some(true), Some(false))
            .state("cancel", Some(false), None, Some(true));
        assert_eq!(check.failures(), &[] as &[String]);
        check.assert();
        assert_eq!(layout.color_of("save"), Some(Color::from(red())));
        assert_eq!(layout.color_of("cancel"), None);
        assert_eq!(layout.text_color_of("caption"), Some(Color::from(blue())));
        assert_eq!(layout.bounds("cancel").origin.x, px(80.));
    }

    #[core::prelude::v1::test]
    fn each_check_reports_a_bad_layout() {
        let mut entries = toolbar();
        // cancel: lower and taller; caption: text wider than it; a child out of the toolbar and the
        // window; a second element named `save`.
        entries[3].bounds = Bounds::new(point(px(80.), px(20.)), size(px(60.), px(40.)));
        entries[2].text_width = Some(px(70.));
        entries.push(entry("toolbar/menu", (190., 0., 20., 10.)));
        entries.push(entry("dialog/save", (0., 50., 10., 10.)));
        let layout = layout_of(entries);
        let mut check = layout.check();
        check
            .same_row("toolbar/save", "cancel")
            .same_height(&["toolbar/save", "cancel"])
            .inside("menu", "toolbar")
            .no_overflow("toolbar")
            .no_overflow("menu")
            .no_overflow("caption")
            .background("toolbar/save", blue())
            .text_color("caption", red())
            .width_within("toolbar/save", 0., 50.)
            .state("toolbar/save", Some(false), Some(false), Some(true))
            .same_row("save", "nothing-here");
        let failures = check.failures().join("\n");
        for expected in [
            "same_row(toolbar/save, cancel)",
            "same_height:",
            "inside(menu, toolbar)",
            "no_overflow(toolbar): toolbar/menu",
            "no_overflow(menu): (190,0 20x10) leaves the 200x100 window",
            "no_overflow(caption): its text needs 70",
            "background(toolbar/save)",
            "text_color(caption)",
            "width_within(toolbar/save)",
            "state(toolbar/save): hovered is true",
            "state(toolbar/save): focused is Some(true)",
            "state(toolbar/save): disabled is false",
            "2 elements named \"save\"",
            "no element named \"nothing-here\"",
        ] {
            assert!(
                failures.contains(expected),
                "missing {expected:?} in:\n{failures}"
            );
        }
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check.assert()))
            .expect_err("assert panics");
        let message = panic.downcast_ref::<String>().expect("a message");
        assert!(
            message.contains("element dump:\n# window 200x100"),
            "{message}"
        );
    }
}
