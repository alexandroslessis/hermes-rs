//! The Skills screen (Hermes's skills manager): every skill by category, a skill's SKILL.md,
//! writes waiting for approval, and the git history of the skills folder with rollback.

use std::sync::Arc;

use gpui::{App, Context, Entity, FocusHandle, Focusable, Window, prelude::*};
use hermes_tools::curator::{ArchivedSkill, CuratorState, SkillState};
use hermes_tools::{Bundle, PendingSkillWrite, SkillCommit, SkillEntry, SkillStore};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use ui::{Callout, Checkbox, Divider, ToggleState, Tooltip, WithScrollbar, prelude::*};

use crate::backend::backend;
use crate::bundle_dialog::{BundleDialog, BundleDialogEvent};
use crate::state::AppState;

pub struct SkillsView {
    skills: Vec<SkillEntry>,
    bundles: Vec<Bundle>,
    /// The bundle shown instead of a skill (by slug).
    selected_bundle: Option<String>,
    bundle_dialog: Option<(Entity<BundleDialog>, Vec<gpui::Subscription>)>,
    /// Skills the curator archived, and whether their list is open.
    archived: Vec<ArchivedSkill>,
    show_archive: bool,
    curator: CuratorState,
    curator_running: bool,
    pending: Vec<PendingSkillWrite>,
    history: Vec<SkillCommit>,
    selected: Option<String>,
    /// The selected skill's SKILL.md.
    document: Option<Entity<Markdown>>,
    show_history: bool,
    notice: Option<(Severity, String)>,
    list_scroll: gpui::ScrollHandle,
    detail_scroll: gpui::ScrollHandle,
    focus_handle: FocusHandle,
}

