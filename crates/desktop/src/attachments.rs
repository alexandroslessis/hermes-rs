//! Attachments from the composer in Hermes's shapes: the upload routes (`POST
//! /api/chat/image-upload`, `POST /api/chat/file-upload`, `GET /api/fs/read-data-url`) and
//! `file.attach`'s answer. Saving, reading and copying the files is `hermes_core::attachments`.
//!
//! Hermes: `hermes_cli/web_routers/files.py` (`/api/chat/image-upload`, `/api/fs/read-data-url`),
//! `hermes_cli/web_routers/uploads.py` (`/api/chat/file-upload`), `tui_gateway/prompt_attachments.py`
//! (`file.attach`); the page's side is `lib/browser-uploads.ts` and
//! `app/session/hooks/use-prompt-actions/index.ts`.

use std::path::Path;

use hermes_core::attachments::{self as core, AttachError, AttachSource, Attached, SavedImage};
use serde_json::{Value, json};

pub use hermes_core::attachments::{MAX_BYTES, is_image};

type Answer = (u16, Value);

/// A refusal as the route's status and `{"detail"}`.
fn refused(error: AttachError) -> Answer {
    let status = match &error {
        AttachError::Invalid(_) => 400,
        AttachError::TooLarge(_) => 413,
        AttachError::NotFound(_) => 404,
        AttachError::Failed(_) => 500,
    };
    (status, json!({"detail": error.message()}))
}

fn saved(image: SavedImage) -> Answer {
    (
        200,
        json!({"ok": true, "path": image.path.display().to_string(), "name": image.name,
               "bytes": image.bytes, "mime_type": image.mime}),
    )
}

/// `POST /api/chat/image-upload` `{data_url, filename}`: the image saved under `images`.
pub fn upload_image(data_dir: &Path, body: &Value) -> Answer {
    core::image_from_data_url(
        data_dir,
        body["data_url"].as_str().unwrap_or_default(),
        body["filename"].as_str().unwrap_or_default(),
    )
    .map_or_else(refused, saved)
}

/// `image.attach_bytes` `{content_base64, filename}`: the image saved under `images`.
pub fn image_from_base64(data_dir: &Path, params: &Value) -> Answer {
    core::image_from_base64(
        data_dir,
        params["content_base64"].as_str().unwrap_or_default(),
        params["filename"].as_str().unwrap_or_default(),
    )
    .map_or_else(refused, saved)
}

/// `POST /api/chat/file-upload` (multipart `file`): the file saved under `uploads`, with the
/// receipt `file.attach` takes back (`staged_upload`).
pub fn store_upload(data_dir: &Path, name: &str, bytes: &[u8]) -> Answer {
    match core::store_upload(data_dir, name, bytes) {
        Ok(staged) => {
            let path = staged.path.display().to_string();
            (
                200,
                json!({"ok": true, "path": path, "size": staged.size,
                       "staged_upload": {"path": path, "name": staged.name, "size": staged.size}}),
            )
        }
        Err(error) => refused(error),
    }
}

/// `GET /api/fs/read-data-url?path=`: a file as a data URL (image chips, previews), up to the
/// attachment limit.
pub fn read_data_url(path: &str) -> Answer {
    match core::read_data_url(path) {
        Ok(data_url) => (200, json!({"dataUrl": data_url})),
        Err(error) => refused(error),
    }
}

/// `prompt.submit`'s `attachments` (`[{path, mime}]`): the paths.
pub fn image_paths(attachments: &Value) -> Vec<String> {
    attachments
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["path"].as_str())
        .map(str::to_string)
        .collect()
}

/// `file.attach`'s `{staged_upload | path | data_url}`: where the bytes come from.
pub fn attach_source(params: &Value) -> Option<AttachSource> {
    if let Some(path) = params["staged_upload"]["path"]
        .as_str()
        .or_else(|| params["path"].as_str())
        .filter(|p| !p.is_empty())
    {
        Some(AttachSource::Path(path.to_string()))
    } else {
        params["data_url"]
            .as_str()
            .map(|data_url| AttachSource::DataUrl(data_url.to_string()))
    }
}

/// `file.attach`'s answer (`FileAttachResponse`).
pub fn attached(attached: Attached) -> Value {
    json!({
        "attached": true,
        "name": attached.name,
        "ref_path": attached.ref_path,
        "ref_text": attached.ref_text,
        "bytes": attached.bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1×1 PNG.
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    #[test]
    fn the_upload_routes_answer_hermes_shapes() {
        let dir = hermes_testdir::tempdir().unwrap();
        let (status, body) = upload_image(
            dir.path(),
            &json!({"data_url": format!("data:image/png;base64,{PNG}"), "filename": "shot.png"}),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["mime_type"], "image/png");
        assert!(body["bytes"].as_u64().unwrap() > 0);
        let path = body["path"].as_str().unwrap();
        assert!(
            read_data_url(path).1["dataUrl"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert_eq!(
            image_paths(&json!([{"path": path, "mime": "image/png"}, {"mime": "x"}])),
            [path]
        );
        let (status, refused) = upload_image(
            dir.path(),
            &json!({"data_url": "data:text/plain;base64,aGk="}),
        );
        assert_eq!(status, 400);
        assert_eq!(
            refused["detail"],
            "Attach a PNG, JPEG, GIF, WebP, BMP or TIFF image."
        );
        assert_eq!(read_data_url("C:/no/such.png").0, 404);
        let (status, staged) = store_upload(dir.path(), "a.txt", b"hi");
        assert_eq!(status, 200);
        assert_eq!(staged["staged_upload"]["name"], "a.txt");
        assert_eq!(staged["path"], staged["staged_upload"]["path"]);
    }

    #[test]
    fn file_attach_reads_its_source_from_the_page_params() {
        assert_eq!(
            attach_source(&json!({"staged_upload": {"path": "C:/u/a"}, "path": "C:/b"})),
            Some(AttachSource::Path("C:/u/a".into()))
        );
        assert_eq!(
            attach_source(&json!({"path": "", "data_url": "data:,aGk="})),
            Some(AttachSource::DataUrl("data:,aGk=".into()))
        );
        assert_eq!(attach_source(&json!({"name": "x"})), None);
    }
}
