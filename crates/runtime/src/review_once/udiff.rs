//! Strict unified-diff application for the evidence review's skill patches.
//!
//! The review model answers a skill `patch` with a unified diff against the SKILL.md it was
//! shown. Applying it must never guess: every hunk's context and removed lines must match the
//! file **exactly** (no fuzz, no whitespace leniency beyond a `\r` at a line end), and the line
//! counts in each `@@ -a,b +c,d @@` header must agree with the hunk body. Like `git apply` (and
//! unlike `patch --fuzz`), a hunk whose exact lines sit at a different line than its header says
//! is still applied, but only when those lines occur exactly once after the previous hunk: line
//! numbers from a model are often off by a few, while the text it quotes is what it saw. A hunk
//! that only adds lines (no context) is refused unless the file is empty, since nothing but its
//! line number would place it. `---`/`+++` headers are optional; when present they must name one
//! `SKILL.md` (no `..`), never `/dev/null` as the new file (a deletion) and `/dev/null` as the old
//! one only for an empty file. Anything else rejects the whole diff with a reason; a half-applied
//! diff never reaches the disk.
//!
//! Not from Hermes (its review edits skills with `skill_manage` find-and-replace calls).

/// One `@@` hunk.
#[derive(Debug)]
struct Hunk {
    old_start: usize,
    old_len: usize,
    /// The lines the file has there now (context and removed), in order.
    old: Vec<String>,
    /// The lines that replace them (context and added), in order.
    new: Vec<String>,
    /// `\ No newline at end of file` after the last old / new line.
    old_no_eol: bool,
    new_no_eol: bool,
}

/// Whether `text` is meant as a unified diff (it has a hunk header) rather than a whole file.
pub fn is_unified_diff(text: &str) -> bool {
    text.lines().any(|line| line.starts_with("@@ -"))
}

/// The file a `---`/`+++` header names, without git's `a/`/`b/` and a trailing timestamp.
fn header_path(line: &str) -> &str {
    let path = line[4..].split('\t').next().unwrap_or_default().trim();
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

/// Checks the `---`/`+++` headers (optional): one pair at most, naming SKILL.md (the only file
/// the review writes) and no other, never deleting it, and creating it only from nothing.
fn check_headers(headers: &[&str], original: &str) -> Result<(), String> {
    let old: Vec<&str> = headers
        .iter()
        .filter(|l| l.starts_with("--- "))
        .map(|l| header_path(l))
        .collect();
    let new: Vec<&str> = headers
        .iter()
        .filter(|l| l.starts_with("+++ "))
        .map(|l| header_path(l))
        .collect();
    if old.len() > 1 || new.len() > 1 {
        return Err("the diff names more than one file".into());
    }
    if new.first() == Some(&"/dev/null") {
        return Err("the diff deletes the file".into());
    }
    if old.first() == Some(&"/dev/null") && !original.is_empty() {
        return Err("the diff creates a file that already exists".into());
    }
    for path in old.iter().chain(&new).filter(|p| **p != "/dev/null") {
        let skill_md = path.rsplit(['/', '\\']).next() == Some("SKILL.md");
        let escapes = path.split(['/', '\\']).any(|part| part == "..") || path.contains(':');
        if !skill_md || escapes {
            return Err(format!(
                "the diff names '{}', not the skill's SKILL.md",
                path.chars().take(80).collect::<String>()
            ));
        }
    }
    Ok(())
}

/// `original` with `diff` applied, or why it doesn't apply cleanly.
pub fn apply(original: &str, diff: &str) -> Result<String, String> {
    let (headers, hunks) = parse(diff)?;
    if hunks.is_empty() {
        return Err("the diff has no hunks".into());
    }
    check_headers(&headers, original)?;
    let crlf = original.contains("\r\n");
    let ends_with_newline = original.ends_with('\n');
    let lines: Vec<&str> = split_lines(original);
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 8);
    let mut cursor = 0usize;
    let mut reaches_end = false;
    let mut last: Option<&Hunk> = None;
    for (i, hunk) in hunks.iter().enumerate() {
        let n = i + 1;
        let pos = locate(&lines, cursor, hunk).map_err(|why| format!("hunk {n} {why}"))?;
        out.extend(lines[cursor..pos].iter().map(|l| l.to_string()));
        out.extend(hunk.new.iter().cloned());
        cursor = pos + hunk.old.len();
        reaches_end = cursor == lines.len();
        last = Some(hunk);
    }
    out.extend(lines[cursor..].iter().map(|l| l.to_string()));
    let final_newline = match last {
        Some(hunk) if reaches_end => {
            if hunk.new_no_eol {
                false
            } else if hunk.old_no_eol || lines.is_empty() {
                true
            } else {
                ends_with_newline
            }
        }
        _ => ends_with_newline,
    };
    let eol = if crlf { "\r\n" } else { "\n" };
    let mut text = out.join(eol);
    if final_newline && !out.is_empty() {
        text.push_str(eol);
    }
    Ok(text)
}

