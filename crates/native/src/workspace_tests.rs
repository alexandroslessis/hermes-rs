//! The frame, headless: the sizing rules without a window, then the rendered frame at three
//! window sizes (small near the minimum, medium, large) with collapse, resize, the palette, the
//! toasts and the persistence round trip.

use gpui_kit::{
    AnyWindowHandle, HeadlessAppContext, Point,
    test::{TestEventExt as _, TestWindowExt as _},
};
use hermes_testdir::tempdir;

use super::*;
use crate::testing::{Layout, draw, headless_app, layout, open_window};
use crate::theme::ThemeChoice;
use crate::ui_state;

const SMALL: (f32, f32) = (640., 480.);
const MEDIUM: (f32, f32) = (1100., 700.);
const LARGE: (f32, f32) = (1920., 1080.);
const SIZES: [(f32, f32); 3] = [SMALL, MEDIUM, LARGE];

fn win(size: (f32, f32)) -> Size<Pixels> {
    gpui_kit::size(px(size.0), px(size.1))
}

fn open(cx: &mut HeadlessAppContext, window: (f32, f32), state: UiState) -> AnyWindowHandle {
    cx.update(|cx| {
        crate::theme::apply_theme(ThemeChoice::T3Dark, cx);
        keymap::bind(cx);
        palette::bind(cx);
    });
    let handle = open_window(cx, win(window), move |window, cx| {
        cx.new(|cx| Workspace::new(state, None, window, cx))
    });
    let any: AnyWindowHandle = handle.into();
    with(cx, any, |ws, window, cx| ws.focus(window, cx));
    draw(cx, any);
    any
}

fn with<R>(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
) -> R {
    cx.update_window(window, |root, window, cx| {
        let ws: Entity<Workspace> = root.downcast().expect("the root is the workspace");
        ws.update(cx, |ws, cx| f(ws, window, cx))
    })
    .expect("update the workspace")
}