impl SkillsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            skills: Vec::new(),
            bundles: Vec::new(),
            selected_bundle: None,
            bundle_dialog: None,
            archived: Vec::new(),
            show_archive: false,
            curator: CuratorState::default(),
            curator_running: false,
            pending: Vec::new(),
            history: Vec::new(),
            selected: None,
            document: None,
            show_history: false,
            notice: None,
            list_scroll: gpui::ScrollHandle::new(),
            detail_scroll: gpui::ScrollHandle::new(),
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
        self.bundles = store.bundles();
        self.archived = store.archived();
        self.curator = store.curator_state();
        if self
            .selected_bundle
            .as_ref()
            .is_some_and(|slug| !self.bundles.iter().any(|b| &b.slug == slug))
        {
            self.selected_bundle = None;
        }
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
        self.selected_bundle = None;
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

    fn select_bundle(&mut self, slug: String, cx: &mut Context<Self>) {
        self.selected_bundle = Some(slug);
        self.selected = None;
        self.document = None;
        cx.notify();
    }

    /// Opens the bundle dialog: for `editing` (a slug), or a new bundle.
    fn open_bundle_dialog(
        &mut self,
        editing: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bundle = editing.and_then(|slug| self.bundles.iter().find(|b| b.slug == slug).cloned());
        let dialog = cx.new(|cx| BundleDialog::new(bundle.as_ref(), window, cx));
        let subscriptions = vec![
            cx.subscribe(&dialog, |this: &mut Self, _, event, cx| match event {
                BundleDialogEvent::Saved(slug) => {
                    this.notice = Some((Severity::Success, format!("Saved /{slug}.")));
                    this.refresh(cx);
                    this.select_bundle(slug.clone(), cx);
                }
            }),
            cx.subscribe(&dialog, |this: &mut Self, _, _: &gpui::DismissEvent, cx| {
                this.bundle_dialog = None;
                cx.notify();
            }),
        ];
        self.bundle_dialog = Some((dialog, subscriptions));
        cx.notify();
    }

    fn delete_bundle(&mut self, slug: String, cx: &mut Context<Self>) {
        self.notice = Some(match Self::store(cx).delete_bundle(&slug) {
            Ok(()) => (Severity::Success, format!("Deleted /{slug}.")),
            Err(error) => (Severity::Error, error),
        });
        self.selected_bundle = None;
        self.refresh(cx);
    }

    /// The bundles at the top of the list.
    fn bundle_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if self.bundles.is_empty() {
            return Vec::new();
        }
        let colors = cx.theme().colors();
        let mut rows = vec![
            div()
                .px_1p5()
                .pt_2()
                .child(
                    Label::new("bundles")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element(),
        ];
        for bundle in &self.bundles {
            let selected = self.selected_bundle.as_deref() == Some(bundle.slug.as_str());
            let slug = bundle.slug.clone();
            rows.push(
                v_flex()
                    .id(SharedString::from(format!("bundle-{}", bundle.slug)))
                    .w_full()
                    .px_1p5()
                    .py_1()
                    .cursor_pointer()
                    .when(selected, |this| this.bg(colors.ghost_element_selected))
                    .hover(|style| style.bg(colors.ghost_element_hover))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Icon::new(IconName::Sparkle)
                                    .size(IconSize::XSmall)
                                    .color(Color::Accent),
                            )
                            .child(Label::new(format!("/{}", bundle.slug))),
                    )
                    .child(
                        Label::new(format!(
                            "{} skills · {}",
                            bundle.skills.len(),
                            bundle.description
                        ))
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_bundle(slug.clone(), cx)),
                    )
                    .into_any_element(),
            );
        }
        rows
    }

    fn render_bundle_detail(&self, bundle: &Bundle, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let (edit, delete) = (bundle.slug.clone(), bundle.slug.clone());
        v_flex()
            .size_full()
            .p_4()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .child(Headline::new(format!("/{}", bundle.slug)).size(HeadlineSize::Small))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("edit-bundle", "Edit")
                                    .style(ButtonStyle::Outlined)
                                    .label_size(LabelSize::Small)
                                    .start_icon(Icon::new(IconName::Pencil).size(IconSize::XSmall))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_bundle_dialog(Some(edit.clone()), window, cx)
                                    })),
                            )
                            .child(
                                IconButton::new("delete-bundle", IconName::Trash)
                                    .icon_color(Color::Muted)
                                    .tooltip(Tooltip::text("Delete the bundle (its skills stay)"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete_bundle(delete.clone(), cx)
                                    })),
                            ),
                    ),
            )
            .child(Label::new(bundle.description.clone()).color(Color::Muted))
            .when(!bundle.instruction.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_0p5()
                        .child(
                            Label::new("Instruction")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .child(Label::new(bundle.instruction.clone())),
                )
            })
            .child(Divider::horizontal())
            .child(
                Label::new(format!(
                    "Typing /{} loads these skills together:",
                    bundle.slug
                ))
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .children(bundle.skills.iter().map(|name| {
                let exists = self.skills.iter().any(|s| &s.name == name);
                let target = name.clone();
                h_flex()
                    .id(SharedString::from(format!("bundle-member-{name}")))
                    .gap_2()
                    .px_1p5()
                    .py_0p5()
                    .rounded_sm()
                    .when(exists, |this| {
                        this.cursor_pointer()
                            .hover(|style| style.bg(colors.ghost_element_hover))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.select(target.clone(), cx)),
                            )
                    })
                    .child(Label::new(name.clone()).color(if exists {
                        Color::Default
                    } else {
                        Color::Muted
                    }))
                    .when(!exists, |this| {
                        this.child(
                            Label::new("missing (skipped)")
                                .size(LabelSize::Small)
                                .color(Color::Warning),
                        )
                    })
            }))
            .into_any_element()
    }

    /// Hermes's `curator run`: now, whatever the schedule says.
    fn run_curator(&mut self, cx: &mut Context<Self>) {
        if self.curator_running {
            return;
        }
        self.curator_running = true;
        self.notice = Some((Severity::Info, "The curator is running…".into()));
        cx.notify();
        let model = AppState::global(cx).read(cx).curator_model();
        crate::state::run_curator(model, cx, |this: &mut Self, outcome, cx| {
            this.curator_running = false;
            this.notice = Some(match outcome {
                Ok(outcome) => (Severity::Success, format!("Curator: {}", outcome.summary)),
                Err(error) => (Severity::Error, format!("The curator failed: {error}")),
            });
            this.refresh(cx);
        });
    }

    fn set_curator_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        let store = Self::store(cx);
        let mut state = store.curator_state();
        state.paused = paused;
        store.save_curator_state(&state);
        self.refresh(cx);
    }

    fn restore_archived(&mut self, name: String, cx: &mut Context<Self>) {
        self.notice = Some(match Self::store(cx).restore_archived(&name) {
            Ok(()) => (Severity::Success, format!("Restored '{name}'.")),
            Err(error) => (Severity::Error, error),
        });
        self.refresh(cx);
    }

    /// The curator's line in the header: its last run, and Run Now / Pause / Report / Archive.
    fn render_curator(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let state = &self.curator;
        let last_run = state
            .last_run_at
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%b %d, %H:%M")
                    .to_string()
            });
        let status = match (&last_run, &state.last_run_summary) {
            _ if state.paused => "Paused.".to_string(),
            (Some(when), Some(summary)) if state.run_count > 0 => {
                format!("Last run {when}: {summary}")
            }
            (_, Some(summary)) => summary.clone(),
            _ => "Hasn't run yet.".to_string(),
        };
        let paused = state.paused;
        let report = state.last_report_path.clone();
        v_flex()
            .gap_1()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(colors.border_variant)
            .bg(colors.editor_background)
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .min_w_0()
                            .child(Icon::new(IconName::Archive).size(IconSize::Small).color(Color::Muted))
                            .child(Label::new("Curator"))
                            .child(
                                Label::new(status)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .child(
                                Button::new("curator-run", if self.curator_running { "Running…" } else { "Run Now" })
                                    .style(ButtonStyle::Outlined)
                                    .label_size(LabelSize::Small)
                                    .disabled(self.curator_running)
                                    .tooltip(Tooltip::text(
                                        "Archive the agent's skills that went unused, and merge overlapping ones if that's on in Settings",
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| this.run_curator(cx))),
                            )
                            .child(
                                Button::new("curator-pause", if paused { "Resume" } else { "Pause" })
                                    .label_size(LabelSize::Small)
                                    .tooltip(Tooltip::text("Scheduled runs (at most weekly, when the app is idle)"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_curator_paused(!paused, cx)
                                    })),
                            )
                            .when_some(report, |this, report| {
                                this.child(
                                    Button::new("curator-report", "Report")
                                        .label_size(LabelSize::Small)
                                        .tooltip(Tooltip::text(report.clone()))
                                        .on_click(move |_, _, _| {
                                            let _ = std::process::Command::new("explorer").arg(&report).spawn();
                                        }),
                                )
                            })
                            .child(
                                Button::new(
                                    "curator-archive",
                                    format!("Archive ({})", self.archived.len()),
                                )
                                .label_size(LabelSize::Small)
                                .disabled(self.archived.is_empty())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_archive = !this.show_archive;
                                    cx.notify();
                                })),
                            ),
                    ),
            )
            .when(self.show_archive && !self.archived.is_empty(), |this| {
                this.children(self.archived.iter().map(|skill| {
                    let name = skill.name.clone();
                    let when = skill
                        .archived_at
                        .as_deref()
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                        .map(|t| t.with_timezone(&chrono::Local).format("%b %d").to_string())
                        .unwrap_or_default();
                    h_flex()
                        .gap_2()
                        .pl_6()
                        .child(Label::new(skill.name.clone()).size(LabelSize::Small))
                        .child(Label::new(when).size(LabelSize::Small).color(Color::Muted))
                        .child(div().flex_1())
                        .child(
                            Button::new(SharedString::from(format!("restore-archived-{}", skill.name)), "Restore")
                                .label_size(LabelSize::Small)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.restore_archived(name.clone(), cx)
                                })),
                        )
                }))
            })
    }

    /// A skill's curator controls: who owns it, its state, and the pin (Hermes's `curator pin`
    /// and `curator adopt`). Nothing for bundled or external skills, which the curator never
    /// touches.
    fn render_lifecycle(&self, name: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let store = Self::store(cx);
        let skill = self.skills.iter().find(|s| s.name == name)?;
        if skill.external || store.is_bundled(&skill.dir) {
            return None;
        }
        let record = store.record(name);
        let managed = record.curator_managed();
        let pinned = record.pinned;
        let (adopt, pin) = (name.to_string(), name.to_string());
        Some(
            h_flex()
                .gap_3()
                .child(
                    Checkbox::new("curator-managed", managed.into())
                        .label("Let the curator manage it")
                        .on_click(cx.listener(move |this, state: &ToggleState, _, cx| {
                            let store = Self::store(cx);
                            if *state == ToggleState::Selected {
                                if let Err(error) = store.adopt(&adopt) {
                                    this.notice = Some((Severity::Error, error));
                                }
                            } else {
                                store.release(&adopt);
                            }
                            this.refresh(cx);
                        })),
                )
                .child(
                    Checkbox::new("skill-pinned", pinned.into())
                        .label("Pinned")
                        .on_click(cx.listener(move |this, state: &ToggleState, _, cx| {
                            Self::store(cx).set_pinned(&pin, *state == ToggleState::Selected);
                            this.refresh(cx);
                        })),
                )
                .when(record.state == SkillState::Stale, |this| {
                    this.child(
                        Label::new("Stale: unused for two weeks; archived after a month")
                            .size(LabelSize::Small)
                            .color(Color::Warning),
                    )
                })
                .into_any_element(),
        )
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

    fn render_list(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = self.bundle_rows(cx);
        let colors = cx.theme().colors();
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
            .track_scroll(&self.list_scroll)
            .vertical_scrollbar_for(&self.list_scroll, window, cx)
            .children(rows)
    }

    fn render_detail(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(bundle) = self
            .selected_bundle
            .as_ref()
            .and_then(|slug| self.bundles.iter().find(|b| &b.slug == slug))
        {
            return self.render_bundle_detail(bundle, cx);
        }
        let (Some(name), Some(document)) = (self.selected.clone(), self.document.clone()) else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(
                    Label::new("Pick a skill to read it, or a bundle to see what it loads.")
                        .color(Color::Muted),
                )
                .into_any_element();
        };
        let lifecycle = self.render_lifecycle(&name, cx);
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
            .track_scroll(&self.detail_scroll)
            .vertical_scrollbar_for(&self.detail_scroll, window, cx)
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
            .children(lifecycle)
            .child(Divider::horizontal())
            .child(
                div()
                    .id("skill-markdown")
                    .child(MarkdownElement::new(document, style)),
            )
            .into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let curator = self.render_curator(cx).into_any_element();
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
                                Button::new("new-bundle", "New Bundle")
                                    .start_icon(Icon::new(IconName::Plus).size(IconSize::Small))
                                    .tooltip(Tooltip::text("Load several skills with one slash command"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_bundle_dialog(None, window, cx)
                                    })),
                            )
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
            .child(curator)
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
        let dialog = self
            .bundle_dialog
            .as_ref()
            .map(|(dialog, _)| dialog.clone());
        v_flex()
            .relative()
            .size_full()
            .bg(colors.panel_background)
            .child(self.render_header(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_list(window, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_detail(window, cx)),
                    ),
            )
            // The bundle dialog over a dimmed screen; pressing outside it cancels.
            .when_some(dialog, |this, dialog| {
                this.child(
                    div()
                        .id("bundle-dialog-backdrop")
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::black().opacity(0.4))
                        .occlude()
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.bundle_dialog = None;
                                cx.notify();
                            }),
                        )
                        .child(dialog),
                )
            })
    }
}
