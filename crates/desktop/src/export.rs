//! `/save`: a conversation as a file, in JSON, Markdown or HTML, optionally with secrets masked.
//!
//! Hermes: `session.export` (`tui_gateway/methods_session.py`) and the page's `/save`
//! (`apps/desktop/src/app/session/hooks/use-prompt-actions/slash-desktop-commands.ts`), which
//! hands the content to a Save As dialog (`server.rs`'s, `patches/0008-save-dialog.patch`).

use hermes_store::SessionRecord;
use hermes_tools::redact::{Source, redact};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Markdown,
    Html,
}

impl Format {
    pub fn parse(format: &str) -> Option<Self> {
        match format.to_ascii_lowercase().as_str() {
            "json" | "snapshot" => Some(Self::Json),
            "md" | "markdown" => Some(Self::Markdown),
            "html" => Some(Self::Html),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Markdown => "md",
            Self::Html => "html",
        }
    }
}

/// `(content, suggested file name)` for a session's transcript rows (`transcript::messages`).
pub fn export(
    record: &SessionRecord,
    rows: &[Value],
    format: Format,
    masked: bool,
) -> (String, String) {
    let clean = |text: &str| {
        if masked {
            redact(text, Source::Output)
        } else {
            text.to_string()
        }
    };
    let title = record
        .title
        .clone()
        .unwrap_or_else(|| "Untitled session".into());
    let content = match format {
        Format::Json => {
            let rows: Vec<Value> = rows
                .iter()
                .map(|row| {
                    let mut row = row.clone();
                    for key in ["text", "reasoning"] {
                        if let Some(text) = row[key].as_str() {
                            row[key] = json!(clean(text));
                        }
                    }
                    if masked && !row["args"].is_null() {
                        row["args"] = json!(clean(&row["args"].to_string()));
                    }
                    row
                })
                .collect();
            serde_json::to_string_pretty(&json!({
                "session_id": record.id,
                "title": title,
                "model": record.model,
                "provider": record.provider,
                "cwd": record.cwd,
                "created_at": record.created_at,
                "updated_at": record.updated_at,
                "messages": rows,
            }))
            .unwrap_or_default()
        }
        Format::Markdown => markdown(&title, record, rows, &clean),
        Format::Html => html(&title, record, rows, &clean),
    };
    (
        content,
        format!("{}.{}", file_stem(&title), format.extension()),
    )
}

fn markdown(
    title: &str,
    record: &SessionRecord,
    rows: &[Value],
    clean: &dyn Fn(&str) -> String,
) -> String {
    let mut out = format!("# {title}\n\n_{} · {}_\n", record.model, record.cwd);
    for row in rows {
        match row["role"].as_str().unwrap_or_default() {
            "user" => out.push_str(&format!(
                "\n## You\n\n{}\n",
                clean(row["text"].as_str().unwrap_or_default())
            )),
            "assistant" => out.push_str(&format!(
                "\n## Hermes\n\n{}\n",
                clean(row["text"].as_str().unwrap_or_default())
            )),
            "tool" => {
                let args = clean(&serde_json::to_string_pretty(&row["args"]).unwrap_or_default());
                let result = clean(row["text"].as_str().unwrap_or_default());
                out.push_str(&format!(
                    "\n<details><summary>Tool: {}</summary>\n\n{}\n\n{}\n\n</details>\n",
                    row["name"].as_str().unwrap_or("tool"),
                    fence("json", &args),
                    fence("", &result)
                ));
            }
            _ => {}
        }
    }
    out
}

