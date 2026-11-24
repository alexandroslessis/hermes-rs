//! CP5.2 in a headless window over a [`FixtureBackend`]: the `@` menu (opens on `@`, filters as the
//! word grows, arrows, Enter, Tab, Escape; a pick is a chip that is deleted as a unit and sent as
//! the reference the runtime reads), the `/` menu (the catalog, arguments and their hint, a
//! command that answers locally shows a row in the timeline and sends nothing to the model, a
//! skill is sent as a prompt, the app's commands go to the frame), the attachments (picked,
//! pasted, dropped; thumbnails; the core's limits; duplicates; what goes with the send), and the
//! layout of the drawer, the chip and the strip at three window widths.

use std::sync::Arc;

use gpui_kit::{ClipboardItem, Image, ImageFormat};

use super::*;
use crate::model::backend::{AttachmentSource, CommandAnswer, CompletionScope};
use crate::thread::composer::data::Picks;
use crate::thread::composer::attach::{self, Kind, MAX_ATTACHMENTS};
use crate::thread::composer::mention::{CommandOrigin, CommandSpec, Suggestion};
use crate::thread::composer::suggest::Entry;

// --- helpers ------------------------------------------------------------------------------------

impl Rig {
    fn view_read<R>(&mut self, f: impl FnOnce(&ComposerView, &gpui_kit::App) -> R) -> R {
        let view = self.view.clone();
        self.read(|cx| f(view.read(cx), cx))
    }

    /// The labels of the rows the drawer lists.
    fn entries(&mut self) -> Vec<String> {
        self.view_read(|v, cx| {
            v.entries(cx)
                .into_iter()
                .map(|e| match e {
                    Entry::Mention(m) => m.label,
                    Entry::Command(c) => c.name,
                })
                .collect()
        })
    }

    fn drawer_open(&mut self) -> bool {
        self.view_read(|v, cx| v.suggest_open(cx))
    }

    fn highlighted(&mut self) -> usize {
        self.view_read(|v, cx| v.highlighted(cx))
    }

    /// The chips in the input: their text and range.
    fn chips(&mut self) -> Vec<(std::ops::Range<usize>, String)> {
        self.view_read(|v, cx| {
            v.input()
                .read(cx)
                .tokens()
                .iter()
                .map(|t| (t.range(), t.token().text().to_string()))
                .collect()
        })
    }

    fn attachments(&mut self) -> Vec<attach::Attachment> {
        let model = self.model.clone();
        self.read(|cx| model.read(cx).attachments().to_vec())
    }

    fn add(&mut self, sources: Vec<AttachmentSource>) {
        self.model(move |m, cx| m.add_attachments(sources, cx));
        self.settle();
    }

    fn view_events(&self) -> Vec<ComposerViewEvent> {
        self.events.borrow().clone()
    }

