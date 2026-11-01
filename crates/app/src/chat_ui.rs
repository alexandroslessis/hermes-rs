//! Pieces of Zed's agent thread view (`crates/agent_ui/src/conversation_view/thread_view.rs`,
//! GPL-3.0-or-later) that the chat reuses as they are: the generating spinner, the token
//! count format, and the context-usage tooltip.

use gpui::{App, Context, Window, prelude::*};
use ui::{SpinnerLabel, SpinnerVariant, prelude::*};

/// Zed's `GeneratingSpinner`: a view, so the spinner's animation keeps its frame across
/// re-renders of the list item around it.
struct GeneratingSpinner {
    variant: SpinnerVariant,
}

impl Render for GeneratingSpinner {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SpinnerLabel::with_variant(self.variant).size(LabelSize::Small)
    }
}

#[derive(IntoElement)]
pub struct GeneratingSpinnerElement {
    variant: SpinnerVariant,
}

impl GeneratingSpinnerElement {
    pub fn new(variant: SpinnerVariant) -> Self {
        Self { variant }
    }
}

impl RenderOnce for GeneratingSpinnerElement {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = match self.variant {
            SpinnerVariant::Dots => "generating-spinner-view",
            SpinnerVariant::Sand => "confirmation-spinner-view",
            _ => "spinner-view",
        };
        window.with_id(id, |window| {
            window.use_state(cx, |_, _| GeneratingSpinner {
                variant: self.variant,
            })
        })
    }
}

/// Zed's `humanize_token_count`: 999, 1.2k, 12k, 1.3M.
pub fn humanize_token_count(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1000..=9999 => {
            let thousands = count / 1000;
            let hundreds = (count % 1000 + 50) / 100;
            if hundreds == 0 {
                format!("{thousands}k")
            } else if hundreds == 10 {
                format!("{}k", thousands + 1)
            } else {
                format!("{thousands}.{hundreds}k")
            }
        }
        10_000..=999_999 => format!("{}k", (count + 500) / 1000),
        1_000_000..=9_999_999 => {
            let millions = count / 1_000_000;
            let hundred_thousands = (count % 1_000_000 + 50_000) / 100_000;
            if hundred_thousands == 0 {
                format!("{millions}M")
            } else if hundred_thousands == 10 {
                format!("{}M", millions + 1)
            } else {
                format!("{millions}.{hundred_thousands}M")
            }
        }
        _ => format!("{}M", (count + 500_000) / 1_000_000),
    }
}

/// Zed's `TokenUsageTooltip`: how full the context window is, and what the session cost.
pub struct TokenUsageTooltip {
    pub percentage: SharedString,
    pub used: SharedString,
    pub max: SharedString,
    pub cost: Option<SharedString>,
    /// Estimated tokens per part of the request: (part, "12.3k").
    pub breakdown: Vec<(SharedString, SharedString)>,
    /// When automatic compression starts, e.g. "150k".
    pub compress_at: Option<SharedString>,
}

