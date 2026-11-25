//! The terminal's stories (CP6.3). `terminal`: the drawer's terminal group as the app draws it (a
//! 720 x 300 pane) over recorded byte streams, in five states: `prompt` (a shell's prompt and the
//! cursor, the floating toolbar), `colours` (16 / 256 / true colour, every attribute, wide and
//! emoji characters, box drawing), `tui` (a full-screen program: frame, list, inverse status bar),
//! `list` (two terminals: t3code's list at the right, one titled by its program), `scrolled` (a
//! selection and the "lines below" indicator). `panel-terminal`: the right panel (380 x 620) with
//! the "+" chooser open (`chooser`) and on the Terminal tab (`tab`). Reference:
//! `app-terminal-open.png` (the drawer), `sidebar-right.png` (the chooser).
//!
//! The terminals' shells are the fixture's scripted ones with no output; the content is put in the
//! emulator once the grid's size is known ([`TerminalView::script`]), so a story draws the same
//! twice without waiting for a pump.

use std::sync::Arc;

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::Side;
use alacritty_terminal::selection::SelectionType;
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled as _, Window, div, px,
};

use super::{Interaction, State, Story, Themes};
use crate::{
    design::{Panel, panel::Edge},
    dev::ProbeExt as _,
    model::{
        AppModel, FixtureBackend,
        fixture::{self, NOW_MS},
    },
    panel::RightPanel,
    terminal::{
        emulator::Emulator,
        fixture::{FixturePty, PROMPT, colours_demo, numbered_lines, tui_demo},
        group::{PtyMaker, Surface, TerminalGroup},
        session::Pty,
    },
};

const fn at_rest(name: &'static str) -> State {
    State {
        name,
        interaction: Interaction::None,
    }
}

const STATES: [State; 5] = [
    at_rest("prompt"),
    at_rest("colours"),
    at_rest("tui"),
    at_rest("list"),
    at_rest("scrolled"),
];

pub const WIDTH: f32 = 720.;
pub const HEIGHT: f32 = 300.;

fn prompt(e: &mut Emulator) {
    e.feed(b"PowerShell 7.5.4\r\n");
    e.feed(PROMPT.as_bytes());
}

fn make(name: &str, window: &mut Window, cx: &mut App) -> Entity<TerminalGroup> {
    let maker: PtyMaker = Arc::new(|cols, rows| FixturePty::scripted(cols, rows) as Arc<dyn Pty>);
    let name = name.to_string();
    cx.new(|cx| {
        let mut group = TerminalGroup::with_maker(maker, Surface::Drawer, cx);
        group.new_terminal(window, cx);
        if name == "list" {
            group.new_terminal(window, cx);
        }
        let terminals = group.terminals().to_vec();
        terminals[0].update(cx, |t, _| match name.as_str() {
            "colours" => t.script(|e, _, _| {
                e.feed(b"PS C:\\fixture\\halo> colors\r\n");
                e.feed(&colours_demo());
                e.feed(PROMPT.as_bytes());
            }),
            "tui" => t.script(|e, cols, rows| {
                e.feed(b"PS C:\\fixture\\halo> tui\r\n");
                e.feed(&tui_demo(cols, rows));
            }),
            "scrolled" => t.script(|e, _, rows| {
                e.feed(&numbered_lines(200));
                e.feed(PROMPT.as_bytes());
                e.scroll(Scroll::Delta(12));
                let (a, b) = (e.point_at(0, rows / 2), e.point_at(9, rows / 2 + 1));
                e.start_selection(SelectionType::Simple, a, Side::Left);
                e.extend_selection(b, Side::Right);
            }),
            _ => t.script(|e, _, _| prompt(e)),
        });
        if name == "list" {
            terminals[1].update(cx, |t, _| {
                t.script(|e, _, _| {
                    e.feed(b"\x1b]0;cargo watch\x07");
                    e.feed(b"[Running 'cargo check']\r\n    \x1b[1;32mFinished\x1b[0m `dev` profile in 0.42s\r\n");
                })
            });
        }
        group
    })
}

fn cell(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let name = state.name;
    let group = window.use_keyed_state(
        gpui_kit::ElementId::Name(format!("terminal-story-{name}").into()),
        cx,
        |window, cx| make(name, window, cx),
    );
    div()
        .id("terminal-frame")
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .child(
            Panel::pane("drawer", Edge::Top)
                .size_full()
                .child(group.read(cx).clone()),
        )
        .probe()
        .into_any_element()
}

/// The drawer's terminals in five states.
pub static TERMINAL: Story = Story {
    name: "terminal",
    title: "Terminal drawer",
    size: (WIDTH + 24., HEIGHT + 24.),
    themes: Themes::T3,
    states: &STATES,
    render: cell,
};

// --- the right panel: the "+" chooser and the Terminal tab -------------------------------------------

const PANEL_STATES: [State; 2] = [at_rest("chooser"), at_rest("tab")];

pub const PANEL_WIDTH: f32 = 380.;
pub const PANEL_HEIGHT: f32 = 620.;

fn panel(name: &str, window: &mut Window, cx: &mut App) -> Entity<RightPanel> {
    let snapshot = fixture::snapshot();
    let backend = FixtureBackend::new(snapshot.clone());
    backend.quiet_terminals();
    let app = cx.new(|cx| {
        AppModel::with_snapshot(Arc::clone(&backend) as _, snapshot, cx).with_clock(|| NOW_MS)
    });
    app.update(cx, |app, cx| app.open("t1", cx));
    let thread = app.read(cx).thread().cloned();
    let name = name.to_string();
    cx.new(|cx| {
        let mut panel = RightPanel::new(app, window, cx);
        panel.set_thread(thread, cx);
        if name == "chooser" {
            panel.toggle_chooser(window, cx);
        } else {
            panel.open_terminal(window, cx);
            if let Some(group) = panel.terminal_group(cx) {
                let terminal = group.read(cx).active().cloned();
                if let Some(terminal) = terminal {
                    terminal.update(cx, |t, _| t.script(|e, _, _| prompt(e)));
                }
            }
        }
        panel
    })
}

fn panel_cell(state: &State, window: &mut Window, cx: &mut App) -> AnyElement {
    let name = state.name;
    let panel = window.use_keyed_state(
        gpui_kit::ElementId::Name(format!("panel-terminal-story-{name}").into()),
        cx,
        |window, cx| panel(name, window, cx),
    );
    div()
        .id("panel-frame")
        .w(px(PANEL_WIDTH))
        .h(px(PANEL_HEIGHT))
        .child(
            Panel::pane("right-panel", Edge::Left)
                .size_full()
                .child(panel.read(cx).clone()),
        )
        .probe()
        .into_any_element()
}

/// The right panel's "+" chooser and its Terminal tab.
pub static PANEL_TERMINAL: Story = Story {
    name: "panel-terminal",
    title: "Right panel: surfaces and terminal",
    size: (PANEL_WIDTH + 24., PANEL_HEIGHT + 24.),
    themes: Themes::T3,
    states: &PANEL_STATES,
    render: panel_cell,
};