    fn app_ids(&self) -> Vec<&'static str> {
        self.view_events()
            .into_iter()
            .filter_map(|e| match e {
                ComposerViewEvent::App(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    fn command_rows(&mut self) -> Vec<(String, String)> {
        let thread = self.thread.clone().unwrap();
        self.read(|cx| {
            thread
                .read(cx)
                .rows()
                .iter()
                .filter_map(|r| match &r.kind {
                    RowKind::Command { command, text } => {
                        Some((command.to_string(), text.to_string()))
                    }
                    _ => None,
                })
                .collect()
        })
    }

    fn user_images(&mut self) -> Vec<usize> {
        let thread = self.thread.clone().unwrap();
        self.read(|cx| {
            thread
                .read(cx)
                .rows()
                .iter()
                .filter_map(|r| match &r.kind {
                    RowKind::User { images, .. } => Some(images.len()),
                    _ => None,
                })
                .collect()
        })
    }

    fn prepares(&self) -> usize {
        self.ops()
            .iter()
            .filter(|op| matches!(op, ComposerOp::Prepare(_)))
            .count()
    }

    fn commands_run(&self) -> Vec<(String, String)> {
        self.ops()
            .into_iter()
            .filter_map(|op| match op {
                ComposerOp::Command { name, argument, .. } => Some((name, argument)),
                _ => None,
            })
            .collect()
    }
}

fn png_file(dir: &hermes_testdir::TestDir, name: &str, w: u32, h: u32) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, attach::test_png(w, h)).unwrap();
    path
}

fn text_file(dir: &hermes_testdir::TestDir, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn path_source(path: &std::path::Path) -> AttachmentSource {
    AttachmentSource::Path(path.to_path_buf())
}

// --- the `@` menu --------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn the_at_sign_opens_the_menu_and_the_word_filters_it() {
    let mut rig = rig();
    rig.type_text("look at @");
    assert!(rig.drawer_open());
    assert_eq!(
        rig.entries(),
        ["main.rs", "lib.rs", "README.md", "Cargo.toml", "src/", "docs/"]
    );
    assert!(rig.present("composer-suggest"));
    rig.type_text("ma");
    assert_eq!(rig.entries(), ["main.rs"]);
    // The core is asked with the whole word, in the thread's folder.
    assert!(rig.ops().iter().any(|op| matches!(
        op,
        ComposerOp::Complete { scope: CompletionScope::Thread(t), word } if t == "t1" && word == "@ma"
    )));
    // The word goes on: the old entries stay until the new ones land (no flicker), and a word
    // that matches nothing says so.
    rig.type_text("zz");
    assert!(rig.entries().is_empty());
    assert!(rig.present("suggest-empty"));
}

#[core::prelude::v1::test]
fn an_email_a_finished_word_and_plain_text_open_nothing() {
    let mut rig = rig();
    rig.type_text("mail me@example.com");
    assert!(!rig.drawer_open());
    rig.set_text("");
    rig.type_text("@src/main.rs ");
    assert!(!rig.drawer_open(), "the caret is past the word");
}

#[core::prelude::v1::test]
fn the_arrows_move_the_highlight_and_wrap() {
    let mut rig = rig();
    rig.type_text("@");
    assert_eq!(rig.highlighted(), 0);
    rig.press("down");
    rig.press("down");
    assert_eq!(rig.highlighted(), 2);
    rig.press("up");
    assert_eq!(rig.highlighted(), 1);
    rig.press("up");
    rig.press("up");
    assert_eq!(rig.highlighted(), 5, "up from the first wraps to the last");
    rig.press("down");
    assert_eq!(rig.highlighted(), 0, "down from the last wraps to the first");
    // The caret did not move in the input: the arrows were the menu's.
    assert_eq!(rig.text(), "@");
}

#[core::prelude::v1::test]
fn enter_makes_a_chip_of_the_highlighted_file_and_the_chip_is_the_reference() {
    let mut rig = rig();
    rig.type_text("fix @");
    rig.press("down");
    rig.press("enter");
    // The chip's text is the reference the runtime expands, and a space follows it.
    assert_eq!(rig.text(), "fix @file:src/lib.rs ");
    assert_eq!(rig.chips(), [(4..20, "@file:src/lib.rs".to_string())]);
    assert!(!rig.drawer_open(), "the pick closes the menu");
    // Nothing was sent by the Enter that picked.
    assert!(rig.sends().is_empty());
    // What is sent is exactly the text.
    rig.type_text("now");
    rig.press("enter");
    assert_eq!(rig.sends(), [("fix @file:src/lib.rs now".to_string(), false)]);
}

#[core::prelude::v1::test]
fn a_click_on_a_row_picks_it() {
    let mut rig = rig();
    rig.type_text("@");
    rig.click("suggest-item:2");
    assert_eq!(rig.text(), "@file:README.md ");
    assert_eq!(rig.chips().len(), 1);
}

#[core::prelude::v1::test]
fn tab_picks_a_file_and_walks_into_a_folder() {
    let mut rig = rig();
    rig.type_text("@main");
    rig.press("tab");
    assert_eq!(rig.text(), "@file:src/main.rs ");
    assert_eq!(rig.chips().len(), 1);
    rig.set_text("");
    rig.type_text("@src");
    // The first match of "src" is a file in src/, the folder is further down: highlight it.
    let names = rig.entries();
    let folder = names.iter().position(|n| n == "src/").expect("the folder");
    for _ in 0..folder {
        rig.press("down");
    }
    rig.press("tab");
    assert_eq!(rig.text(), "@src/", "Tab on a folder walks into it");
    assert!(rig.chips().is_empty());
    // The menu is open again for what is inside.
    assert!(rig.drawer_open());
    assert!(rig.ops().iter().any(|op| matches!(
        op,
        ComposerOp::Complete { word, .. } if word == "@src/"
    )));
}

#[core::prelude::v1::test]
fn a_folder_picked_with_enter_is_a_folder_chip() {
    let mut rig = rig();
    rig.type_text("@docs");
    rig.press("enter");
    assert_eq!(rig.text(), "@folder:docs/ ");
    assert_eq!(rig.chips(), [(0..13, "@folder:docs/".to_string())]);
}

#[core::prelude::v1::test]
fn a_prefix_is_typed_on_not_picked() {
    let mut rig = rig();
    rig.backend.set_paths(vec![Suggestion::from_item(
        "@file:",
        "@file:",
        "attach file",
    )]);
    rig.type_text("@");
    rig.press("enter");
    assert_eq!(rig.text(), "@file:");
    assert!(rig.chips().is_empty());
}

#[core::prelude::v1::test]
fn escape_puts_the_menu_away_until_the_word_is_a_new_one() {
    let mut rig = rig();
    rig.type_text("@ma");
    assert!(rig.drawer_open());
    rig.press("escape");
    assert!(!rig.drawer_open());
    rig.type_text("i");
    assert!(!rig.drawer_open(), "the same word: still away");
    assert_eq!(rig.text(), "@mai");
    // Another word: open again.
    rig.type_text(" @s");
    assert!(rig.drawer_open());
}

#[core::prelude::v1::test]
fn enter_with_nothing_listed_is_a_send() {
    let mut rig = rig();
    rig.type_text("@zzzz");
    assert!(rig.entries().is_empty());
    rig.press("enter");
    assert_eq!(rig.sends(), [("@zzzz".to_string(), false)]);
}

#[core::prelude::v1::test]
fn backspace_deletes_a_chip_as_one_unit() {
    let mut rig = rig();
    rig.type_text("see @");
    rig.press("enter");
    assert_eq!(rig.text(), "see @file:src/main.rs ");
    // The space first, then the chip whole.
    rig.press("backspace");
    assert_eq!(rig.text(), "see @file:src/main.rs");
    rig.press("backspace");
    assert_eq!(rig.text(), "see ");
    assert!(rig.chips().is_empty());
    // And undo brings it back as a chip.
    rig.press("ctrl-z");
    assert_eq!(rig.text(), "see @file:src/main.rs");
    assert_eq!(rig.chips().len(), 1);
}

#[core::prelude::v1::test]
fn the_caret_moves_over_a_chip_in_one_step() {
    let mut rig = rig();
    rig.set_text("a @file:x.rs b");
    rig.type_text("");
    rig.press("home");
    rig.press("right");
    rig.press("right");
    // 'a', ' ' passed: the caret is at the chip's start; one more step is the chip's end.
    let at = rig.view_read(|v, cx| v.input().read(cx).cursor());
    assert_eq!(at, 2);
    rig.press("right");
    let at = rig.view_read(|v, cx| v.input().read(cx).cursor());
    assert_eq!(at, 2 + "@file:x.rs".len());
}

#[core::prelude::v1::test]
fn copy_and_paste_keep_the_chip_and_a_manual_reference_stays_text_while_it_is_typed() {
    let mut rig = rig();
    rig.type_text("see @");
    rig.press("enter");
    assert_eq!(rig.chips().len(), 1);
    rig.press("ctrl-a");
    rig.press("ctrl-c");
    rig.set_text("");
    rig.press("ctrl-v");
    assert_eq!(rig.text(), "see @file:src/main.rs ");
    assert_eq!(rig.chips().len(), 1, "a pasted reference is a chip again");
    // Typed by hand it is text (a half-typed path must not turn into a chip under the caret).
    rig.set_text("");
    rig.type_text("@file:src/a.rs ");
    assert!(rig.chips().is_empty());
}

#[core::prelude::v1::test]
fn a_draft_with_references_loads_with_chips_and_an_old_draft_loads_as_it_was() {
    let mut rig = rig();
    // A draft saved by CP5.1 is plain text.
    let old = "refactor this\nthen run the tests @ home";
    let content = view::content_of(old);
    assert!(content.tokens().is_empty());
    rig.set_text(old);
    assert_eq!(rig.text(), old);
    assert!(rig.chips().is_empty());
    // One with references gets its chips back, and its text is the same text.
    let with = "see @file:src/a.rs and @folder:docs/ and more";
    rig.set_text(with);
    assert_eq!(rig.text(), with);
    let chips: Vec<_> = rig.chips().into_iter().map(|(_, t)| t).collect();
    assert_eq!(chips, ["@file:src/a.rs", "@folder:docs/"]);
    // The draft the frame keeps is the text, so a restart reads it back the same.
    let last = rig.drafts().last().cloned().unwrap();
    assert_eq!(last.1, with);
}

#[core::prelude::v1::test]
fn a_second_composer_starts_from_a_draft_with_chips() {
    let mut rig = rig();
    let draft = "look at @file:src/main.rs please".to_string();
    let (model, key) = (rig.model.clone(), "t1".to_string());
    let window = rig.window;
    let made = rig
        .cx
        .update_window(window, |_, window, cx| {
            cx.new(|cx| ComposerView::new(model, key, Some(draft), window, cx))
        })
        .unwrap();
    let chips = rig.read(|cx| made.read(cx).input().read(cx).tokens().len());
    assert_eq!(chips, 1);
}

#[core::prelude::v1::test]
fn a_chip_with_a_space_in_the_path_is_quoted_so_the_runtime_reads_it_whole() {
    let mut rig = rig();
    rig.backend.set_paths(vec![Suggestion::from_item(
        "@file:my docs/a b.txt",
        "a b.txt",
        "my docs",
    )]);
    rig.type_text("@a");
    rig.press("enter");
    assert_eq!(rig.text(), "@file:`my docs/a b.txt` ");
    assert_eq!(rig.chips().len(), 1);
}

// --- the `/` menu --------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn a_slash_lists_the_catalog_with_the_reports_and_the_apps_commands() {
    let mut rig = rig();
    rig.type_text("/");
    let names = rig.entries();
    for expected in [
        "plan", "review", "init", "deploy-checklist", "usage", "diff", "status", "new", "model",
        "settings",
    ] {
        assert!(names.iter().any(|n| n == expected), "{expected} in {names:?}");
    }
    rig.type_text("us");
    assert_eq!(rig.entries(), ["usage", "status"]);
    // A skill carries a badge.
    rig.set_text("");
    rig.type_text("/deploy");
    assert_eq!(rig.entries(), ["deploy-checklist"]);
    assert!(rig.present("suggest-badge:0"));
}

