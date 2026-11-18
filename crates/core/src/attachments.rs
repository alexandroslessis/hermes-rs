//! Attachments from the composer: images (pasted, dropped, picked) and other files.
//!
//! An image is saved first (under the data folder's `images`) and its path rides with the prompt
//! (`SubmitTurn::images`), or is held for the session's next prompt ([`Core::hold_image`]); the
//! runtime sends it to the model, or describes it when the model can't see. Another file is
//! uploaded (saved under `uploads`), then copied into the session's folder
//! (`.hermes/attachments`, [`Core::attach_file`]), and the prompt carries an `@file:` reference to
//! it, which the agent reads like any `@file:` reference.
//!
//! Hermes: `hermes_cli/web_routers/files.py` (`/api/chat/image-upload`, `/api/fs/read-data-url`),
//! `hermes_cli/web_routers/uploads.py` (`/api/chat/file-upload`), `tui_gateway/prompt_attachments.py`
//! (`file.attach`); the page's side is `lib/browser-uploads.ts` and
//! `app/session/hooks/use-prompt-actions/index.ts`.

use std::path::{Path, PathBuf};

use base64::Engine as _;

use crate::Core;

/// The largest image or file the composer may attach (the page's `BROWSER_UPLOAD_MAX_BYTES`).
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
/// The largest image read back with a prompt (Hermes's `_ATTACH_BYTES_MAX_BYTES`).
const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

/// Why an upload or a read failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachError {
    /// The request is wrong (no data, not an image type, not base64).
    Invalid(String),
    /// Over [`MAX_BYTES`].
    TooLarge(String),
    /// No such file.
    NotFound(String),
    /// The disk refused (a folder or a file couldn't be written or read).
    Failed(String),
}

impl AttachError {
    /// The sentence for the user.
    pub fn message(&self) -> &str {
        match self {
            AttachError::Invalid(text)
            | AttachError::TooLarge(text)
            | AttachError::NotFound(text)
            | AttachError::Failed(text) => text,
        }
    }

    fn too_large(what: &str) -> Self {
        AttachError::TooLarge(format!(
            "The {what} is larger than {} MiB.",
            MAX_BYTES >> 20
        ))
    }
}

/// An image saved under the data folder's `images`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedImage {
    pub path: PathBuf,
    /// The file's name (`<time>_<id>_<name>.<ext>`).
    pub name: String,
    pub bytes: usize,
    pub mime: &'static str,
}

/// An uploaded file waiting under `uploads` for [`Core::attach_file`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedUpload {
    pub path: PathBuf,
    pub name: String,
    pub size: usize,
}

/// Where an attached file's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachSource {
    /// A file on disk (an upload's staged path, or a path the page names).
    Path(String),
    /// The bytes as a data URL (or bare base64).
    DataUrl(String),
}

/// A file copied into the session's folder, and the reference the prompt carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    pub name: Option<String>,
    /// Relative to the session's folder when inside it, with `/`.
    pub ref_path: String,
    /// `@file:<ref_path>` (quoted when needed).
    pub ref_text: String,
    pub bytes: usize,
}

const IMAGE_TYPES: [(&str, &str); 6] = [
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
    ("image/bmp", "bmp"),
    ("image/tiff", "tiff"),
];

/// The media type of a file, by its extension.
pub fn mime_for(path: &Path) -> &'static str {
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
    // The counter is unbounded and each name is checked once: a free one always comes.
    (2..)
        .map(|n| folder.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("a free name")
}

