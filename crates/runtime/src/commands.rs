//! Hermes's `/rollback` and `/diff` (`hermes_cli/cli_commands_mixin.py`, `tools/working_diff.py`;
//! MIT). The CLI prints their output; here it becomes a chat message the user sees and the model
//! doesn't.

use std::io::Read as _;
use std::path::Path;

use hermes_tools::checkpoints::{self, Checkpoints};

/// Rollback diffs show this many lines (`rollback.more_lines`); `/diff` 400.
const ROLLBACK_DIFF_LINES: usize = 80;
const DIFF_LINES: usize = 400;
const MAX_UNTRACKED_FILES: usize = 50;

pub const NOT_ENABLED: &str = "Checkpoints are not enabled. Turn them on in Settings → Safety → Checkpoints; snapshots are taken before Hermes changes files from then on.";

/// What `/rollback` did: its report, and whether files were restored (the last chat turn is then
/// undone so the conversation matches the files).
pub struct RollbackOutcome {
    pub report: String,
    pub restored: bool,
}

fn fenced(language: &str, text: &str) -> String {
    format!("```{language}\n{text}\n```")
}

/// Up to `limit` lines of a diff, with Hermes's "more lines" note.
fn capped_diff(diff: &str, limit: usize, note: impl Fn(usize) -> String) -> String {
    let lines: Vec<&str> = diff.lines().collect();
    if lines.len() <= limit {
        return fenced("diff", diff);
    }
    format!(
        "{}\n\n{}",
        fenced("diff", &lines[..limit].join("\n")),
        note(lines.len() - limit)
    )
}

fn summarize_paths(paths: &[String]) -> String {
    let shown = paths.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    if paths.len() > 5 {
        format!("{shown} (+{} more)", paths.len() - 5)
    } else {
        shown
    }
}

/// `_handle_rollback_command`: `/rollback` lists, `/rollback diff N` previews, `/rollback N
/// [file] [--all]` restores (keeping hand edits unless `--all`).
pub fn rollback(checkpoints: &Checkpoints, cwd: &Path, home: &Path, args: &str) -> RollbackOutcome {
    let plain = |report: String| RollbackOutcome {
        report,
        restored: false,
    };
    if !checkpoints.enabled() {
        return plain(NOT_ENABLED.into());
    }
    let workdir = checkpoints.working_dir_for(cwd, home);
    let shown = hermes_tools::display_path(&workdir);
    let mut args: Vec<&str> = args.split_whitespace().collect();
    let restore_all = args
        .iter()
        .any(|a| a.eq_ignore_ascii_case("--all") || a.eq_ignore_ascii_case("--force"));
    args.retain(|a| !a.eq_ignore_ascii_case("--all") && !a.eq_ignore_ascii_case("--force"));
    let list = checkpoints.list(&workdir);
    if args.is_empty() {
        return plain(fenced("", &checkpoints::format_list(&list, &shown)));
    }
    let is_diff = args[0].eq_ignore_ascii_case("diff");
    if is_diff && args.len() < 2 {
        return plain("Usage: /rollback diff <N>".into());
    }
    if list.is_empty() {
        return plain(format!("No checkpoints found for {shown}"));
    }
    let target = args[usize::from(is_diff)];
    // `_resolve_checkpoint_ref`: a list number, or a hash prefix.
    let hash = match target.parse::<usize>() {
        Ok(n) if (1..=list.len()).contains(&n) => list[n - 1].hash.clone(),
        Ok(_) => return plain(format!("Invalid checkpoint number. Use 1-{}.", list.len())),
        Err(_) => match list
            .iter()
            .find(|c| c.hash.starts_with(&target.to_lowercase()))
        {
            Some(checkpoint) => checkpoint.hash.clone(),
            None => return plain(format!("Invalid checkpoint number. Use 1-{}.", list.len())),
        },
    };
    if is_diff {
        return plain(match checkpoints.diff(&workdir, &hash) {
            Err(error) => format!("❌ {error}"),
            Ok(diff) if diff.stat.is_empty() && diff.diff.is_empty() => {
                "No changes since this checkpoint.".into()
            }
            Ok(diff) => {
                let mut parts = Vec::new();
                if !diff.stat.is_empty() {
                    parts.push(fenced("", &diff.stat));
                }
                if !diff.diff.is_empty() {
                    parts.push(capped_diff(&diff.diff, ROLLBACK_DIFF_LINES, |more| {
                        format!("... ({more} more lines, showing first {ROLLBACK_DIFF_LINES})")
                    }));
                }
                parts.join("\n\n")
            }
        });
    }
    let file = args.get(1).copied();
    match checkpoints.restore(&workdir, &hash, file, !restore_all && file.is_none()) {
        Err(error) => plain(format!("❌ {error}")),
        Ok(restored) => {
            let mut lines = vec![match file {
                Some(file) => format!(
                    "✅ Restored {file} from checkpoint {}: {}",
                    restored.restored_to, restored.reason
                ),
                None => format!(
                    "✅ Restored to checkpoint {}: {}",
                    restored.restored_to, restored.reason
                ),
            }];
            if !restored.skipped_user_edits.is_empty() {
                lines.push(format!(
                    "↷ Kept your hand-edits: {}\nUse /rollback <N> --all to restore those too.",
                    summarize_paths(&restored.skipped_user_edits)
                ));
            }
            if !restored.skipped_oversize.is_empty() {
                lines.push(format!(
                    "↷ Kept (too large for checkpoints, no stored copy to revert to): {}",
                    summarize_paths(&restored.skipped_oversize)
                ));
            }
            if !restored.failed_deletes.is_empty() {
                lines.push(format!(
                    "⚠️ Could not remove (left in place): {}",
                    summarize_paths(&restored.failed_deletes)
                ));
            }
            lines.push("A pre-rollback snapshot was saved automatically.".into());
            RollbackOutcome {
                report: lines.join("\n\n"),
                restored: true,
            }
        }
    }
}

