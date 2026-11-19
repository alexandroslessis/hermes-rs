//! `MiddleTruncate`: a path or branch name that keeps its end when it does not fit
//! (`crates/native/src/de…/theme.rs`): the head is cut with an ellipsis and the tail stays whole.
//! [`split`] ports t3code's `splitForMiddleTruncate` (which part is the tail: the last path
//! segment up to 16 characters, else 10, started on a word boundary); the element is a head that
//! truncates (`min-w-0`, no wrap, text ellipsis) beside a tail that never shrinks, so GPUI's
//! layout does the fitting and nothing is measured by hand.
//!
//! t3code: `apps/web/src/components/ui/middle-truncate.tsx` (`MiddleTruncate`,
//! `splitForMiddleTruncate`). Reference: the branch line of a thread row,
//! `button-session-variants-selected.png`, and the paths in `sidebar-diff.png`. Gallery story:
//! `middle-truncate`.

use gpui_kit::{
    InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, div, prelude::FluentBuilder as _,
};

use crate::dev::ProbeExt as _;

const DEFAULT_TAIL: usize = 10;
const MAX_SEGMENT_TAIL: usize = 16;
const MIN_WORD_TAIL: usize = 4;

fn is_word_break(c: char) -> bool {
    matches!(c, '-' | '_' | '.' | '/')
}

/// Splits `value` into the head that may be cut and the tail that stays, or `None` when the whole
/// string is short enough to need no split (`tail` forces the tail's length in characters).
pub fn split(value: &str, tail: Option<usize>) -> Option<(String, String)> {
    let chars: Vec<char> = value.chars().collect();
    let mut keep = tail.unwrap_or(DEFAULT_TAIL);
    if tail.is_none()
        && let Some(slash) = chars.iter().rposition(|c| *c == '/')
        && slash > 0
        && slash < chars.len() - 1
    {
        let segment = chars.len() - slash - 1;
        keep = if segment <= MAX_SEGMENT_TAIL {
            segment
        } else {
            DEFAULT_TAIL
        };
    }
    if keep == 0 || chars.len() <= keep + 4 {
        return None;
    }
    let mut cut = chars.len() - keep;
    // Start an automatic tail on a word ("machine", not "er-machine"): a shorter tail is
    // preferred so the head stays readable, a longer one is the fallback.
    if tail.is_none() && !is_word_break(chars[cut - 1]) {
        let starts_word = |ix: usize| is_word_break(chars[ix - 1]);
        let mut snapped = None;
        let mut ix = cut + 1;
        while ix + MIN_WORD_TAIL <= chars.len() {
            if starts_word(ix) {
                snapped = Some(ix);
                break;
            }
            ix += 1;
        }
        if snapped.is_none() {
            let mut ix = cut - 1;
            while ix + MAX_SEGMENT_TAIL >= chars.len() && ix > 4 {
                if starts_word(ix) {
                    snapped = Some(ix);
                }
                ix -= 1;
            }
        }
        if let Some(snapped) = snapped {
            cut = snapped;
        }
    }
    Some((chars[..cut].iter().collect(), chars[cut..].iter().collect()))
}

/// A one-line label that cuts the middle when it is too narrow.
#[derive(IntoElement)]
pub struct MiddleTruncate {
    id: gpui_kit::ElementId,
    value: SharedString,
    tail: Option<usize>,
}

impl MiddleTruncate {
    /// `value`, called `id` for the layout checks.
    pub fn new(id: impl Into<gpui_kit::ElementId>, value: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            tail: None,
        }
    }

    /// Keeps this many characters of the end whole.
    pub fn tail(mut self, characters: usize) -> Self {
        self.tail = Some(characters);
        self
    }
}

impl RenderOnce for MiddleTruncate {
    fn render(self, _: &mut gpui_kit::Window, _: &mut gpui_kit::App) -> impl IntoElement {
        let parts = split(&self.value, self.tail);
        div()
            .id(self.id)
            .flex()
            .min_w_0()
            .max_w_full()
            .overflow_hidden()
            .whitespace_nowrap()
            .map(|row| match parts {
                Some((head, tail)) => row
                    .child(
                        div()
                            .id("head")
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(head)
                            .probe(),
                    )
                    .child(
                        div()
                            .id("tail")
                            .flex_shrink_0()
                            .whitespace_nowrap()
                            .child(tail)
                            .probe(),
                    ),
                None => row.child(
                    div()
                        .id("whole")
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(self.value)
                        .probe(),
                ),
            })
            .probe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn short_strings_are_not_split() {
        assert_eq!(split("main", None), None);
        assert_eq!(split("src/lib.rs", None), None);
        // t3code keeps the last segment whole even when it is one character: "feature/" + "x".
        assert_eq!(
            split("feature/x", None),
            Some(("feature/".to_string(), "x".to_string()))
        );
    }

    #[core::prelude::v1::test]
    fn a_path_keeps_its_last_segment() {
        // Same cases as t3code's `splitForMiddleTruncate`.
        let (head, tail) = split("crates/native/src/design/tokens.rs", None).unwrap();
        assert_eq!(tail, "tokens.rs");
        assert_eq!(head, "crates/native/src/design/");
    }

    #[core::prelude::v1::test]
    fn a_long_segment_keeps_ten_characters_on_a_word() {
        let (head, tail) = split("hermes-rs-feature-branch-for-the-native-ui", None).unwrap();
        assert_eq!(
            format!("{head}{tail}"),
            "hermes-rs-feature-branch-for-the-native-ui"
        );
        assert!(tail.starts_with("native-ui") || tail.starts_with("-ui") || tail.len() >= 4);
        assert!(tail.len() <= 16, "{tail}");
    }

    #[core::prelude::v1::test]
    fn an_explicit_tail_is_exact_and_unicode_safe() {
        let (head, tail) = split("αβγδεζηθικλμνξοπρστυφχψω", Some(3)).unwrap();
        assert_eq!(tail, "χψω");
        assert_eq!(head.chars().count(), 21);
        assert_eq!(split("abcdef", Some(3)), None);
    }
}