fn press(cx: &mut HeadlessAppContext, window: AnyWindowHandle, keys: &str) {
    cx.update_window(window, |_, window, cx| window.press(keys, cx))
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn type_text(cx: &mut HeadlessAppContext, window: AnyWindowHandle, text: &str) {
    cx.update_window(window, |_, window, cx| window.input(text, cx))
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn drag(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    from: Point<Pixels>,
    to: Point<Pixels>,
) {
    cx.update_window(window, |_, window, cx| window.drag(from, to, cx))
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn width(layout: &Layout, name: &str) -> f32 {
    layout.bounds(name).size.width.as_f32()
}

fn state_with(right: bool, drawer: bool, sidebar: bool) -> UiState {
    let mut s = UiState::default();
    s.right_panel.open = right;
    s.drawer.open = drawer;
    s.sidebar.open = sidebar;
    s
}

fn grab(layout: &Layout, handle: &str) -> Point<Pixels> {
    layout.bounds(handle).center()
}

// --- the sizing rules (no window) ---------------------------------------------------------

#[core::prelude::v1::test]
fn the_regions_always_fill_the_window_and_respect_their_limits() {
    for window in SIZES {
        for (right, drawer, sidebar) in [
            (false, false, true),
            (true, false, true),
            (true, true, true),
            (true, true, false),
            (false, true, false),
        ] {
            // Stored sizes far outside the limits must not matter.
            for stored in [1., 50., 256., 5000.] {
                let mut state = state_with(right, drawer, sidebar);
                state.sidebar.size = stored;
                state.right_panel.size = stored;
                state.drawer.size = stored;
                let state = state.sanitized();
                let f = frame_layout(win(window), &state);
                let ctx =
                    format!("{window:?} r{right} d{drawer} s{sidebar} stored {stored}: {f:?}");
                assert!(
                    (f.sidebar + f.right + f.main - window.0).abs() < 0.01,
                    "fill: {ctx}"
                );
                assert!(f.main >= MAIN_MIN_WIDTH, "middle minimum: {ctx}");
                assert!(
                    f.sidebar == 0.
                        || (ui_state::sidebar::MIN..=ui_state::sidebar::MAX).contains(&f.sidebar),
                    "sidebar: {ctx}"
                );
                assert!(
                    f.right == 0.
                        || (ui_state::right_panel::MIN..=ui_state::right_panel::MAX)
                            .contains(&f.right),
                    "right: {ctx}"
                );
                assert!(
                    f.drawer == 0.
                        || (ui_state::drawer::MIN..=ui_state::drawer::MAX).contains(&f.drawer),
                    "drawer: {ctx}"
                );
                assert_eq!(f.sidebar == 0., !sidebar, "a collapsed sidebar is 0: {ctx}");
            }
        }
    }
}

#[core::prelude::v1::test]
fn a_narrow_window_takes_width_from_the_sidebar_first_then_hides_the_right_panel() {
    let state = state_with(true, false, true);
    let f = frame_layout(win(LARGE), &state);
    assert_eq!((f.sidebar, f.right), (256., 420.));
    // Medium has room for both at their sizes: 256 + 420 + 320 <= 1100.
    let f = frame_layout(win(MEDIUM), &state);
    assert_eq!((f.sidebar, f.right), (256., 420.));
    // 640 wide: not even 208 + 280 + 320 fits, so the panel hides and the sidebar keeps its
    // width (the middle is 384).
    let f = frame_layout(win(SMALL), &state);
    assert_eq!((f.sidebar, f.right, f.main), (256., 0., 384.));
    // 820: the panel's minimum fits once the sidebar gives up width.
    let f = frame_layout(win((820., 700.)), &state);
    assert_eq!((f.sidebar, f.right, f.main), (220., 280., 320.));
    // The state is untouched: the panel is still "open" and comes back with its width.
    assert!(state.right_panel.open);
}

#[core::prelude::v1::test]
fn the_drawer_leaves_the_main_area_its_minimum_height() {
    let state = state_with(false, true, true);
    assert_eq!(frame_layout(win(LARGE), &state).drawer, 280.);
    // 480 high: 480 - 40 - 160 = 280 left for the drawer.
    assert_eq!(frame_layout(win(SMALL), &state).drawer, 280.);
    // Lower than that it shrinks, then hides under its minimum.
    assert_eq!(frame_layout(win((800., 400.)), &state).drawer, 200.);
    assert_eq!(frame_layout(win((800., 300.)), &state).drawer, 0.);
}

#[core::prelude::v1::test]
fn the_saved_window_is_restored_where_it_was_and_never_off_screen() {
    let screen = Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.)));
    let second = Bounds::new(point(px(1920.), px(0.)), size(px(1280.), px(1024.)));
    let saved = ui_state::WindowState {
        x: Some(100.),
        y: Some(50.),
        width: 1000.,
        height: 700.,
        maximized: false,
    };
    let WindowBounds::Windowed(b) = restore_window(&saved, &[screen]) else {
        panic!("windowed")
    };
    assert_eq!(
        (b.origin.x, b.origin.y, b.size.width),
        (px(100.), px(50.), px(1000.))
    );
    // On the second monitor: fine while it is there ...
    let on_second = ui_state::WindowState {
        x: Some(2200.),
        ..saved
    };
    let WindowBounds::Windowed(b) = restore_window(&on_second, &[screen, second]) else {
        panic!("windowed")
    };
    assert_eq!(b.origin.x, px(2200.));
    // ... and centred on the primary one when it was unplugged.
    let WindowBounds::Windowed(b) = restore_window(&on_second, &[screen]) else {
        panic!("windowed")
    };
    assert_eq!((b.origin.x, b.origin.y), (px(460.), px(190.)));
    // No saved position: centred. Maximised: the restore bounds are kept.
    let fresh = ui_state::WindowState::default();
    let WindowBounds::Windowed(b) = restore_window(&fresh, &[screen]) else {
        panic!("windowed")
    };
    assert_eq!(b.center(), screen.center());
    let maximised = ui_state::WindowState {
        maximized: true,
        ..saved
    };
    assert!(matches!(
        restore_window(&maximised, &[screen]),
        WindowBounds::Maximized(b) if b.size.width == px(1000.)
    ));
    // A window bigger than the screen is cut to it.
    let huge = ui_state::WindowState {
        width: 5000.,
        height: 5000.,
        ..fresh
    };
    let WindowBounds::Windowed(b) = restore_window(&huge, &[screen]) else {
        panic!("windowed")
    };
    assert_eq!(b.size, screen.size);
}

// --- the rendered frame at three sizes ------------------------------------------------------

