//! Attachments from the composer: images (pasted, dropped, picked) and other files.
//!
//! An image is uploaded first (`POST /api/chat/image-upload`, a data URL, saved under the data
//! folder's `images`) and its path rides with the prompt (`prompt.submit`'s `attachments`); the
//! runtime sends it to the model, or describes it when the model can't see. Another file is
//! uploaded (`POST /api/chat/file-upload`, saved under `uploads`), then copied into the
//! session's folder (`file.attach`, `.hermes/attachments`), and the prompt carries an
//! `@file:` reference to it, which the agent reads like any `@file:` reference.
//!
//! Hermes: `hermes_cli/web_routers/files.py` (`/api/chat/image-upload`, `/api/fs/read-data-url`),
//! `hermes_cli/web_routers/uploads.py` (`/api/chat/file-upload`), `tui_gateway/prompt_attachments.py`
//! (`file.attach`); the page's side is `lib/browser-uploads.ts` and
//! `app/session/hooks/use-prompt-actions/index.ts`.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Value, json};

/// The largest image or file the composer may attach (the page's `BROWSER_UPLOAD_MAX_BYTES`).
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
/// The largest image read back with a prompt (Hermes's `_ATTACH_BYTES_MAX_BYTES`).
const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

type Answer = (u16, Value);

fn error(status: u16, detail: impl Into<String>) -> Answer {
    (status, json!({"detail": detail.into()}))
}

const IMAGE_TYPES: [(&str, &str); 6] = [
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
    ("image/bmp", "bmp"),
    ("image/tiff", "tiff"),
];

fn mime_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "md" | "log" | "csv" => "text/plain",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