/// `_handle_diff_command`: `/diff [working|staged|all|session] [--stat] [path…]`, git's view of
/// the working folder (`session`: everything since the oldest checkpoint).
pub fn diff(checkpoints: &Checkpoints, cwd: &Path, home: &Path, args: &str) -> String {
    let mut stat_only = false;
    let mut mode = "working";
    let mut paths = Vec::new();
    for arg in args.split_whitespace() {
        match arg.to_lowercase().as_str() {
            "--stat" | "stat" => stat_only = true,
            "staged" | "--staged" | "cached" | "--cached" => mode = "staged",
            "all" | "--all" | "head" => mode = "all",
            "session" => mode = "session",
            _ => paths.push(arg.trim_matches(['"', '\'']).to_string()),
        }
    }
    if mode == "session" {
        if !checkpoints.enabled() {
            return "Checkpoints are not enabled, so there's no session baseline. Turn them on in Settings → Safety → Checkpoints.\n(Plain /diff still works — it uses git directly.)".into();
        }
        let workdir = checkpoints.working_dir_for(cwd, home);
        return match checkpoints.session_diff(&workdir) {
            None => "No changes — Hermes hasn't edited any files here yet.".into(),
            Some(Err(error)) => format!("❌ {error}"),
            Some(Ok(diff)) if diff.stat.is_empty() && diff.diff.is_empty() => {
                "No changes — Hermes hasn't edited any files here yet.".into()
            }
            Some(Ok(diff)) => render(
                &diff.stat,
                &diff.diff,
                &[],
                "Session",
                stat_only,
                "run /diff session --stat for a summary",
            ),
        };
    }
    match git(&["rev-parse", "--is-inside-work-tree"], cwd) {
        None => return "git is not installed or not on PATH.".into(),
        Some((false, _)) => return "Not a git repository.".into(),
        Some((true, _)) => {}
    }
    let mut base: Vec<&str> = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
    match mode {
        "staged" => base.push("--cached"),
        "all" => base.push("HEAD"),
        _ => {}
    }
    let with_paths = |extra: Option<&'static str>| -> Vec<String> {
        let mut args: Vec<String> = base.iter().map(|a| a.to_string()).collect();
        args.extend(extra.map(str::to_string));
        if !paths.is_empty() {
            args.push("--".into());
            args.extend(paths.iter().cloned());
        }
        args
    };
    let stat = git(&with_paths(Some("--stat")), cwd)
        .map(|(_, out)| out)
        .unwrap_or_default();
    let mut body = git(&with_paths(None), cwd)
        .map(|(_, out)| out)
        .unwrap_or_default();
    let untracked: Vec<String> = if mode != "staged" && paths.is_empty() {
        git(&["ls-files", "--others", "--exclude-standard"], cwd)
            .map(|(_, out)| {
                out.lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    // New files as additions (`git diff --no-index NUL <file>`), so they aren't invisible.
    for rel in untracked.iter().take(MAX_UNTRACKED_FILES) {
        if let Some((_, out)) = git(
            &[
                "diff",
                "--no-ext-diff",
                "--no-color",
                "--no-index",
                "--",
                "NUL",
                rel,
            ],
            cwd,
        ) && !out.trim().is_empty()
        {
            body = format!("{}\n{}", body.trim_end(), out.trim_end());
        }
    }
    if untracked.len() > MAX_UNTRACKED_FILES {
        body.push_str(&format!(
            "\n... ({} more untracked files not shown)",
            untracked.len() - MAX_UNTRACKED_FILES
        ));
    }
    let (stat, body) = (stat.trim(), body.trim());
    if stat.is_empty() && body.is_empty() && untracked.is_empty() {
        return "No changes.".into();
    }
    let label = match mode {
        "staged" => "Staged",
        "all" => "All (vs HEAD)",
        _ => "Unstaged",
    };
    render(
        stat,
        body,
        &untracked,
        label,
        stat_only,
        "run /diff --stat for a summary",
    )
}

fn render(
    stat: &str,
    diff: &str,
    untracked: &[String],
    label: &str,
    stat_only: bool,
    hint: &str,
) -> String {
    let mut parts = Vec::new();
    if !stat.is_empty() {
        parts.push(format!("{label}:\n\n{}", fenced("", stat)));
    }
    if !untracked.is_empty() {
        let mut lines: Vec<String> = untracked
            .iter()
            .take(20)
            .map(|rel| format!("    + {rel}"))
            .collect();
        if untracked.len() > 20 {
            lines.push(format!("    ... and {} more", untracked.len() - 20));
        }
        parts.push(format!("Untracked:\n\n{}", fenced("", &lines.join("\n"))));
    }
    if !diff.is_empty() && !stat_only {
        parts.push(capped_diff(diff, DIFF_LINES, |more| {
            format!("... ({more} more lines — {hint})")
        }));
    }
    parts.join("\n\n")
}

/// git in the user's repository with its config's code-running hooks off; `None` when git can't
/// start. Exit status 1 counts as success for `diff --no-index`.
pub(crate) fn git<S: AsRef<str>>(args: &[S], cwd: &Path) -> Option<(bool, String)> {
    use std::os::windows::process::CommandExt as _;
    let mut child = std::process::Command::new("git")
        .args([
            "-c",
            "core.quotePath=false",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=NUL",
            "-c",
            "core.pager=cat",
        ])
        .args(args.iter().map(AsRef::as_ref))
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x0800_0000)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(8 * 1024 * 1024).read_to_end(&mut out);
        out
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned();
    let ok = status.is_some_and(|s| {
        s.success() || (args.iter().any(|a| a.as_ref() == "--no-index") && s.code() == Some(1))
    });
    Some((ok, out))
}

fn thousands(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Hermes's `/usage`: the session's token use, cost and context, with the estimated breakdown.
pub fn usage_report(
    model: &str,
    totals: &hermes_store::UsageTotals,
    context_length: Option<usize>,
    compressions: usize,
    breakdown: &crate::ContextBreakdown,
) -> String {
    let mut lines = vec!["📊 **Session Token Usage**".to_string(), String::new()];
    lines.push(format!("Model: `{model}`"));
    if totals.calls == 0 {
        lines.push(format!(
            "Estimated context: ~{} tokens",
            thousands(breakdown.total() as i64)
        ));
        lines.push("_(Detailed usage available after the first agent response)_".into());
    } else {
        lines.push(format!("Input tokens: {}", thousands(totals.input_tokens)));
        if totals.cache_read_tokens > 0 {
            lines.push(format!(
                "Cache read tokens: {}",
                thousands(totals.cache_read_tokens)
            ));
        }
        if totals.cache_write_tokens > 0 {
            lines.push(format!(
                "Cache write tokens: {}",
                thousands(totals.cache_write_tokens)
            ));
        }
        lines.push(format!(
            "Output tokens: {}",
            thousands(totals.output_tokens)
        ));
        lines.push(format!(
            "Total: {}",
            thousands(totals.input_tokens + totals.output_tokens)
        ));
        lines.push(format!("API calls: {}", thousands(totals.calls)));
        if let Some(cost) = totals.cost {
            lines.push(format!("Cost: ${cost:.4}"));
        }
        if let Some(total) = context_length.filter(|&n| n > 0) {
            let used = totals.last_input_tokens;
            lines.push(format!(
                "Context: {} / {} ({}%)",
                thousands(used),
                thousands(total as i64),
                (used as f64 * 100.0 / total as f64).round()
            ));
        }
    }
    if compressions > 0 {
        lines.push(format!("Compressions: {compressions}"));
    }
    let total = breakdown.total().max(1) as f64;
    lines.push(String::new());
    lines.push("🧩 **Context breakdown** _(estimated)_".into());
    for (label, count) in [
        ("System prompt", breakdown.system_prompt),
        ("Tool definitions", breakdown.tool_definitions),
        ("Skills", breakdown.skills),
        ("Memory", breakdown.memory),
        ("Conversation", breakdown.conversation),
    ] {
        if count > 0 || label == "Conversation" {
            lines.push(format!(
                "• {label}: ~{} ({}%)",
                thousands(count as i64),
                (count as f64 * 100.0 / total).round()
            ));
        }
    }
    lines.join("\n")
}

/// Hermes's `/insights [--days N]`: activity, tokens, models and tools over the last N days.
pub fn insights_report(store: &hermes_store::Store, args: &str) -> String {
    let mut days = 30i64;
    let mut words = args.split_whitespace();
    while let Some(word) = words.next() {
        let value = match word.strip_prefix("--days=") {
            Some(value) => Some(value.to_string()),
            None if word == "--days" => words.next().map(str::to_string),
            None => Some(word.to_string()),
        };
        match value.and_then(|v| v.parse::<i64>().ok()).filter(|&d| d > 0) {
            Some(value) => days = value,
            None => return format!("Invalid --days value: {word}"),
        }
    }
    let since = chrono::Utc::now().timestamp_millis() - days * 86_400_000;
    let insights = match store.insights(since) {
        Ok(insights) => insights,
        Err(error) => return format!("Error generating insights: {error:#}"),
    };
    if insights.sessions == 0 {
        return format!("No sessions found in the last {days} days.");
    }
    let day = |ms: Option<i64>| {
        ms.and_then(chrono::DateTime::from_timestamp_millis)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%b %d, %Y")
                    .to_string()
            })
            .unwrap_or_default()
    };
    let mut lines = vec![
        format!("📊 **Hermes Insights**: last {days} days"),
        format!(
            "Period: {} — {}",
            day(insights.first_ms),
            day(insights.last_ms)
        ),
        String::new(),
        "📋 **Overview**".into(),
        format!(
            "Sessions: {} · Messages: {} · User messages: {} · Tool calls: {}",
            thousands(insights.sessions),
            thousands(insights.messages),
            thousands(insights.user_messages),
            thousands(insights.tool_calls)
        ),
        format!(
            "Input tokens: {} · Output tokens: {} · Total: {}",
            thousands(insights.input_tokens),
            thousands(insights.output_tokens),
            thousands(insights.input_tokens + insights.output_tokens)
        ),
        format!(
            "Avg messages/session: {:.1}",
            insights.messages as f64 / insights.sessions as f64
        ),
    ];
    if let Some(cost) = insights.cost {
        lines.push(format!("Estimated cost: ${cost:.2}"));
    }
    if !insights.models.is_empty() {
        lines.push(String::new());
        lines.push("🤖 **Models Used**".into());
        lines.push("| Model | Sessions | Tokens |\n|---|---:|---:|".into());
        for (model, sessions, tokens) in &insights.models {
            lines.push(format!(
                "| `{model}` | {sessions} | {} |",
                thousands(*tokens)
            ));
        }
    }
    if !insights.tools.is_empty() {
        let total: i64 = insights.tools.iter().map(|(_, n)| n).sum();
        lines.push(String::new());
        lines.push("🔧 **Top Tools**".into());
        lines.push("| Tool | Calls | % |\n|---|---:|---:|".into());
        for (tool, count) in &insights.tools {
            lines.push(format!(
                "| `{tool}` | {} | {:.1}% |",
                thousands(*count),
                *count as f64 * 100.0 / total.max(1) as f64
            ));
        }
    }
    lines.join("\n")
}
