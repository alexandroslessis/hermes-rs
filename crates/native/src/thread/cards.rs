//! The work log's rows, drawn: a run of tool calls ([`work_row`]: the call's card when there is
//! one, else a header "Worked for 1m 12s 8 steps" that opens the list), one card per call
//! ([`entry_view`]: command, file edit, read, search, web, delegate, memory and skill write,
//! generic), and the turn's footer ([`footer_row`]: changed files as a tree, usage, Retry, Branch).
//!
//! Costs: a card draws what its model holds. Output is parsed when it arrives (`tools::Output`),
//! never here; a command's output is a `uniform_list` (only the visible lines are built, whatever
//! the length; it follows the tail while the command runs); a diff shows its first 60 lines; an
//! opened group draws its last 150 calls. Nothing here reads the core: clicks call the thread
//! model, and "open the diff" / "open the screen" are actions the workspace answers.
//!
//! t3code: `apps/web/src/components/chat/WorkLog.tsx` (the row: a 24 px line, a 16 px icon in a
//! 24 px box, `text-sm` in `--secondary-label`, a 12 px chevron, `ms-7` details),
//! `TimelineWorkGroupRows.tsx` (the group header), `TimelineWorkEntryRow.tsx`, `CommandBlock.tsx`
//! (`$ command`), `CommandOutputPanel.tsx` (the output, ANSI colours),
//! `ChangedFilesTree.tsx` + `DiffStatLabel.tsx` (the footer's card, `+N -M`). References:
//! `app-thread-dark.png` (the thread's tool rows), `sidebar-diff-content.png` (the diff's colours),
//! `app-terminal-open.png` (output colours). The cards and the footer were not in a screenshot:
//! they follow the source (docs/native-ui/reference/t3code/README.md).

use std::ops::Range;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, ClipboardItem, ElementId, Entity, FontStyle, FontWeight, HighlightStyle, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, UnderlineStyle,
    UniformListScrollHandle,
    assets::IconName,
    component::{ActiveTheme as _, Sizable as _, Size as KitSize},
    div,
    prelude::FluentBuilder as _,
    px, uniform_list,
};

use super::ansi;
use super::data::duration_label;
use super::diff::{Diff, Kind as DiffKind, compact_count};
use super::model::{Row, ThreadModel};
use super::tools::{Detail, Node, Output, Screen, Status, Subtask, ToolCard, ToolKind};
use super::work::{Footer, ToolEntry, WorkGroup};
use super::{OpenDiffTab, OpenScreen};
use crate::design::{
    Palette,
    button::{self, Button, Variant},
    icons::icon,
    tokens::{radius, text},
};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// One line of output.
pub const LINE_H: f32 = 18.;
/// An output list shows at most this many lines before it scrolls.
pub const MAX_LINES: usize = 16;
/// A diff shows its first lines; the rest is "N more lines".
pub const MAX_DIFF: usize = 60;
/// An opened group draws its last calls only.
pub const MAX_ENTRIES: usize = 150;
/// The footer's tree draws this many rows.
pub const MAX_TREE: usize = 250;
/// t3code's `ms-7`: what is under a row starts under its label.
const INDENT: f32 = 30.;

/// t3code's diff colours: `--diff-addition-foreground` (emerald 700 / 400) and
/// `--diff-deletion-foreground` (red 700 / 400), the palette's `success_foreground` and
/// `destructive_foreground` (the same Tailwind v4 colours, and the other themes' own).
pub fn diff_colors(palette: &Palette) -> (Hsla, Hsla) {
    (
        palette.success_foreground.hsla(),
        palette.destructive_foreground.hsla(),
    )
}

fn tool_icon(kind: ToolKind) -> IconName {
    match kind {
        ToolKind::Command => IconName::SquareTerminal,
        ToolKind::Edit => IconName::FilePen,
        ToolKind::Read => IconName::FileText,
        ToolKind::Search => IconName::Search,
        ToolKind::Web => IconName::Globe,
        ToolKind::Delegate => IconName::Bot,
        ToolKind::Memory => IconName::Brain,
        ToolKind::Skill => IconName::BookOpen,
        ToolKind::Generic => IconName::Wrench,
    }
}

fn sized(name: IconName, size: f32) -> gpui_kit::component::Icon {
    icon(name).with_size(KitSize::Size(px(size)))
}

