//! `.probe()`: names an element for the element dump ([`super::Dump`]) and the layout checks
//! (`crate::testing`). It registers the element with GPUI Kit's observation (bounds, visibility,
//! focus, disabled, accessible role and label: `.test_support()`) and records, each time the
//! element paints, what Kit's observation doesn't: its background and border colour from its
//! computed style (hover and active styles included), the text colour its content inherits, whether
//! the pointer is over its hitbox, and, for `.probe_text(..)`, the width its text needs on one line.
//!
//! Without the `dev` feature (and outside tests) `.probe()` returns the element unchanged, so views
//! call it unconditionally at no cost.
//!
//! The forwarding element follows GPUI Kit's own observer (`gpui-base`
//! `test_support.rs`, `Observed`): the same Element impl shape, and `compute_style` read in paint.

use gpui_kit::{Element, Hitbox, InteractiveElement, SharedString};

#[cfg(any(test, feature = "dev"))]
pub use imp::{Probe, ProbeFacts, facts};

/// What `.probe()` returns: the element wrapped for observation, or the element itself.
#[cfg(any(test, feature = "dev"))]
pub type Probed<E> = gpui_kit::base::ObservedElement<Probe<E>>;
/// What `.probe()` returns: the element wrapped for observation, or the element itself.
#[cfg(not(any(test, feature = "dev")))]
pub type Probed<E> = E;

/// Names an element (it must have an id) for the dump and the layout checks.
pub trait ProbeExt: Element<PrepaintState = Option<Hitbox>> + InteractiveElement + Sized {
    /// Observe this element: bounds, colours, hover, focus, disabled.
    fn probe(self) -> Probed<Self> {
        #[cfg(any(test, feature = "dev"))]
        {
            use gpui_kit::TestSupportExt as _;
            Probe::new(self, None).test_support()
        }
        #[cfg(not(any(test, feature = "dev")))]
        {
            self
        }
    }

    /// Like [`Self::probe`], for an element showing `text` on one line: the dump also records the
    /// width that text needs, so `no_overflow` can tell a clipped or squeezed label.
    fn probe_text(self, text: impl Into<SharedString>) -> Probed<Self> {
        #[cfg(any(test, feature = "dev"))]
        {
            use gpui_kit::TestSupportExt as _;
            Probe::new(self, Some(text.into())).test_support()
        }
        #[cfg(not(any(test, feature = "dev")))]
        {
            let _ = text;
            self
        }
    }
}

impl<E: Element<PrepaintState = Option<Hitbox>> + InteractiveElement> ProbeExt for E {}

#[cfg(any(test, feature = "dev"))]
mod imp {
    use std::{cell::RefCell, collections::HashMap};

    use gpui_kit::{
        App, Bounds, Element, ElementId, FocusHandle, GlobalElementId, Hitbox, Hsla,
        InspectorElementId, InteractiveElement, Interactivity, IntoElement, LayoutId,
        ParentElement, Pixels, SharedString, StatefulInteractiveElement, StyleRefinement, Styled,
        Window,
    };

    /// What a probed element looked like when it last painted.
    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct ProbeFacts {
        /// Its own background (solid colours only), after hover and active styles.
        pub background: Option<Hsla>,
        /// Its border colour, when it has one.
        pub border: Option<Hsla>,
        /// The text colour its content gets (its own, else the inherited one).
        pub text_color: Hsla,
        /// Whether the pointer was over its hitbox.
        pub hovered: Option<bool>,
        /// The width its text needs on one line, for `.probe_text(..)`.
        pub text_width: Option<Pixels>,
    }

    type WindowKey = usize;

    thread_local! {
        // Keyed by window and element path; an element that stops painting keeps its last entry,
        // which the dump ignores because Kit's observation no longer lists the element.
        static FACTS: RefCell<HashMap<(WindowKey, Vec<ElementId>), ProbeFacts>> =
            RefCell::new(HashMap::new());
    }

    fn window_key(window: &Window) -> WindowKey {
        std::rc::Rc::as_ptr(window.text_system()) as usize
    }

    /// The facts recorded for the element at `path` in `window`.
    pub fn facts(window: &Window, path: &[ElementId]) -> Option<ProbeFacts> {
        let key = (window_key(window), path.to_vec());
        FACTS.with(|facts| facts.borrow().get(&key).cloned())
    }