#[core::prelude::v1::test]
fn a_command_that_answers_locally_shows_a_row_and_sends_nothing_to_the_model() {
    let mut rig = rig();
    let before_rows = rig.user_images();
    rig.type_text("/usa");
    assert_eq!(rig.entries(), ["usage"]);
    rig.press("enter");
    // Run, not sent.
    assert_eq!(rig.commands_run(), [("usage".to_string(), String::new())]);
    assert!(rig.sends().is_empty(), "a report is no prompt");
    assert_eq!(
        rig.command_rows(),
        [("/usage".to_string(), "Output of /usage".to_string())]
    );
    assert_eq!(rig.text(), "", "the input is cleared");
    assert_eq!(rig.drafts().last().map(|d| d.1.clone()), Some(String::new()));
    // It is on the screen only: no user row was added, nothing is expected from the core.
    assert_eq!(rig.user_images(), before_rows);
    assert_eq!(rig.expected(), 0);
}

#[core::prelude::v1::test]
fn a_command_with_arguments_is_completed_shows_its_hint_and_runs_with_them() {
    let mut rig = rig();
    rig.type_text("/di");
    rig.press("enter");
    // Completed, not run: it takes arguments.
    assert_eq!(rig.text(), "/diff ");
    assert!(rig.commands_run().is_empty());
    assert!(rig.present("suggest-arguments"));
    let usage = rig.view_read(|v, cx| v.argument_usage(cx));
    assert_eq!(usage.as_deref(), Some("/diff [git diff arguments]"));
    rig.type_text("HEAD~1");
    rig.press("enter");
    assert_eq!(
        rig.commands_run(),
        [("diff".to_string(), "HEAD~1".to_string())]
    );
    assert_eq!(
        rig.command_rows(),
        [("/diff HEAD~1".to_string(), "Output of /diff HEAD~1".to_string())]
    );
}