/// A file name safe to write: the last path segment, without control characters or leading dots.
fn clean_name(name: &str, fallback: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

/// A path in `folder` for `name` that doesn't exist yet (`name`, `name (2)`, …).
fn unique_path(folder: &Path, name: &str) -> PathBuf {
    let candidate = folder.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| folder.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("a free name")
}

/// `POST /api/chat/image-upload` `{data_url, filename}`: the image saved under `images`.
pub fn upload_image(data_dir: &Path, body: &Value) -> Answer {
    let data_url = body["data_url"].as_str().unwrap_or_default();
    let Some((header, payload)) = data_url.split_once(',') else {
        return error(400, "data_url is required");
    };
    let mime = header
        .strip_prefix("data:")
        .and_then(|h| h.strip_suffix(";base64"))
        .unwrap_or_default()
        .to_lowercase();
    let Some((_, ext)) = IMAGE_TYPES.iter().find(|(m, _)| *m == mime) else {
        return error(400, "Attach a PNG, JPEG, GIF, WebP, BMP or TIFF image.");
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(payload.trim()) else {
        return error(400, "The image data isn't valid base64.");
    };
    save_image(
        data_dir,
        &bytes,
        &mime,
        ext,
        body["filename"].as_str().unwrap_or_default(),
    )
}

/// The image type of `bytes`, by their first bytes: `(mime, extension)`.
fn sniff_image(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    let kind = match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        [b'B', b'M', ..] => "image/bmp",
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => "image/tiff",
        _ => return None,
    };
    IMAGE_TYPES.iter().find(|(m, _)| *m == kind).copied()
}

/// `image.attach_bytes` `{content_base64, filename}`: the image saved under `images`.
pub fn image_from_base64(data_dir: &Path, params: &Value) -> Answer {
    let payload = params["content_base64"].as_str().unwrap_or_default();
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(payload.trim()) else {
        return error(400, "The image data isn't valid base64.");
    };
    let Some((mime, ext)) = sniff_image(&bytes) else {
        return error(400, "Attach a PNG, JPEG, GIF, WebP, BMP or TIFF image.");
    };
    save_image(
        data_dir,
        &bytes,
        mime,
        ext,
        params["filename"].as_str().unwrap_or_default(),
    )
}

/// Whether `path` names an image the model can be sent (`image.attach`).
pub fn is_image(path: &Path) -> bool {
    IMAGE_TYPES.iter().any(|(mime, _)| *mime == mime_for(path))
}

fn save_image(data_dir: &Path, bytes: &[u8], mime: &str, ext: &str, filename: &str) -> Answer {
    if bytes.len() > MAX_BYTES {
        return error(
            413,
            format!("The image is larger than {} MiB.", MAX_BYTES >> 20),
        );
    }
    let folder = data_dir.join("images");
    if let Err(e) = std::fs::create_dir_all(&folder) {
        return error(500, format!("Could not create the image folder: {e}"));
    }
    let stem = clean_name(filename, "pasted-image");
    let stem = Path::new(&stem)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "pasted-image".into());
    let name = format!(
        "{}_{}_{stem}.{ext}",
        chrono::Local::now().format("%Y%m%d_%H%M%S"),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let path = folder.join(&name);
    if let Err(e) = std::fs::write(&path, bytes) {
        return error(500, format!("Could not save the image: {e}"));
    }
    (
        200,
        json!({"ok": true, "path": path.display().to_string(), "name": name,
               "bytes": bytes.len(), "mime_type": mime}),
    )
}

/// `POST /api/chat/file-upload` (multipart `file`): the file saved under `uploads`, with the
/// receipt `file.attach` takes back (`staged_upload`).
pub fn store_upload(data_dir: &Path, name: &str, bytes: &[u8]) -> Answer {
    if bytes.len() > MAX_BYTES {
        return error(
            413,
            format!("The file is larger than {} MiB.", MAX_BYTES >> 20),
        );
    }
    let folder = data_dir
        .join("uploads")
        .join(uuid::Uuid::new_v4().simple().to_string());
    if let Err(e) = std::fs::create_dir_all(&folder) {
        return error(500, format!("Could not create the upload folder: {e}"));
    }
    let name = clean_name(name, "attachment");
    let path = folder.join(&name);
    if let Err(e) = std::fs::write(&path, bytes) {
        return error(500, format!("Could not save the file: {e}"));
    }
    let path = path.display().to_string();
    (
        200,
        json!({"ok": true, "path": path, "size": bytes.len(),
               "staged_upload": {"path": path, "name": name, "size": bytes.len()}}),
    )
}

/// `GET /api/fs/read-data-url?path=`: a file as a data URL (image chips, previews), up to the
/// attachment limit.
pub fn read_data_url(path: &str) -> Answer {
    let path = Path::new(path);
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() as usize <= MAX_BYTES => {}
        Ok(meta) if meta.is_file() => {
            return error(
                413,
                format!("The file is larger than {} MiB.", MAX_BYTES >> 20),
            );
        }
        _ => return error(404, "No such file."),
    }
    match std::fs::read(path) {
        Ok(bytes) => (
            200,
            json!({"dataUrl": format!(
                "data:{};base64,{}",
                mime_for(path),
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )}),
        ),
        Err(e) => error(500, format!("Could not read the file: {e}")),
    }
}

/// `prompt.submit`'s `attachments` (`[{path, mime}]`): the images' bytes.
pub fn image_bytes(attachments: &Value) -> Result<Vec<Vec<u8>>, String> {
    let Some(list) = attachments.as_array() else {
        return Ok(Vec::new());
    };
    list.iter()
        .filter_map(|a| a["path"].as_str())
        .map(|path| {
            let size = std::fs::metadata(path)
                .map_err(|e| format!("The attached image {path} can't be read: {e}"))?
                .len();
            if size > MAX_IMAGE_BYTES {
                return Err(format!("The attached image {path} is too large."));
            }
            std::fs::read(path).map_err(|e| format!("The attached image {path} can't be read: {e}"))
        })
        .collect()
}