/// The file's lines without their terminators (a final newline makes no extra empty line).
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// Where `hunk`'s old lines start: at its header's line when they are there, else at their one
/// exact occurrence at or after `cursor`.
fn locate(lines: &[&str], cursor: usize, hunk: &Hunk) -> Result<usize, String> {
    let matches_at = |pos: usize| {
        pos >= cursor
            && pos + hunk.old.len() <= lines.len()
            && lines[pos..pos + hunk.old.len()]
                .iter()
                .zip(&hunk.old)
                .all(|(have, want)| *have == want.as_str())
    };
    if hunk.old.is_empty() {
        // A pure insertion has no text to find: only its header would say where it goes, and a
        // model's line numbers are often off. Into a file that has lines, context is required.
        if !lines.is_empty() {
            return Err(
                "adds lines without any context lines, so where they go can't be checked".into(),
            );
        }
        if hunk.old_start >= cursor && hunk.old_start <= lines.len() {
            return Ok(hunk.old_start);
        }
        return Err(format!(
            "inserts after line {} which is not in the file",
            hunk.old_start
        ));
    }
    let stated = hunk.old_start.saturating_sub(1);
    if matches_at(stated) {
        return Ok(stated);
    }
    let found: Vec<usize> = (cursor..lines.len())
        .filter(|&pos| matches_at(pos))
        .take(2)
        .collect();
    match found.as_slice() {
        [pos] => Ok(*pos),
        [] => Err("does not match the file (its context or removed lines differ)".into()),
        _ => Err("matches the file in several places; add context lines".into()),
    }
}

/// The file header lines before the first hunk, and the hunks.
fn parse(diff: &str) -> Result<(Vec<&str>, Vec<Hunk>), String> {
    let header = regex::Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
        .map_err(|e| e.to_string())?;
    let raw: Vec<&str> = split_lines(diff);
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut i = 0usize;
    // File headers before the first hunk.
    while i < raw.len() && !raw[i].starts_with("@@") {
        let line = raw[i];
        let header_line = line.trim().is_empty()
            || [
                "diff ", "--- ", "+++ ", "index ", "new file", "old mode", "new mode",
            ]
            .iter()
            .any(|p| line.starts_with(p));
        if !header_line {
            return Err(format!("line {} comes before the first hunk", i + 1));
        }
        i += 1;
    }
    let headers = raw[..i].to_vec();
    while i < raw.len() {
        let line = raw[i];
        if line.trim().is_empty() && raw[i..].iter().all(|l| l.trim().is_empty()) {
            break;
        }
        let Some(caps) = header.captures(line) else {
            return Err(format!(
                "line {} is outside a hunk (a hunk header's line counts are wrong?)",
                i + 1
            ));
        };
        let number = |k: usize, default: usize| -> Result<usize, String> {
            caps.get(k).map_or(Ok(default), |m| {
                m.as_str()
                    .parse::<usize>()
                    .map_err(|_| format!("line {}: bad number in the hunk header", i + 1))
            })
        };
        let old_start = number(1, 0)?;
        let old_len = number(2, 1)?;
        let new_len = number(4, 1)?;
        if old_len > 0 && old_start == 0 {
            return Err(format!("line {}: a hunk can't start at line 0", i + 1));
        }
        i += 1;
        let mut hunk = Hunk {
            old_start,
            old_len,
            old: Vec::new(),
            new: Vec::new(),
            old_no_eol: false,
            new_no_eol: false,
        };
        let mut last_kind = ' ';
        while hunk.old.len() < old_len || hunk.new.len() < new_len {
            let Some(body) = raw.get(i) else {
                return Err(format!(
                    "hunk {} is shorter than its header says",
                    hunks.len() + 1
                ));
            };
            // A context line whose single space a model dropped is still the empty line.
            let (kind, text) = match body.chars().next() {
                None => (' ', ""),
                Some(c @ (' ' | '-' | '+')) => (c, &body[1..]),
                Some('\\') => {
                    set_no_eol(&mut hunk, last_kind);
                    i += 1;
                    continue;
                }
                Some(_) => {
                    return Err(format!(
                        "line {} in hunk {} starts with neither ' ', '-' nor '+'",
                        i + 1,
                        hunks.len() + 1
                    ));
                }
            };
            match kind {
                ' ' => {
                    hunk.old.push(text.to_string());
                    hunk.new.push(text.to_string());
                }
                '-' => hunk.old.push(text.to_string()),
                _ => hunk.new.push(text.to_string()),
            }
            last_kind = kind;
            i += 1;
        }
        if hunk.old.len() != old_len || hunk.new.len() != new_len {
            return Err(format!(
                "hunk {} has {} old and {} new lines, its header says {old_len} and {new_len}",
                hunks.len() + 1,
                hunk.old.len(),
                hunk.new.len()
            ));
        }
        if raw.get(i).is_some_and(|l| l.starts_with('\\')) {
            set_no_eol(&mut hunk, last_kind);
            i += 1;
        }
        if let Some(previous) = hunks.last()
            && hunk.old_start < previous.old_start + previous.old_len
            && !(hunk.old_len == 0 && previous.old_len == 0)
        {
            return Err(format!(
                "hunk {} overlaps or precedes the hunk before it",
                hunks.len() + 1
            ));
        }
        hunks.push(hunk);
    }
    Ok((headers, hunks))
}

