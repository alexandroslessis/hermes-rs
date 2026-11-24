//! Attachments of the composer, with no window in them: what an attachment is ([`Attachment`]), the
//! limits (the core's byte limit, a count limit of our own), and the work done off the UI thread
//! when one is added ([`prepare_path`], [`prepare_bytes`]: check the size and the kind, decode an
//! image and shrink it to a thumbnail so a 16 MiB photo never reaches the renderer whole).
//!
//! What the core enforces and where this module follows it (`hermes_core::attachments`):
//! - a file or image over [`MAX_BYTES`] (16 MiB) is refused, with the core's sentence;
//! - an image is one of PNG, JPEG, GIF, WebP, BMP, TIFF (`is_image`, by extension for a file, by
//!   its first bytes for pasted data); anything else, SVG and PDF included, is an ordinary file
//!   that the agent reads through an `@file:` reference;
//! - the core has no limit on how many: [`MAX_ATTACHMENTS`] is t3code's 100 per message
//!   (`packages/contracts/src/chatAttachment.ts`, `PROVIDER_SEND_TURN_MAX_ATTACHMENTS`).
//!
//! The core checks again when the prompt is sent (`Backend::send`), so a file that grew or moved in
//! between is refused there with the core's own text.
//!
//! t3code: `apps/web/src/components/chat/ComposerAttachmentStrip.tsx` (the 64 px tiles, the file
//! rows with a name and a size), `ComposerImageThumbnail.tsx` (a thumbnail made off the render
//! path), `composerAttachmentFiles.ts` (duplicates, limits), `lib/imageCompression.ts`.
//!
//! Hermes: `lib/browser-uploads.ts` of the page (`BROWSER_UPLOAD_MAX_BYTES`).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hermes_core::attachments::{MAX_BYTES, is_image, mime_for};

/// The most attachments one message carries (t3code's `PROVIDER_SEND_TURN_MAX_ATTACHMENTS`; the
/// core has no limit).
pub const MAX_ATTACHMENTS: usize = 100;
/// The longest edge of a thumbnail, in pixels (the strip's tiles are 64 px, the timeline's 101 px
/// wide, at up to 2x).
pub const THUMB_EDGE: u32 = 256;
/// The most memory a decode may take: a photo that needs more is attached without a thumbnail.
const DECODE_ALLOC: u64 = 512 * 1024 * 1024;

/// What an attachment is for the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Sent with the prompt for the model to see.
    Image,
    /// Copied into the thread's folder and referenced as `@file:`.
    File,
}

/// Where an attachment's bytes come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A file on disk (dropped, picked, or pasted from Explorer).
    Path(PathBuf),
    /// Bytes with no file (an image pasted from the clipboard).
    Bytes { data: Arc<Vec<u8>>, mime: String },
}

/// An attachment waiting in the composer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    /// Names it for the strip's elements and for removal.
    pub id: u64,
    /// The name shown (a file's own, `pasted-image.png` for clipboard data).
    pub name: String,
    pub size: usize,
    pub kind: Kind,
    pub source: Source,
    /// The thumbnail (PNG, at most [`THUMB_EDGE`] on a side) of an image that could be decoded.
    pub thumb: Option<Arc<Vec<u8>>>,
    /// What [`duplicate_key`] says; two attachments with the same key are the same file.
    pub key: String,
}

/// What preparing a file or some bytes found out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub name: String,
    pub size: usize,
    pub kind: Kind,
    pub source: Source,
    pub thumb: Option<Arc<Vec<u8>>>,
    pub key: String,
}

impl Prepared {
    /// The attachment this becomes, with its id.
    pub fn into_attachment(self, id: u64) -> Attachment {
        Attachment {
            id,
            name: self.name,
            size: self.size,
            kind: self.kind,
            source: self.source,
            thumb: self.thumb,
            key: self.key,
        }
    }
}

