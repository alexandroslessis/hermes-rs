//! The Skills screen (Hermes's skills manager): every skill by category, a skill's SKILL.md,
//! writes waiting for approval, and the git history of the skills folder with rollback.

use std::sync::Arc;

use gpui::{App, Context, Entity, FocusHandle, Focusable, Window, prelude::*};
use hermes_tools::{PendingSkillWrite, SkillCommit, SkillEntry, SkillStore};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use ui::{Callout, Checkbox, Divider, ToggleState, Tooltip, prelude::*};

use crate::backend::backend;
use crate::state::AppState;

pub struct SkillsView {
    skills: Vec<SkillEntry>,
    pending: Vec<PendingSkillWrite>,
    history: Vec<SkillCommit>,
    selected: Option<String>,
    /// The selected skill's SKILL.md.
    document: Option<Entity<Markdown>>,
    show_history: bool,
    notice: Option<(Severity, String)>,
    focus_handle: FocusHandle,
}

impl SkillsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            skills: Vec::new(),
            pending: Vec::new(),
            history: Vec::new(),
            selected: None,
            document: None,
            show_history: false,
            notice: None,
            focus_handle: cx.focus_handle(),
        };
        view.refresh(cx);
        view
    }

    fn store(cx: &App) -> Arc<SkillStore> {
        backend(cx).runtime.skills()
    }

    /// Re-reads the folder (the agent may have changed skills since the screen was shown).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = Self::store(cx);
        store.commit_outside_changes();
        self.skills = store.catalog();
        self.pending = store.pending();
        self.history = store.history(30);
        if let Some(name) = self.selected.clone() {
            self.select(name, cx);
        }
        cx.notify();
    }

    fn select(&mut self, name: String, cx: &mut Context<Self>) {
        let text = self
            .skills
            .iter()
            .find(|s| s.name == name)
            .and_then(|s| std::fs::read_to_string(s.skill_md()).ok());
        let Some(text) = text else {
            self.selected = None;
            self.document = None;
            cx.notify();
            return;
        };
        // Show the body; the frontmatter is metadata.
        let body = hermes_tools::skills::parse_frontmatter(&text).1;
        let languages = AppState::global(cx).read(cx).languages.clone();
        self.document =
            Some(cx.new(|cx| {
                Markdown::new(body.trim().to_string().into(), Some(languages), None, cx)
            }));
        self.selected = Some(name);
        cx.notify();
    }

    fn report(&mut self, result: serde_json::Value, done: &str, cx: &mut Context<Self>) {
        self.notice = Some(if result["success"] == serde_json::json!(true) {
            (Severity::Success, done.to_string())
        } else {
            (
                Severity::Error,
                result["error"]
                    .as_str()
                    .unwrap_or("The change failed.")
                    .to_string(),
            )
        });
        self.refresh(cx);
    }

    fn delete(&mut self, name: String, cx: &mut Context<Self>) {
        let result = Self::store(cx).manage_tool(&serde_json::json!({
            "operations": [{"action": "delete", "name": name}]
        }));
        self.selected = None;
        self.document = None;
        self.report(result, "Deleted. The skills history can bring it back.", cx);
    }

    fn set_approval(&mut self, on: bool, cx: &mut Context<Self>) {
        let store = Self::store(cx);
        let mut config = store.config();
        config.write_approval = on;
        store.set_config(config);
        AppState::global(cx).update(cx, |state, cx| {
            state.config.skill_write_approval = on;
            state.save(cx);
        });
        cx.notify();
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut category: Option<Option<String>> = None;
        for skill in &self.skills {
            if category.as_ref() != Some(&skill.category) {
                category = Some(skill.category.clone());
                rows.push(
                    div()
                        .px_1p5()
                        .pt_2()
                        .child(
                            Label::new(skill.category.clone().unwrap_or_else(|| "general".into()))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .into_any_element(),
                );
            }
            let selected = self.selected.as_deref() == Some(skill.name.as_str());
            let name = skill.name.clone();
            rows.push(
                v_flex()
                    .id(SharedString::from(format!("skill-{}", skill.name)))
                    .w_full()
                    .px_1p5()
                    .py_1()
                    .cursor_pointer()
                    .when(selected, |this| this.bg(colors.ghost_element_selected))
                    .hover(|style| style.bg(colors.ghost_element_hover))
                    .child(Label::new(skill.name.clone()))
                    .child(
                        Label::new(hermes_tools::skills::clip_description(&skill.description))
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.select(name.clone(), cx)))
                    .into_any_element(),
            );
        }
        v_flex()
            .id("skills-list")
            .w(px(300.))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(colors.border)
            .overflow_y_scroll()
            .children(rows)
    }

    fn render_detail(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (Some(name), Some(document)) = (self.selected.clone(), self.document.clone()) else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(Label::new("Pick a skill to read it.").color(Color::Muted))
                .into_any_element();
        };
        let skill = self.skills.iter().find(|s| s.name == name);
        let (views, uses) = Self::store(cx).usage(&name);
        let dir = skill.map(|s| s.dir.clone());
        let linked: Vec<String> = dir
            .as_deref()
            .map(|d| {
                SkillStore::linked_files(d)
                    .into_values()
                    .flatten()
                    .collect()
            })
            .unwrap_or_default();
        let style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        v_flex()
            .id("skill-detail")
            .size_full()
            .p_4()
            .gap_2()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .justify_between()
                    .child(Headline::new(name.clone()).size(HeadlineSize::Small))
                    .child(
                        h_flex()
                            .gap_1()
                            .when_some(dir, |this, dir| {
                                this.child(
                                    IconButton::new("open-skill-folder", IconName::Folder)
                                        .icon_color(Color::Muted)
                                        .tooltip(Tooltip::text("Open the skill's folder"))
                                        .on_click(move |_, _, _| {
                                            let _ = std::process::Command::new("explorer")
                                                .arg(&dir)
                                                .spawn();
                                        }),
                                )
                            })
                            .child(
                                IconButton::new("delete-skill", IconName::Trash)
                                    .icon_color(Color::Muted)
                                    .tooltip(Tooltip::text("Delete (the history can restore it)"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete(name.clone(), cx)
                                    })),
                            ),
                    ),
            )
            .child(
                Label::new(format!(
                    "Loaded {uses} times · {views} views{}",
                    if linked.is_empty() {
                        String::new()
                    } else {
                        format!(" · files: {}", linked.join(", "))
                    }
                ))
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .child(Divider::horizontal())
            .child(
                div()
                    .id("skill-markdown")
                    .child(MarkdownElement::new(document, style)),
            )
            .into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let approval = AppState::global(cx).read(cx).config.skill_write_approval;
        let root = Self::store(cx).root().to_path_buf();
        v_flex()
            .p_4()
            .gap_2()
            .border_b_1()
            .border_color(colors.border)
            .child(
                h_flex()
                    .justify_between()
                    .child(Headline::new("Skills").size(HeadlineSize::Large))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("skills-history", if self.show_history { "Hide history" } else { "History" })
                                    .start_icon(Icon::new(IconName::RotateCcw).size(IconSize::Small))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_history = !this.show_history;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("open-skills-folder", "Open folder")
                                    .start_icon(Icon::new(IconName::Folder).size(IconSize::Small))
                                    .tooltip(Tooltip::text(root.display().to_string()))
                                    .on_click(move |_, _, _| {
                                        let _ = std::process::Command::new("explorer").arg(&root).spawn();
                                    }),
                            ),
                    ),
            )
            .child(
                Label::new(format!(
                    "{} skills. The agent loads a skill when a task matches its description, and writes new ones as it learns. Type /skill-name in the message box to use one directly.",
                    self.skills.len()
                ))
                .color(Color::Muted),
            )
            .child(
                Checkbox::new(
                    "skill-write-approval",
                    if approval { ToggleState::Selected } else { ToggleState::Unselected },
                )
                .label("Ask before the agent changes a skill")
                .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                    this.set_approval(*state == ToggleState::Selected, cx)
                })),
            )
            .when_some(self.notice.clone(), |this, (severity, text)| {
                this.child(Callout::new().severity(severity).title(text))
            })
            .children(self.pending.iter().map(|write| {
                let (approve, discard) = (write.id.clone(), write.id.clone());
                v_flex()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(colors.border_variant)
                    .bg(colors.editor_background)
                    .child(Label::new("Waiting for approval").size(LabelSize::Small).color(Color::Accent))
                    .child(div().text_sm().child(write.summary.clone()))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(SharedString::from(format!("approve-{}", write.id)), "Approve")
                                    .style(ButtonStyle::Filled)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let result = Self::store(cx).approve(&approve);
                                        this.report(result, "Approved and saved.", cx);
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("discard-{}", write.id)), "Discard")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        Self::store(cx).discard(&discard);
                                        this.notice = Some((Severity::Info, "Discarded.".into()));
                                        this.refresh(cx);
                                    })),
                            ),
                    )
            }))
            .when(self.show_history, |this| {
                this.child(
                    v_flex()
                        .id("skills-history-list")
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .gap_0p5()
                        .children(self.history.iter().enumerate().map(|(i, commit)| {
                            let id = commit.id.clone();
                            h_flex()
                                .gap_2()
                                .child(Label::new(commit.when.clone()).size(LabelSize::Small).color(Color::Muted))
                                .child(div().flex_1().min_w_0().text_sm().child(commit.message.clone()))
                                .when(i > 0, |this| {
                                    this.child(
                                        Button::new(SharedString::from(format!("restore-{}", commit.id)), "Restore")
                                            .label_size(LabelSize::Small)
                                            .tooltip(Tooltip::text("Put every skill back the way it was after this change"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                let result = Self::store(cx).restore(&id);
                                                this.notice = Some(match result {
                                                    Ok(()) => (Severity::Success, format!("Restored the skills to {id}.")),
                                                    Err(e) => (Severity::Error, e),
                                                });
                                                this.refresh(cx);
                                            })),
                                    )
                                })
                        })),
                )
            })
    }
}

impl Focusable for SkillsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SkillsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        v_flex()
            .size_full()
            .bg(colors.panel_background)
            .child(self.render_header(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_list(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_detail(window, cx)),
                    ),
            )
    }
}