#[core::prelude::v1::test]
fn tab_completes_a_command_without_running_it() {
    let mut rig = rig();
    rig.type_text("/us");
    rig.press("tab");
    assert_eq!(rig.text(), "/usage ");
    assert!(rig.commands_run().is_empty());
    // Typing it out and pressing Enter runs it.
    rig.press("enter");
    assert_eq!(rig.commands_run().len(), 1);
}

#[core::prelude::v1::test]
fn a_skill_is_sent_as_a_prompt_and_shown_as_what_was_typed() {
    let mut rig = rig();
    rig.backend.set_answer(
        "deploy-checklist",
        CommandAnswer::Skill {
            message: "LOADED SKILL TEXT".into(),
            display: "/deploy-checklist for v2".into(),
        },
    );
    rig.type_text("/deploy-checklist for v2");
    rig.press("enter");
    let sent = rig
        .ops()
        .into_iter()
        .find_map(|op| match op {
            ComposerOp::Send { request, .. } => Some(request),
            _ => None,
        })
        .expect("the skill is sent");
    assert_eq!(sent.text, "LOADED SKILL TEXT");
    assert_eq!(sent.display.as_deref(), Some("/deploy-checklist for v2"));
    // The row shows what was typed when its turn starts, not the loaded text.
    let id = rig.admission_of_last_send();
    rig.emit(started(&id));
    assert_eq!(rig.last_user_row().as_deref(), Some("/deploy-checklist for v2"));
}

#[core::prelude::v1::test]
fn the_apps_commands_go_to_the_frame_and_the_model_picker_opens() {
    let mut rig = rig();
    rig.type_text("/new");
    rig.press("enter");
    assert_eq!(rig.app_ids(), ["chat.new"]);
    assert_eq!(rig.text(), "");
    rig.type_text("/settings");
    rig.press("enter");
    rig.type_text("/terminal");
    rig.press("enter");
    assert_eq!(rig.app_ids(), ["chat.new", "settings.open", "terminal.toggle"]);
    rig.type_text("/model");
    rig.press("enter");
    assert_eq!(rig.open_menu(), Some(Menu::Model));
    assert!(rig.sends().is_empty());
}

#[core::prelude::v1::test]
fn a_slash_that_is_no_command_is_an_ordinary_prompt() {
    let mut rig = rig();
    // A path.
    rig.type_text("/home/me/file is broken");
    rig.press("enter");
    assert_eq!(rig.sends(), [("/home/me/file is broken".to_string(), false)]);
    assert!(rig.commands_run().is_empty());
    // A name the catalog does not list (the catalog was read when the menu opened).
    rig.type_text("/zzz thing");
    rig.press("enter");
    assert_eq!(rig.sends().last().unwrap().0, "/zzz thing");
    assert!(rig.commands_run().is_empty());
}

#[core::prelude::v1::test]
fn a_command_the_core_refuses_keeps_the_text_and_says_why() {
    let mut rig = rig();
    rig.type_text("/usage");
    rig.backend.fail_with(Some("session_id is required"));
    rig.press("tab");
    rig.press("enter");
    assert!(rig
        .notices()
        .iter()
        .any(|(kind, title)| *kind == Kind2::Error && title == "/usage did not run"));
    assert_eq!(rig.text(), "/usage ");
    assert!(rig.command_rows().is_empty());
}

type Kind2 = crate::design::toast::Kind;

#[core::prelude::v1::test]
fn the_new_thread_screen_runs_a_command_with_no_session() {
    let mut rig = draft_rig();
    rig.type_text("/usage");
    rig.press("tab");
    rig.press("enter");
    assert_eq!(
        rig.ops().last(),
        Some(&ComposerOp::Command {
            session: None,
            name: "usage".into(),
            argument: String::new()
        })
    );
    // No thread to show it in: a notice.
    assert!(rig.notices().is_empty() || rig.notices().iter().all(|(k, _)| *k == Kind2::Info));
}

#[core::prelude::v1::test]
fn the_placeholder_names_the_two_menus() {
    assert_eq!(view::PLACEHOLDER, "Ask anything. @ files, / commands");
}

#[core::prelude::v1::test]
fn the_command_list_follows_the_core_catalog() {
    let mut rig = rig();
    rig.backend.set_catalog(vec![CommandSpec::from_core(
        "ship-it",
        "Ship",
        CommandOrigin::Skill,
    )]);
    rig.type_text("/ship");
    assert_eq!(rig.entries(), ["ship-it"]);
}

// --- attachments -----------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn a_picked_image_shows_a_tile_with_a_thumbnail_and_can_be_removed() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = rig();
    let path = png_file(&dir, "shot.png", 400, 300);
    rig.add(vec![path_source(&path)]);
    let attached = rig.attachments();
    assert_eq!(attached.len(), 1);
    assert_eq!(attached[0].name, "shot.png");
    assert_eq!(attached[0].kind, Kind::Image);
    let thumb = attached[0].thumb.clone().expect("a thumbnail");
    assert!(attach::png_size(&thumb).0 <= attach::THUMB_EDGE);
    let id = attached[0].id;
    let layout = rig.layout();
    let tile = layout.bounds(&format!("attach-tile:{id}"));
    assert_eq!((tile.size.width.as_f32(), tile.size.height.as_f32()), (64., 64.));
    let mut check = layout.check();
    check
        .inside("composer-attachments", "composer")
        .inside(&format!("attach-tile:{id}"), "composer-attachments")
        .no_overflow("composer-attachments");
    check.assert();
    // The input is under the strip.
    assert!(layout.bounds("composer-input").top() >= layout.bounds("composer-attachments").bottom());
    // The remove button takes it off again.
    rig.click(&format!("attach-remove:{id}"));
    assert!(rig.attachments().is_empty());
    assert!(!rig.present("composer-attachments"));
}