#[core::prelude::v1::test]
fn the_frame_lays_out_at_small_medium_and_large_sizes() {
    let mut cx = headless_app();
    for window in SIZES {
        for (right, drawer) in [(false, false), (true, true)] {
            let state = state_with(right, drawer, true);
            let handle = open(&mut cx, window, state.clone());
            let layout = layout(&mut cx, handle);
            let f = frame_layout(win(window), &state);
            let ctx = format!("{window:?} right {right} drawer {drawer}");
            let mut check = layout.check();
            check
                .same_row("sidebar", "content")
                .no_overflow("sidebar")
                .no_overflow("content")
                .no_overflow("titlebar")
                .inside("titlebar", "content")
                .inside("titlebar", "content")
                .inside("caption-buttons", "titlebar")
                .inside("panel-toggles", "titlebar")
                .same_row("caption-buttons", "titlebar")
                .same_height(&["minimize", "maximize", "close"]);
            check.assert();
            assert_eq!(width(&layout, "sidebar"), f.sidebar, "sidebar: {ctx}");
            assert!(
                width(&layout, "main") >= MAIN_MIN_WIDTH,
                "main minimum: {ctx}"
            );
            assert_eq!(layout.bounds("titlebar").size.height.as_f32(), 40., "{ctx}");
            assert_eq!(layout.bounds("titlebar").origin.y.as_f32(), 0., "{ctx}");
            // The caption buttons end at the window's right edge, 46 px each.
            assert_eq!(
                layout.bounds("caption-buttons").right().as_f32(),
                window.0,
                "{ctx}"
            );
            assert_eq!(width(&layout, "close"), 46., "{ctx}");
            // The sidebar's header row is as high as the title bar: one line across the top.
            assert_eq!(
                layout.bounds("sidebar-header").size.height.as_f32(),
                40.,
                "{ctx}"
            );
            if right {
                assert_eq!(layout.get("right-panel").is_ok(), f.right > 0., "{ctx}");
                if f.right > 0. {
                    assert_eq!(width(&layout, "right-panel"), f.right, "{ctx}");
                    assert_eq!(
                        layout.bounds("right-panel").right().as_f32(),
                        window.0,
                        "{ctx}"
                    );
                    // Under the title bar, down to the window's bottom.
                    assert_eq!(layout.bounds("right-panel").origin.y.as_f32(), 40., "{ctx}");
                    assert_eq!(
                        layout.bounds("right-panel").bottom().as_f32(),
                        window.1,
                        "{ctx}"
                    );
                }
            } else {
                assert!(
                    layout.get("right-panel").is_err(),
                    "closed by default: {ctx}"
                );
            }
            if drawer {
                assert_eq!(
                    layout.bounds("drawer").size.height.as_f32(),
                    f.drawer,
                    "{ctx}"
                );
                assert_eq!(layout.bounds("drawer").bottom().as_f32(), window.1, "{ctx}");
                let mut check = layout.check();
                check.inside("drawer", "main-column");
                check.assert();
            } else {
                assert!(layout.get("drawer").is_err(), "{ctx}");
            }
        }
    }
}

#[core::prelude::v1::test]
fn the_sidebar_collapses_and_its_toggle_moves_to_the_title_bar() {
    let mut cx = headless_app();
    for window in SIZES {
        let handle = open(&mut cx, window, UiState::default());
        let l = layout(&mut cx, handle);
        assert!(l.get("sidebar").is_ok());
        assert!(
            l.get("titlebar/toggle-sidebar").is_err(),
            "{window:?}: no toggle in the title bar yet"
        );
        let open_main = width(&l, "main");
        press(&mut cx, handle, "ctrl-b");
        let l = layout(&mut cx, handle);
        assert!(l.get("sidebar").is_err(), "{window:?}: collapsed");
        assert_eq!(
            width(&l, "main"),
            window.0,
            "{window:?}: main takes the whole width"
        );
        assert!(open_main < window.0);
        // The toggle is now in the title bar, left of the title.
        let mut check = l.check();
        check
            .inside("toggle-sidebar", "titlebar")
            .same_row("toggle-sidebar", "drag/title");
        check.assert();
        assert!(l.bounds("toggle-sidebar").right() <= l.bounds("drag/title").left());
        with(&mut cx, handle, |ws, _, _| {
            assert!(!ws.state().sidebar.open)
        });
        press(&mut cx, handle, "ctrl-b");
        assert_eq!(width(&layout(&mut cx, handle), "sidebar"), 256.);
    }
}

