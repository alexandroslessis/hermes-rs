//! `/save`: a conversation as a file, in Markdown or HTML (JSON is Hermes's transcript shape: the
//! page's side builds it, `crates/desktop/src/export.rs`), optionally with secrets masked, and the
//! file name it suggests.
//!
//! Hermes: `session.export` (`tui_gateway/methods_session.py`) and the page's `/save`
//! (`apps/desktop/src/app/session/hooks/use-prompt-actions/slash-desktop-commands.ts`), which
//! hands the content to a Save As dialog (`crates/desktop/src/server.rs`'s,
//! `patches/0008-save-dialog.patch`).

use hermes_store::SessionRecord;
use hermes_tools::redact::{Source, redact};

use crate::transcript::{Entry, Speaker};

/// The kinds of file a conversation saves as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Markdown,
    Html,
}

impl Format {
    /// `json` (or `snapshot`), `md` (or `markdown`), `html`; any case.
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

/// The session's title as the file shows it.
pub fn title(record: &SessionRecord) -> String {
    record
        .title
        .clone()
        .unwrap_or_else(|| "Untitled session".into())
}

/// The suggested file name: `hermes-<title words>.<ext>`.
pub fn file_name(record: &SessionRecord, format: Format) -> String {
    format!("{}.{}", file_stem(&title(record)), format.extension())
}

/// `text` with secrets masked when `masked`.
pub fn clean(text: &str, masked: bool) -> String {
    if masked {
        redact(text, Source::Output)
    } else {
        text.to_string()
    }
}

/// The conversation (`transcript::entries`) as a Markdown or HTML document; `None` for JSON.
pub fn document(
    record: &SessionRecord,
    entries: &[Entry],
    format: Format,
    masked: bool,
) -> Option<String> {
    let clean = |text: &str| clean(text, masked);
    let title = title(record);
    match format {
        Format::Json => None,
        Format::Markdown => Some(markdown(&title, record, entries, &clean)),
        Format::Html => Some(html(&title, record, entries, &clean)),
    }
}

fn markdown(
    title: &str,
    record: &SessionRecord,
    entries: &[Entry],
    clean: &dyn Fn(&str) -> String,
) -> String {
    let mut out = format!("# {title}\n\n_{} · {}_\n", record.model, record.cwd);
    for entry in entries {
        match entry {
            Entry::Message(message) => {
                let who = match message.speaker {
                    Speaker::User => "You",
                    Speaker::Assistant => "Hermes",
                };
                out.push_str(&format!("\n## {who}\n\n{}\n", clean(&message.text)));
            }
            Entry::Tool(tool) => {
                let args = clean(&serde_json::to_string_pretty(&tool.args).unwrap_or_default());
                let result = clean(tool.result.as_deref().unwrap_or_default());
                out.push_str(&format!(
                    "\n<details><summary>Tool: {}</summary>\n\n{}\n\n{}\n\n</details>\n",
                    tool.name,
                    fence("json", &args),
                    fence("", &result)
                ));
            }
        }
    }
    out
}

fn html(
    title: &str,
    record: &SessionRecord,
    entries: &[Entry],
    clean: &dyn Fn(&str) -> String,
) -> String {
    let mut body = String::new();
    for entry in entries {
        match entry {
            Entry::Message(message) => {
                let (class, who) = match message.speaker {
                    Speaker::User => ("user", "You"),
                    Speaker::Assistant => ("assistant", "Hermes"),
                };
                body.push_str(&format!(
                    "<section class={class}><h2>{who}</h2><pre>{}</pre></section>\n",
                    escape(&clean(&message.text))
                ));
            }
            Entry::Tool(tool) => body.push_str(&format!(
                "<details class=tool><summary>Tool: {}</summary><pre>{}</pre><pre>{}</pre></details>\n",
                escape(&tool.name),
                escape(&clean(&serde_json::to_string_pretty(&tool.args).unwrap_or_default())),
                escape(&clean(tool.result.as_deref().unwrap_or_default()))
            )),
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
    use crate::transcript::{MessageEntry, ToolEntry};
    use serde_json::json;

    pub(crate) fn record() -> SessionRecord {
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

    fn said(speaker: Speaker, text: &str) -> Entry {
        Entry::Message(MessageEntry {
            speaker,
            text: text.into(),
            reasoning: None,
            timestamp: 0.0,
            row_id: 0,
            failed_turn: None,
        })
    }

    fn entries() -> Vec<Entry> {
        vec![
            said(
                Speaker::User,
                "use key sk-proj-abcdefghijklmnopqrstuvwxyz123456",
            ),
            Entry::Tool(ToolEntry {
                name: "terminal".into(),
                args: json!({"command": "ls"}),
                call_id: "c1".into(),
                result: Some("a.txt\n```b```".into()),
                timestamp: 0.0,
            }),
            said(Speaker::Assistant, "Done <ok>."),
        ]
    }

    #[test]
    fn every_document_has_the_turns_and_a_file_name() {
        for (format, ext) in [(Format::Markdown, "md"), (Format::Html, "html")] {
            let content = document(&record(), &entries(), format, false).unwrap();
            assert_eq!(
                file_name(&record(), format),
                format!("hermes-fix-the-build.{ext}")
            );
            assert!(content.contains("a.txt"), "{format:?}");
            assert!(
                content.contains("sk-proj-abcdefghijklmnopqrstuvwxyz123456"),
                "{format:?}"
            );
        }
        assert!(document(&record(), &entries(), Format::Json, false).is_none());
        let html = document(&record(), &entries(), Format::Html, false).unwrap();
        assert!(html.contains("Done &lt;ok&gt;.") && html.contains("Fix the &lt;build&gt;"));
        // A result holding backticks stays inside a longer fence.
        let md = document(&record(), &entries(), Format::Markdown, false).unwrap();
        assert!(md.contains("````\na.txt\n```b```\n````"), "{md}");
        assert!(md.contains("## You") && md.contains("## Hermes"));
    }

    #[test]
    fn redact_masks_secrets() {
        for format in [Format::Markdown, Format::Html] {
            let content = document(&record(), &entries(), format, true).unwrap();
            assert!(
                !content.contains("abcdefghijklmnopqrstuvwxyz123456"),
                "{format:?}"
            );
        }
        assert!(!clean("sk-proj-abcdefghijklmnopqrstuvwxyz123456", true).contains("abcdefgh"));
        assert_eq!(Format::parse("Snapshot"), Some(Format::Json));
        assert_eq!(Format::parse("txt"), None);
    }
}