#[core::prelude::v1::test]
fn another_file_is_a_row_with_its_name_and_size() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = rig();
    let path = text_file(&dir, "notes.txt", &"x".repeat(1536));
    rig.add(vec![path_source(&path)]);
    let attached = rig.attachments();
    assert_eq!(attached[0].kind, Kind::File);
    let id = attached[0].id;
    let layout = rig.layout();
    assert_eq!(attached[0].name, "notes.txt");
    assert_eq!(attached[0].size, 1536);
    let mut check = layout.check();
    check
        .inside(&format!("attach-file:{id}"), "composer-attachments")
        .same_row(&format!("attach-name:{id}"), &format!("attach-size:{id}"))
        .same_row(&format!("attach-size:{id}"), &format!("attach-remove:{id}"));
    check.assert();
}

#[core::prelude::v1::test]
fn the_same_file_twice_is_attached_once_with_a_notice() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = rig();
    let path = png_file(&dir, "a.png", 20, 20);
    rig.add(vec![path_source(&path), path_source(&path)]);
    assert_eq!(rig.attachments().len(), 1);
    assert!(rig
        .notices()
        .iter()
        .any(|(k, t)| *k == Kind2::Info && t == "Already attached"));
    // The same bytes pasted twice are the same image too.
    let bytes = Arc::new(attach::test_png(10, 10));
    let source = AttachmentSource::Bytes {
        data: bytes,
        name: "pasted-image.png".into(),
    };
    rig.add(vec![source.clone()]);
    rig.add(vec![source]);
    assert_eq!(rig.attachments().len(), 2);
}

#[core::prelude::v1::test]
fn what_the_core_refuses_is_a_toast_with_its_sentence_and_nothing_is_attached() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = rig();
    // Over the core's 16 MiB (a sparse file: nothing like that is written).
    let huge = dir.path().join("huge.bin");
    let file = std::fs::File::create(&huge).unwrap();
    file.set_len(hermes_core::attachments::MAX_BYTES as u64 + 1).unwrap();
    drop(file);
    let folder = dir.path().join("sub");
    std::fs::create_dir(&folder).unwrap();
    let missing = dir.path().join("gone.txt");
    let broken = text_file(&dir, "bad.png", "not an image");
    rig.add(vec![
        path_source(&huge),
        path_source(&folder),
        path_source(&missing),
        path_source(&broken),
    ]);
    assert!(rig.attachments().is_empty());
    let problems: Vec<String> = rig
        .view_events()
        .into_iter()
        .filter_map(|e| match e {
            ComposerViewEvent::Notice(spec) if spec.kind == Kind2::Error => spec.description,
            _ => None,
        })
        .collect();
    assert_eq!(problems.len(), 4, "{problems:?}");
    assert_eq!(problems[0], "huge.bin: The file is larger than 16 MiB.");
    assert!(problems[1].contains("is a folder"), "{}", problems[1]);
    assert_eq!(problems[2], "gone.txt: No such file.");
    assert!(problems[3].starts_with("bad.png: "), "{}", problems[3]);
}

#[core::prelude::v1::test]
fn a_message_takes_at_most_the_limit_and_says_so() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = rig();
    let sources: Vec<_> = (0..MAX_ATTACHMENTS + 3)
        .map(|n| path_source(&text_file(&dir, &format!("f{n}.txt"), &format!("{n}"))))
        .collect();
    rig.add(sources);
    assert_eq!(rig.attachments().len(), MAX_ATTACHMENTS);
    assert!(rig.notices().iter().any(|(_, t)| t == "Too many attachments"));
    // One more after the limit is refused at once, without reading it.
    let reads = rig.prepares();
    let extra = text_file(&dir, "extra.txt", "x");
    rig.add(vec![path_source(&extra)]);
    assert_eq!(rig.attachments().len(), MAX_ATTACHMENTS);
    assert_eq!(rig.prepares(), reads);
}

#[core::prelude::v1::test]
fn a_pasted_image_is_attached_and_a_paste_with_text_is_text() {
    let mut rig = rig();
    let image = Image::from_bytes(ImageFormat::Png, attach::test_png(30, 20));
    rig.cx
        .update(|cx| cx.write_to_clipboard(ClipboardItem::new_image(&image)));
    rig.press("ctrl-v");
    let attached = rig.attachments();
    assert_eq!(attached.len(), 1);
    assert_eq!(attached[0].name, "pasted-image.png");
    assert!(matches!(attached[0].source, attach::Source::Bytes { .. }));
    assert_eq!(rig.text(), "", "nothing was typed into the input");
    // Text on the clipboard is pasted as text.
    rig.cx
        .update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("hello".into())));
    rig.press("ctrl-v");
    assert_eq!(rig.text(), "hello");
    assert_eq!(rig.attachments().len(), 1);
}

#[core::prelude::v1::test]
fn pasted_bytes_that_are_no_image_are_refused() {
    let mut rig = rig();
    rig.add(vec![AttachmentSource::Bytes {
        data: Arc::new(b"definitely not an image".to_vec()),
        name: "pasted-image.png".into(),
    }]);
    assert!(rig.attachments().is_empty());
    assert!(rig.notices().iter().any(|(k, t)| *k == Kind2::Error && t == "Could not attach"));
}

