//! The panel above the composer's card when the agent waits for the user (Phase 5, CP5.3): an
//! approval of a flagged command or file edit, or one of the agent's questions. It sits on the
//! card like t3code's composer banners (a drawer 12 px narrower than the card, tucked under its
//! rounded top), takes the keyboard when it appears (unless the user is typing), and closes the
//! moment the core says the request is settled.
//!
//! **Approval**: the command in a mono block (or, for an edit by `write_file` / `patch`, the file's
//! path and the diff of the call that is running, from the thread's timeline, drawn by the tool
//! cards' own diff view), why the runtime flagged it, and the choices the core offers: Allow once
//! (Enter, 1), Allow for this session (2), Always allow (3), Deny (Esc, 4); Manual and Plan modes
//! offer only once and deny. Several waiting requests queue: "1 of 2". The panel's keys are the
//! Tauri page's (Enter allows once, Escape denies) and t3code's (digits pick the n-th choice),
//! with Up/Down to move over the buttons.
//!
//! **Question**: the agent's question, its choices as rows with a radio (a checkbox when several
//! may be picked) and the digit that picks them, and a field for a free answer (the core adds
//! "Other": free text is always allowed; with no choices it is the only way). Enter in the field
//! sends the text; Skip (Escape) answers nothing, which the agent is told. The core says nothing
//! about a secret question, so no answer is masked (see the progress note).
//!
//! While a request waits the composer's input stays usable: a message typed and sent is queued
//! behind the waiting turn as always (t3code turns the composer into "decline and redirect";
//! the core has no such operation).
//!
//! t3code: `components/chat/ComposerBanner.tsx` (the attached drawer, its warning tint:
//! `--chat-composer-attached-outline` at `warning` 28%, tint 8%), `ComposerPendingApprovalActions.tsx`
//! (the buttons), `TimelinePendingApprovalRow.tsx` (the command in `font-mono text-xs leading-5`,
//! the reason below it in muted text, "Waiting for approval"), `ComposerPendingUserInputPanel.tsx`
//! (the question, the rows with a radio or checkbox and a `Kbd` digit at the right). Reference
//! screenshots: `composer-model-picker-permission.png` and `app-thread-dark.png` for the card the
//! drawer sits on (t3code has no capture of a pending approval or question; see
//! `docs/native-ui/reference/t3code/README.md`).
//! Hermes (Tauri page): `apps/desktop/src/components/assistant-ui/tool/approval.tsx` (Run / Deny,
//! the queue's "1 / 2"), `lib/keybinds/approval-keys.ts`.

