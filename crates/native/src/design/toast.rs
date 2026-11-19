//! Toasts: short notices for things that have no other place on screen (a background review
//! finished, a queued prompt could not start, a settings file failed to load). Two parts.
//!
//! - [`Toasts`] is the **model**: the stack of live toasts, with a clock passed in so the logic is
//!   deterministic (`push`, `dismiss`, `hold` while the pointer is over the stack, `expire`,
//!   `next_deadline`). At most [`MAX_VISIBLE`] show at once, the newest at the bottom; older ones
//!   wait and appear as the visible ones go.
//! - [`Toast`] is the **view** of one notice: a glass card (`Panel::card`'s surface) with a
//!   coloured icon, a title, an optional description, an optional action and a dismiss button.
//!   The workspace stacks them bottom right and drives `Toasts` with a timer.
//!
//! The app does not use `hermes_core` until Phase 3, so the source of toasts is [`ToastSpec`]: a
//! plain value a test or [`crate::fake`] makes; Phase 3 maps `CoreEvent`s that have no other
//! place (`CuratorFinished`, `QueuedFailed`, a failed background turn) onto it.
//!
//! t3code: `apps/web/src/components/ui/toast.tsx` (`ToastProvider`: bottom-right viewport, `max-w-90`
//! (360 px), `--toast-inset` 32 px, glass card `rounded-lg`, `TOAST_ICONS` coloured by type, the
//! dismiss orb, Base UI's 5 s default timeout, the timer pausing while the viewport is hovered).
//! No reference screenshot exists and t3code was not run to take one (progress note); gallery
//! story: `toast`.
//!
//! Not copied: the stacked-card peek animation and the swipe to dismiss.

use std::time::Duration;

use gpui_kit::{
    AnyElement, App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, assets::IconName, component::Icon,
    component::Sizable as _, component::Size as KitSize, div, prelude::FluentBuilder as _, px,
};
use std::rc::Rc;

use super::{
    button::{self, Button},
    tokens::{radius, text},
};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// How many toasts show at once.
pub const MAX_VISIBLE: usize = 3;
/// How long a notice stays unless it says otherwise (Base UI's default, which t3code uses).
pub const DEFAULT_TTL: Duration = Duration::from_secs(5);
/// How long an error stays (long enough to read and copy).
pub const ERROR_TTL: Duration = Duration::from_secs(10);
/// The card's width (`max-w-90`).
pub const WIDTH: f32 = 360.;
/// The gap between the window's edge and the stack (`--toast-inset` at `sm:`).
pub const INSET: f32 = 32.;
/// The gap between two cards.
pub const GAP: f32 = 12.;

/// What a notice is about; decides the icon and its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Information.
    Info,
    /// Something finished.
    Success,
    /// Something needs attention.
    Warning,
    /// Something failed.
    Error,
    /// Something is under way (stays until dismissed or replaced).
    Loading,
}

impl Kind {
    /// Every kind, in the gallery's order.
    pub const ALL: [Kind; 5] = [
        Kind::Info,
        Kind::Success,
        Kind::Warning,
        Kind::Error,
        Kind::Loading,
    ];

    /// The name used in the gallery.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Info => "info",
            Kind::Success => "success",
            Kind::Warning => "warning",
            Kind::Error => "error",
            Kind::Loading => "loading",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Kind::Info => IconName::Info,
            Kind::Success => IconName::CircleCheck,
            Kind::Warning => IconName::TriangleAlert,
            Kind::Error => IconName::CircleAlert,
            Kind::Loading => IconName::LoaderCircle,
        }
    }

    /// How long it stays by default; `None` stays until dismissed.
    pub fn default_ttl(self) -> Option<Duration> {
        match self {
            Kind::Loading => None,
            Kind::Error => Some(ERROR_TTL),
            _ => Some(DEFAULT_TTL),
        }
    }
}