#[core::prelude::v1::test]
fn files_dropped_on_the_card_are_attached() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = rig();
    let (a, b) = (png_file(&dir, "a.png", 8, 8), text_file(&dir, "b.txt", "b"));
    let paths = vec![a, b];
    let at = rig.layout().bounds("composer").center();
    let window = rig.window;
    // The platform's file drag: it enters the window, moves, and is dropped.
    rig.cx
        .update_window(window, |_, window, cx| {
            use gpui_kit::{ExternalPaths, FileDropEvent, PlatformInput};
            window.dispatch_event(
                PlatformInput::FileDrop(FileDropEvent::Entered {
                    position: at,
                    paths: ExternalPaths(paths.iter().cloned().collect()),
                }),
                cx,
            );
            window.dispatch_event(PlatformInput::FileDrop(FileDropEvent::Pending { position: at }), cx);
            window.dispatch_event(PlatformInput::FileDrop(FileDropEvent::Submit { position: at }), cx);
        })
        .unwrap();
    rig.settle();
    let names: Vec<String> = rig.attachments().into_iter().map(|a| a.name).collect();
    assert_eq!(names, ["a.png", "b.txt"]);
}

#[core::prelude::v1::test]
fn the_paperclip_asks_the_picker_and_attaches_what_it_returns() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = rig();
    let chosen = vec![png_file(&dir, "one.png", 8, 8), text_file(&dir, "two.md", "# two")];
    let answer = chosen.clone();
    let view = rig.view.clone();
    rig.cx.update(|cx| {
        view.update(cx, |view, _| {
            view.set_file_picker(std::rc::Rc::new(move |cx: &mut gpui_kit::App| {
                let answer = answer.clone();
                cx.spawn(async move |_| Some(answer))
            }))
        })
    });
    rig.click("attach-button");
    rig.settle();
    let names: Vec<String> = rig.attachments().into_iter().map(|a| a.name).collect();
    assert_eq!(names, ["one.png", "two.md"]);
    // A cancelled dialog changes nothing.
    let view = rig.view.clone();
    rig.cx.update(|cx| {
        view.update(cx, |view, _| {
            view.set_file_picker(std::rc::Rc::new(|cx: &mut gpui_kit::App| {
                cx.spawn(async move |_| None)
            }))
        })
    });
    rig.click("attach-button");
    assert_eq!(rig.attachments().len(), 2);
}

#[core::prelude::v1::test]
fn a_send_carries_the_attachments_and_the_thread_shows_the_image() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = rig();
    let image = png_file(&dir, "shot.png", 64, 48);
    let file = text_file(&dir, "log.txt", "line");
    rig.add(vec![path_source(&image), path_source(&file)]);
    rig.type_text("what is wrong here");
    rig.press("enter");
    let request = rig
        .ops()
        .into_iter()
        .find_map(|op| match op {
            ComposerOp::Send { request, .. } => Some(request),
            _ => None,
        })
        .expect("a send");
    assert_eq!(request.text, "what is wrong here");
    let names: Vec<_> = request.attachments.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["shot.png", "log.txt"]);
    // The strip is emptied once the core took the prompt.
    assert!(rig.attachments().is_empty());
    assert!(!rig.present("composer-attachments"));
    // Its turn starts: the user's row has the image as a thumbnail (the file has none).
    let id = rig.admission_of_last_send();
    let before_rows = rig.user_images().len();
    rig.emit(started(&id));
    let rows = rig.user_images();
    assert_eq!(rows.len(), before_rows + 1);
    assert_eq!(rows.last(), Some(&1));
}

#[core::prelude::v1::test]
fn attachments_alone_can_be_sent() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = rig();
    rig.add(vec![path_source(&png_file(&dir, "only.png", 8, 8))]);
    assert_eq!(rig.text(), "");
    rig.press("enter");
    let sent: Vec<_> = rig
        .ops()
        .into_iter()
        .filter(|op| matches!(op, ComposerOp::Send { .. }))
        .collect();
    assert_eq!(sent.len(), 1);
}

#[core::prelude::v1::test]
fn a_refused_send_keeps_the_text_and_the_attachments() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = rig();
    rig.add(vec![path_source(&png_file(&dir, "keep.png", 8, 8))]);
    rig.type_text("hello");
    rig.backend.fail_with(Some("Nothing is changed"));
    rig.press("enter");
    assert_eq!(rig.text(), "hello");
    assert_eq!(rig.attachments().len(), 1);
    assert!(rig.present("composer-attachments"));
}

#[core::prelude::v1::test]
fn a_command_with_attachments_is_an_ordinary_prompt() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = rig();
    rig.add(vec![path_source(&png_file(&dir, "p.png", 8, 8))]);
    rig.type_text("/plan the thing");
    rig.press("enter");
    assert!(rig.commands_run().is_empty());
    assert_eq!(rig.sends(), [("/plan the thing".to_string(), false)]);
}

#[core::prelude::v1::test]
fn the_first_send_of_a_new_thread_carries_the_attachments_and_a_failure_gives_them_back() {
    let dir = hermes_testdir::tempdir().unwrap();
    let mut rig = draft_rig();
    rig.add(vec![path_source(&png_file(&dir, "first.png", 8, 8))]);
    rig.set_text("go");
    let created: Rc<RefCell<Vec<ComposerEvent>>> = Rc::default();
    let seen = created.clone();
    let sub = rig
        .cx
        .update(|cx| cx.subscribe(&rig.model.clone(), move |_, e: &ComposerEvent, _| {
            seen.borrow_mut().push(e.clone())
        }));
    rig.press("enter");
    let request = created
        .borrow()
        .iter()
        .find_map(|e| match e {
            ComposerEvent::Created { request, .. } => Some(request.clone()),
            _ => None,
        })
        .expect("Created");
    assert_eq!(request.attachments.len(), 1);
    drop(sub);
    // The new thread's composer (a model of its own) sends it and the core refuses: the
    // attachments are back in that composer, with the text restored.
    let backend = rig.backend.clone();
    let thread_model = {
        let backend = backend.clone();
        rig.cx.update(|cx| cx.new(|cx| ThreadModel::new("t1", backend, cx)))
    };
    let new_model = {
        let (backend, thread) = (backend.clone(), thread_model.clone());
        rig.cx
            .update(|cx| cx.new(|cx| ComposerModel::new(Target::Thread("t1".into()), backend, Some(thread), cx)))
    };
    rig.backend.fail_with(Some("no provider"));
    let events: Rc<RefCell<Vec<ComposerEvent>>> = Rc::default();
    let seen = events.clone();
    let _sub = rig
        .cx
        .update(|cx| cx.subscribe(&new_model, move |_, e: &ComposerEvent, _| seen.borrow_mut().push(e.clone())));
    rig.cx.update(|cx| {
        new_model.update(cx, |m, cx| m.send_first(request, Picks::default(), cx))
    });
    rig.cx.run_until_parked();
    let restored = rig.cx.update(|cx| new_model.read(cx).attachments().len());
    assert_eq!(restored, 1);
    assert!(events
        .borrow()
        .iter()
        .any(|e| matches!(e, ComposerEvent::Failed { .. })));
}

