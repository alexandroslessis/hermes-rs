//! `Kbd` and `KbdGroup`: a key hint. `Kbd` is one key (20 px high, the foreground at 7% behind
//! muted 12 px text; the `outline` variant is a bordered "physical key" with a thicker bottom
//! edge); `KbdGroup` draws a chord such as `Ctrl+Shift+P` as one `Kbd` per key. GPUI Kit's `Kbd`
//! (from a `Keystroke`, with its own colours) is not used for the look; [`chord_label`] is the one
//! place that turns a GPUI keystroke into the label we show (`Ctrl+Shift+P`), so the palette, the
//! tooltips and the keymap table all spell shortcuts alike.
//!
//! t3code: `apps/web/src/components/ui/kbd.tsx` (`Kbd`: `h-5 min-w-5 rounded px-1 text-xs
//! text-muted-foreground bg-foreground/[0.07]`, `outline`: `border border-b-2
//! border-foreground/15 px-1.5`; `KbdGroup`: `gap-1`); `formatShortcutKeyLabel` in
//! `keybindings.ts`. No reference screenshot of its own exists (it shows in the palette and in
//! tooltips: `button-sidebar_toggle-hover.png`); gallery story: `kbd`.

use gpui_kit::{
    InteractiveElement as _, IntoElement, Keystroke, ParentElement as _, RenderOnce, SharedString,
    Styled as _, div, prelude::FluentBuilder as _, px,
};

use super::tokens::text;
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// One key hint.
#[derive(IntoElement, Clone)]
pub struct Kbd {
    key: SharedString,
    outline: bool,
}

impl Kbd {
    /// A hint showing `key` (`Ctrl`, `K`, `Enter`).
    pub fn new(key: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            outline: false,
        }
    }

    /// The bordered "physical key" look, for hints inside running text.
    pub fn outline(mut self) -> Self {
        self.outline = true;
        self
    }
}

impl RenderOnce for Kbd {
    fn render(self, _: &mut gpui_kit::Window, cx: &mut gpui_kit::App) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        div()
            .id("kbd")
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .h(px(20.))
            .min_w(px(20.))
            .rounded(px(4.))
            .px(px(if self.outline { 6. } else { 4. }))
            .text_size(text::XS.size)
            .line_height(text::XS.line_height)
            .text_color(palette.muted_foreground.hsla())
            .when(!self.outline, |d| {
                d.bg(palette.foreground.with_alpha(18).hsla())
            })
            .when(self.outline, |d| {
                d.border_1()
                    .border_b_2()
                    .border_color(palette.foreground.with_alpha(38).hsla())
            })
            .child(div().id("key").child(self.key.clone()).probe_text(self.key))
            .probe()
    }
}

/// A chord, one [`Kbd`] per key with a 4 px gap (`Ctrl+Shift+P` is three).
#[derive(IntoElement, Clone)]
pub struct KbdGroup {
    keys: Vec<SharedString>,
    outline: bool,
}

impl KbdGroup {
    /// A chord written `Ctrl+Shift+P` (split on `+`; a literal plus is written `Plus`).
    pub fn new(chord: &str) -> Self {
        Self {
            keys: chord
                .split('+')
                .map(|k| k.trim().to_string().into())
                .collect(),
            outline: false,
        }
    }

    /// The bordered look for every key.
    pub fn outline(mut self) -> Self {
        self.outline = true;
        self
    }
}

impl RenderOnce for KbdGroup {
    fn render(self, _: &mut gpui_kit::Window, _: &mut gpui_kit::App) -> impl IntoElement {
        div()
            .id("kbd-group")
            .flex()
            .items_center()
            .gap(px(4.))
            .children(self.keys.into_iter().map(|key| {
                let kbd = Kbd::new(key);
                if self.outline { kbd.outline() } else { kbd }
            }))
            .probe()
    }
}

/// The label of a keystroke as shown to the user: `Ctrl+Alt+Shift+K`, `Ctrl+Enter`, `F6`.
pub fn chord_label(keystroke: &Keystroke) -> String {
    let m = keystroke.modifiers;
    let mut parts: Vec<String> = Vec::new();
    if m.control {
        parts.push("Ctrl".into());
    }
    if m.alt {
        parts.push("Alt".into());
    }
    if m.shift {
        parts.push("Shift".into());
    }
    if m.platform {
        parts.push("Win".into());
    }
    parts.push(key_label(&keystroke.key));
    parts.join("+")
}

fn key_label(key: &str) -> String {
    match key {
        "enter" => "Enter".into(),
        "escape" => "Esc".into(),
        "backspace" => "Backspace".into(),
        "delete" => "Delete".into(),
        "space" => "Space".into(),
        "tab" => "Tab".into(),
        "up" => "Up".into(),
        "down" => "Down".into(),
        "left" => "Left".into(),
        "right" => "Right".into(),
        "pageup" => "Page Up".into(),
        "pagedown" => "Page Down".into(),
        other if other.chars().count() == 1 => other.to_uppercase(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn labels_spell_keystrokes_the_windows_way() {
        let label = |s: &'static str| chord_label(&Keystroke::parse(s).unwrap());
        assert_eq!(label("ctrl-k"), "Ctrl+K");
        assert_eq!(label("ctrl-shift-p"), "Ctrl+Shift+P");
        assert_eq!(label("ctrl-alt-b"), "Ctrl+Alt+B");
        assert_eq!(label("f6"), "F6");
        assert_eq!(label("ctrl-enter"), "Ctrl+Enter");
        assert_eq!(label("escape"), "Esc");
        assert_eq!(label("ctrl-["), "Ctrl+[");
    }
}