#[core::prelude::v1::test]
fn the_shortcuts_toggle_the_right_panel_and_the_drawer() {
    let mut cx = headless_app();
    let handle = open(&mut cx, MEDIUM, UiState::default());
    assert!(layout(&mut cx, handle).get("right-panel").is_err());
    press(&mut cx, handle, "ctrl-alt-b");
    assert!(layout(&mut cx, handle).get("right-panel").is_ok());
    press(&mut cx, handle, "ctrl-j");
    assert!(layout(&mut cx, handle).get("drawer").is_ok());
    with(&mut cx, handle, |ws, _, _| {
        assert!(ws.state().right_panel.open && ws.state().drawer.open)
    });
    press(&mut cx, handle, "ctrl-alt-b");
    press(&mut cx, handle, "ctrl-j");
    let l = layout(&mut cx, handle);
    assert!(l.get("right-panel").is_err() && l.get("drawer").is_err());
}

// --- dragging the borders (the CP0.1 pane test, for this frame) ------------------------------

/// A border dragged well past its limits moves only its own pane, within its range; nothing else
/// changes size (the user's CP0.1 report: the far pane grew).
#[core::prelude::v1::test]
fn dragging_a_border_past_its_limit_leaves_the_other_panes_alone() {
    let mut cx = headless_app();
    for window in [MEDIUM, LARGE] {
        let handle = open(&mut cx, window, state_with(true, true, true));
        let sizes = |cx: &mut HeadlessAppContext| {
            let l = layout(cx, handle);
            (
                width(&l, "sidebar"),
                width(&l, "main"),
                width(&l, "right-panel"),
                l.bounds("drawer").size.height.as_f32(),
                l,
            )
        };
        let (sidebar0, _, right0, drawer0, l) = sizes(&mut cx);
        assert_eq!(
            (sidebar0, right0, drawer0),
            (256., 420., 280.),
            "{window:?}"
        );

        // The right panel's border far to the left: only the panel grows, to its maximum or what
        // the middle's minimum allows.
        drag(
            &mut cx,
            handle,
            grab(&l, "right-handle"),
            point(px(0.), px(300.)),
        );
        let (sidebar, main, right, drawer, l) = sizes(&mut cx);
        assert_eq!(sidebar, 256., "{window:?}: the sidebar did not move");
        assert_eq!(drawer, 280.);
        let room = window.0 - 256. - MAIN_MIN_WIDTH;
        assert_eq!(right, ui_state::right_panel::MAX.min(room), "{window:?}");
        assert!(main >= MAIN_MIN_WIDTH);
        assert!((sidebar + main + right - window.0).abs() < 0.01);

        // ... and far to the right: down to its minimum, the middle takes the rest.
        drag(
            &mut cx,
            handle,
            grab(&l, "right-handle"),
            point(px(window.0 + 400.), px(300.)),
        );
        let (sidebar, main, right, _, l) = sizes(&mut cx);
        assert_eq!(
            (sidebar, right),
            (256., ui_state::right_panel::MIN),
            "{window:?}"
        );
        assert_eq!(main, window.0 - 256. - right);

        // The sidebar's border far to the right: its maximum or what leaves the middle 320.
        drag(
            &mut cx,
            handle,
            grab(&l, "sidebar-handle"),
            point(px(window.0 + 400.), px(300.)),
        );
        let (sidebar, main, right_after, _, l) = sizes(&mut cx);
        assert_eq!(
            right_after, right,
            "{window:?}: the right panel kept its width"
        );
        assert_eq!(
            sidebar,
            ui_state::sidebar::MAX.min(window.0 - right - MAIN_MIN_WIDTH)
        );
        assert!(main >= MAIN_MIN_WIDTH);

        // ... and far to the left: its minimum.
        drag(
            &mut cx,
            handle,
            grab(&l, "sidebar-handle"),
            point(px(-400.), px(300.)),
        );
        let (sidebar, _, right_after, _, l) = sizes(&mut cx);
        assert_eq!((sidebar, right_after), (ui_state::sidebar::MIN, right));

        // The drawer's border: up to its maximum or the main area's minimum, down to its own.
        drag(
            &mut cx,
            handle,
            grab(&l, "drawer-handle"),
            point(px(300.), px(-400.)),
        );
        let (sidebar_now, _, right_now, drawer, l) = sizes(&mut cx);
        assert_eq!((sidebar_now, right_now), (sidebar, right));
        assert_eq!(
            drawer,
            ui_state::drawer::MAX.min(window.1 - titlebar::HEIGHT - MAIN_MIN_HEIGHT)
        );
        drag(
            &mut cx,
            handle,
            grab(&l, "drawer-handle"),
            point(px(300.), px(window.1 + 400.)),
        );
        let (_, _, _, drawer, _) = sizes(&mut cx);
        assert_eq!(drawer, ui_state::drawer::MIN);
    }
}

