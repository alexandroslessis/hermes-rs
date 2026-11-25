//! A fake repository for the right panel: what each scope answers, and what was asked of it (the
//! fixture backend's diff, for the gallery, the snapshots, `hermes-native --fixture` and the
//! tests). The patches are made here, small and varied: a Rust file with two hunks, a new Python
//! file, a deleted file, a rename, a binary file, a file with very long lines and one with no
//! newline at its end.

use super::data::{DiffData, DiffRequest, Revert, Scope, names_file};

/// A hunk of `old` lines turned into `new` lines at line `at` (1-based), with the lines around
/// them as context.
fn hunk(at: usize, context_before: &[&str], old: &[&str], new: &[&str], after: &[&str]) -> String {
    let (before, tail) = (context_before.len(), after.len());
    let mut out = format!(
        "@@ -{},{} +{},{} @@\n",
        at.saturating_sub(before).max(1),
        before + old.len() + tail,
        at.saturating_sub(before).max(1),
        before + new.len() + tail
    );
    for line in context_before {
        out.push_str(&format!(" {line}\n"));
    }
    for line in old {
        out.push_str(&format!("-{line}\n"));
    }
    for line in new {
        out.push_str(&format!("+{line}\n"));
    }
    for line in after {
        out.push_str(&format!(" {line}\n"));
    }
    out
}

fn modified(path: &str, hunks: &[String]) -> String {
    format!(
        "diff --git a/{path} b/{path}\nindex 3f2a1b0..9c8d7e6 100644\n--- a/{path}\n+++ b/{path}\n{}",
        hunks.concat()
    )
}

/// `crates/app/src/main.rs`: two hunks.
pub fn rust_file() -> String {
    modified(
        "src/main.rs",
        &[
            hunk(
                3,
                &["use std::io;", "", "fn main() {"],
                &["    println!(\"hello\");"],
                &[
                    "    let name = std::env::args().nth(1).unwrap_or_default();",
                    "    println!(\"hello, {name}\");",
                ],
                &["    run();", "}"],
            ),
            hunk(
                20,
                &["fn run() {"],
                &["    let _ = io::stdin();"],
                &[
                    "    let mut line = String::new();",
                    "    let _ = io::stdin().read_line(&mut line);",
                ],
                &["}", ""],
            ),
        ],
    )
}

/// A new Python file.
pub fn python_file() -> String {
    "diff --git a/tools/greet.py b/tools/greet.py\nnew file mode 100644\nindex 0000000..5d4c3b2\n--- /dev/null\n+++ b/tools/greet.py\n@@ -0,0 +1,8 @@\n+import sys\n+\n+\n+def greet(name: str) -> str:\n+    return f\"hello, {name}\"\n+\n+\n+print(greet(sys.argv[1]))\n".to_string()
}

/// A deleted file.
pub fn deleted_file() -> String {
    "diff --git a/notes/old.md b/notes/old.md\ndeleted file mode 100644\nindex 8a7b6c5..0000000\n--- a/notes/old.md\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-# Old notes\n-\n-Nothing here any more.\n".to_string()
}

/// A rename with a change.
pub fn renamed_file() -> String {
    "diff --git a/docs/intro.md b/docs/guide/intro.md\nsimilarity index 82%\nrename from docs/intro.md\nrename to docs/guide/intro.md\nindex 1a2b3c4..5d6e7f8 100644\n--- a/docs/intro.md\n+++ b/docs/guide/intro.md\n@@ -1,3 +1,3 @@\n # Introduction\n \n-Read this first.\n+Read this first, then the guide.\n".to_string()
}

/// A binary file.
pub fn binary_file() -> String {
    "diff --git a/assets/logo.png b/assets/logo.png\nindex 1111111..2222222 100644\nBinary files a/assets/logo.png and b/assets/logo.png differ\n".to_string()
}

/// A file whose lines are far wider than any panel, and which ends without a newline.
pub fn long_lines_file() -> String {
    let long: String = "word ".repeat(60);
    format!(
        "diff --git a/data/long.json b/data/long.json\nindex 4e4e4e4..5f5f5f5 100644\n--- a/data/long.json\n+++ b/data/long.json\n@@ -1,3 +1,3 @@\n {{\n-  \"text\": \"{long}old\",\n+  \"text\": \"{long}new\",\n   \"end\": true\n\\ No newline at end of file\n"
    )
}

/// A config file edit.
pub fn toml_file() -> String {
    modified(
        "Cargo.toml",
        &[hunk(
            8,
            &["[dependencies]", "serde = \"1\""],
            &["tokio = \"1\""],
            &["tokio = { version = \"1\", features = [\"rt\"] }"],
            &["anyhow = \"1\""],
        )],
    )
}