/// A notice to show.
#[derive(Clone, Debug, PartialEq)]
pub struct ToastSpec {
    /// What it is about.
    pub kind: Kind,
    /// The headline.
    pub title: String,
    /// More, in muted text.
    pub description: Option<String>,
    /// The label of an action button (what it does is the caller's).
    pub action: Option<String>,
    /// How long it stays; `None` takes the kind's default ([`Kind::default_ttl`]).
    pub ttl: Option<Duration>,
}

impl ToastSpec {
    /// A notice of `kind` with a title.
    pub fn new(kind: Kind, title: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            description: None,
            action: None,
            ttl: None,
        }
    }

    /// Adds a description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Adds an action button.
    pub fn action(mut self, label: impl Into<String>) -> Self {
        self.action = Some(label.into());
        self
    }

    /// Sets how long it stays.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }
}

/// A toast's identity, stable while it lives.
pub type ToastId = u64;

/// A live toast: its notice and when it goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Live {
    /// Its id.
    pub id: ToastId,
    /// What it says.
    pub spec: ToastSpec,
    /// The clock reading at which it expires; `None`: stays until dismissed.
    pub expires: Option<Duration>,
}

/// The stack of live toasts (model; times are readings of one monotonic clock passed in).
#[derive(Debug, Default)]
pub struct Toasts {
    items: Vec<Live>,
    next_id: ToastId,
    /// While `Some`, the clock reading when the pointer came over the stack: the expiry of every
    /// toast is pushed back by how long it stays there.
    held_since: Option<Duration>,
}

impl Toasts {
    /// An empty stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a toast at clock reading `now`; returns its id.
    pub fn push(&mut self, spec: ToastSpec, now: Duration) -> ToastId {
        let id = self.next_id;
        self.next_id += 1;
        let expires = spec
            .ttl
            .or_else(|| spec.kind.default_ttl())
            .map(|ttl| now + ttl);
        self.items.push(Live { id, spec, expires });
        id
    }

    /// Removes a toast; true when it was there.
    pub fn dismiss(&mut self, id: ToastId) -> bool {
        let before = self.items.len();
        self.items.retain(|t| t.id != id);
        // The last one gone under the pointer: the stack is no longer drawn, so the pointer's
        // "left" never arrives; without this the next toasts would never expire.
        if self.items.is_empty() {
            self.held_since = None;
        }
        self.items.len() != before
    }

    /// The pointer is over the stack at `now`: nothing expires until [`Toasts::release`].
    pub fn hold(&mut self, now: Duration) {
        self.held_since.get_or_insert(now);
    }

    /// The pointer left at `now`: the time spent over the stack is added to every expiry.
    pub fn release(&mut self, now: Duration) {
        if let Some(since) = self.held_since.take() {
            let held = now.saturating_sub(since);
            for toast in &mut self.items {
                if let Some(expires) = &mut toast.expires {
                    *expires += held;
                }
            }
        }
    }

    /// Removes the toasts due at `now` (none while held); returns their ids.
    pub fn expire(&mut self, now: Duration) -> Vec<ToastId> {
        if self.held_since.is_some() {
            return Vec::new();
        }
        let due: Vec<ToastId> = self
            .items
            .iter()
            .filter(|t| t.expires.is_some_and(|at| at <= now))
            .map(|t| t.id)
            .collect();
        self.items.retain(|t| !due.contains(&t.id));
        due
    }

    /// When the next *visible* toast expires, for the timer (`None`: nothing to wait for, or held).
    pub fn next_deadline(&self) -> Option<Duration> {
        if self.held_since.is_some() {
            return None;
        }
        self.visible().iter().filter_map(|t| t.expires).min()
    }

    /// The toasts shown now: the newest [`MAX_VISIBLE`], oldest first (so the newest is last, at
    /// the bottom of the stack).
    pub fn visible(&self) -> &[Live] {
        let from = self.items.len().saturating_sub(MAX_VISIBLE);
        &self.items[from..]
    }

