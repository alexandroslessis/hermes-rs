// Release builds are GUI-only (no console window); debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appearance;
mod assets;

use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, size};
use ui::{ListItem, prelude::*};

use crate::{appearance::Appearance, assets::Assets};

/// M0 placeholder window: a session sidebar and an empty chat area built from Zed's `ui`
/// components, to prove the vendored GPUI/theme/ui stack works on its own.
struct Workspace {
    sessions: Vec<SharedString>,
    selected: usize,
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();

        let sidebar = v_flex()
            .w(px(260.))
            .h_full()
            .p_2()
            .gap_1()
            .bg(colors.panel_background)
            .border_r_1()
            .border_color(colors.border)
            .child(
                Button::new("new-session", "New session")
                    .style(ButtonStyle::Filled)
                    .start_icon(Icon::new(IconName::Plus).size(IconSize::Small))
                    .full_width()
                    .on_click(cx.listener(|this, _, _, cx| {
                        let title = format!("Session {}", this.sessions.len() + 1);
                        this.sessions.push(title.into());
                        this.selected = this.sessions.len() - 1;
                        cx.notify();
                    })),
            )
            .children(self.sessions.iter().enumerate().map(|(index, title)| {
                ListItem::new(("session", index))
                    .toggle_state(index == self.selected)
                    .start_slot(Icon::new(IconName::Chat).color(Color::Muted))
                    .child(Label::new(title.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = index;
                        cx.notify();
                    }))
            }));

        let chat = v_flex()
            .flex_1()
            .h_full()
            .items_center()
            .justify_center()
            .gap_2()
            .bg(colors.editor_background)
            .child(
                Icon::new(IconName::Sparkle)
                    .size(IconSize::XLarge)
                    .color(Color::Accent),
            )
            .child(Headline::new("hermes-rs").size(HeadlineSize::Large))
            .child(
                Label::new(format!(
                    "{} is selected. The agent arrives in M1.",
                    self.sessions[self.selected]
                ))
                .color(Color::Muted),
            );

        h_flex()
            .size_full()
            .font(theme::theme_settings(cx).ui_font(cx).clone())
            .text_color(colors.text)
            .child(sidebar)
            .child(chat)
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(Assets)
        .run(|cx: &mut App| {
            Assets.load_fonts(cx).expect("bundled fonts are valid");
            theme::init(theme::LoadThemes::All(Box::new(Assets)), cx);
            theme::set_theme_settings_provider(Box::new(Appearance::default()), cx);

            let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("hermes-rs".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| Workspace {
                        sessions: vec!["Welcome".into()],
                        selected: 0,
                    })
                },
            )
            .expect("failed to open the main window");
            cx.activate(true);
        });
}
