//! The gallery: every component and screen as a story, with fake data, in each of its states. The
//! `hermes-native-gallery` binary shows them ([`GalleryView`]); the snapshot tests
//! (`cargo test -p hermes-native --features snapshots`) render each story's states to PNG and
//! compare them with the approved images in `crates/native/snapshots/`.
//!
//! A story is a [`Story`] constant listed once in [`register_stories!`] below; that list is what
//! both the gallery and the snapshot harness read.
//!
//! Like Zed's storybook and GPUI Kit's `story` crate (structure only; no code from either).

mod button;
mod composer;
mod design;
mod frame;
mod header;
mod panel;
mod panel_files;
mod sidebar;
mod terminal;
#[cfg(all(test, feature = "snapshots"))]
mod snapshots;
mod thread;
mod tokens;
mod view;

pub use view::GalleryView;

use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    Styled as _, Window, component::ActiveTheme as _, div,
};

use crate::dev::ProbeExt as _;

/// One component or screen, shown in each of its states.
pub struct Story {
    /// Snapshot file stem (`crates/native/snapshots/<name>-<theme>.png`): kebab-case, unique.
    pub name: &'static str,
    /// What the gallery lists.
    pub title: &'static str,
    /// The window each state is rendered in for snapshots, in logical pixels.
    pub size: (f32, f32),
    /// The themes the snapshot harness renders it under.
    pub themes: Themes,
    /// The states, left to right.
    pub states: &'static [State],
    /// Renders the story in one state.
    pub render: fn(&State, &mut Window, &mut App) -> AnyElement,
}

/// Which light and dark theme a story's snapshots are taken in. The CP0.2 and CP2.1 stories
/// (the Kit button, the token swatches) keep GPUI Kit's own themes, which their baselines were
/// made under; every story of the design layer is checked under t3code's two themes, the ones the
/// app wears (`-t3-light.png`, `-t3-dark.png`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Themes {
    /// GPUI Kit's default light and dark (`-light.png`, `-dark.png`).
    Kit,
    /// t3code's default light and dark (`-t3-light.png`, `-t3-dark.png`).
    T3,
}

impl Themes {
    /// The light and the dark theme.
    pub fn pair(self) -> [crate::theme::ThemeChoice; 2] {
        use crate::theme::ThemeChoice;
        match self {
            Themes::Kit => [ThemeChoice::DefaultLight, ThemeChoice::DefaultDark],
            Themes::T3 => [ThemeChoice::T3Light, ThemeChoice::T3Dark],
        }
    }
}

/// One state of a story: a name, and what the harness does to the rendered story to reach it
/// (hover, press, focus); states that are a property of the component (disabled) are rendered
/// by [`Story::render`] from the name.
pub struct State {
    /// The state's name, shown in the gallery (`normal`, `hover`, `pressed`, `focused`, `disabled`).
    pub name: &'static str,
    /// What the harness does after the first frame.
    pub interaction: Interaction,
}