fn html(
    title: &str,
    record: &SessionRecord,
    rows: &[Value],
    clean: &dyn Fn(&str) -> String,
) -> String {
    let mut body = String::new();
    for row in rows {
        let text = |key: &str| escape(&clean(row[key].as_str().unwrap_or_default()));
        match row["role"].as_str().unwrap_or_default() {
            "user" => body.push_str(&format!("<section class=user><h2>You</h2><pre>{}</pre></section>\n", text("text"))),
            "assistant" => body.push_str(&format!("<section class=assistant><h2>Hermes</h2><pre>{}</pre></section>\n", text("text"))),
            "tool" => body.push_str(&format!(
                "<details class=tool><summary>Tool: {}</summary><pre>{}</pre><pre>{}</pre></details>\n",
                escape(row["name"].as_str().unwrap_or("tool")),
                escape(&clean(&serde_json::to_string_pretty(&row["args"]).unwrap_or_default())),
                text("text")
            )),
            _ => {}
        }
    }
    format!(
        "<!doctype html>\n<html><head><meta charset=utf-8><title>{t}</title><style>\
body{{font:15px/1.5 system-ui,sans-serif;max-width:860px;margin:2rem auto;padding:0 1rem;color:#1f2328}}\
h1{{font-size:1.4rem}}h2{{font-size:.85rem;text-transform:uppercase;color:#59636e;margin:1.5rem 0 .3rem}}\
pre{{white-space:pre-wrap;word-wrap:break-word;font:inherit;margin:0}}.tool pre{{font:12px ui-monospace,monospace;background:#f6f8fa;padding:.5rem;border-radius:6px;margin:.3rem 0}}\
.user pre{{background:#f6f8fa;padding:.6rem .8rem;border-radius:8px}}summary{{cursor:pointer;color:#59636e;margin-top:.6rem}}\
</style></head><body><h1>{t}</h1><p><small>{m} · {c}</small></p>\n{body}</body></html>\n",
        t = escape(title),
        m = escape(&record.model),
        c = escape(&record.cwd),
    )
}

fn fence(lang: &str, text: &str) -> String {
    // A fence longer than any backtick run inside.
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let ticks = "`".repeat(longest.max(2) + 1);
    format!("{ticks}{lang}\n{text}\n{ticks}")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A file name from the title: letters, digits and dashes.
fn file_stem(title: &str) -> String {
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "hermes-session".into()
    } else {
        format!("hermes-{slug}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> SessionRecord {
        SessionRecord {
            id: "s1".into(),
            title: Some("Fix the <build>".into()),
            cwd: "C:/work".into(),
            provider: "mock".into(),
            model: "m".into(),
            system_prompt: String::new(),
            created_at: 1,
            updated_at: 2,
            pinned: false,
        }
    }

    fn rows() -> Vec<Value> {
        vec![
            json!({"role": "user", "text": "use key sk-proj-abcdefghijklmnopqrstuvwxyz123456"}),
            json!({"role": "tool", "name": "terminal", "args": {"command": "ls"}, "text": "a.txt\n```b```"}),
            json!({"role": "assistant", "text": "Done <ok>."}),
        ]
    }

    #[test]
    fn every_format_has_the_turns_and_a_file_name() {
        for (format, ext) in [
            (Format::Json, "json"),
            (Format::Markdown, "md"),
            (Format::Html, "html"),
        ] {
            let (content, name) = export(&record(), &rows(), format, false);
            assert_eq!(name, format!("hermes-fix-the-build.{ext}"));
            assert!(content.contains("a.txt"), "{format:?}");
            assert!(
                content.contains("sk-proj-abcdefghijklmnopqrstuvwxyz123456"),
                "{format:?}"
            );
        }
        let (json, _) = export(&record(), &rows(), Format::Json, false);
        assert_eq!(
            serde_json::from_str::<Value>(&json).unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let (html, _) = export(&record(), &rows(), Format::Html, false);
        assert!(html.contains("Done &lt;ok&gt;.") && html.contains("Fix the &lt;build&gt;"));
        // A result holding backticks stays inside a longer fence.
        let (md, _) = export(&record(), &rows(), Format::Markdown, false);
        assert!(md.contains("````\na.txt\n```b```\n````"), "{md}");
    }

    #[test]
    fn redact_masks_secrets() {
        for format in [Format::Json, Format::Markdown, Format::Html] {
            let (content, _) = export(&record(), &rows(), format, true);
            assert!(
                !content.contains("abcdefghijklmnopqrstuvwxyz123456"),
                "{format:?}"
            );
        }
    }
}