    /// How many are waiting behind the visible ones.
    pub fn hidden(&self) -> usize {
        self.items.len().saturating_sub(MAX_VISIBLE)
    }

    /// Every live toast, oldest first.
    pub fn all(&self) -> &[Live] {
        &self.items
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// One notice, drawn.
#[derive(IntoElement)]
pub struct Toast {
    id: gpui_kit::ElementId,
    kind: Kind,
    title: SharedString,
    description: Option<SharedString>,
    action: Option<SharedString>,
    on_dismiss: Option<Handler>,
    on_action: Option<Handler>,
}

impl Toast {
    /// A toast called `id` for `spec`.
    pub fn new(id: impl Into<gpui_kit::ElementId>, spec: &ToastSpec) -> Self {
        Self {
            id: id.into(),
            kind: spec.kind,
            title: spec.title.clone().into(),
            description: spec.description.clone().map(Into::into),
            action: spec.action.clone().map(Into::into),
            on_dismiss: None,
            on_action: None,
        }
    }

    /// Called when the dismiss button is clicked.
    pub fn on_dismiss(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(f));
        self
    }

    /// Called when the action button is clicked.
    pub fn on_action(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_action = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for Toast {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let accent = match self.kind {
            Kind::Info => palette.info,
            Kind::Success => palette.success,
            Kind::Warning => palette.warning,
            Kind::Error => palette.destructive,
            Kind::Loading => palette.muted_foreground,
        };
        let surface = palette.panel_surface.over(palette.background).hsla();
        let on_dismiss = self.on_dismiss;
        let on_action = self.on_action;
        div()
            .id(self.id)
            .flex()
            .items_start()
            .gap(px(8.))
            .w(px(WIDTH))
            .max_w_full()
            .px(px(12.))
            .py(px(10.))
            .rounded(radius::LG)
            .border_1()
            .border_color(palette.panel_edge.hsla())
            .bg(surface)
            .text_color(palette.popover_foreground.hsla())
            .text_size(text::SM.size)
            .line_height(text::SM.line_height)
            .child(
                div()
                    .id("icon")
                    .flex_shrink_0()
                    .h(text::SM.line_height)
                    .flex()
                    .items_center()
                    .child(
                        Icon::new(self.kind.icon())
                            .with_size(KitSize::Size(px(16.)))
                            .text_color(accent.hsla()),
                    )
                    .probe(),
            )
            .child(
                div()
                    .id("body")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        div()
                            .id("title")
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(self.title)
                            .probe(),
                    )
                    .when_some(self.description, |d, description| {
                        d.child(
                            div()
                                .id("description")
                                .text_color(palette.muted_foreground.hsla())
                                .child(description)
                                .probe(),
                        )
                    })
                    .when_some(self.action, |d, label| {
                        let mut action = Button::new("action", label)
                            .variant(button::Variant::Outline)
                            .size(button::Size::Xs);
                        if let Some(f) = on_action {
                            action = action.on_click(move |e, w, cx| f(e, w, cx));
                        }
                        d.child(div().id("actions").flex().mt(px(6.)).child(action).probe())
                    })
                    .probe(),
            )
            .child({
                let mut dismiss = Button::icon_only("dismiss", IconName::WindowClose)
                    .variant(button::Variant::GhostMuted)
                    .size(button::Size::Micro)
                    .tooltip("Dismiss");
                if let Some(f) = on_dismiss {
                    dismiss = dismiss.on_click(move |e, w, cx| f(e, w, cx));
                }
                dismiss
            })
            .probe()
    }
}

/// The toasts of `stack` as elements, oldest first (the workspace puts them in a column at the
/// bottom right, so the newest sits lowest).
pub fn stack_elements(toasts: &Toasts, on_dismiss: impl Fn(ToastId) -> Handler) -> Vec<AnyElement> {
    toasts
        .visible()
        .iter()
        .map(|live| {
            Toast::new(("toast", live.id as usize), &live.spec)
                .on_dismiss({
                    let handler = on_dismiss(live.id);
                    move |e, w, cx| handler(e, w, cx)
                })
                .into_any_element()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn info(title: &str) -> ToastSpec {
        ToastSpec::new(Kind::Info, title)
    }

    #[core::prelude::v1::test]
    fn a_toast_expires_after_its_time_and_not_before() {
        let mut t = Toasts::new();
        let id = t.push(info("a"), secs(0));
        assert!(t.expire(secs(4)).is_empty());
        assert_eq!(t.expire(secs(5)), vec![id]);
        assert!(t.is_empty());
    }

    #[core::prelude::v1::test]
    fn errors_stay_longer_and_loading_stays_until_dismissed() {
        let mut t = Toasts::new();
        let error = t.push(ToastSpec::new(Kind::Error, "e"), secs(0));
        let loading = t.push(ToastSpec::new(Kind::Loading, "l"), secs(0));
        assert!(t.expire(secs(9)).is_empty());
        assert_eq!(t.expire(secs(10)), vec![error]);
        assert!(t.expire(secs(10_000)).is_empty());
        assert!(t.dismiss(loading));
        assert!(!t.dismiss(loading));
    }

    #[core::prelude::v1::test]
    fn the_newest_three_show_and_older_ones_wait() {
        let mut t = Toasts::new();
        let ids: Vec<_> = (0..5)
            .map(|i| t.push(info(&format!("t{i}")), secs(i)))
            .collect();
        let shown: Vec<_> = t.visible().iter().map(|l| l.id).collect();
        assert_eq!(shown, ids[2..]);
        assert_eq!(t.hidden(), 2);
        // The newest is last: it is drawn lowest in the stack.
        assert_eq!(t.visible().last().unwrap().spec.title, "t4");
        // Dismissing a visible one lets a waiting one in.
        t.dismiss(ids[3]);
        let shown: Vec<_> = t.visible().iter().map(|l| l.id).collect();
        assert_eq!(shown, vec![ids[1], ids[2], ids[4]]);
    }

    #[core::prelude::v1::test]
    fn holding_the_pointer_over_the_stack_pauses_every_timer() {
        let mut t = Toasts::new();
        let id = t.push(info("a"), secs(0));
        t.hold(secs(2));
        assert_eq!(t.next_deadline(), None);
        assert!(t.expire(secs(100)).is_empty());
        t.release(secs(12)); // held for 10 s: now due at 15 s
        assert_eq!(t.next_deadline(), Some(secs(15)));
        assert!(t.expire(secs(14)).is_empty());
        assert_eq!(t.expire(secs(15)), vec![id]);
    }

    #[core::prelude::v1::test]
    fn dismissing_the_last_toast_under_the_pointer_ends_the_hold() {
        let mut t = Toasts::new();
        let id = t.push(info("a"), secs(0));
        t.hold(secs(1));
        t.dismiss(id);
        // The stack is gone, so no "pointer left" comes; a new toast still expires.
        let next = t.push(info("b"), secs(2));
        assert_eq!(t.next_deadline(), Some(secs(7)));
        assert_eq!(t.expire(secs(7)), vec![next]);
    }

    #[core::prelude::v1::test]
    fn the_deadline_is_the_earliest_of_the_visible_toasts() {
        let mut t = Toasts::new();
        t.push(info("a").ttl(secs(30)), secs(0));
        t.push(info("b").ttl(secs(8)), secs(1));
        assert_eq!(t.next_deadline(), Some(secs(9)));
        assert_eq!(Toasts::new().next_deadline(), None);
    }

    #[core::prelude::v1::test]
    fn ids_are_not_reused() {
        let mut t = Toasts::new();
        let a = t.push(info("a"), secs(0));
        t.dismiss(a);
        let b = t.push(info("b"), secs(0));
        assert_ne!(a, b);
    }
}
