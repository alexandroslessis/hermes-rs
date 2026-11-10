//! The editor's prompt (ACP content blocks) as what the agent is sent: one text, and the images.
//! A file the editor links becomes an `@file:` reference the runtime expands (`references.rs`);
//! a file it embeds (an unsaved buffer) is put in the text itself.
//!
//! Hermes: `acp_adapter/content.py` (`_content_blocks_to_openai_user_content`,
//! `_resource_link_to_parts`, `_embedded_resource_to_parts`).

use serde_json::Value;

/// An image block's bytes, still base64 (the gateway's `image.attach_bytes` takes it as is).
#[derive(Debug, PartialEq)]
pub struct Image {
    pub base64: String,
    pub name: String,
}

#[derive(Debug, Default, PartialEq)]
pub struct Prompt {
    pub text: String,
    pub images: Vec<Image>,
}

impl Prompt {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty()
    }
}

/// `file:///C:/Users/a%20b/x.rs` → `C:\Users\a b\x.rs`; `None` for any other scheme.
fn file_path(uri: &str) -> Option<String> {
    let url = url::Url::parse(uri).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    let path = url.to_file_path().ok()?;
    Some(path.display().to_string())
}

/// `@file:` takes a bare path; one with spaces or brackets is quoted so it reads back whole.
fn reference(path: &str) -> String {
    if !path.contains(|c: char| c.is_whitespace() || "()[]{}<>\"'`".contains(c)) {
        return format!("@file:{path}");
    }
    let quote = ["`", "\"", "'"]
        .into_iter()
        .find(|quote| !path.contains(quote))
        .unwrap_or("`");
    format!("@file:{quote}{path}{quote}")
}

fn display_name(uri: &str, name: &str) -> String {
    if !name.is_empty() {
        return name.to_string();
    }
    file_path(uri)
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| uri.to_string())
}

fn extension(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        _ => "png",
    }
}

/// The blocks as one prompt. Parts are separated by a blank line.
pub fn from_blocks(blocks: &[Value]) -> Prompt {
    let mut parts: Vec<String> = Vec::new();
    let mut images = Vec::new();
    for block in blocks {
        match block["type"].as_str().unwrap_or_default() {
            "text" => {
                if let Some(text) = block["text"].as_str().filter(|t| !t.is_empty()) {
                    parts.push(text.to_string());
                }
            }
            "resource_link" => {
                let uri = block["uri"].as_str().unwrap_or_default();
                match file_path(uri) {
                    Some(path) => parts.push(reference(&path)),
                    None => parts.push(format!(
                        "[{}]({uri})",
                        display_name(uri, block["name"].as_str().unwrap_or_default())
                    )),
                }
            }
            "resource" => {
                let resource = &block["resource"];
                let uri = resource["uri"].as_str().unwrap_or_default();
                let label = file_path(uri).unwrap_or_else(|| display_name(uri, ""));
                match resource["text"].as_str() {
                    Some(text) => parts.push(format!("{label}:\n```\n{text}\n```")),
                    None => parts.push(format!(
                        "[attached {}: binary, not included]",
                        display_name(uri, "")
                    )),
                }
            }
            "image" => {
                if let Some(data) = block["data"].as_str().filter(|d| !d.is_empty()) {
                    let mime = block["mimeType"].as_str().unwrap_or("image/png");
                    images.push(Image {
                        base64: data.to_string(),
                        name: format!("pasted-image.{}", extension(mime)),
                    });
                }
            }
            other => tracing::debug!("ignoring a {other} block in the prompt"),
        }
    }
    Prompt {
        text: parts.join("\n\n"),
        images,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_blocks_are_joined_with_a_blank_line() {
        let prompt = from_blocks(&[
            json!({"type": "text", "text": "one"}),
            json!({"type": "text", "text": "two"}),
        ]);
        assert_eq!(prompt.text, "one\n\ntwo");
        assert!(!prompt.is_empty());
    }

    #[test]
    fn a_linked_file_becomes_a_reference_with_its_windows_path() {
        let prompt = from_blocks(&[json!({
            "type": "resource_link", "uri": "file:///C:/Users/a/src/lib.rs", "name": "lib.rs"
        })]);
        assert_eq!(prompt.text, "@file:C:\\Users\\a\\src\\lib.rs");
    }

    #[test]
    fn a_path_with_spaces_is_quoted_so_it_reads_back_whole() {
        let prompt = from_blocks(&[json!({
            "type": "resource_link", "uri": "file:///C:/My%20Docs/a%20b.rs", "name": "a b.rs"
        })]);
        assert_eq!(prompt.text, "@file:`C:\\My Docs\\a b.rs`");
    }

    #[test]
    fn a_link_that_is_not_a_file_stays_a_link() {
        let prompt = from_blocks(&[json!({
            "type": "resource_link", "uri": "https://example.com/doc", "name": "the doc"
        })]);
        assert_eq!(prompt.text, "[the doc](https://example.com/doc)");
    }

    #[test]
    fn an_embedded_buffer_is_put_in_the_text() {
        let prompt = from_blocks(&[json!({
            "type": "resource",
            "resource": {"uri": "file:///C:/p/a.rs", "text": "fn main() {}", "mimeType": "text/x-rust"}
        })]);
        assert_eq!(prompt.text, "C:\\p\\a.rs:\n```\nfn main() {}\n```");
    }

    #[test]
    fn a_binary_resource_is_named_not_included() {
        let prompt = from_blocks(&[json!({
            "type": "resource", "resource": {"uri": "file:///C:/p/a.bin", "blob": "AAAA"}
        })]);
        assert_eq!(prompt.text, "[attached a.bin: binary, not included]");
    }

    #[test]
    fn an_image_is_kept_for_attaching() {
        let prompt = from_blocks(&[
            json!({"type": "text", "text": "what is this?"}),
            json!({"type": "image", "data": "AAAA", "mimeType": "image/jpeg"}),
        ]);
        assert_eq!(prompt.images.len(), 1);
        assert_eq!(prompt.images[0].name, "pasted-image.jpg");
        assert_eq!(prompt.images[0].base64, "AAAA");
    }

    #[test]
    fn nothing_to_send_is_empty() {
        assert!(from_blocks(&[json!({"type": "audio", "data": "x"})]).is_empty());
        assert!(from_blocks(&[]).is_empty());
        // An image alone is a prompt.
        assert!(!from_blocks(&[json!({"type": "image", "data": "AAAA"})]).is_empty());
    }
}