use gpui_kit::{
    AnyElement, Context, ElementId, Focusable as _, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Sizable as _, Size as KitSize,
        input::{Escape, Textarea},
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use super::ask::{
    Ask, Answer, Approval, Choice, Nav, Question, Reaction, Request, Subject, non_ascii, visible,
};
use super::view::ComposerView;
use crate::design::{
    Button, MiddleTruncate,
    button::{Size, Variant},
    icons::icon,
    tokens::{Palette, radius, text},
};
use crate::dev::ProbeExt as _;
use crate::thread::cards::diff_lines;

/// How far the drawer is inset from the card's sides (`--chat-composer-drawer-inset`).
pub const INSET: f32 = 12.;
/// How far the drawer's bottom is tucked under the card (`1rem + 1px`).
pub const OVERLAP: f32 = 16.;
/// The command block's tallest (`max-h-40`).
pub const COMMAND_MAX: f32 = 160.;
/// The mini diff shows this many lines.
pub const DIFF_LINES: usize = 8;
/// The most the whole drawer takes (`max-h-[min(24rem,40dvh)]`): a question with a long text
/// scrolls inside it.
pub const BODY_MAX: f32 = 384.;
/// What the answer field says when there are choices, and when there are none (t3code's words).
pub const FIELD_HINT: &str = "Type your own answer, or leave this blank to use the selected option";
pub const FIELD_ONLY: &str = "Type your answer";
/// What the composer's input says while the agent waits: a message sent now is queued.
pub const ASKED_HINT: &str = "Answer above, or type a message to queue";
/// How long after a request appears its keys answer nothing (review, CP5.3): a key that was on
/// its way when the panel appeared (the user typing a `2`, pressing Enter) is not an answer.
pub const KEY_GUARD: std::time::Duration = std::time::Duration::from_millis(500);
/// What an edit approval says when the call's diff is not in the timeline (the thread was opened
/// after the call started): the request carries only the path.
pub const NO_DIFF: &str = "The diff of this change is not available here; only the path is known.";

impl ComposerView {
    /// The drawer for the request the thread waits on, if there is one.
    pub(super) fn ask_panel(
        &self,
        palette: &Palette,
        dark: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (request, count, responding, edit) = {
            let model = self.model.read(cx);
            let request = model.current_ask()?.clone();
            let edit = match &request.ask {
                Ask::Approval(a) => match a.subject() {
                    Subject::Edit { tool, path } => model.pending_edit(&tool, &path, cx),
                    Subject::Command(_) => None,
                },
                Ask::Clarify(_) => None,
            };
            (
                request.clone(),
                model.ask_count(),
                model.is_responding(&request.prompt_id),
                edit,
            )
        };
        let body = match &request.ask {
            Ask::Approval(a) => self.approval_body(a, &request, count, responding, edit, palette, cx),
            Ask::Clarify(q) => self.question_body(q, &request, count, responding, palette, cx),
        };
        let warn = matches!(request.ask, Ask::Approval(_));
        let view = cx.entity();
        // The card's surface, tinted for an approval (t3code: warning at 8% over it).
        let surface = palette.surface_raised.over(palette.background);
        let (fill, edge) = if warn {
            (
                palette.warning.with_alpha(20).over(surface).hsla(),
                palette.warning.hsla().opacity(0.28),
            )
        } else {
            (
                surface.hsla(),
                palette
                    .foreground
                    .hsla()
                    .opacity(if dark { 0.05 } else { 0.08 }),
            )
        };
        let focus = self.ask_focus.clone();
        Some(
            div()
                .id("ask-panel")
                .w_full()
                .px(px(INSET))
                // Its bottom edge goes under the card, whose rounded top corners show it.
                .mb(px(-OVERLAP))
                .child(
                    div()
                        .id(if warn { "approval-panel" } else { "question-panel" })
                        .key_context("AskPanel")
                        .track_focus(&focus)
                        .on_key_down(move |event: &KeyDownEvent, window, cx| {
                            let key = &event.keystroke;
                            if key.modifiers.control || key.modifiers.alt || key.modifiers.platform
                            {
                                return;
                            }
                            let name = key.key.clone();
                            // A held key (auto-repeat) never answers: an Enter held on one request
                            // must not allow the next one that appears under it (Tauri's
                            // `approval-keys.ts` rule). Arrows may repeat.
                            if event.is_held && !matches!(name.as_str(), "up" | "down" | "tab") {
                                cx.stop_propagation();
                                return;
                            }
                            let taken = view.update(cx, |this, cx| this.ask_key(&name, window, cx));
                            if taken {
                                cx.stop_propagation();
                            }
                        })
                        .flex()
                        .flex_col()
                        .w_full()
                        .pb(px(OVERLAP))
                        .rounded_t(radius::XL2)
                        .border_1()
                        .border_b_0()
                        .border_color(edge)
                        .bg(fill)
                        .text_color(palette.foreground.hsla())
                        .child(body)
                        .probe(),
                )
                .probe()
                .into_any_element(),
        )
    }

    fn header(
        &self,
        id: &'static str,
        glyph: IconName,
        glyph_color: gpui_kit::Hsla,
        title: &str,
        count: usize,
        palette: &Palette,
    ) -> AnyElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(16.))
            .pt(px(10.))
            .min_w_0()
            .child(
                div()
                    .flex_shrink_0()
                    .child(icon(glyph).with_size(KitSize::Size(px(16.))).text_color(glyph_color)),
            )
            .child(
                div()
                    .id("ask-title")
                    .min_w_0()
                    .text_size(text::SM.size)
                    .line_height(text::SM.line_height)
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::from(title.to_string()))
                    .probe(),
            )
            .child(div().flex_1())
            .when(count > 1, |row| {
                row.child(
                    div()
                        .id("ask-count")
                        .flex_shrink_0()
                        .text_size(text::XS.size)
                        .font_weight(FontWeight::MEDIUM)
                        // The foreground at 80%: muted text on the warning tint read too faint.
                        .text_color(palette.foreground.hsla().opacity(0.8))
                        .child(format!("1 of {count}"))
                        .probe(),
                )
            })
            .probe()
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn approval_body(
        &self,
        approval: &Approval,
        request: &Request,
        count: usize,
        responding: bool,
        edit: Option<crate::thread::tools::ToolCard>,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mono = cx.theme().mono_font_family.clone();
        let subject = approval.subject();
        let title = match &subject {
            Subject::Command(_) => "Command approval",
            Subject::Edit { .. } => "File change approval",
        };
        // What runs, exactly: hidden and control characters escaped, a note when there were any
        // or when a letter is not ASCII (a lookalike).
        let shown_text = match &subject {
            Subject::Command(command) => command.as_str(),
            Subject::Edit { path, .. } => path.as_str(),
        };
        let (_, escaped) = visible(shown_text);
        let warning = match (escaped, non_ascii(shown_text)) {
            (true, _) => Some("Holds hidden or control characters, shown as \\x.. and \\u{..} escapes"),
            (false, true) => Some("Holds characters outside ASCII: check for lookalike letters"),
            _ => None,
        };
        let detail: AnyElement = match &subject {
            Subject::Command(command) => div()
                .id("approval-command")
                .mx(px(16.))
                .mt(px(8.))
                .max_h(px(COMMAND_MAX))
                .overflow_y_scroll()
                .px(px(8.))
                .py(px(6.))
                .rounded(radius::MD)
                .bg(palette.code_background.hsla())
                .text_color(palette.code_foreground.hsla())
                .font_family(mono.clone())
                .text_size(text::XS.size)
                .line_height(px(20.))
                .child(SharedString::from(visible(command).0))
                .probe()
                .into_any_element(),
            Subject::Edit { tool, path } => {
                let diff = edit.as_ref().and_then(|card| match &card.detail {
                    crate::thread::tools::Detail::Edit { diff, from_args, .. } => {
                        Some((diff.clone(), *from_args))
                    }
                    _ => None,
                });
                div()
                    .id("approval-edit")
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .mx(px(16.))
                    .mt(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .min_w_0()
                            .text_size(text::XS.size)
                            .text_color(palette.muted_foreground.hsla())
                            .child(
                                div().flex_shrink_0().child(
                                    icon(IconName::FilePen)
                                        .with_size(KitSize::Size(px(14.)))
                                        .text_color(palette.icon_muted.hsla()),
                                ),
                            )
                            .child(div().flex_shrink_0().child(match tool.as_str() {
                                "patch" => "Edit",
                                _ => "Write",
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_family(mono.clone())
                                    .text_color(palette.foreground.hsla())
                                    .child(MiddleTruncate::new("approval-path", visible(path).0)),
                            ),
                    )
                    .when(diff.is_none(), |col| {
                        col.child(
                            div()
                                .id("approval-no-diff")
                                .text_size(text::XS.size)
                                .text_color(palette.muted_foreground.hsla())
                                .child(NO_DIFF)
                                .probe(),
                        )
                    })
                    .when_some(diff, |col, (diff, from_args)| {
                        col.child(
                            div()
                                .id("approval-diff")
                                .rounded(radius::MD)
                                .border_1()
                                .border_color(palette.border.hsla())
                                .overflow_hidden()
                                .text_size(text::XS.size)
                                .child(diff_lines(
                                    "approval-diff-lines".into(),
                                    &diff,
                                    DIFF_LINES,
                                    from_args,
                                    palette,
                                    &mono,
                                ))
                                .probe(),
                        )
                    })
                    .probe()
                    .into_any_element()
            }
        };
        let reason = div()
            .id("approval-reason")
            .mx(px(16.))
            .mt(px(6.))
            .flex()
            .items_start()
            .gap(px(4.))
            .text_size(text::XS.size)
            .line_height(text::XS.line_height)
            .text_color(palette.muted_foreground.hsla())
            .child(div().flex_shrink_0().child("Flagged:"))
            .child(
                div()
                    .min_w_0()
                    .child(SharedString::from(visible(&approval.reason).0)),
            )
            .probe();
        let warning = warning.map(|words| {
            div()
                .id("approval-warning")
                .mx(px(16.))
                .mt(px(4.))
                .flex()
                .items_start()
                .gap(px(4.))
                .text_size(text::XS.size)
                .line_height(text::XS.line_height)
                .text_color(palette.warning_foreground.hsla())
                .child(
                    div().flex_shrink_0().pt(px(2.)).child(
                        icon(IconName::OctagonAlert)
                            .with_size(KitSize::Size(px(12.)))
                            .text_color(palette.warning_foreground.hsla()),
                    ),
                )
                .child(div().min_w_0().child(words))
                .probe()
        });

        // The buttons: Deny at the left, the rest as the core offers them, the primary last.
        let prompt = request.prompt_id.clone();
        let highlighted = self.nav.highlight.and_then(|i| approval.choices.get(i)).copied();
        let button = |choice: Choice, cx: &mut Context<Self>| -> AnyElement {
            let view = cx.entity();
            let id = prompt.clone();
            let (variant, hint): (Variant, String) = match choice {
                Choice::Once => (Variant::Default, "\u{21b5}".into()),
                Choice::Deny => (Variant::GhostMuted, "Esc".into()),
                Choice::Session => (Variant::Outline, "2".into()),
                Choice::Always => (Variant::Outline, "3".into()),
            };
            let number = approval
                .choices
                .iter()
                .position(|c| *c == choice)
                .map_or(0, |i| i + 1);
            Button::new(ElementId::Name(format!("approval-{}", choice.id()).into()), choice.label())
                .variant(variant)
                .size(Size::Sm)
                .hint(hint)
                .selected(highlighted == Some(choice) && choice != Choice::Once)
                .disabled(responding)
                .tooltip(format!("{}  ({number})", choice.hint()))
                .on_click(move |_, window, cx| {
                    let (view, id) = (view.clone(), id.clone());
                    view.update(cx, |this, cx| this.pick_approval(&id, choice, window, cx))
                })
                .into_any_element()
        };
        // Two pairs that never split: Deny with "for this session", "Always" with the primary.
        // A narrow card puts the second pair under the first, both right-aligned, so the primary
        // stays the bottom-right button (review, CP5.3: one ragged wrap of four read out of order).
        let pair = |id: &'static str, of: [Choice; 2], cx: &mut Context<Self>| -> Option<AnyElement> {
            let buttons: Vec<AnyElement> = of
                .into_iter()
                .filter(|c| approval.choices.contains(c))
                .map(|c| button(c, cx))
                .collect();
            (!buttons.is_empty()).then(|| {
                div()
                    .id(id)
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(6.))
                    .children(buttons)
                    .into_any_element()
            })
        };
        let actions: AnyElement = if self.nav.confirm_always {
            self.confirm_always_row(approval, &request.prompt_id, responding, palette, cx)
        } else {
            let first = pair("approval-pair-deny", [Choice::Deny, Choice::Session], cx);
            let second = pair("approval-pair-allow", [Choice::Always, Choice::Once], cx);
            div()
                .id("approval-actions")
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap(px(6.))
                .px(px(12.))
                .pt(px(10.))
                .pb(px(8.))
                .children(first)
                .children(second)
                .probe()
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .w_full()
            .child(self.header(
                "approval-header",
                IconName::ShieldAlert,
                palette.warning_foreground.hsla(),
                title,
                count,
                palette,
            ))
            .child(detail)
            .children(warning)
            .child(reason)
            .child(actions)
            .into_any_element()
    }

    /// "Always allow" asks before it persists (the Tauri page's confirm dialog, its words): the
    /// runtime adds the flagged kind of command (the reason, not this one command line) to the
    /// permanent allowlist, for every thread from now on.
    fn confirm_always_row(
        &self,
        approval: &Approval,
        prompt_id: &str,
        responding: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (back_view, yes_view) = (cx.entity(), cx.entity());
        let (back_id, yes_id) = (prompt_id.to_string(), prompt_id.to_string());
        div()
            .id("approval-confirm")
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(16.))
            .pt(px(10.))
            .pb(px(8.))
            .child(
                div()
                    .id("approval-confirm-text")
                    .text_size(text::XS.size)
                    .line_height(text::XS.line_height)
                    .text_color(palette.foreground.hsla())
                    .child(SharedString::from(always_words(&approval.reason)))
                    .probe(),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(6.))
                    .child(
                        Button::new("approval-confirm-cancel", "Cancel")
                            .variant(Variant::GhostMuted)
                            .size(Size::Sm)
                            .hint("Esc")
                            .disabled(responding)
                            .on_click(move |_, _, cx| {
                                let _ = &back_id;
                                back_view.update(cx, |this, cx| {
                                    this.nav.confirm_always = false;
                                    cx.notify();
                                })
                            }),
                    )
                    .child(
                        Button::new("approval-confirm-always", "Always allow")
                            .variant(Variant::Destructive)
                            .size(Size::Sm)
                            .hint("\u{21b5}")
                            .disabled(responding)
                            .on_click(move |_, window, cx| {
                                let id = yes_id.clone();
                                yes_view.update(cx, |this, cx| {
                                    this.answer_request(
                                        &id,
                                        Answer::Approval(Choice::Always),
                                        window,
                                        cx,
                                    )
                                })
                            }),
                    ),
            )
            .probe()
            .into_any_element()
    }

    /// Takes an approval's choice as a click on its button would (the stories).
    pub fn choose(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_ask(window, cx);
        if let Some(id) = self.ask_for.clone() {
            self.pick_approval(&id, choice, window, cx);
        }
    }

    /// A click on one of an approval's buttons: "Always allow" asks first.
    fn pick_approval(
        &mut self,
        prompt_id: &str,
        choice: Choice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.nav.pick(choice) {
            Reaction::Answer(answer) => self.answer_request(prompt_id, answer, window, cx),
            _ => cx.notify(),
        }
    }

    fn question_body(
        &self,
        question: &Question,
        request: &Request,
        count: usize,
        responding: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let choices = question.choices.len();
        let rows: Vec<AnyElement> = question
            .choices
            .iter()
            .enumerate()
            .map(|(ix, label)| {
                let view = cx.entity();
                let picked = self.nav.picked.contains(&ix);
                let highlighted = self.nav.highlight == Some(ix);
                let multi = question.multi_select;
                let text_value = label.clone();
                let id = request.prompt_id.clone();
                let mark = if multi {
                    checkbox(picked, palette)
                } else {
                    radio(picked, palette)
                };
                div()
                    .id(ElementId::Name(format!("ask-choice-{ix}").into()))
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .w_full()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(radius::MD)
                    .when(!responding, |d| d.cursor_pointer())
                    .text_color(if picked || highlighted {
                        palette.foreground.hsla()
                    } else {
                        palette.foreground.hsla().opacity(0.7)
                    })
                    .when(highlighted, |d| d.bg(palette.muted.hsla().opacity(0.3)))
                    .when(!responding, |d| {
                        d.hover(|s| s.bg(palette.muted.hsla().opacity(0.3)))
                    })
                    .on_click(move |_, window, cx| {
                        if responding {
                            return;
                        }
                        let (view, id, text_value) = (view.clone(), id.clone(), text_value.clone());
                        view.update(cx, |this, cx| {
                            this.pick_choice(&id, ix, &text_value, window, cx)
                        })
                    })
                    .child(div().flex().h(px(20.)).items_center().flex_shrink_0().child(mark))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_size(text::SM.size)
                            .line_height(text::SM.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .child(SharedString::from(label.clone())),
                    )
                    .when(ix < 9, |d| {
                        d.child(
                            div()
                                .flex()
                                .h(px(20.))
                                .items_center()
                                .flex_shrink_0()
                                .child(crate::design::Kbd::new((ix + 1).to_string())),
                        )
                    })
                    .probe()
                    .into_any_element()
            })
            .collect();

        let view = cx.entity();
        let id = request.prompt_id.clone();
        let multi = question.multi_select;
        let picked_any = !self.nav.picked.is_empty();
        let sendable_now = !responding
            && (picked_any || !self.answer_input.read(cx).value().trim().is_empty());
        let (skip_view, send_view, skip_id, send_id) =
            (view.clone(), view, id.clone(), id.clone());
        let answer = div()
            .id("ask-field")
            .mx(px(16.))
            .mt(px(4.))
            .px(px(10.))
            .py(px(6.))
            .rounded(radius::MD)
            .border_1()
            .border_color(if self.nav.highlight == Some(choices) {
                palette.ring.hsla()
            } else {
                palette.input.hsla()
            })
            .text_size(text::SM.size)
            .text_color(palette.foreground.hsla())
            // Escape in the field hands the keyboard back to the panel (a second one skips).
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                window.focus(&this.ask_focus, cx);
                cx.stop_propagation();
            }))
            .child(Textarea::new(&self.answer_input).appearance(false).bordered(false))
            .probe();
        let actions = div()
            .id("ask-actions")
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(px(6.))
            .px(px(12.))
            .pt(px(10.))
            .pb(px(8.))
            .child(
                Button::new("ask-skip", "Skip")
                    .variant(Variant::GhostMuted)
                    .size(Size::Sm)
                    .hint("Esc")
                    .disabled(responding)
                    .tooltip("Answer nothing; the agent is told you skipped it")
                    .on_click(move |_, window, cx| {
                        let (view, id) = (skip_view.clone(), skip_id.clone());
                        view.update(cx, |this, cx| {
                            this.answer_request(&id, Answer::Text(String::new()), window, cx)
                        })
                    }),
            )
            .child(
                Button::new("ask-send", "Send")
                    .variant(Variant::Default)
                    .size(Size::Sm)
                    .hint("\u{21b5}")
                    .disabled(!sendable_now)
                    .tooltip(if multi {
                        "Send the picked choices, or your own answer"
                    } else {
                        "Send your own answer"
                    })
                    .on_click(move |_, window, cx| {
                        let (view, id) = (send_view.clone(), send_id.clone());
                        view.update(cx, |this, cx| this.send_field(&id, window, cx))
                    }),
            )
            .probe();

        div()
            .flex()
            .flex_col()
            .w_full()
            .child(self.header(
                "question-header",
                IconName::MessageCircleQuestionMark,
                palette.muted_foreground.hsla(),
                "Question",
                count,
                palette,
            ))
            .child(
                div()
                    .id("ask-body")
                    .flex()
                    .flex_col()
                    .max_h(px(BODY_MAX))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .id("ask-question")
                            .mx(px(16.))
                            .mt(px(6.))
                            .mb(px(4.))
                            .text_size(text::SM.size)
                            .line_height(text::SM.line_height)
                            .text_color(palette.foreground.hsla().opacity(0.85))
                            .child(SharedString::from(question.text.clone()))
                            .probe(),
                    )
                    .when(choices > 0, |col| {
                        col.child(
                            div()
                                .id("ask-choices")
                                .flex()
                                .flex_col()
                                .mx(px(8.))
                                .children(rows)
                                .probe(),
                        )
                    })
                    .child(answer)
                    .probe(),
            )
            .child(actions)
            .into_any_element()
    }

    // --- keys and clicks ----------------------------------------------------------------------

    /// A key pressed with the panel focused. True when it was the panel's.
    pub(super) fn ask_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(request) = self.model.read(cx).current_ask().cloned() else {
            return false;
        };
        if self.model.read(cx).is_responding(&request.prompt_id) {
            // A key while the answer is on its way is the panel's too, and does nothing.
            return matches!(key, "enter" | "escape");
        }
        let typed = self.answer_input.read(cx).value().to_string();
        // A key pressed in the first moments of a request was meant for something else (the user
        // was typing when it appeared): it is the panel's, and answers nothing.
        let fresh = self.ask_shown_at.is_some_and(|at| {
            cx.background_executor().now().saturating_duration_since(at) < KEY_GUARD
        });
        if fresh {
            return matches!(key, "enter" | "escape" | "space")
                || super::ask::digit_index(key).is_some();
        }
        let reaction = self.nav.key(&request.ask, key, &typed);
        match reaction {
            Reaction::Ignored => false,
            Reaction::Moved => {
                cx.notify();
                true
            }
            Reaction::FocusField => {
                self.answer_input
                    .update(cx, |input, cx| input.focus(window, cx));
                cx.notify();
                true
            }
            Reaction::Answer(answer) => {
                self.answer_request(&request.prompt_id, answer, window, cx);
                true
            }
        }
    }

    /// Puts `text` in the question's free-text field (the stories and the tests).
    pub fn set_answer_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        // The request must be the one on screen first (a new one clears the field).
        self.sync_ask(window, cx);
        self.answer_input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        cx.notify();
    }

    /// Escape in the composer's own input while a request waits and nothing is typed: the same as
    /// Escape in the panel (deny, or skip the question). True when it was taken.
    pub(super) fn escape_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let idle = self.text(cx).trim().is_empty() && self.menu.is_none();
        idle && self.model.read(cx).current_ask().is_some() && self.ask_key("escape", window, cx)
    }

    /// Answers a request (a click, a key): the model sends it once; the panel closes when the
    /// core has taken it.
    pub(super) fn answer_request(
        &mut self,
        prompt_id: &str,
        answer: Answer,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model
            .update(cx, |m, cx| m.answer(prompt_id, answer, cx));
    }

    /// A click on a choice row: a single-select answers with it, a multi-select toggles it.
    fn pick_choice(
        &mut self,
        prompt_id: &str,
        ix: usize,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let multi = matches!(
            self.model.read(cx).current_ask().map(|r| &r.ask),
            Some(Ask::Clarify(q)) if q.multi_select
        );
        self.nav.highlight = Some(ix);
        if multi {
            if let Some(at) = self.nav.picked.iter().position(|p| *p == ix) {
                self.nav.picked.remove(at);
            } else {
                self.nav.picked.push(ix);
            }
            cx.notify();
        } else {
            self.answer_request(prompt_id, Answer::Text(text.to_string()), window, cx);
        }
    }

    /// The Send button, or Enter in the answer field: the typed text, else the picked choices.
    pub(super) fn send_field(&mut self, prompt_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Ask::Clarify(question)) = self.model.read(cx).current_ask().map(|r| r.ask.clone())
        else {
            return;
        };
        let typed = self.answer_input.read(cx).value().to_string();
        let picked = self.nav.picked_texts(&question);
        let answer = super::ask::clarify_answer(&typed, &picked);
        if answer.is_empty() {
            return;
        }
        self.answer_request(prompt_id, Answer::Text(answer), window, cx);
    }

    /// A new request is on screen (or the last one closed): the keyboard state starts over and,
    /// unless the user is typing a message, the panel takes the keyboard.
    pub(super) fn sync_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.model.read(cx).current_ask().cloned();
        let id = current.as_ref().map(|r| r.prompt_id.clone());
        if id == self.ask_for {
            return;
        }
        let had_one = self.ask_for.is_some();
        self.ask_shown_at = id.as_ref().map(|_| cx.background_executor().now());
        self.ask_for = id;
        self.nav = current.as_ref().map(|r| Nav::start(&r.ask)).unwrap_or_default();
        self.answer_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        // What the input and the field say.
        let (field, input_hint) = match current.as_ref().map(|r| &r.ask) {
            Some(Ask::Clarify(q)) if q.choices.is_empty() => (FIELD_ONLY, ASKED_HINT),
            Some(Ask::Clarify(_)) => (FIELD_HINT, ASKED_HINT),
            Some(Ask::Approval(_)) => (FIELD_HINT, ASKED_HINT),
            None => (FIELD_HINT, super::view::PLACEHOLDER),
        };
        self.answer_input
            .update(cx, |input, cx| input.set_placeholder(field, window, cx));
        self.input
            .update(cx, |input, cx| input.set_placeholder(input_hint, window, cx));
        self.input_hint = input_hint;
        match current {
            Some(request) => {
                // Not while the user types a message: a click or Tab gets there. Nor from
                // anywhere but the composer (the sidebar's search, the palette, a dialog): the
                // keyboard is taken only from the composer's own empty input, from the panel of
                // the request before, or when nothing has it.
                let typing = !self.text(cx).trim().is_empty() || self.menu.is_some();
                let input_has_it = self.input.read(cx).focus_handle(cx).is_focused(window);
                let ours = input_has_it
                    || self.ask_focus.contains_focused(window, cx)
                    || self.answer_input.read(cx).focus_handle(cx).is_focused(window)
                    || window.focused(cx).is_none();
                if !typing && ours {
                    match &request.ask {
                        Ask::Clarify(q) if q.choices.is_empty() => self
                            .answer_input
                            .update(cx, |input, cx| input.focus(window, cx)),
                        _ => window.focus(&self.ask_focus, cx),
                    }
                }
            }
            None => {
                // The panel that had the keyboard is gone: the input has it back.
                if had_one && self.ask_focus.contains_focused(window, cx) {
                    self.focus_input(window, cx);
                }
            }
        }
        cx.notify();
    }
}