/// An id from two numbers (the dump names it `name:<a << 16 | b>`).
fn id2(name: &'static str, a: usize, b: usize) -> ElementId {
    ElementId::NamedInteger(name.into(), ((a as u64) << 16) | b as u64)
}

// --- a run of calls ---------------------------------------------------------------------------

/// A group row: the card itself for one call, else the header and, opened, the list of calls.
pub fn work_row(model: &Entity<ThreadModel>, row: &Row, group: &WorkGroup, cx: &App) -> AnyElement {
    let key = row.key;
    if let [only] = group.entries.as_slice() {
        return div()
            .id(("work", key as usize))
            .child(entry_view(model, key, only, cx))
            .probe()
            .into_any_element();
    }
    let palette = cx.design_theme().palette;
    let now = model.read(cx).now_ms();
    let summary = group.summary(now);
    let toggle = model.clone();
    let skipped = group.entries.len().saturating_sub(MAX_ENTRIES);
    // t3code's turn fold (`MessagesTimeline.tsx`, `TurnFoldTimelineRow`; `WorkingTimelineRow`
    // while it runs): no icon, the label in muted text that turns foreground on hover, a 14 px
    // chevron right after it, `px-1`, and a hairline (`border-b border-border/60`, `pt-1 pb-2`)
    // between the fold and what follows. Reference: `app-thread-dark.png` ("Worked for 2.8s >").
    let muted = palette.muted_foreground.hsla();
    let hover_ink = palette.foreground.hsla();
    let header = div()
        .id(("work-head", key as usize))
        .flex()
        .items_center()
        .gap(px(4.))
        .min_h(px(24.))
        .px(px(4.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_color(muted)
        .hover(move |s| s.text_color(hover_ink))
        .on_click(move |_, _, cx| toggle.update(cx, |m, cx| m.toggle_group(key, cx)))
        .child(
            div()
                .id(("work-label", key as usize))
                .min_w_0()
                .truncate()
                .child(summary.label.clone())
                .probe_text(summary.label.clone()),
        )
        .child(
            div()
                .id(("work-steps", key as usize))
                .flex_shrink_0()
                .child(format!("\u{b7} {}", summary.steps))
                .probe(),
        )
        .when(summary.failed, |d| {
            d.child(
                div()
                    .text_color(palette.destructive.hsla())
                    .child(sized(IconName::Close, 12.)),
            )
        })
        .child(sized(
            if group.open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            },
            14.,
        ))
        .probe();
    let header = div()
        .pt(px(4.))
        .pb(px(8.))
        .mb(px(4.))
        .border_b_1()
        .border_color(palette.border.hsla().opacity(0.6))
        .child(header);
    div()
        .id(("work", key as usize))
        .flex()
        .flex_col()
        .text_size(text::SM.size)
        .line_height(text::SM.line_height)
        .child(header)
        .when(group.open, |d| {
            d.child(
                div()
                    .id(("work-list", key as usize))
                    .flex()
                    .flex_col()
                    .max_h(px(288. + 80.))
                    .overflow_y_scroll()
                    .when(skipped > 0, |d| {
                        d.child(
                            div()
                                .px(px(2.))
                                .py(px(2.))
                                .text_size(text::XS.size)
                                .text_color(palette.muted_foreground.hsla())
                                .child(format!("{skipped} earlier steps not shown")),
                        )
                    })
                    .children(
                        group
                            .entries
                            .iter()
                            .skip(skipped)
                            .map(|e| entry_view(model, key, e, cx)),
                    )
                    .probe(),
            )
        })
        .probe()
        .into_any_element()
}

// --- one card ---------------------------------------------------------------------------------

/// t3code's `DiffStatLabel`, `layout="inline"` (`gap-1`, the text size of its line).
fn stat(additions: u32, deletions: u32, palette: &Palette, mono: &SharedString) -> AnyElement {
    let (add, del) = diff_colors(palette);
    div()
        .flex()
        .gap(px(4.))
        .flex_shrink_0()
        .font_family(mono.clone())
        .text_size(text::XS.size)
        .child(
            div()
                .text_color(add)
                .child(format!("+{}", compact_count(additions))),
        )
        .child(
            div()
                .text_color(del)
                .child(format!("-{}", compact_count(deletions))),
        )
        .into_any_element()
}

/// `DiffStatLabel`'s default `aligned` layout in the tree: two right-aligned 4ch columns,
/// `gap-2`, `text-3xs`, so the counts line up down the tree.
fn stat_aligned(
    additions: u32,
    deletions: u32,
    palette: &Palette,
    mono: &SharedString,
) -> AnyElement {
    let (add, del) = diff_colors(palette);
    let col = |s: String, color: Hsla| {
        div()
            .w(px(26.))
            .flex()
            .justify_end()
            .text_color(color)
            .child(s)
    };
    div()
        .flex()
        .gap(px(8.))
        .flex_shrink_0()
        .font_family(mono.clone())
        .text_size(text::XXS3.size)
        .line_height(text::XXS3.line_height)
        .child(col(format!("+{}", compact_count(additions)), add))
        .child(col(format!("-{}", compact_count(deletions)), del))
        .into_any_element()
}

/// One call as a card: its line, and its details when opened.
pub fn entry_view(
    model: &Entity<ThreadModel>,
    row_key: u64,
    e: &ToolEntry,
    cx: &App,
) -> AnyElement {
    let palette = cx.design_theme().palette;
    let dark = cx.design_theme().appearance.is_dark();
    let card = &e.card;
    let key = e.key as usize;
    let running = card.status == Status::Running;
    let failed = card.status == Status::Failed;
    let expandable = card.expandable();
    let toggle = model.clone();
    let entry_key = e.key;
    let mono = cx.theme().mono_font_family.clone();
    let secondary = palette.secondary_label.hsla();
    let muted = palette.muted_foreground.hsla();

    let icon_color = if failed {
        palette.destructive.hsla()
    } else {
        palette.icon_muted.hsla()
    };
    let title: AnyElement = if card.kind == ToolKind::Command {
        div()
            .min_w_0()
            .truncate()
            .font_family(mono.clone())
            .text_size(text::XS.size)
            .text_color(secondary)
            .child(
                div()
                    .flex()
                    .child(div().mr(px(6.)).text_color(muted).child("$"))
                    .child(card.title.clone()),
            )
            .into_any_element()
    } else {
        div()
            .min_w_0()
            .truncate()
            .text_color(secondary)
            .child(card.title.clone())
            .into_any_element()
    };
    let head = div()
        .id(("entry-head", key))
        .flex()
        .items_center()
        .gap(px(6.))
        .min_h(px(24.))
        .px(px(2.))
        .rounded(px(6.))
        .when(expandable, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(palette.accent.hsla()))
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |m, cx| m.toggle_entry(row_key, entry_key, cx))
                })
        })
        .child(
            div()
                .w(px(24.))
                .flex()
                .justify_center()
                .text_color(icon_color)
                .child(sized(
                    if running {
                        IconName::LoaderCircle
                    } else {
                        tool_icon(card.kind)
                    },
                    16.,
                )),
        )
        .child(div().min_w_0().flex_shrink_1().child(title))
        .when(!card.subtitle.is_empty(), |d| {
            d.child(
                div()
                    .min_w_0()
                    .flex_shrink_1()
                    .truncate()
                    .text_size(text::XS.size)
                    .text_color(if failed {
                        palette.destructive.hsla()
                    } else {
                        muted
                    })
                    .child(card.subtitle.clone()),
            )
        })
        .child(div().flex_1())
        .when_some(card.exit_code.filter(|c| *c != 0), |d, code| {
            d.child(
                div()
                    .id(("exit-code", key))
                    .flex_shrink_0()
                    .font_family(mono.clone())
                    .text_size(text::XS.size)
                    .text_color(palette.destructive.hsla())
                    .child(format!("exit {code}"))
                    .probe(),
            )
        })
        .when_some(card.duration_ms.filter(|_| !running), |d, ms| {
            d.child(
                div()
                    .flex_shrink_0()
                    .text_size(text::XS.size)
                    .text_color(muted)
                    .child(duration_label(ms)),
            )
        })
        .when_some(edit_counts(card), |d, (a, del)| d.child(stat(a, del, &palette, &mono)))
        .when(failed && card.subtitle.is_empty(), |d| {
            d.child(
                div()
                    .text_color(palette.destructive.hsla())
                    .child(sized(IconName::Close, 12.)),
            )
        })
        .when(expandable, |d| {
            d.child(
                div()
                    .w(px(16.))
                    .flex()
                    .justify_center()
                    .text_color(palette.icon_muted.hsla())
                    .child(sized(
                        if e.open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        12.,
                    )),
            )
        })
        .probe();

    div()
        .id(("entry", key))
        .flex()
        .flex_col()
        .text_size(text::SM.size)
        .line_height(text::SM.line_height)
        .child(head)
        .when(e.open && expandable, |d| {
            d.child(body(model, row_key, e, &palette, dark, cx))
        })
        .probe()
        .into_any_element()
}