/// Why something could not be attached: the sentence the user is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `1.5 MB`, `320 KB`, `12 B`: a size as t3code's strip shows it (`formatAttachmentSize`).
pub fn format_size(bytes: usize) -> String {
    const KB: f64 = 1024.;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{} KB", trim_one(b / KB))
    } else {
        format!("{} MB", trim_one(b / (KB * KB)))
    }
}

/// One decimal, none for a whole number below 100 (`1.5`, `12`, `320`).
fn trim_one(value: f64) -> String {
    if value >= 100. {
        format!("{value:.0}")
    } else {
        let text = format!("{value:.1}");
        text.strip_suffix(".0").map(str::to_string).unwrap_or(text)
    }
}

/// The sentence for a file over the core's limit (`AttachError::too_large`'s wording).
fn too_large(name: &str, what: &str) -> Refusal {
    Refusal(format!(
        "{name}: The {what} is larger than {} MiB.",
        MAX_BYTES >> 20
    ))
}

/// The sentence for one more attachment than a message takes.
pub fn too_many() -> Refusal {
    Refusal(format!(
        "A message takes at most {MAX_ATTACHMENTS} attachments."
    ))
}

/// What identifies a file: its path as the system spells it (case folded: Windows paths are), or
/// the content of pasted bytes.
pub fn duplicate_key(source: &Source) -> String {
    match source {
        Source::Path(path) => {
            let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            format!(
                "path:{}",
                canonical
                    .to_string_lossy()
                    .trim_start_matches(r"\\?\")
                    .to_lowercase()
            )
        }
        Source::Bytes { data, .. } => {
            let mut hasher = DefaultHasher::new();
            data.hash(&mut hasher);
            format!("bytes:{}:{:x}", data.len(), hasher.finish())
        }
    }
}

/// Checks a file and reads what the strip shows of it. Blocking: run it off the UI thread.
pub fn prepare_path(path: &Path) -> Result<Prepared, Refusal> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let meta = std::fs::metadata(path).map_err(|e| {
        Refusal(match e.kind() {
            std::io::ErrorKind::NotFound => format!("{name}: No such file."),
            _ => format!("{name}: Can't read it ({e})."),
        })
    })?;
    if meta.is_dir() {
        return Err(Refusal(format!(
            "{name} is a folder. Type @ to mention a folder instead."
        )));
    }
    let image = is_image(path);
    if meta.len() as usize > MAX_BYTES {
        return Err(too_large(&name, if image { "image" } else { "file" }));
    }
    let source = Source::Path(path.to_path_buf());
    let key = duplicate_key(&source);
    if !image {
        // Opening it proves it can be read; the bytes stay on disk until the send.
        std::fs::File::open(path).map_err(|e| Refusal(format!("{name}: Can't read it ({e}).")))?;
        return Ok(Prepared {
            name,
            size: meta.len() as usize,
            kind: Kind::File,
            source,
            thumb: None,
            key,
        });
    }
    let bytes =
        std::fs::read(path).map_err(|e| Refusal(format!("{name}: Can't read it ({e}).")))?;
    let thumb = match thumbnail(&bytes) {
        Ok(png) => Some(Arc::new(png)),
        // Too big to shrink here: still an image the model can be sent.
        Err(Thumb::TooBig) => None,
        Err(Thumb::Unreadable(why)) => return Err(Refusal(format!("{name}: {why}"))),
    };
    Ok(Prepared {
        name,
        size: bytes.len(),
        kind: Kind::Image,
        source,
        thumb,
        key,
    })
}