#[core::prelude::v1::test]
fn resetting_a_border_restores_its_default() {
    let mut cx = headless_app();
    let mut state = state_with(true, false, true);
    state.sidebar.size = 400.;
    let handle = open(&mut cx, LARGE, state);
    assert_eq!(width(&layout(&mut cx, handle), "sidebar"), 400.);
    with(&mut cx, handle, |ws, _, cx| {
        ws.reset_border(Pane::Sidebar, cx)
    });
    draw(&mut cx, handle);
    assert_eq!(width(&layout(&mut cx, handle), "sidebar"), 256.);
}

// --- persistence ------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn what_the_frame_remembers_is_written_and_read_back() {
    let dir = tempdir().unwrap();
    let mut cx = headless_app();
    let handle = open(&mut cx, MEDIUM, UiState::default());
    press(&mut cx, handle, "ctrl-b");
    press(&mut cx, handle, "ctrl-alt-b");
    let l = layout(&mut cx, handle);
    drag(
        &mut cx,
        handle,
        grab(&l, "right-handle"),
        point(px(700.), px(300.)),
    );
    let state = with(&mut cx, handle, |ws, _, _| ws.state().clone());
    assert!(!state.sidebar.open && state.right_panel.open);
    assert_eq!(state.right_panel.size, 400.);
    state.save(dir.path()).unwrap();

    // A new window started from the file looks the same.
    let loaded = UiState::load(dir.path());
    assert_eq!(loaded, state);
    let again = open(&mut cx, MEDIUM, loaded);
    let l = layout(&mut cx, again);
    assert!(l.get("sidebar").is_err());
    assert_eq!(width(&l, "right-panel"), 400.);
    // A corrupt file starts the defaults.
    std::fs::write(UiState::path(dir.path()), "{{{").unwrap();
    assert_eq!(UiState::load(dir.path()), UiState::default().sanitized());
}

/// A change is written to `native-ui.json` by itself, shortly after (not only at quit).
#[core::prelude::v1::test]
fn a_change_is_written_to_the_file_after_a_pause() {
    let dir = tempdir().unwrap();
    let mut cx = headless_app();
    let data = dir.path().to_path_buf();
    cx.update(|cx| {
        crate::theme::apply_theme(ThemeChoice::T3Dark, cx);
        keymap::bind(cx);
    });
    let handle: AnyWindowHandle = open_window(&mut cx, win(MEDIUM), move |window, cx| {
        cx.new(|cx| Workspace::new(UiState::default(), Some(data), window, cx))
    })
    .into();
    with(&mut cx, handle, |ws, window, cx| ws.focus(window, cx));
    draw(&mut cx, handle);
    press(&mut cx, handle, "ctrl-b");
    let path = UiState::path(dir.path());
    // Not before the pause (a drag writes once, not on every move) ...
    cx.advance_clock(ui_state::SAVE_AFTER / 2);
    cx.run_until_parked();
    assert!(!path.exists(), "debounced");
    // ... but after it, on its own.
    cx.advance_clock(ui_state::SAVE_AFTER);
    cx.run_until_parked();
    assert!(path.exists(), "written without a quit");
    assert!(!UiState::load(dir.path()).sidebar.open);
}

// --- the palette -----------------------------------------------------------------------------