fn edit_counts(card: &ToolCard) -> Option<(u32, u32)> {
    match &card.detail {
        Detail::Edit {
            additions,
            deletions,
            ..
        } if card.status != Status::Failed && (*additions > 0 || *deletions > 0) => {
            Some((*additions, *deletions))
        }
        _ => None,
    }
}

/// The text a command card's copy button puts on the clipboard: the command, and its whole
/// output as plain text (the list clips long lines and scrolls; this is all of it).
pub fn copy_text(command: &str, output: &Output) -> String {
    let body = match output.lines() {
        Some(lines) => lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        None => ansi::parse(&output.raw)
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
    };
    if body.trim().is_empty() {
        command.to_string()
    } else {
        format!("$ {command}\n{body}")
    }
}

/// t3code's `CommandBlock` copy button (`icon-xs`, ghost).
/// The text is built on the click, not per frame (the output is shared, not copied).
fn copy_button(id: (&'static str, usize), command: &str, output: &Output) -> AnyElement {
    let (command, output) = (command.to_string(), output.clone());
    button::icon_button(id, IconName::Copy, "Copy command and output")
        .variant(Variant::GhostMuted)
        .size(button::Size::Micro)
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_text(&command, &output)))
        })
        .into_any_element()
}

/// The surface details sit on: t3code's command block (`rounded-lg border border-border
/// bg-muted/50`).
fn surface(id: (&'static str, usize), palette: &Palette) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap(px(6.))
        .min_w_0()
        .p(px(8.))
        .rounded(radius::LG)
        .border_1()
        .border_color(palette.border.hsla())
        .bg(palette.muted.hsla().opacity(0.5))
        .text_size(text::XS.size)
        .line_height(text::XS.line_height)
        .text_color(palette.foreground.hsla().opacity(0.85))
}

/// What an opened card shows, indented under its label.
fn body(
    model: &Entity<ThreadModel>,
    row_key: u64,
    e: &ToolEntry,
    palette: &Palette,
    dark: bool,
    cx: &App,
) -> AnyElement {
    div()
        .id(("entry-body", e.key as usize))
        .ml(px(INDENT))
        .mt(px(2.))
        .mb(px(6.))
        .min_w_0()
        .child(body_inner(model, row_key, e, palette, dark, cx))
        .probe()
        .into_any_element()
}

fn body_inner(
    model: &Entity<ThreadModel>,
    row_key: u64,
    e: &ToolEntry,
    palette: &Palette,
    dark: bool,
    cx: &App,
) -> AnyElement {
    let card = &e.card;
    let key = e.key as usize;
    let muted = palette.muted_foreground.hsla();
    let mono = cx.theme().mono_font_family.clone();
    let running = card.status == Status::Running;
    let wrapper = div().min_w_0();
    match &card.detail {
        Detail::Command {
            command,
            cwd,
            note,
            output,
        } => wrapper
            .child(
                surface(("command", key), palette)
                    .font_family(mono.clone())
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(6.))
                            .child(div().text_color(muted).child("$"))
                            .child(div().min_w_0().flex_1().child(command.clone()))
                            .child(copy_button(("copy-output", key), command, output)),
                    )
                    .when_some(cwd.clone(), |d, cwd| {
                        d.child(div().text_color(muted).child(cwd))
                    })
                    .child(output_block(
                        ("output", key),
                        output,
                        &e.scroll,
                        running,
                        palette,
                        dark,
                        &mono,
                    ))
                    .when_some(note.clone(), |d, note| {
                        d.child(
                            div()
                                .font_family(cx.theme().font_family.clone())
                                .text_color(muted)
                                .child(note),
                        )
                    })
                    .probe(),
            )
            .into_any_element(),
        Detail::Edit {
            path,
            diff,
            from_args,
            ..
        } => {
            let open = OpenDiffTab {
                path: Some(path.clone()),
            };
            wrapper
                .child(
                    surface(("diff", key), palette)
                        .p(px(0.))
                        .overflow_hidden()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .px(px(8.))
                                .py(px(4.))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .font_family(mono.clone())
                                        .child(path.clone()),
                                )
                                .child(
                                    Button::new(("open-diff", key), "Open diff")
                                        .icon(IconName::FileDiff)
                                        .variant(Variant::GhostMuted)
                                        .size(button::Size::Micro)
                                        .on_click(move |_, window, cx| {
                                            window.dispatch_action(Box::new(open.clone()), cx)
                                        }),
                                ),
                        )
                        .child(diff_view(key, diff, *from_args, palette, &mono))
                        .probe(),
                )
                .into_any_element()
        }
        Detail::Text(output) => wrapper
            .child(
                surface(("text", key), palette)
                    .font_family(mono.clone())
                    .child(output_block(
                        ("output", key),
                        output,
                        &e.scroll,
                        false,
                        palette,
                        dark,
                        &mono,
                    ))
                    .probe(),
            )
            .into_any_element(),
        Detail::Delegate(tasks) => wrapper
            .child(
                div()
                    .id(("tasks", key))
                    .flex()
                    .flex_col()
                    .children(
                        tasks
                            .iter()
                            .enumerate()
                            .map(|(i, t)| subtask_view(model, row_key, e.key, i, t, palette, cx)),
                    )
                    .probe(),
            )
            .into_any_element(),
        Detail::Write { message, link } => {
            let (screen, target) = match link {
                Screen::Memory(target) => (
                    if target == "user" { "user" } else { "memory" },
                    target.clone(),
                ),
                Screen::Skill(name) => ("skill", name.clone()),
            };
            let action = OpenScreen {
                screen: screen.to_string(),
                target,
            };
            wrapper
                .child(
                    div()
                        .id(("write", key))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .min_w_0()
                                .text_size(text::XS.size)
                                .text_color(muted)
                                .child(message.clone()),
                        )
                        .child(
                            Button::new(("open-screen", key), link.label())
                                .icon(IconName::ExternalLink)
                                .variant(Variant::GhostMuted)
                                .size(button::Size::Micro)
                                .on_click(move |_, window, cx| {
                                    window.dispatch_action(Box::new(action.clone()), cx)
                                }),
                        )
                        .probe(),
                )
                .into_any_element()
        }
        Detail::Generic { args, result } => {
            wrapper
                .child(
                    surface(("generic", key), palette)
                        .font_family(mono.clone())
                        .when(!args.is_empty(), |d| {
                            d.child(div().children(args.lines().take(12).map(|l| {
                                div().h(px(LINE_H)).whitespace_nowrap().child(l.to_string())
                            })))
                        })
                        .child(output_block(
                            ("output", key),
                            result,
                            &e.scroll,
                            false,
                            palette,
                            dark,
                            &mono,
                        ))
                        .probe(),
                )
                .into_any_element()
        }
        Detail::None => wrapper.into_any_element(),
    }
}