// --- layout ----------------------------------------------------------------------------------------

#[core::prelude::v1::test]
fn the_drawer_sits_on_the_card_and_inside_the_window_at_three_widths() {
    for w in [640., 1100., 1920.] {
        let mut rig = rig_with(
            Target::Thread("t1".into()),
            (w, 600.),
            |b: &FixtureBackend| {
                // A long list: it must scroll, not grow past the window.
                b.set_paths(
                    (0..40)
                        .map(|n| {
                            Suggestion::from_item(
                                &format!("@file:src/some/deeply/nested/folder/file_{n:02}.rs"),
                                &format!("file_{n:02}.rs"),
                                "src/some/deeply/nested/folder",
                            )
                        })
                        .collect(),
                )
            },
        );
        rig.type_text("@file");
        let layout = rig.layout();
        let card = layout.bounds("composer");
        let drawer = layout.bounds("composer-suggest");
        assert!(
            (drawer.bottom().as_f32() - (card.top().as_f32() - 6.)).abs() < 1.5,
            "{w}: the drawer ends just above the card ({} / {})",
            drawer.bottom().as_f32(),
            card.top().as_f32()
        );
        assert!(drawer.left().as_f32() >= 0. && drawer.right().as_f32() <= w, "{w}: inside the window");
        assert!(drawer.top().as_f32() >= 0., "{w}: not past the top");
        assert!(drawer.size.height.as_f32() <= suggest::MAX_HEIGHT + 2., "{w}: at most 288");
        // Inside the card's width, with the inset at both sides.
        assert!((drawer.left().as_f32() - card.left().as_f32() - suggest::INSET).abs() < 1.5);
        assert!((card.right().as_f32() - drawer.right().as_f32() - suggest::INSET).abs() < 1.5);
        let mut check = layout.check();
        check
            .inside("composer-suggest", "shell")
            .inside("suggest-list", "composer-suggest")
            .same_row("suggest-label:0", "suggest-description:0");
        check.assert();
        assert!(
            layout.bounds("suggest-label:0").right() <= layout.bounds("suggest-description:0").left(),
            "{w}: the name is not under its description"
        );
    }
}

#[core::prelude::v1::test]
fn the_highlighted_row_is_scrolled_into_view() {
    let mut rig = rig_with(
        Target::Thread("t1".into()),
        (800., 600.),
        |b: &FixtureBackend| {
            b.set_paths(
                (0..40)
                    .map(|n| Suggestion::from_item(&format!("@file:f{n:02}.rs"), &format!("f{n:02}.rs"), ""))
                    .collect(),
            )
        },
    );
    rig.type_text("@f");
    for _ in 0..30 {
        rig.press("down");
    }
    assert_eq!(rig.highlighted(), 30);
    let layout = rig.layout();
    let list = layout.bounds("suggest-list");
    let row = layout.bounds("suggest-item:30");
    assert!(
        row.top() >= list.top() && row.bottom() <= list.bottom(),
        "row 30 ({}..{}) inside the list ({}..{})",
        row.top().as_f32(),
        row.bottom().as_f32(),
        list.top().as_f32(),
        list.bottom().as_f32()
    );
}

#[core::prelude::v1::test]
fn a_chip_sits_in_the_line_with_the_words_around_it() {
    let mut rig = rig();
    rig.set_text("see @file:src/main.rs now");
    rig.focus_input();
    let layout = rig.layout();
    let chip = layout.bounds("mention-chip:4");
    let input = layout.bounds("composer-input");
    assert!(chip.top() >= input.top() && chip.bottom() <= input.bottom(), "inside the input");
    // The words before and after it are on its row: the chip is one line high.
    let (before, after) = rig.view_read(|v, cx| {
        let input = v.input().read(cx);
        (
            input.range_to_bounds(&(0..4)).expect("bounds of 'see '"),
            input.range_to_bounds(&(21..25)).expect("bounds of ' now'"),
        )
    });
    let line = rig.view_read(|v, cx| v.input().read(cx).line_height()).expect("a line height");
    assert!((chip.size.height.as_f32() - line.as_f32()).abs() < 1., "one line high");
    for text in [before, after] {
        assert!(
            (text.center().y.as_f32() - chip.center().y.as_f32()).abs() < 2.,
            "text and chip share a row ({} / {})",
            text.center().y.as_f32(),
            chip.center().y.as_f32()
        );
    }
    assert!(chip.right() <= after.left() + gpui_kit::px(1.));
    assert!(chip.left() >= before.right() - gpui_kit::px(1.));
}

