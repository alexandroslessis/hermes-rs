//! The new/edit skill bundle dialog: a name (its slug is the slash command), a description, an
//! optional instruction added to every invocation, and the skills it loads (Hermes's
//! `hermes bundles create`).

use std::collections::BTreeSet;

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    ScrollHandle, Subscription, Window, prelude::*,
};
use hermes_tools::Bundle;
use ui::{Checkbox, TintColor, ToggleState, Tooltip, prelude::*};

use crate::backend::backend;
use crate::text_area::{TextArea, TextAreaEvent};

pub enum BundleDialogEvent {
    /// The bundle was saved; carries its slug.
    Saved(String),
}

pub struct BundleDialog {
    /// The bundle being edited (by slug); `None` creates one.
    editing: Option<String>,
    name: Entity<TextArea>,
    description: Entity<TextArea>,
    instruction: Entity<TextArea>,
    /// Every skill, and the ones in the bundle.
    skills: Vec<String>,
    chosen: BTreeSet<String>,
    error: Option<String>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<BundleDialogEvent> for BundleDialog {}
impl EventEmitter<DismissEvent> for BundleDialog {}

impl BundleDialog {
    pub fn new(bundle: Option<&Bundle>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = |placeholder: &'static str, text: String, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut area = TextArea::new(placeholder, cx).single_line();
                area.set_text(text, cx);
                area
            })
        };
        let name = field(
            "e.g. release-day (typed as /release-day)",
            bundle.map(|b| b.name.clone()).unwrap_or_default(),
            cx,
        );
        let description = field(
            "What the bundle is for",
            bundle.map(|b| b.description.clone()).unwrap_or_default(),
            cx,
        );
        let instruction = field(
            "Optional: guidance added every time the bundle is used",
            bundle.map(|b| b.instruction.clone()).unwrap_or_default(),
            cx,
        );
        name.focus_handle(cx).focus(window, cx);
        let subscriptions = [&name, &description, &instruction]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this: &mut Self, _, event, cx| {
                    if let TextAreaEvent::Submit = event {
                        this.save(cx);
                    }
                })
            })
            .collect();
        let skills: Vec<String> = backend(cx)
            .runtime
            .skills()
            .catalog()
            .into_iter()
            .map(|s| s.name)
            .collect();
        Self {
            editing: bundle.map(|b| b.slug.clone()),
            name,
            description,
            instruction,
            skills,
            chosen: bundle
                .map(|b| b.skills.iter().cloned().collect())
                .unwrap_or_default(),
            error: None,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let text = |input: &Entity<TextArea>| input.read(cx).text().trim().to_string();
        let skills: Vec<String> = self.chosen.iter().cloned().collect();
        let result = backend(cx).runtime.skills().save_bundle(
            &text(&self.name),
            &text(&self.description),
            &skills,
            &text(&self.instruction),
            self.editing.as_deref(),
        );
        match result {
            Ok(bundle) => {
                cx.emit(BundleDialogEvent::Saved(bundle.slug));
                cx.emit(DismissEvent);
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }
}

impl Focusable for BundleDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name.focus_handle(cx)
    }
}

impl Render for BundleDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<AnyElement> = self
            .skills
            .iter()
            .map(|skill| {
                let chosen = self.chosen.contains(skill);
                let name = skill.clone();
                Checkbox::new(
                    SharedString::from(format!("bundle-skill-{skill}")),
                    chosen.into(),
                )
                .label(skill.clone())
                .on_click(cx.listener(move |this, state: &ToggleState, _, cx| {
                    if *state == ToggleState::Selected {
                        this.chosen.insert(name.clone());
                    } else {
                        this.chosen.remove(&name);
                    }
                    cx.notify();
                }))
                .into_any_element()
            })
            .collect();
        let skill_list = {
            let colors = cx.theme().colors();
            let wrapper = div()
                .h(rems(14.))
                .rounded_md()
                .border_1()
                .border_color(colors.border_variant)
                .bg(colors.editor_background);
            let content = v_flex().id("bundle-skills").p_2().gap_1().children(rows);
            crate::scroll::with_scrollbar(wrapper, content, &self.scroll, window, cx)
        };
        let colors = cx.theme().colors();
        let field = |label: &'static str, input: &Entity<TextArea>| {
            v_flex()
                .gap_1()
                .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.editor_background)
                        .child(input.clone()),
                )
        };
        let title = if self.editing.is_some() {
            "Edit Bundle"
        } else {
            "New Bundle"
        };
        v_flex()
            .id("bundle-dialog")
            .w(rems(32.))
            .max_w_full()
            .p_4()
            .gap_3()
            .elevation_3(cx)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(DismissEvent);
                    cx.stop_propagation();
                }
            }))
            .child(
                h_flex()
                    .justify_between()
                    .child(Headline::new(title).size(HeadlineSize::Small))
                    .child(
                        IconButton::new("close-bundle-dialog", IconName::Close)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Close"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
            .child(
                Label::new(
                    "A bundle loads several skills with one slash command. It wins over a skill \
                     with the same name.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .child(field("Name", &self.name))
            .child(field("Description", &self.description))
            .child(field("Instruction", &self.instruction))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(format!("Skills ({} chosen)", self.chosen.len()))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(skill_list),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
            })
            .child(
                h_flex()
                    .pt_1()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel-bundle", "Cancel")
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        Button::new("save-bundle", "Save Bundle")
                            .style(ButtonStyle::Tinted(TintColor::Accent))
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}