impl Render for TokenUsageTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let separator_color = Color::Custom(cx.theme().colors().text_disabled.opacity(0.6));
        let (percentage, used, max, cost) = (
            self.percentage.clone(),
            self.used.clone(),
            self.max.clone(),
            self.cost.clone(),
        );
        let (breakdown, compress_at) = (self.breakdown.clone(), self.compress_at.clone());
        ui::tooltip_container(cx, move |container, cx| {
            container
                .min_w_40()
                .child(
                    Label::new("Context")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .child(
                    h_flex()
                        .gap_0p5()
                        .child(Label::new(percentage))
                        .child(Label::new("\u{2022}").color(separator_color).mx_1())
                        .child(Label::new(used))
                        .child(Label::new("/").color(separator_color))
                        .child(Label::new(max).color(Color::Muted)),
                )
                .when(!breakdown.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .mt_1p5()
                            .pt_1p5()
                            .gap_0p5()
                            .border_t_1()
                            .border_color(cx.theme().colors().border_variant)
                            .child(
                                Label::new("Estimated breakdown")
                                    .color(Color::Muted)
                                    .size(LabelSize::Small),
                            )
                            .children(breakdown.into_iter().map(|(part, tokens)| {
                                h_flex()
                                    .gap_4()
                                    .justify_between()
                                    .child(Label::new(part).size(LabelSize::Small))
                                    .child(
                                        Label::new(tokens)
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                            }))
                            .when_some(compress_at, |this, at| {
                                this.child(
                                    Label::new(format!("Compresses automatically at {at}"))
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                )
                            }),
                    )
                })
                .when_some(cost, |this, cost| {
                    this.child(
                        v_flex()
                            .mt_1p5()
                            .pt_1p5()
                            .gap_0p5()
                            .border_t_1()
                            .border_color(cx.theme().colors().border_variant)
                            .child(
                                Label::new("Cost")
                                    .color(Color::Muted)
                                    .size(LabelSize::Small),
                            )
                            .child(Label::new(cost)),
                    )
                })
        })
    }
}

/// The tool call label: what the call does, as markdown (paths and patterns in code spans), like
/// the labels Zed's agent gives its tools.
/// Paths inside `cwd`, the session's folder, show relative to it (Zed shows project paths).
pub fn tool_label(name: &str, args: &serde_json::Value, cwd: Option<&str>) -> String {
    let field = |key: &str| {
        args.get(key).and_then(|v| v.as_str()).map(|s| {
            let s = if key == "path" {
                relative_path(s, cwd)
            } else {
                s
            };
            s.replace('`', "'").replace('\n', " ")
        })
    };
    let code = |text: Option<String>| text.map(|t| format!(" `{t}`")).unwrap_or_default();
    match name {
        "read_file" => format!("Read{}", code(field("path"))),
        "write_file" => format!("Write{}", code(field("path"))),
        "patch" => format!("Edit{}", code(field("path"))),
        "search_files" => {
            let target = if field("target").as_deref() == Some("files") {
                "files"
            } else {
                "content"
            };
            format!(
                "Search {target} for{}{}",
                code(field("pattern")),
                field("path")
                    .map(|p| format!(" in `{p}`"))
                    .unwrap_or_default()
            )
        }
        "terminal" => format!("Run{}", code(field("command"))),
        "process_manage" => format!(
            "{} process{}",
            field("action")
                .map(capitalize)
                .unwrap_or_else(|| "Manage".into()),
            code(field("session_id"))
        ),
        "todo_list" => match args.get("todos").and_then(|t| t.as_array()) {
            Some(todos) => format!("Update the task list ({} items)", todos.len()),
            None => "Read the task list".into(),
        },
        "memory" => {
            let target = if field("target").as_deref() == Some("user") {
                "the user profile"
            } else {
                "memory"
            };
            let count = args
                .get("operations")
                .and_then(|o| o.as_array())
                .map_or(1, Vec::len);
            format!(
                "Update {target} ({count} change{})",
                if count == 1 { "" } else { "s" }
            )
        }
        "session_search" => match (field("query"), field("session_id")) {
            (Some(query), _) => format!("Search past sessions for `{query}`"),
            (None, Some(id)) => format!("Read session `{id}`"),
            (None, None) => "List recent sessions".into(),
        },
        "skills_list" => "List skills".into(),
        "skill_view" => format!("Load skill{}", code(field("name"))),
        "skill_manage" => {
            let ops = args.get("operations").and_then(|o| o.as_array());
            let first = ops.and_then(|ops| ops.first());
            let op = |key: &str| {
                first
                    .and_then(|op| op.get(key))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            };
            let more = ops.map_or(0, |ops| ops.len().saturating_sub(1));
            format!(
                "{} skill{}{}",
                op("action")
                    .map(|a| capitalize(a.replace('_', " ")))
                    .unwrap_or_else(|| "Change".into()),
                code(op("name")),
                if more > 0 {
                    format!(" (+{more} more)")
                } else {
                    String::new()
                }
            )
        }
        "web_search" => format!("Search the web for{}", code(field("query"))),
        "web_extract" => {
            let urls: Vec<&str> = args
                .get("urls")
                .and_then(|u| u.as_array())
                .map(|u| u.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let more = urls.len().saturating_sub(1);
            format!(
                "Read{}{}",
                code(urls.first().map(|u| u.to_string())),
                if more > 0 {
                    format!(" (+{more} more)")
                } else {
                    String::new()
                }
            )
        }
        "vision_analyze" => format!("Look at{}", code(field("image_url"))),
        "delegate_task" => {
            let count = args
                .get("tasks")
                .and_then(|t| t.as_array())
                .map_or(1, Vec::len);
            format!("Delegate {count} task{}", if count == 1 { "" } else { "s" })
        }
        "clarify" => "Ask a question".into(),
        "invalid tool call" => "Invalid tool call".into(),
        other => format!("`{other}`"),
    }
}

/// `path` without the `cwd` prefix when it lies inside it (Windows paths: case and slash
/// direction don't matter). `cwd` itself shows as `.`.
fn relative_path<'a>(path: &'a str, cwd: Option<&str>) -> &'a str {
    let Some(cwd) = cwd
        .map(|c| c.trim_end_matches(['/', '\\']))
        .filter(|c| !c.is_empty())
    else {
        return path;
    };
    let same = |a: &str, b: &str| {
        a.len() == b.len()
            && a.chars().zip(b.chars()).all(|(x, y)| {
                let norm = |c: char| {
                    if c == '\\' {
                        '/'
                    } else {
                        c.to_ascii_lowercase()
                    }
                };
                norm(x) == norm(y)
            })
    };
    match path.get(..cwd.len()) {
        Some(prefix) if same(prefix, cwd) => {
            let rest = path[cwd.len()..].trim_start_matches(['/', '\\']);
            if rest.is_empty() {
                "."
            } else if rest.len() == path.len() - cwd.len() {
                // No separator after the prefix: `C:/work2` isn't inside `C:/work`.
                path
            } else {
                rest
            }
        }
        _ => path,
    }
}