/// An image sent as a data URL (`data:image/png;base64,...`), saved under `images`.
pub fn image_from_data_url(
    data_dir: &Path,
    data_url: &str,
    filename: &str,
) -> Result<SavedImage, AttachError> {
    let Some((header, payload)) = data_url.split_once(',') else {
        return Err(AttachError::Invalid("data_url is required".into()));
    };
    let mime = header
        .strip_prefix("data:")
        .and_then(|h| h.strip_suffix(";base64"))
        .unwrap_or_default()
        .to_lowercase();
    let Some((mime, ext)) = IMAGE_TYPES.iter().find(|(m, _)| *m == mime) else {
        return Err(AttachError::Invalid(
            "Attach a PNG, JPEG, GIF, WebP, BMP or TIFF image.".into(),
        ));
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(payload.trim()) else {
        return Err(AttachError::Invalid(
            "The image data isn't valid base64.".into(),
        ));
    };
    save_image(data_dir, &bytes, mime, ext, filename)
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

/// An image sent as bare base64 (`image.attach_bytes`): its type from its bytes, saved under
/// `images`.
pub fn image_from_base64(
    data_dir: &Path,
    content_base64: &str,
    filename: &str,
) -> Result<SavedImage, AttachError> {
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(content_base64.trim()) else {
        return Err(AttachError::Invalid(
            "The image data isn't valid base64.".into(),
        ));
    };
    let Some((mime, ext)) = sniff_image(&bytes) else {
        return Err(AttachError::Invalid(
            "Attach a PNG, JPEG, GIF, WebP, BMP or TIFF image.".into(),
        ));
    };
    save_image(data_dir, &bytes, mime, ext, filename)
}

/// Whether `path` names an image the model can be sent (`image.attach`).
pub fn is_image(path: &Path) -> bool {
    IMAGE_TYPES.iter().any(|(mime, _)| *mime == mime_for(path))
}

fn save_image(
    data_dir: &Path,
    bytes: &[u8],
    mime: &'static str,
    ext: &str,
    filename: &str,
) -> Result<SavedImage, AttachError> {
    if bytes.len() > MAX_BYTES {
        return Err(AttachError::too_large("image"));
    }
    let folder = data_dir.join("images");
    std::fs::create_dir_all(&folder)
        .map_err(|e| AttachError::Failed(format!("Could not create the image folder: {e}")))?;
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
    std::fs::write(&path, bytes)
        .map_err(|e| AttachError::Failed(format!("Could not save the image: {e}")))?;
    Ok(SavedImage {
        path,
        name,
        bytes: bytes.len(),
        mime,
    })
}

/// An uploaded file saved under `uploads` (each in a folder of its own), for
/// [`Core::attach_file`].
pub fn store_upload(
    data_dir: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<StagedUpload, AttachError> {
    if bytes.len() > MAX_BYTES {
        return Err(AttachError::too_large("file"));
    }
    let folder = data_dir
        .join("uploads")
        .join(uuid::Uuid::new_v4().simple().to_string());
    std::fs::create_dir_all(&folder)
        .map_err(|e| AttachError::Failed(format!("Could not create the upload folder: {e}")))?;
    let name = clean_name(name, "attachment");
    let path = folder.join(&name);
    std::fs::write(&path, bytes)
        .map_err(|e| AttachError::Failed(format!("Could not save the file: {e}")))?;
    Ok(StagedUpload {
        path,
        name,
        size: bytes.len(),
    })
}

/// A file as a data URL (image chips, previews), up to the attachment limit.
pub fn read_data_url(path: &str) -> Result<String, AttachError> {
    let path = Path::new(path);
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() as usize <= MAX_BYTES => {}
        Ok(meta) if meta.is_file() => return Err(AttachError::too_large("file")),
        _ => return Err(AttachError::NotFound("No such file.".into())),
    }
    let bytes = std::fs::read(path)
        .map_err(|e| AttachError::Failed(format!("Could not read the file: {e}")))?;
    Ok(format!(
        "data:{};base64,{}",
        mime_for(path),
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// The attached images' bytes, for the prompt.
pub fn image_bytes(paths: &[String]) -> Result<Vec<Vec<u8>>, String> {
    paths
        .iter()
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

/// A file copied into `cwd`'s `.hermes/attachments` (under `name`, else its own name; a taken
/// name gets ` (2)`, ...), and the `@file:` reference the prompt carries.
pub fn attach_file(
    cwd: &Path,
    name: &str,
    source: Option<AttachSource>,
) -> Result<Attached, String> {
    let (bytes, source_name) = match source {
        Some(AttachSource::Path(path)) => {
            let size = std::fs::metadata(&path)
                .map_err(|e| format!("Can't read {path}: {e}"))?
                .len();
            if size as usize > MAX_BYTES {
                return Err(format!("The file is larger than {} MiB.", MAX_BYTES >> 20));
            }
            let bytes = std::fs::read(&path).map_err(|e| format!("Can't read {path}: {e}"))?;
            (
                bytes,
                Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string()),
            )
        }
        Some(AttachSource::DataUrl(data_url)) => {
            let payload = data_url
                .split_once(',')
                .map_or(data_url.as_str(), |(_, p)| p);
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(payload.trim())
                .map_err(|_| "The file data isn't valid base64.".to_string())?;
            (bytes, None)
        }
        None => return Err("path or data_url required".into()),
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
    Ok(Attached {
        name: target.file_name().map(|n| n.to_string_lossy().to_string()),
        ref_text: format!(
            "@file:{}",
            hermes_tools::references::format_value(&reference)
        ),
        ref_path: reference,
        bytes: bytes.len(),
    })
}

impl Core {
    /// [`attach_file`] into the session's folder. Blocking.
    pub fn attach_file(
        &self,
        session_id: &str,
        name: &str,
        source: Option<AttachSource>,
    ) -> Result<Attached, String> {
        let record = self
            .runtime
            .store()
            .session(session_id)
            .ok()
            .flatten()
            .ok_or(format!("No session {session_id}."))?;
        attach_file(Path::new(&record.cwd), name, source)
    }

    /// Holds an image for the session's next prompt (the session's actor keeps it,
    /// `PageState::images`); how many are held now.
    pub fn hold_image(&self, session_id: &str, path: &str) -> Result<usize, String> {
        self.runtime
            .session_handle(session_id)
            .map_err(|e| format!("{e:#}"))
            .and_then(|handle| handle.attach_image(path).map_err(|e| e.to_string()))
    }

    /// Lets go of a held image: whether it was held, and how many are held now.
    pub fn detach_image(&self, session_id: &str, path: &str) -> Result<(bool, usize), String> {
        match self.runtime.existing_session_handle(session_id) {
            Some(handle) => handle.detach_image(path).map_err(|e| e.to_string()),
            None => Ok((false, 0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1×1 PNG.
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    #[test]
    fn an_uploaded_image_is_saved_and_read_back_with_the_prompt() {
        let dir = hermes_testdir::tempdir().unwrap();
        let saved = image_from_data_url(
            dir.path(),
            &format!("data:image/png;base64,{PNG}"),
            "shot.png",
        )
        .unwrap();
        assert_eq!(saved.mime, "image/png");
        let path = saved.path.display().to_string();
        assert!(path.ends_with("_shot.png"));
        let images = image_bytes(std::slice::from_ref(&path)).unwrap();
        assert_eq!(images.len(), 1);
        assert!(images[0].starts_with(b"\x89PNG"));
        assert!(
            read_data_url(&path)
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        // Bytes alone (`image.attach_bytes`): the type comes from the bytes.
        let sniffed = image_from_base64(dir.path(), PNG, "x").unwrap();
        assert!(sniffed.path.display().to_string().ends_with("_x.png"));
        assert!(is_image(Path::new("a.JPG")) && !is_image(Path::new("a.svg")));
        // Not an image type: refused.
        assert!(matches!(
            image_from_data_url(dir.path(), "data:text/plain;base64,aGk=", ""),
            Err(AttachError::Invalid(_))
        ));
        assert!(image_bytes(&["C:/no/such.png".to_string()]).is_err());
        assert!(matches!(
            read_data_url("C:/no/such.png"),
            Err(AttachError::NotFound(_))
        ));
    }

    #[test]
    fn an_uploaded_file_is_copied_into_the_session_and_referenced() {
        let data = hermes_testdir::tempdir().unwrap();
        let session = hermes_testdir::tempdir().unwrap();
        let staged = store_upload(data.path(), "my notes.txt", b"hello").unwrap();
        assert_eq!(staged.size, 5);
        let source = || Some(AttachSource::Path(staged.path.display().to_string()));
        let attached = attach_file(session.path(), "my notes.txt", source()).unwrap();
        assert_eq!(attached.ref_path, ".hermes/attachments/my notes.txt");
        assert_eq!(
            attached.ref_text,
            "@file:`.hermes/attachments/my notes.txt`"
        );
        assert_eq!(
            std::fs::read_to_string(session.path().join(".hermes/attachments/my notes.txt"))
                .unwrap(),
            "hello"
        );
        // The same name again gets its own copy.
        let again = attach_file(session.path(), "my notes.txt", source()).unwrap();
        assert_eq!(again.ref_path, ".hermes/attachments/my notes (2).txt");
        // Bytes from the page (data_url) work too; nothing at all is refused.
        let inline = attach_file(
            session.path(),
            "a.md",
            Some(AttachSource::DataUrl("data:text/plain;base64,aGk=".into())),
        )
        .unwrap();
        assert_eq!(inline.ref_text, "@file:.hermes/attachments/a.md");
        assert!(attach_file(session.path(), "x", None).is_err());
    }
}