fn subtask_view(
    model: &Entity<ThreadModel>,
    row_key: u64,
    entry: u64,
    i: usize,
    task: &Subtask,
    palette: &Palette,
    cx: &App,
) -> AnyElement {
    let toggle = model.clone();
    let muted = palette.muted_foreground.hsla();
    let mono = cx.theme().mono_font_family.clone();
    let status_icon = match task.status {
        Status::Running => IconName::LoaderCircle,
        Status::Done => IconName::Check,
        Status::Failed => IconName::Close,
    };
    let status_color = match task.status {
        Status::Failed => palette.destructive.hsla(),
        _ => palette.icon_muted.hsla(),
    };
    let tools = task.tools.len();
    let sub = [
        (tools > 0).then(|| format!("{tools} tool{}", if tools == 1 { "" } else { "s" })),
        task.duration_s.map(|s| duration_label((s * 1000.) as u64)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" \u{b7} ");
    div()
        .id(id2("subtask", entry as usize, i))
        .flex()
        .flex_col()
        .child(
            div()
                .id(id2("subtask-head", entry as usize, i))
                .flex()
                .items_center()
                .gap(px(6.))
                .min_h(px(24.))
                .px(px(2.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|s| s.bg(palette.accent.hsla()))
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |m, cx| m.toggle_subtask(row_key, entry, i, cx))
                })
                .child(
                    div()
                        .w(px(16.))
                        .text_color(status_color)
                        .child(sized(status_icon, 14.)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_color(palette.secondary_label.hsla())
                        .child(task.goal.lines().next().unwrap_or_default().to_string()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(text::XS.size)
                        .text_color(muted)
                        .child(sub),
                )
                .child(div().text_color(palette.icon_muted.hsla()).child(sized(
                    if task.open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    12.,
                )))
                .probe(),
        )
        .when(task.open, |d| {
            d.child(
                div()
                    .id(id2("subtask-body", entry as usize, i))
                    .ml(px(22.))
                    .pl(px(10.))
                    .border_l_1()
                    .border_color(palette.border.hsla())
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .text_size(text::XS.size)
                    .text_color(muted)
                    .children(task.tools.iter().map(|t| {
                        div()
                            .flex()
                            .gap(px(6.))
                            .font_family(mono.clone())
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(if t.failed {
                                        palette.destructive.hsla()
                                    } else {
                                        palette.secondary_label.hsla()
                                    })
                                    .child(t.name.clone()),
                            )
                            .child(div().min_w_0().truncate().child(t.preview.clone()))
                    }))
                    .when(!task.summary.is_empty(), |d| {
                        d.child(
                            div()
                                .pt(px(4.))
                                .text_size(text::SM.size)
                                .text_color(palette.foreground.hsla().opacity(0.85))
                                .child(task.summary.clone()),
                        )
                    })
                    .probe(),
            )
        })
        .probe()
        .into_any_element()
}