fn capitalize(text: String) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_read_like_zed() {
        assert_eq!(humanize_token_count(999), "999");
        assert_eq!(humanize_token_count(1050), "1.1k");
        assert_eq!(humanize_token_count(9960), "10k");
        assert_eq!(humanize_token_count(256_000), "256k");
        assert_eq!(humanize_token_count(1_250_000), "1.3M");
    }

    #[test]
    fn tool_labels_describe_the_call() {
        let args = serde_json::json!({"path": "src/main.rs"});
        assert_eq!(tool_label("read_file", &args, None), "Read `src/main.rs`");
        let args = serde_json::json!({"pattern": "TODO", "path": "src", "target": "content"});
        assert_eq!(
            tool_label("search_files", &args, None),
            "Search content for `TODO` in `src`"
        );
        let args = serde_json::json!({"operations": [{"action": "write_file", "name": "deploy"}]});
        assert_eq!(
            tool_label("skill_manage", &args, None),
            "Write file skill `deploy`"
        );
    }

    #[test]
    fn paths_inside_the_session_folder_are_relative() {
        let cwd = Some("C:/Work/");
        let label = |path: &str| tool_label("read_file", &serde_json::json!({"path": path}), cwd);
        assert_eq!(label(r"c:\work\src\main.rs"), r"Read `src\main.rs`");
        assert_eq!(label("C:/Work"), "Read `.`");
        assert_eq!(label("C:/Work2/a.rs"), "Read `C:/Work2/a.rs`");
        assert_eq!(label("D:/other.rs"), "Read `D:/other.rs`");
    }
}