/// Checks pasted image bytes (named `name`, `pasted-image.png` when empty).
pub fn prepare_bytes(bytes: Vec<u8>, name: &str) -> Result<Prepared, Refusal> {
    let name = if name.trim().is_empty() {
        "pasted-image.png".to_string()
    } else {
        name.to_string()
    };
    if bytes.len() > MAX_BYTES {
        return Err(too_large(&name, "image"));
    }
    let Some(mime) = sniff(&bytes) else {
        return Err(Refusal(format!(
            "{name}: Attach a PNG, JPEG, GIF, WebP, BMP or TIFF image."
        )));
    };
    let thumb = match thumbnail(&bytes) {
        Ok(png) => Some(Arc::new(png)),
        Err(Thumb::TooBig) => None,
        Err(Thumb::Unreadable(why)) => return Err(Refusal(format!("{name}: {why}"))),
    };
    let size = bytes.len();
    let source = Source::Bytes {
        data: Arc::new(bytes),
        mime: mime.to_string(),
    };
    let key = duplicate_key(&source);
    Ok(Prepared {
        name,
        size,
        kind: Kind::Image,
        source,
        thumb,
        key,
    })
}

/// The media type of image bytes, from their first bytes (the core's `sniff_image`, which is not
/// public).
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    Some(match bytes {
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
    })
}

/// The file name of image bytes of media type `mime` (`pasted-image.png`).
pub fn pasted_name(mime: &str) -> String {
    let ext = match mime {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        _ => "png",
    };
    format!("pasted-image.{ext}")
}

/// The media type a file of this name has.
pub fn mime_of(path: &Path) -> &'static str {
    mime_for(path)
}

enum Thumb {
    /// The bytes are not an image the decoder reads.
    Unreadable(String),
    /// The decoder would need more memory than it may take.
    TooBig,
}

/// An image shrunk to fit [`THUMB_EDGE`], as PNG.
fn thumbnail(bytes: &[u8]) -> Result<Vec<u8>, Thumb> {
    use image::{ImageReader, Limits};
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| Thumb::Unreadable(format!("Not an image ({e}).")))?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(DECODE_ALLOC);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|e| match e {
        image::ImageError::Limits(_) => Thumb::TooBig,
        other => Thumb::Unreadable(format!("Not an image it can read ({other}).")),
    })?;
    let small = decoded.thumbnail(THUMB_EDGE, THUMB_EDGE);
    let mut png = Vec::new();
    small
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| Thumb::Unreadable(format!("Could not make a thumbnail ({e}).")))?;
    Ok(png)
}

/// A PNG of `w` x `h` with a gradient tinted by `shade`, as bytes (stories).
pub fn sample_png(w: u32, h: u32, shade: u8) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([
            (x * 255 / w.max(1)) as u8,
            (y * 255 / h.max(1)) as u8,
            shade,
            255,
        ])
    });
    let mut out = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// A PNG of `w` x `h` with a gradient, as bytes (tests).
#[cfg(test)]
pub fn test_png(w: u32, h: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
    });
    let mut out = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// A decoded image's size, for tests.