fn set_no_eol(hunk: &mut Hunk, last_kind: char) {
    match last_kind {
        '-' => hunk.old_no_eol = true,
        '+' => hunk.new_no_eol = true,
        _ => {
            hunk.old_no_eol = true;
            hunk.new_no_eol = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "---\nname: deploy\n---\n# Deploy\n\n1. Build.\n2. Run `make deploy`.\n3. Check the logs.\n";

    #[test]
    fn a_clean_diff_applies() {
        let diff = "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -6,3 +6,4 @@\n 1. Build.\n-2. Run `make deploy`.\n+2. Run `pnpm deploy` (never npm).\n 3. Check the logs.\n+4. Tell the user.\n";
        let out = apply(FILE, diff).unwrap();
        assert_eq!(
            out,
            "---\nname: deploy\n---\n# Deploy\n\n1. Build.\n2. Run `pnpm deploy` (never npm).\n3. Check the logs.\n4. Tell the user.\n"
        );
    }

    #[test]
    fn a_wrong_line_number_with_exact_unique_text_applies() {
        let diff = "@@ -2,2 +2,2 @@\n 1. Build.\n-2. Run `make deploy`.\n+2. Run `make ship`.\n";
        let out = apply(FILE, diff).unwrap();
        assert!(out.contains("2. Run `make ship`.") && !out.contains("make deploy"));
    }

    #[test]
    fn context_that_differs_is_rejected_never_fuzzed() {
        // One context line differs by a word: patch --fuzz would apply this; we don't.
        let diff = "@@ -6,3 +6,3 @@\n 1. Build it.\n-2. Run `make deploy`.\n+2. Run `make ship`.\n 3. Check the logs.\n";
        let error = apply(FILE, diff).unwrap_err();
        assert!(error.starts_with("hunk 1 does not match"), "{error}");
        // Trailing whitespace counts too.
        let diff = "@@ -7 +7 @@\n-2. Run `make deploy`. \n+2. x\n";
        assert!(apply(FILE, diff).is_err());
    }

    #[test]
    fn ambiguous_text_is_rejected() {
        let file = "a\nx\nb\nx\nc\n";
        let diff = "@@ -9,1 +9,1 @@\n-x\n+y\n";
        let error = apply(file, diff).unwrap_err();
        assert!(error.contains("several places"), "{error}");
        // At its stated line it is unambiguous.
        assert_eq!(
            apply(file, "@@ -4,1 +4,1 @@\n-x\n+y\n").unwrap(),
            "a\nx\nb\ny\nc\n"
        );
    }

    #[test]
    fn wrong_counts_and_stray_lines_are_rejected() {
        // The header says 3 old lines, the body has 2.
        let diff = "@@ -6,3 +6,3 @@\n 1. Build.\n-2. Run `make deploy`.\n+2. x\n";
        assert!(apply(FILE, diff).is_err());
        // Prose before the hunk.
        let diff = "Here is the patch:\n@@ -7 +7 @@\n-2. Run `make deploy`.\n+2. x\n";
        assert!(
            apply(FILE, diff)
                .unwrap_err()
                .contains("before the first hunk")
        );
        // A line after the hunk is complete.
        let diff = "@@ -7 +7 @@\n-2. Run `make deploy`.\n+2. x\n+3. extra\n";
        assert!(apply(FILE, diff).unwrap_err().contains("outside a hunk"));
        // No hunk at all.
        assert!(apply(FILE, "--- a\n+++ b\n").is_err());
        // Hunks out of order.
        let diff =
            "@@ -8 +8 @@\n-3. Check the logs.\n+3. y\n@@ -7 +7 @@\n-2. Run `make deploy`.\n+2. x\n";
        assert!(apply(FILE, diff).unwrap_err().contains("precedes"));
    }

    #[test]
    fn a_dropped_space_on_an_empty_context_line_is_the_empty_line() {
        let diff = "@@ -4,3 +4,3 @@\n # Deploy\n\n-1. Build.\n+1. Build with --release.\n";
        let out = apply(FILE, diff).unwrap();
        assert!(out.contains("\n\n1. Build with --release.\n"));
    }

    #[test]
    fn line_endings_and_the_final_newline_are_kept() {
        let crlf = FILE.replace('\n', "\r\n");
        let diff = "@@ -8 +8 @@\n-3. Check the logs.\n+3. Read the logs.\n";
        let out = apply(&crlf, diff).unwrap();
        assert!(out.ends_with("3. Read the logs.\r\n") && !out.contains("\r\r"));
        let bare = FILE.trim_end_matches('\n');
        assert!(apply(bare, diff).unwrap().ends_with("3. Read the logs."));
        let diff =
            "@@ -8 +8 @@\n-3. Check the logs.\n+3. Read the logs.\n\\ No newline at end of file\n";
        assert!(apply(FILE, diff).unwrap().ends_with("3. Read the logs."));
    }

    #[test]
    fn a_new_file_from_an_empty_one() {
        let diff = "--- /dev/null\n+++ b/SKILL.md\n@@ -0,0 +1,3 @@\n+---\n+name: x\n+---\n";
        assert_eq!(apply("", diff).unwrap(), "---\nname: x\n---\n");
        assert!(is_unified_diff(diff));
        assert!(!is_unified_diff(FILE));
    }

    #[test]
    fn an_insertion_needs_context_in_a_file_that_has_lines() {
        // Without context only the (often wrong) line number would say where it goes.
        let error = apply(FILE, "@@ -8,0 +9,1 @@\n+4. Done.\n").unwrap_err();
        assert!(error.contains("without any context"), "{error}");
        assert!(apply(FILE, "@@ -99,0 +100,1 @@\n+x\n").is_err());
        // With a context line it is checked and placed.
        let diff = "@@ -8 +8,2 @@\n 3. Check the logs.\n+4. Done.\n";
        assert!(
            apply(FILE, diff)
                .unwrap()
                .ends_with("3. Check the logs.\n4. Done.\n")
        );
    }

    #[test]
    fn file_headers_must_name_skill_md_and_never_delete_it() {
        let hunk = "@@ -8 +8 @@\n-3. Check the logs.\n+3. Read the logs.\n";
        for ok in [
            "--- a/SKILL.md\n+++ b/SKILL.md\n",
            "--- skills/deploy/SKILL.md\t2026-01-01\n+++ skills/deploy/SKILL.md\n",
            "diff --git a/SKILL.md b/SKILL.md\nindex 1..2 100644\n",
        ] {
            assert!(apply(FILE, &format!("{ok}{hunk}")).is_ok(), "{ok}");
        }
        for (bad, why) in [
            (
                "--- a/../../evil.md\n+++ b/../../evil.md\n",
                "not the skill's SKILL.md",
            ),
            (
                "--- a/../../SKILL.md\n+++ b/../../SKILL.md\n",
                "not the skill's SKILL.md",
            ),
            (
                "--- a/SKILL.md\n+++ b/references/x.md\n",
                "not the skill's SKILL.md",
            ),
            (
                "--- C:\\x\\SKILL.md\n+++ C:\\x\\SKILL.md\n",
                "not the skill's SKILL.md",
            ),
            ("--- a/SKILL.md\n+++ /dev/null\n", "deletes"),
            ("--- /dev/null\n+++ b/SKILL.md\n", "already exists"),
            (
                "--- a/SKILL.md\n--- a/SKILL.md\n+++ b/SKILL.md\n",
                "more than one file",
            ),
            (
                "rename from SKILL.md\nrename to x.md\n",
                "before the first hunk",
            ),
            (
                "Binary files a/SKILL.md and b/SKILL.md differ\n",
                "before the first hunk",
            ),
            ("deleted file mode 100644\n", "before the first hunk"),
        ] {
            let error = apply(FILE, &format!("{bad}{hunk}")).unwrap_err();
            assert!(error.contains(why), "{bad}: {error}");
        }
        // A second file's hunks after the first file's: outside a hunk.
        let two = format!("--- a/SKILL.md\n+++ b/SKILL.md\n{hunk}--- a/x.md\n+++ b/x.md\n{hunk}");
        assert!(apply(FILE, &two).unwrap_err().contains("outside a hunk"));
    }
}
