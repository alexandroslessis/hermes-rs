// Release builds are GUI-only (no console window); debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appearance;
mod assets;
mod backend;
mod bundle_dialog;
mod chat;
mod chat_ui;
mod config;
mod hermes_import;
mod memory_view;
mod model_picker;
mod notification;
mod onboarding;
mod palette;
mod project_dialog;
mod review_pane;
mod scroll;
mod settings;
mod sidebar;
mod skills_view;
mod state;
mod text_area;
mod title_bar;
mod update;
mod workspace;

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, point,
    px, size,
};

use crate::appearance::Appearance;
use crate::assets::Assets;
use crate::backend::{Backend, BackendGlobal};
use crate::state::AppState;
use crate::workspace::{
    CloseTab, GoToSession, NewSession, NextTab, OpenMemory, OpenSettings, OpenSkills, PreviousTab,
    ToggleCommandPalette, ToggleReview, ToggleSidebar, Workspace,
};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let (backend, requests) = match Backend::start() {
        Ok(started) => started,
        Err(error) => {
            eprintln!("hermes-rs could not start: {error:#}");
            return;
        }
    };
    let backend = Arc::new(backend);

    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            Assets.load_fonts(cx).expect("bundled fonts are valid");
            theme::init(theme::LoadThemes::All(Box::new(Assets)), cx);
            theme::set_theme_settings_provider(Box::new(Appearance::default()), cx);
            cx.set_global(BackendGlobal(backend.clone()));
            AppState::init(backend.data_dir.clone(), cx);
            text_area::bind_keys(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-n", NewSession, None),
                KeyBinding::new("ctrl-t", NewSession, None),
                KeyBinding::new("ctrl-w", CloseTab, None),
                KeyBinding::new("ctrl-tab", NextTab, None),
                KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
                KeyBinding::new("ctrl-b", ToggleSidebar, None),
                KeyBinding::new("ctrl-,", OpenSettings, None),
                KeyBinding::new("ctrl-shift-m", OpenMemory, None),
                KeyBinding::new("ctrl-shift-k", OpenSkills, None),
                KeyBinding::new("ctrl-shift-p", ToggleCommandPalette, None),
                KeyBinding::new("f1", ToggleCommandPalette, None),
                KeyBinding::new("ctrl-p", GoToSession, None),
                KeyBinding::new("ctrl-shift-d", ToggleReview, None),
                KeyBinding::new("ctrl-f", chat::Find, Some("Chat")),
                // Copy the text selected in a chat message.
                KeyBinding::new("ctrl-c", markdown::Copy, Some("Markdown")),
            ]);

            let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        titlebar: Some(TitlebarOptions {
                            title: Some("Hermes".into()),
                            appears_transparent: true,
                            traffic_light_position: Some(point(px(9.), px(9.))),
                        }),
                        window_min_size: Some(size(px(640.), px(420.))),
                        ..Default::default()
                    },
                    |window, cx| cx.new(|cx| Workspace::new(requests, window, cx)),
                )
                .expect("failed to open the main window");
            window
                .update(cx, |_, window, _| window.activate_window())
                .ok();
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}