/// What the "Always allow" confirmation says (the Tauri page's `alwaysDescription`, our store's
/// place: the runtime keeps the allowlist in the data folder; Settings lists it, Phase 7).
pub fn always_words(reason: &str) -> String {
    format!(
        "Always allow commands flagged \u{201c}{}\u{201d}? This adds the pattern to your permanent \
         allowlist: Hermes won\u{2019}t ask again for commands like this, in this thread or any \
         other, until you remove it.",
        super::ask::one_line(&visible(reason).0, 120)
    )
}

/// A radio mark (t3code's `OptionRadio`): a ring, filled with a dot when picked.
fn radio(checked: bool, palette: &Palette) -> AnyElement {
    div()
        .size(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(palette.foreground.hsla().opacity(0.55))
        .when(checked, |d| {
            d.bg(palette.primary.hsla()).child(
                div()
                    .size(px(6.))
                    .rounded_full()
                    .bg(palette.primary_foreground.hsla()),
            )
        })
        .into_any_element()
}

/// A checkbox mark: a rounded square, filled with a check when picked.
fn checkbox(checked: bool, palette: &Palette) -> AnyElement {
    div()
        .size(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius::SM)
        .border_1()
        .border_color(palette.foreground.hsla().opacity(0.55))
        .when(checked, |d| {
            d.bg(palette.primary.hsla()).child(
                icon(IconName::Check)
                    .with_size(KitSize::Size(px(12.)))
                    .text_color(palette.primary_foreground.hsla()),
            )
        })
        .into_any_element()
}