/// A patch of `files` files with `lines` added lines each: for the size checks.
pub fn big_patch(files: usize, lines: usize) -> String {
    let mut out = String::new();
    for f in 0..files {
        let path = format!("generated/part{f:03}.rs");
        out.push_str(&format!(
            "diff --git a/{path} b/{path}\nnew file mode 100644\nindex 0000000..abcdef0\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{lines} @@\n"
        ));
        for n in 0..lines {
            out.push_str(&format!(
                "+pub fn item_{f}_{n}(value: u32) -> u32 {{ value.wrapping_mul({n}) + {f} }}\n"
            ));
        }
    }
    out
}

/// Everything the working tree has.
pub fn working_tree() -> String {
    [
        toml_file(),
        deleted_file(),
        renamed_file(),
        binary_file(),
        long_lines_file(),
        rust_file(),
        python_file(),
    ]
    .concat()
}

/// The fixture repository's state and log.
#[derive(Debug, Default)]
pub struct FixtureDiffs {
    /// The patch each scope answers (the working tree's is also what a narrowed turn starts from).
    pub working: String,
    pub turn: String,
    pub thread: String,
    /// The commit `HEAD` is at (`None`: not a repository, or a fixture whose turns have no known
    /// base: "this turn" then narrows the working tree to the turn's own files).
    pub head: Option<String>,
    /// Every diff asked for.
    pub reads: Vec<DiffRequest>,
    /// Every revert that went through: `(scope, path)`.
    pub reverted: Vec<(Scope, String)>,
    /// The blob id each of those reverts was asked with (what the panel showed).
    pub reverted_seen: Vec<Option<String>>,
    /// Every open in the editor: `(session, path, line)`.
    pub opened: Vec<(String, String, Option<u32>)>,
    /// The next read (or revert) fails with this text.
    pub failure: Option<String>,
    pub revert_failure: Option<String>,
}

impl FixtureDiffs {
    /// The sample repository: a working tree with every kind of change, a turn that touched two
    /// files, a thread that touched three.
    pub fn sample() -> Self {
        Self {
            working: working_tree(),
            turn: [rust_file(), python_file()].concat(),
            thread: [rust_file(), python_file(), toml_file()].concat(),
            ..Self::default()
        }
    }

    pub fn read(&mut self, request: &DiffRequest) -> Result<DiffData, String> {
        self.reads.push(request.clone());
        if let Some(message) = self.failure.take() {
            return Err(message);
        }
        let (patch, revert, narrowed, note) = match request.scope {
            Scope::WorkingTree => (self.working.clone(), Revert::ToHead, false, None),
            Scope::Thread => (self.thread.clone(), Revert::ToThreadStart, false, None),
            Scope::Turn if request.turn.base.is_some() => {
                (self.turn.clone(), Revert::None, false, None)
            }
            Scope::Turn if request.turn.files.is_empty() => (
                String::new(),
                Revert::None,
                true,
                Some("No edits of the latest turn are known.".to_string()),
            ),
            Scope::Turn => (
                self.working.clone(),
                Revert::None,
                true,
                Some(
                    "Showing the working tree's changes to the files the latest turn edited."
                        .to_string(),
                ),
            ),
        };
        Ok(DiffData {
            patch,
            note,
            skipped: 0,
            revert,
            narrowed,
        })
    }

    /// Takes the file's section out of the scope's patch, as a real revert would make it vanish.
    pub fn revert(
        &mut self,
        request: &DiffRequest,
        path: &str,
        seen: Option<&str>,
    ) -> Result<(), String> {
        if let Some(message) = self.revert_failure.take() {
            return Err(message);
        }
        let patch = match request.scope {
            Scope::WorkingTree => &mut self.working,
            Scope::Thread => &mut self.thread,
            Scope::Turn => return Err("A file cannot be reverted from the turn's diff.".into()),
        };
        let mut kept = String::new();
        let mut skipping = false;
        for line in patch.split_inclusive('\n') {
            if let Some(head) = line.strip_prefix("diff --git ") {
                let name = head.rsplit_once(" b/").map_or(head, |(_, b)| b).trim();
                skipping = names_file(path, name) && names_file(name, path);
            }
            if !skipping {
                kept.push_str(line);
            }
        }
        *patch = kept;
        self.reverted.push((request.scope, path.to_string()));
        self.reverted_seen.push(seen.map(str::to_string));
        Ok(())
    }
}