// --- output -----------------------------------------------------------------------------------

/// One line as styled text: the line's runs are highlights over its text.
fn line_view(line: &ansi::Line, palette: &Palette, dark: bool) -> AnyElement {
    let base = palette.muted_foreground.hsla();
    let highlights: Vec<(Range<usize>, HighlightStyle)> = line
        .spans
        .iter()
        .map(|span| {
            let mut style = HighlightStyle::default();
            if let Some(color) = span.style.fg {
                style.color = Some(ansi::resolve(color, dark).hsla());
            }
            if span.style.bold {
                style.font_weight = Some(FontWeight::BOLD);
            }
            if span.style.italic {
                style.font_style = Some(FontStyle::Italic);
            }
            if span.style.dim {
                style.fade_out = Some(0.4);
            }
            if span.style.underline {
                style.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    color: None,
                    wavy: false,
                });
            }
            (span.range.clone(), style)
        })
        .collect();
    div()
        .h(px(LINE_H))
        .whitespace_nowrap()
        .overflow_hidden()
        .text_color(base)
        .child(StyledText::new(SharedString::from(line.text.clone())).with_highlights(highlights))
        .into_any_element()
}

/// The lines of `output` in a virtual list at most [`MAX_LINES`] tall.
fn output_block(
    id: (&'static str, usize),
    output: &Output,
    scroll: &UniformListScrollHandle,
    follow: bool,
    palette: &Palette,
    dark: bool,
    mono: &SharedString,
) -> AnyElement {
    let Some(lines) = output.lines().cloned() else {
        // Not parsed yet (it parses as the card opens): the text alone, clipped.
        return div()
            .h(px(LINE_H))
            .text_color(palette.muted_foreground.hsla())
            .child("...")
            .into_any_element();
    };
    if lines.is_empty() {
        return div()
            .text_color(palette.muted_foreground.hsla())
            .child(if follow {
                "Waiting for output..."
            } else {
                "No output."
            })
            .into_any_element();
    }
    let n = lines.len();
    if follow {
        scroll.scroll_to_item(n - 1, ScrollStrategy::Bottom);
    }
    let palette = *palette;
    let rows: Arc<Vec<ansi::Line>> = lines;
    div()
        .child(
            uniform_list(id, n, move |range, _, _| {
                range
                    .map(|ix| line_view(&rows[ix], &palette, dark))
                    .collect::<Vec<_>>()
            })
            .track_scroll(scroll)
            .h(px(n.min(MAX_LINES) as f32 * LINE_H))
            .font_family(mono.clone())
            .probe(),
        )
        .into_any_element()
}

// --- diff -------------------------------------------------------------------------------------

fn diff_view(
    key: usize,
    diff: &Diff,
    from_args: bool,
    palette: &Palette,
    mono: &SharedString,
) -> AnyElement {
    let (add, del) = diff_colors(palette);
    let muted = palette.muted_foreground.hsla();
    let shown = diff.lines.len().min(MAX_DIFF);
    div()
        .id(("diff-lines", key))
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(palette.border.hsla())
        .font_family(mono.clone())
        .children(diff.lines.iter().take(MAX_DIFF).map(|line| {
            let (marker, color, tint) = match line.kind {
                DiffKind::Add => ("+", Some(add), Some(add.opacity(0.12))),
                DiffKind::Del => ("-", Some(del), Some(del.opacity(0.12))),
                DiffKind::Context => (" ", None, None),
                DiffKind::Hunk => ("", Some(muted), None),
            };
            let number = match line.kind {
                DiffKind::Del => line.old_no,
                _ => line.new_no,
            };
            div()
                .flex()
                .h(px(LINE_H))
                .when_some(tint, |d, tint| d.bg(tint))
                .text_color(color.unwrap_or(palette.foreground.hsla().opacity(0.8)))
                .child(
                    div()
                        .w(px(40.))
                        .flex_shrink_0()
                        .pr(px(8.))
                        .flex()
                        .justify_end()
                        .text_color(muted)
                        .child(number.map(|n| n.to_string()).unwrap_or_default()),
                )
                .child(div().w(px(12.)).flex_shrink_0().child(marker))
                .child(
                    div()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(line.text.clone()),
                )
        }))
        .when(diff.lines.len() > shown, |d| {
            d.child(
                div()
                    .px(px(8.))
                    .py(px(2.))
                    .text_color(muted)
                    .child(format!("{} more lines", diff.lines.len() - shown)),
            )
        })
        .when(from_args, |d| {
            d.child(
                div()
                    .px(px(8.))
                    .py(px(2.))
                    .text_color(muted)
                    .child("The lines around the change are not shown."),
            )
        })
        .probe()
        .into_any_element()
}

// --- the turn's footer ------------------------------------------------------------------------

/// The end of a turn: its changed files, its usage, Retry and Branch.
pub fn footer_row(model: &Entity<ThreadModel>, row: &Row, footer: &Footer, cx: &App) -> AnyElement {
    let palette = cx.design_theme().palette;
    let dark = cx.design_theme().appearance.is_dark();
    let key = row.key;
    let muted = palette.muted_foreground.hsla();
    let mono = cx.theme().mono_font_family.clone();
    let files = &footer.files;
    let (additions, deletions) = files
        .iter()
        .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
    let rows = super::tools::tree_rows(&footer.tree, &footer.closed);
    let hidden = rows.len().saturating_sub(MAX_TREE);
    let toggle_all = model.clone();
    let open_all = OpenDiffTab { path: None };
    let usage = [
        footer.usage.map(|u| u.label()),
        footer.elapsed_ms.map(duration_label),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" \u{b7} ");
    let (retry, branch) = (model.clone(), model.clone());
    let busy = footer.busy;
    let can_retry = model.read(cx).can_retry(key);
    let has_dirs = footer.has_dirs();

    let card = (!files.is_empty()).then(|| {
        div()
            .id(("changed-files", key as usize))
            .flex()
            .flex_col()
            .rounded(radius::LG)
            // `bg-secondary dark:bg-input/20`.
            .bg(if dark {
                palette.input.hsla().opacity(0.2)
            } else {
                palette.secondary.hsla()
            })
            .child(
                div()
                    .id(("changed-head", key as usize))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .text_size(text::XS.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(palette.foreground.hsla())
                            .child(format!(
                                "{} changed file{}",
                                files.len(),
                                if files.len() == 1 { "" } else { "s" }
                            ))
                            .when(additions > 0 || deletions > 0, |d| {
                                d.child(stat(additions, deletions, &palette, &mono))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .when(footer.has_dirs(), |d| {
                                d.child(
                                    button::icon_button(
                                        ("collapse-all", key as usize),
                                        if footer.all_closed() {
                                            IconName::ChevronsUpDown
                                        } else {
                                            IconName::ChevronsDownUp
                                        },
                                        if footer.all_closed() {
                                            "Expand all folders"
                                        } else {
                                            "Collapse all folders"
                                        },
                                    )
                                    .variant(Variant::GhostMuted)
                                    .size(button::Size::Micro)
                                    .on_click(
                                        move |_, _, cx| {
                                            toggle_all
                                                .update(cx, |m, cx| m.toggle_all_dirs(key, cx))
                                        },
                                    ),
                                )
                            })
                            .child(
                                Button::new(("open-diff-all", key as usize), "Open diff")
                                    .icon(IconName::FileDiff)
                                    .variant(Variant::GhostMuted)
                                    .size(button::Size::Xs)
                                    .on_click(move |_, window, cx| {
                                        window.dispatch_action(Box::new(open_all.clone()), cx)
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .id(("changed-tree", key as usize))
                    .flex()
                    .flex_col()
                    .p(px(8.))
                    .children(
                        rows.iter()
                            .take(MAX_TREE)
                            .enumerate()
                            .map(|(i, r)| tree_row(model, key, i, r, &palette, has_dirs, &mono)),
                    )
                    .when(hidden > 0, |d| {
                        d.child(
                            div()
                                .px(px(8.))
                                .text_size(text::XS.size)
                                .text_color(muted)
                                .child(format!("{hidden} more rows")),
                        )
                    })
                    .probe(),
            )
            .probe()
    });

    div()
        .id(("footer", key as usize))
        .flex()
        .flex_col()
        .gap(px(6.))
        .text_size(text::SM.size)
        .line_height(text::SM.line_height)
        .when_some(card, |d, card| d.child(card))
        .child(
            div()
                .id(("footer-actions", key as usize))
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(text::XS.size)
                .text_color(muted)
                .child(
                    Button::new(("retry", key as usize), "Retry")
                        .icon(IconName::RotateCcw)
                        .variant(Variant::GhostMuted)
                        .size(button::Size::Xs)
                        .disabled(busy || footer.user_row.is_none() || !can_retry)
                        .on_click(move |_, _, cx| retry.update(cx, |m, cx| m.retry(key, cx))),
                )
                .child(
                    Button::new(("branch", key as usize), "Branch from here")
                        .icon(IconName::GitBranch)
                        .variant(Variant::GhostMuted)
                        .size(button::Size::Xs)
                        .disabled(busy || footer.last_row.is_none())
                        .on_click(move |_, _, cx| branch.update(cx, |m, cx| m.branch(key, cx))),
                )
                .when(!usage.is_empty(), |d| {
                    d.child(
                        div()
                            .id(("footer-usage", key as usize))
                            .ml(px(8.))
                            .child(usage.clone())
                            .probe(),
                    )
                })
                .probe(),
        )
        .probe()
        .into_any_element()
}

fn tree_row(
    model: &Entity<ThreadModel>,
    key: u64,
    i: usize,
    row: &super::tools::TreeRow,
    palette: &Palette,
    has_dirs: bool,
    mono: &SharedString,
) -> AnyElement {
    let left = 8. + row.depth as f32 * 14.;
    let muted = palette.muted_foreground.hsla();
    let accent = palette.accent.hsla().opacity(0.6);
    let base = div()
        .id(id2("tree-row", key as usize, i))
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(28.))
        .pl(px(left))
        .pr(px(8.))
        .rounded(px(6.))
        .cursor_pointer()
        .hover(move |s| s.bg(accent))
        .font_family(mono.clone());
    match &row.node {
        Node::Dir {
            name,
            path,
            additions,
            deletions,
            ..
        } => {
            let toggle = model.clone();
            let path = path.clone();
            base.on_click(move |_, _, cx| toggle.update(cx, |m, cx| m.toggle_dir(key, &path, cx)))
                .child(div().text_color(muted).child(sized(
                    if row.open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    14.,
                )))
                .child(div().text_color(muted).child(sized(
                    if row.open {
                        IconName::FolderOpen
                    } else {
                        IconName::FolderClosed
                    },
                    14.,
                )))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(text::XXS2.size)
                        .text_color(muted)
                        .child(name.clone()),
                )
                .child(div().flex_1())
                .when(*additions > 0 || *deletions > 0, |d| {
                    d.child(stat_aligned(*additions, *deletions, palette, mono))
                })
                .probe()
                .into_any_element()
        }
        Node::File {
            name,
            path,
            additions,
            deletions,
        } => {
            let action = OpenDiffTab {
                path: Some(path.clone()),
            };
            base.on_click(move |_, window, cx| window.dispatch_action(Box::new(action.clone()), cx))
                // t3code's spacer under the folders' chevrons, only in a tree with folders.
                .when(has_dirs || row.depth > 0, |d| d.child(div().w(px(14.)).flex_shrink_0()))
                .child(div().text_color(muted).child(sized(IconName::File, 14.)))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(text::XS.size)
                        .text_color(palette.foreground.hsla().opacity(0.85))
                        .child(name.clone()),
                )
                .child(div().flex_1())
                .when(*additions > 0 || *deletions > 0, |d| {
                    d.child(stat_aligned(*additions, *deletions, palette, mono))
                })
                .probe()
                .into_any_element()
        }
    }
}