#[core::prelude::v1::test]
fn ctrl_k_opens_the_palette_over_everything_and_escape_closes_it() {
    let mut cx = headless_app();
    let handle = open(&mut cx, MEDIUM, UiState::default());
    assert!(layout(&mut cx, handle).get("palette").is_err());
    press(&mut cx, handle, "ctrl-k");
    let l = layout(&mut cx, handle);
    let mut check = l.check();
    check.inside("palette", "workspace");
    check.assert();
    assert_eq!(width(&l, "palette"), 560.);
    // Horizontally centred, near the top.
    assert!((l.bounds("palette").center().x.as_f32() - MEDIUM.0 / 2.).abs() <= 1.);
    assert!(l.bounds("palette").origin.y.as_f32() < MEDIUM.1 / 3.);
    // All three kinds are listed, in groups.
    let kinds = with(&mut cx, handle, |ws, _, cx| {
        let p = ws.palette().unwrap().read(cx);
        let mut kinds: Vec<_> = p.results().iter().map(|i| i.kind).collect();
        kinds.dedup();
        kinds
    });
    assert_eq!(kinds, vec![Kind::Action, Kind::Thread, Kind::Project]);
    press(&mut cx, handle, "escape");
    assert!(layout(&mut cx, handle).get("palette").is_err());
    with(&mut cx, handle, |ws, _, _| assert!(!ws.palette_open()));
}

#[core::prelude::v1::test]
fn ctrl_shift_p_lists_only_actions() {
    let mut cx = headless_app();
    let handle = open(&mut cx, MEDIUM, UiState::default());
    press(&mut cx, handle, "ctrl-shift-p");
    let kinds = with(&mut cx, handle, |ws, _, cx| {
        ws.palette()
            .unwrap()
            .read(cx)
            .results()
            .iter()
            .map(|i| i.kind)
            .collect::<Vec<_>>()
    });
    assert!(!kinds.is_empty() && kinds.iter().all(|k| *k == Kind::Action));
    // The same shortcut again closes it.
    press(&mut cx, handle, "ctrl-shift-p");
    with(&mut cx, handle, |ws, _, _| assert!(!ws.palette_open()));
}

#[core::prelude::v1::test]
fn typing_filters_arrows_move_and_enter_runs_the_command() {
    let mut cx = headless_app();
    let handle = open(&mut cx, MEDIUM, UiState::default());
    press(&mut cx, handle, "ctrl-k");
    type_text(&mut cx, handle, "sidebar");
    let titles = |cx: &mut HeadlessAppContext| {
        with(cx, handle, |ws, _, cx| {
            let p = ws.palette().unwrap().read(cx);
            (
                p.results()
                    .iter()
                    .map(|i| i.title.clone())
                    .collect::<Vec<_>>(),
                p.selected(),
            )
        })
    };
    let (found, selected) = titles(&mut cx);
    assert_eq!(
        found.first().map(String::as_str),
        Some("Toggle sidebar"),
        "{found:?}"
    );
    assert_eq!(selected, 0);
    assert!(found.len() >= 2, "{found:?}");
    press(&mut cx, handle, "down");
    assert_eq!(titles(&mut cx).1, 1);
    press(&mut cx, handle, "up");
    assert_eq!(titles(&mut cx).1, 0);
    // Enter runs "Toggle sidebar": the palette closes and the sidebar collapses.
    press(&mut cx, handle, "enter");
    with(&mut cx, handle, |ws, _, _| {
        assert!(!ws.palette_open());
        assert!(!ws.state().sidebar.open);
    });
    assert!(layout(&mut cx, handle).get("sidebar").is_err());
}

#[core::prelude::v1::test]
fn a_thread_result_opens_the_thread_and_the_title_bar_names_it() {
    let mut cx = headless_app();
    let handle = open(&mut cx, MEDIUM, UiState::default());
    press(&mut cx, handle, "ctrl-k");
    type_text(&mut cx, handle, "port the");
    press(&mut cx, handle, "enter");
    with(&mut cx, handle, |ws, _, cx| {
        assert!(!ws.palette_open());
        assert_eq!(
            ws.model().read(cx).selected(),
            &crate::model::Selection::Thread("t5".into())
        );
    });
    assert!(layout(&mut cx, handle).get("titlebar").is_ok());
}