#[cfg(test)]
pub fn png_size(png: &[u8]) -> (u32, u32) {
    let image = image::load_from_memory(png).expect("a png");
    (image.width(), image.height())
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::test_png as png;

    #[test]
    fn sizes_read_as_t3code_writes_them() {
        assert_eq!(format_size(12), "12 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(320 * 1024), "320 KB");
        assert_eq!(format_size(2 * 1024 * 1024), "2 MB");
        assert_eq!(format_size(16 * 1024 * 1024), "16 MB");
    }

    #[test]
    fn an_image_file_gets_a_small_thumbnail() {
        let dir = hermes_testdir::tempdir().unwrap();
        let path = dir.path().join("big.png");
        std::fs::write(&path, png(1200, 800)).unwrap();
        let prepared = prepare_path(&path).unwrap();
        assert_eq!(prepared.kind, Kind::Image);
        assert_eq!(prepared.name, "big.png");
        let (w, h) = png_size(prepared.thumb.as_ref().unwrap());
        assert!(
            w <= THUMB_EDGE && h <= THUMB_EDGE && w == THUMB_EDGE,
            "{w}x{h}"
        );
        // The aspect ratio is kept.
        assert_eq!(h, 171);
    }

    #[test]
    fn another_file_is_a_file_without_a_thumbnail() {
        let dir = hermes_testdir::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "hello").unwrap();
        let prepared = prepare_path(&path).unwrap();
        assert_eq!(prepared.kind, Kind::File);
        assert_eq!(prepared.size, 5);
        assert!(prepared.thumb.is_none());
        // An svg is not an image the model can be sent: a file.
        let svg = dir.path().join("logo.svg");
        std::fs::write(&svg, "<svg/>").unwrap();
        assert_eq!(prepare_path(&svg).unwrap().kind, Kind::File);
    }

    #[test]
    fn a_missing_file_a_folder_and_a_broken_image_are_refused_with_a_sentence() {
        let dir = hermes_testdir::tempdir().unwrap();
        let missing = prepare_path(&dir.path().join("gone.txt")).unwrap_err();
        assert_eq!(missing.0, "gone.txt: No such file.");
        let folder = prepare_path(dir.path()).unwrap_err();
        assert!(folder.0.contains("is a folder"), "{folder}");
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, b"not a png at all").unwrap();
        let refusal = prepare_path(&bad).unwrap_err();
        assert!(refusal.0.starts_with("bad.png: "), "{refusal}");
    }

    #[test]
    fn a_file_over_the_cores_limit_is_refused_with_its_wording() {
        let dir = hermes_testdir::tempdir().unwrap();
        let path = dir.path().join("huge.bin");
        // Sparse: no real 16 MiB written to disk.
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_BYTES as u64 + 1).unwrap();
        drop(file);
        let refusal = prepare_path(&path).unwrap_err();
        assert_eq!(refusal.0, "huge.bin: The file is larger than 16 MiB.");
        let image = dir.path().join("huge.png");
        let file = std::fs::File::create(&image).unwrap();
        file.set_len(MAX_BYTES as u64 + 1).unwrap();
        drop(file);
        assert_eq!(
            prepare_path(&image).unwrap_err().0,
            "huge.png: The image is larger than 16 MiB."
        );
    }

    #[test]
    fn pasted_bytes_are_checked_by_their_first_bytes() {
        let prepared = prepare_bytes(png(10, 10), "").unwrap();
        assert_eq!(prepared.name, "pasted-image.png");
        assert!(matches!(&prepared.source, Source::Bytes { mime, .. } if mime == "image/png"));
        assert!(prepared.thumb.is_some());
        let refusal = prepare_bytes(b"hello".to_vec(), "x").unwrap_err();
        assert!(refusal.0.contains("PNG, JPEG"), "{refusal}");
        assert_eq!(pasted_name("image/jpeg"), "pasted-image.jpg");
    }

    #[test]
    fn the_same_file_has_the_same_key_however_it_is_spelled() {
        let dir = hermes_testdir::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "a").unwrap();
        let spelled = dir.path().join(".").join("A.TXT");
        let a = duplicate_key(&Source::Path(path.clone()));
        let b = duplicate_key(&Source::Path(spelled));
        // Windows folds case; elsewhere the second path does not exist and keys differ.
        if cfg!(windows) {
            assert_eq!(a, b);
        }
        let same = Source::Bytes {
            data: Arc::new(vec![1, 2, 3]),
            mime: "image/png".into(),
        };
        assert_eq!(duplicate_key(&same), duplicate_key(&same.clone()));
        let other = Source::Bytes {
            data: Arc::new(vec![1, 2, 4]),
            mime: "image/png".into(),
        };
        assert_ne!(duplicate_key(&same), duplicate_key(&other));
    }

    #[test]
    fn an_image_the_decoder_may_not_hold_is_attached_without_a_thumbnail() {
        // A 1x1 header claiming a huge size cannot be built cheaply; the limit path is the
        // decoder's own `Limits` error, which maps to "no thumbnail".
        let dir = hermes_testdir::tempdir().unwrap();
        let path = dir.path().join("wide.png");
        std::fs::write(&path, png(4000, 3000)).unwrap();
        let prepared = prepare_path(&path).unwrap();
        // 4000 x 3000 decodes (48 MB) and shrinks.
        assert!(prepared.thumb.is_some());
        assert!(prepared.size > 0);
    }
}