/// What the snapshot harness and the layout tests do to put a story in a state. The gallery
/// leaves these to the person at the mouse.
#[derive(Clone, Copy, Debug)]
pub enum Interaction {
    /// Nothing.
    None,
    /// The pointer rests on the element with this id.
    Hover(&'static str),
    /// The left button is held down on the element with this id.
    Press(&'static str),
    /// Keyboard focus is on the element with this id (reached by Tab order).
    Focus(&'static str),
}

/// Lists the stories; the gallery and the snapshot harness both iterate [`all`].
macro_rules! register_stories {
    ($($story:path),* $(,)?) => {
        /// Every registered story, in gallery order.
        pub fn all() -> &'static [&'static Story] {
            static ALL: &[&Story] = &[$(&$story),*];
            ALL
        }
    };
}

register_stories![
    button::STATES_STORY,
    tokens::SWATCHES_STORY,
    design::BUTTON_DEFAULT,
    design::BUTTON_OUTLINE,
    design::BUTTON_GHOST,
    design::BUTTON_GHOST_MUTED,
    design::BUTTON_DESTRUCTIVE,
    design::BUTTON_SIZES,
    design::ICON_BUTTON,
    design::LIST_ROW,
    design::THREAD_ROW,
    design::THREAD_STATUS,
    design::SECTION_HEADER,
    design::PANEL,
    design::BADGE,
    design::KBD,
    design::TOOLTIP,
    design::TOAST,
    design::EMPTY_STATE,
    design::MIDDLE_TRUNCATE,
    frame::TITLE_BAR,
    frame::PALETTE,
    frame::APP_FRAME,
    sidebar::SIDEBAR,
    sidebar::SIDEBAR_MORE,
    sidebar::EMPTY_STATES,
    composer::COMPOSER,
    composer::COMPOSER_MENUS,
    composer::COMPOSER_NEW,
    composer::COMPOSER_MENTIONS,
    composer::COMPOSER_COMMANDS,
    composer::COMPOSER_ATTACHMENTS,
    composer::COMPOSER_ASKS,
    sidebar::SIDEBAR_WAITING,
    thread::THREAD,
    thread::THREAD_LIVE,
    thread::TOOLS_COMMAND,
    thread::TOOLS_EDIT,
    thread::TOOLS_LOOKUP,
    thread::TOOLS_AGENT,
    thread::TOOLS_GROUP,
    thread::TOOLS_FOOTER,
    header::THREAD_HEADER,
    panel::PANEL_DIFF,
    panel_files::PANEL_FILES,
    panel_files::FILE_FINDER,
    terminal::TERMINAL,
    terminal::PANEL_TERMINAL,
];

/// One story in one state, on the theme's background: the root view of a snapshot window, and
/// what the gallery shows in each column. Its element is `story`, the crop of a snapshot.
pub struct StoryCell {
    story: &'static Story,
    state: usize,
}

impl StoryCell {
    /// `story` in its `state`-th state.
    pub fn new(story: &'static Story, state: usize) -> Self {
        Self { story, state }
    }
}

impl Render for StoryCell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The story's element takes the size of its content (the crop), not the window's.
        div()
            .size_full()
            .flex()
            .items_start()
            .bg(cx.theme().background)
            .child(story_cell(self.story, self.state, window, cx))
    }
}

/// The element of one story in one state: `story`, padded, on the theme's background, as large
/// as its content.
pub fn story_cell(story: &Story, state: usize, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let (background, foreground) = (theme.background, theme.foreground);
    div()
        .id("story")
        .flex()
        .items_start()
        .p_3()
        .bg(background)
        .text_color(foreground)
        .child((story.render)(&story.states[state], window, cx))
        .probe()
        .into_any_element()
}

/// Opens a window of the story's size showing `story` in its `state`-th state under `theme`,
/// put there by the state's interaction, and draws it.
#[cfg(test)]
pub(crate) fn open_story(
    cx: &mut gpui_kit::HeadlessAppContext,
    story: &'static Story,
    state: usize,
    theme: crate::theme::ThemeChoice,
) -> gpui_kit::AnyWindowHandle {
    use crate::testing::{draw, open_window};
    use gpui_kit::{AppContext as _, px, size};

    cx.update(|cx| crate::theme::apply_theme(theme, cx));
    let (w, h) = story.size;
    let window = open_window(cx, size(px(w), px(h)), |_, cx| {
        cx.new(|_| StoryCell::new(story, state))
    });
    let interaction = story.states[state].interaction;
    cx.update_window(window.into(), |_, window, cx| {
        interact(interaction, window, cx)
    })
    .expect("interact with the story");
    cx.run_until_parked();
    draw(cx, window.into());
    window.into()
}

/// Puts the rendered story into `interaction`'s state (hover, press, focus) and draws the frame.
#[cfg(any(test, feature = "dev"))]
pub fn interact(interaction: Interaction, window: &mut Window, cx: &mut App) {
    use gpui_kit::{
        MouseButton,
        test::{TestEventExt as _, TestWindowExt as _},
    };
    match interaction {
        Interaction::None => {}
        Interaction::Hover(id) => window.hover(id, cx),
        Interaction::Press(id) => {
            window.hover(id, cx);
            let center = window.find(id).bounds().center();
            window.pointer_down(center, MouseButton::Left, cx);
        }
        Interaction::Focus(id) => {
            // Tab order, as a keyboard user gets there; a story has few tab stops. A key press
            // first, so the window counts the input as keyboard and draws the `focus-visible`
            // ring (a bare modifier: no binding, no text).
            if let Ok(key) = gpui_kit::Keystroke::parse("shift") {
                window.dispatch_keystroke(key, cx);
            }
            for _ in 0..16 {
                window.focus_next(cx);
                window.render_frame(cx);
                if window.find(id).focused() == Some(true) {
                    break;
                }
            }
        }
    }
    window.render_frame(cx);
}