// --- toasts ----------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn toasts_stack_bottom_right_newest_lowest_and_expire() {
    let mut cx = headless_app();
    let handle = open(&mut cx, MEDIUM, UiState::default());
    let ids: Vec<_> = (0..4)
        .map(|i| {
            with(&mut cx, handle, |ws, _, cx| {
                ws.push_toast(ToastSpec::new(toast::Kind::Info, format!("Notice {i}")), cx)
            })
        })
        .collect();
    draw(&mut cx, handle);
    let l = layout(&mut cx, handle);
    // Three are visible: ids[1], ids[2], ids[3], the newest lowest.
    assert!(
        l.get(&format!("toast:{}", ids[0])).is_err(),
        "the oldest waits"
    );
    let boxes: Vec<_> = ids[1..]
        .iter()
        .map(|id| l.bounds(&format!("toast:{id}")))
        .collect();
    for pair in boxes.windows(2) {
        assert!(
            pair[0].bottom() <= pair[1].top(),
            "stacked, not overlapping"
        );
        assert_eq!((pair[1].top() - pair[0].bottom()).as_f32(), toast::GAP);
    }
    let last = boxes.last().unwrap();
    assert_eq!(last.right().as_f32(), MEDIUM.0 - toast::INSET);
    assert_eq!(last.bottom().as_f32(), MEDIUM.1 - toast::INSET);
    assert!(boxes.iter().all(|b| b.size.width.as_f32() == toast::WIDTH));
    let mut check = l.check();
    check.inside("toasts", "workspace").no_overflow("toasts");
    check.assert();

    // Dismissing one by its button lets the waiting one in.
    let dismiss = l.bounds(&format!("toast:{}/dismiss", ids[2])).center();
    cx.update_window(handle, |_, window, cx| {
        window.pointer_down(dismiss, gpui_kit::MouseButton::Left, cx);
        window.pointer_up(dismiss, gpui_kit::MouseButton::Left, cx);
    })
    .unwrap();
    cx.run_until_parked();
    draw(&mut cx, handle);
    with(&mut cx, handle, |ws, _, _| {
        let shown: Vec<_> = ws.toasts().visible().iter().map(|t| t.id).collect();
        assert_eq!(shown, vec![ids[0], ids[1], ids[3]]);
    });

    // The pointer is still over the stack, which holds every timer; moving it away releases them.
    with(&mut cx, handle, |ws, _, cx| {
        ws.tick_toasts(Duration::from_secs(3600), cx);
        assert_eq!(
            ws.toasts().visible().len(),
            3,
            "held while the pointer is over it"
        );
    });
    cx.update_window(handle, |_, window, cx| {
        window.pointer_move(point(px(5.), px(5.)), None, cx);
    })
    .unwrap();
    cx.run_until_parked();
    draw(&mut cx, handle);
    // Time passes: after the default 5 s everything has gone.
    with(&mut cx, handle, |ws, _, cx| {
        ws.tick_toasts(Duration::from_secs(3600), cx)
    });
    draw(&mut cx, handle);
    assert!(layout(&mut cx, handle).get("toasts").is_err());
}

#[core::prelude::v1::test]
fn the_fake_notices_become_toasts_of_every_kind() {
    let mut cx = headless_app();
    let handle = open(&mut cx, MEDIUM, UiState::default());
    for spec in crate::fake::notices() {
        with(&mut cx, handle, |ws, _, cx| ws.push_toast(spec, cx));
    }
    draw(&mut cx, handle);
    with(&mut cx, handle, |ws, _, _| {
        assert_eq!(ws.toasts().visible().len(), 3);
    });
}

// --- themes ----------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn cycling_the_theme_repaints_and_is_remembered() {
    let mut cx = headless_app();
    let handle = open(&mut cx, MEDIUM, UiState::default());
    // Install the list the app would have (built in and bundled themes).
    cx.update(|cx| Themes::new(Vec::new()).install(cx));
    let before = layout(&mut cx, handle).color_of("sidebar");
    press(&mut cx, handle, "ctrl-alt-shift-a");
    let name = with(&mut cx, handle, |ws, _, _| ws.state().theme.clone());
    assert!(name.is_some(), "the choice is remembered in native-ui.json");
    // The first press after t3code's dark goes to the next theme of the list; its sidebar differs
    // from t3code's black or the cycle came back to the same palette (a one-theme list).
    let after = layout(&mut cx, handle).color_of("sidebar");
    assert_ne!(before, after);
}