    /// The forwarding element behind `.probe()`.
    pub struct Probe<E> {
        inner: E,
        text: Option<SharedString>,
    }

    impl<E: Element> Probe<E> {
        pub(super) fn new(inner: E, text: Option<SharedString>) -> Self {
            Self { inner, text }
        }
    }

    impl<E: Element<PrepaintState = Option<Hitbox>> + InteractiveElement> IntoElement for Probe<E> {
        type Element = Self;
        fn into_element(self) -> Self {
            self
        }
    }

    impl<E: Element<PrepaintState = Option<Hitbox>> + InteractiveElement> Element for Probe<E> {
        type RequestLayoutState = E::RequestLayoutState;
        type PrepaintState = Option<Hitbox>;

        fn id(&self) -> Option<ElementId> {
            Element::id(&self.inner)
        }

        fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
            self.inner.source_location()
        }

        fn request_layout(
            &mut self,
            id: Option<&GlobalElementId>,
            inspector: Option<&InspectorElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (LayoutId, Self::RequestLayoutState) {
            self.inner.request_layout(id, inspector, window, cx)
        }

        fn prepaint(
            &mut self,
            id: Option<&GlobalElementId>,
            inspector: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            layout: &mut Self::RequestLayoutState,
            window: &mut Window,
            cx: &mut App,
        ) -> Self::PrepaintState {
            self.inner
                .prepaint(id, inspector, bounds, layout, window, cx)
        }

        fn paint(
            &mut self,
            id: Option<&GlobalElementId>,
            inspector: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            layout: &mut Self::RequestLayoutState,
            hitbox: &mut Self::PrepaintState,
            window: &mut Window,
            cx: &mut App,
        ) {
            if let Some(global_id) = id {
                let style =
                    self.inner
                        .interactivity()
                        .compute_style(id, hitbox.as_ref(), window, cx);
                let mut text_style = window.text_style();
                if let Some(color) = style.text.color {
                    text_style.color = color;
                }
                let text_width = self.text.as_ref().map(|text| {
                    let font_size = text_style.font_size.to_pixels(window.rem_size());
                    let run = text_style.to_run(text.len());
                    window
                        .text_system()
                        .shape_line(text.clone(), font_size, &[run], None)
                        .width
                });
                let recorded = ProbeFacts {
                    background: style
                        .background
                        .as_ref()
                        .and_then(|fill| fill.color())
                        .and_then(|background| background.as_solid()),
                    border: style.border_color,
                    text_color: text_style.color,
                    hovered: hitbox.as_ref().map(|hitbox| hitbox.is_hovered(window)),
                    text_width,
                };
                let key = (window_key(window), global_id.to_vec());
                FACTS.with(|facts| facts.borrow_mut().insert(key, recorded));
            }
            self.inner
                .paint(id, inspector, bounds, layout, hitbox, window, cx);
        }

        fn a11y_role(&self) -> Option<gpui_kit::Role> {
            self.inner.a11y_role()
        }

        fn write_a11y_info(&self, node: &mut gpui_kit::accesskit::Node) {
            self.inner.write_a11y_info(node);
        }

        fn a11y_synthetic_children(
            &mut self,
            prepaint: &mut Self::PrepaintState,
            builder: &mut gpui_kit::A11ySubtreeBuilder,
        ) {
            self.inner.a11y_synthetic_children(prepaint, builder);
        }
    }

    impl<E: Element<PrepaintState = Option<Hitbox>> + InteractiveElement + Styled> Styled for Probe<E> {
        fn style(&mut self) -> &mut StyleRefinement {
            self.inner.style()
        }
    }

    impl<E: Element<PrepaintState = Option<Hitbox>> + InteractiveElement> InteractiveElement
        for Probe<E>
    {
        fn interactivity(&mut self) -> &mut Interactivity {
            self.inner.interactivity()
        }

        fn track_focus(mut self, focus: &FocusHandle) -> Self {
            self.inner = self.inner.track_focus(focus);
            self
        }
    }

    impl<E: Element<PrepaintState = Option<Hitbox>> + InteractiveElement + ParentElement>
        ParentElement for Probe<E>
    {
        fn extend(&mut self, elements: impl IntoIterator<Item = gpui_kit::AnyElement>) {
            self.inner.extend(elements);
        }
    }

    impl<E: Element<PrepaintState = Option<Hitbox>> + StatefulInteractiveElement>
        StatefulInteractiveElement for Probe<E>
    {
    }
}