/// `file.attach` `{name, staged_upload | path | data_url}`: the file copied into the session's
/// folder (`.hermes/attachments`), and the `@file:` reference the prompt carries
/// (`FileAttachResponse`).
pub fn attach_file(cwd: &Path, params: &Value) -> Result<Value, String> {
    let name = params["name"].as_str().unwrap_or_default();
    let (bytes, source_name) = if let Some(path) = params["staged_upload"]["path"]
        .as_str()
        .or_else(|| params["path"].as_str())
        .filter(|p| !p.is_empty())
    {
        let size = std::fs::metadata(path)
            .map_err(|e| format!("Can't read {path}: {e}"))?
            .len();
        if size as usize > MAX_BYTES {
            return Err(format!("The file is larger than {} MiB.", MAX_BYTES >> 20));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("Can't read {path}: {e}"))?;
        (
            bytes,
            Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string()),
        )
    } else if let Some(data_url) = params["data_url"].as_str() {
        let payload = data_url.split_once(',').map_or(data_url, |(_, p)| p);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload.trim())
            .map_err(|_| "The file data isn't valid base64.".to_string())?;
        (bytes, None)
    } else {
        return Err("path or data_url required".into());
    };
    let folder = cwd.join(".hermes").join("attachments");
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("Can't create {}: {e}", folder.display()))?;
    let name = clean_name(
        if name.is_empty() {
            source_name.as_deref().unwrap_or_default()
        } else {
            name
        },
        "attachment",
    );
    let target = unique_path(&folder, &name);
    std::fs::write(&target, &bytes)
        .map_err(|e| format!("Can't write {}: {e}", target.display()))?;
    let reference = target
        .strip_prefix(cwd)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| target.display().to_string());
    Ok(json!({
        "attached": true,
        "name": target.file_name().map(|n| n.to_string_lossy().to_string()),
        "ref_path": reference,
        "ref_text": format!("@file:{}", hermes_tools::references::format_value(&reference)),
        "bytes": bytes.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1×1 PNG.
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    #[test]
    fn an_uploaded_image_is_saved_and_read_back_with_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let (status, body) = upload_image(
            dir.path(),
            &json!({"data_url": format!("data:image/png;base64,{PNG}"), "filename": "shot.png"}),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["mime_type"], "image/png");
        let path = body["path"].as_str().unwrap();
        assert!(path.ends_with("_shot.png"));
        let images = image_bytes(&json!([{"path": path, "mime": "image/png"}])).unwrap();
        assert_eq!(images.len(), 1);
        assert!(images[0].starts_with(b"\x89PNG"));
        assert!(
            read_data_url(path).1["dataUrl"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        // Bytes alone (`image.attach_bytes`): the type comes from the bytes.
        let (status, sniffed) =
            image_from_base64(dir.path(), &json!({"content_base64": PNG, "filename": "x"}));
        assert_eq!(status, 200);
        assert!(sniffed["path"].as_str().unwrap().ends_with("_x.png"));
        assert!(is_image(Path::new("a.JPG")) && !is_image(Path::new("a.svg")));
        // Not an image type: refused.
        assert_eq!(
            upload_image(
                dir.path(),
                &json!({"data_url": "data:text/plain;base64,aGk="})
            )
            .0,
            400
        );
        assert!(image_bytes(&json!([{"path": "C:/no/such.png"}])).is_err());
    }

    #[test]
    fn an_uploaded_file_is_copied_into_the_session_and_referenced() {
        let data = tempfile::tempdir().unwrap();
        let session = tempfile::tempdir().unwrap();
        let (status, body) = store_upload(data.path(), "my notes.txt", b"hello");
        assert_eq!(status, 200);
        let staged = body["staged_upload"].clone();
        let attached = attach_file(
            session.path(),
            &json!({"name": "my notes.txt", "staged_upload": staged}),
        )
        .unwrap();
        assert_eq!(attached["ref_path"], ".hermes/attachments/my notes.txt");
        assert_eq!(
            attached["ref_text"],
            "@file:`.hermes/attachments/my notes.txt`"
        );
        assert_eq!(
            std::fs::read_to_string(session.path().join(".hermes/attachments/my notes.txt"))
                .unwrap(),
            "hello"
        );
        // The same name again gets its own copy.
        let again = attach_file(
            session.path(),
            &json!({"name": "my notes.txt", "staged_upload": staged}),
        )
        .unwrap();
        assert_eq!(again["ref_path"], ".hermes/attachments/my notes (2).txt");
        // Bytes from the page (data_url) work too; nothing at all is refused.
        let inline = attach_file(
            session.path(),
            &json!({"name": "a.md", "data_url": "data:text/plain;base64,aGk="}),
        )
        .unwrap();
        assert_eq!(inline["ref_text"], "@file:.hermes/attachments/a.md");
        assert!(attach_file(session.path(), &json!({"name": "x"})).is_err());
    }
}