#[core::prelude::v1::test]
fn a_long_reference_is_cut_inside_a_narrow_card() {
    let mut rig = rig_with(Target::Thread("t1".into()), (384., 600.), |_: &FixtureBackend| {});
    let long = format!("@file:{}/very_long_file_name.rs", "nested_folder/".repeat(12));
    rig.set_text(&long);
    rig.focus_input();
    let layout = rig.layout();
    let chip = layout.bounds("mention-chip:0");
    let input = layout.bounds("composer-input");
    assert!(chip.right() <= input.right() + gpui_kit::px(1.), "the chip stays in the input");
}

#[core::prelude::v1::test]
fn the_strip_wraps_and_scrolls_with_many_files_at_three_widths() {
    for w in [640., 1100., 1920.] {
        let dir = hermes_testdir::tempdir().unwrap();
        let mut rig = rig_with(Target::Thread("t1".into()), (w, 700.), |_: &FixtureBackend| {});
        let mut sources = Vec::new();
        for n in 0..14 {
            sources.push(path_source(&png_file(&dir, &format!("image-{n}.png"), 16 + n, 16)));
        }
        for n in 0..5 {
            sources.push(path_source(&text_file(
                &dir,
                &format!("a-rather-long-file-name-number-{n}-{}.txt", "x".repeat(60)),
                "x",
            )));
        }
        rig.add(sources);
        assert_eq!(rig.attachments().len(), 19);
        let layout = rig.layout();
        let strip = layout.bounds("composer-attachments");
        assert!(strip.size.height.as_f32() <= strip::MAX_HEIGHT + 1., "{w}: capped ({})", strip.size.height.as_f32());
        // It scrolls: what does not fit is clipped, not pushed out of the card.
        let mut check = layout.check();
        check
            .inside("composer-attachments", "composer")
            .inside("composer", "shell");
        check.assert();
        // The tiles wrap: at 640 px fewer fit in a row than at 1920.
        let first = layout.bounds("attach-tile:1");
        let ninth = layout.bounds("attach-tile:9");
        if w >= 1100. {
            assert_eq!(first.top(), ninth.top(), "{w}: nine tiles fit one row");
        } else {
            assert!(ninth.top() >= first.top(), "{w}");
        }
        // The long names are cut inside their rows.
        let name = layout.bounds("attach-name:15");
        assert!(name.right() <= layout.bounds("composer").right());
    }
}

#[core::prelude::v1::test]
fn the_card_keeps_its_footer_on_one_row_with_the_paperclip() {
    for w in [352., 384., 640., 1100.] {
        let mut rig = rig_with(Target::Thread("t1".into()), (w, 600.), |b: &FixtureBackend| {
            b.set_controls(
                "t1",
                controls(|c| {
                    c.context = Some((18_000, 200_000));
                    c.model = "openrouter/some-vendor/a-very-long-model-name".into();
                }),
            )
        });
        let layout = rig.layout();
        let mut check = layout.check();
        check
            .inside("attach-button", "composer-footer")
            .inside("composer-send", "composer")
            .inside("model-chip", "composer")
            .same_row("attach-button", "mode-chip")
            .same_row("mode-chip", "composer-send");
        check.assert();
        assert!(layout.bounds("attach-button").right() <= layout.bounds("mode-chip").left() + gpui_kit::px(1.));
        // The send button stays inside the card's padding.
        assert!(
            layout.bounds("composer-send").right() <= layout.bounds("composer").right() - gpui_kit::px(8.),
            "{w}: the send button is inside the card"
        );
        // Each chip's name is shown with room for a few letters, or not at all (it fell under
        // the chip's 28 px, which clip it): never a lone `o...`.
        for chip in ["model-chip", "mode-chip"] {
            let (outer, label) = (layout.bounds(chip), layout.bounds(&format!("{chip}/label")));
            let shown = label.top() < outer.bottom();
            assert!(
                !shown || label.size.width.as_f32() >= crate::thread::composer::view::LABEL_MIN - 1.,
                "{w}: {chip}'s label is {} px",
                label.size.width.as_f32()
            );
            if shown {
                assert!((label.center().y - outer.center().y).abs() < gpui_kit::px(1.5), "{w}: {chip} centred");
            }
        }
        if w >= 1100. {
            assert!(layout.bounds("model-chip/label").top() < layout.bounds("model-chip").bottom(), "{w}: the name shows");
        }
    }
}

#[core::prelude::v1::test]
fn a_footer_too_narrow_for_the_models_name_shows_its_icon_and_keeps_the_modes_name() {
    // A 640 px window with the right panel leaves a card about 300 px wide.
    let mut rig = rig_with(Target::Thread("t1".into()), (330., 600.), |b: &FixtureBackend| {
        b.set_controls(
            "t1",
            controls(|c| c.model = "openrouter/some-vendor/a-very-long-model-name".into()),
        )
    });
    let layout = rig.layout();
    let mut check = layout.check();
    check.no_overflow("mode-chip/label").inside("composer-send", "composer");
    check.assert();
}

use std::cell::RefCell;
use std::rc::Rc;


#[core::prelude::v1::test]
fn reading_the_trigger_in_a_one_megabyte_draft_is_cheap() {
    let mut rig = rig();
    let big = "0123456789abcdef ".repeat(64 * 1024); // 1.1 MB
    rig.set_text(&format!("{big}@ma"));
    let view = rig.view.clone();
    let started = std::time::Instant::now();
    rig.cx.update(|cx| {
        view.update(cx, |v, cx| {
            for _ in 0..100 {
                v.sync_trigger(cx)
            }
        })
    });
    let each = started.elapsed() / 100;
    eprintln!("sync_trigger on a 1.1 MB draft: {each:?} each");
    // Every caret move and every edit reads it (0.8 ms measured in a debug build): under a frame.
    assert!(each < std::time::Duration::from_millis(16), "{each:?}");
}
