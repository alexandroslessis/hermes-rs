//! Images for vision models (Hermes's `tools/vision_tools.py` native path and
//! `agent/image_routing.py`, MIT): load an image from a path, an http(s) URL or a `data:` URL,
//! crop a region at full resolution, and shrink it to Hermes's embed budget (it is re-sent with
//! every later request), so the model sees it natively.

use std::io::Cursor;
use std::path::Path;

use base64::Engine as _;
use image::{DynamicImage, GenericImageView as _, ImageFormat};

/// `_MAX_BASE64_BYTES`: the largest payload any provider takes.
const MAX_BASE64_BYTES: usize = 20 * 1024 * 1024;
/// `_EMBED_MAX_DIMENSION`: the long edge an embedded image is scaled to.
pub const EMBED_MAX_DIMENSION: u32 = 1568;
/// `_DEFAULT_EMBED_TARGET_BYTES`: base64 size an image re-sent every turn is shrunk to.
pub const EMBED_TARGET_BYTES: usize = 256 * 1024;
/// Images the user attaches get more room (they're the point of the message).
pub const ATTACHMENT_TARGET_BYTES: usize = 1024 * 1024;
/// The largest source image read (download or file).
const MAX_SOURCE_BYTES: u64 = 50 * 1024 * 1024;
/// Image extensions recognized on dropped or pasted files (`_IMAGE_EXTS`).
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif"];

/// An image ready for a message: base64 data, its MIME type, and what was done to it.
#[derive(Debug, Clone)]
pub struct PreparedImage {
    pub data: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    /// `_build_scale_note`: how the image relates to the original (crop and scaling).
    pub note: Option<String>,
}

/// Whether a file name looks like an image.
pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str()))
}

/// The raw bytes of `source`: a `data:` URL, an http(s) URL (public addresses only) or a path
/// (resolved against `cwd`).
pub async fn load_source(source: &str, cwd: &Path, home: &Path) -> Result<Vec<u8>, String> {
    let source = source.trim();
    if let Some(rest) = source.strip_prefix("data:") {
        let (_, data) = rest
            .split_once(";base64,")
            .ok_or("Only base64 data: URLs are supported.")?;
        return base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|e| format!("Invalid base64 image data: {e}"));
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        if !crate::web::is_safe_url(source).await {
            return Err("Blocked: URL targets a private or internal network address".into());
        }
        let response = reqwest::Client::new()
            .get(source)
            .header("User-Agent", "hermes-agent")
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| format!("Could not download the image: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "Could not download the image: HTTP {}",
                response.status().as_u16()
            ));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_SOURCE_BYTES)
        {
            return Err("The image is larger than 50 MB.".into());
        }
        return response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("Could not download the image: {e}"));
    }
    let path = crate::paths::resolve(source, cwd, home);
    let size = std::fs::metadata(&path)
        .map_err(|_| format!("Image not found: {}", crate::display_path(&path)))?
        .len();
    if size > MAX_SOURCE_BYTES {
        return Err("The image is larger than 50 MB.".into());
    }
    std::fs::read(&path).map_err(|e| format!("Could not read {}: {e}", crate::display_path(&path)))
}

fn mime_of(format: ImageFormat) -> Option<&'static str> {
    match format {
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Gif => Some("image/gif"),
        ImageFormat::WebP => Some("image/webp"),
        _ => None,
    }
}

fn encode(image: &DynamicImage, format: ImageFormat, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    match format {
        ImageFormat::Jpeg => {
            let rgb = DynamicImage::ImageRgb8(image.to_rgb8());
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
            rgb.write_with_encoder(encoder).map_err(|e| e.to_string())?;
        }
        _ => image
            .write_to(&mut out, format)
            .map_err(|e| e.to_string())?,
    }
    Ok(out.into_inner())
}

fn base64_len(bytes: usize) -> usize {
    bytes.div_ceil(3) * 4
}

/// `_prepare_image` + the embed caps: decodes `bytes`, crops `region` ([x1, y1, x2, y2] in
/// original pixels) first so it keeps full resolution, then keeps the original encoding when it
/// is within `max_dimension` and `target_bytes` and a format every provider takes; otherwise
/// re-encodes as JPEG, lowering quality and then size until it fits.
pub fn prepare(
    bytes: &[u8],
    region: Option<[u32; 4]>,
    max_dimension: u32,
    target_bytes: usize,
) -> Result<PreparedImage, String> {
    let format =
        image::guess_format(bytes).map_err(|_| "Unrecognized image format.".to_string())?;
    let mut image = image::load_from_memory_with_format(bytes, format)
        .map_err(|e| format!("The image could not be decoded: {e}"))?;
    let (original_width, original_height) = image.dimensions();
    let mut notes = Vec::new();
    if let Some([x1, y1, x2, y2]) = region {
        let (x1, x2) = (x1.min(original_width), x2.min(original_width));
        let (y1, y2) = (y1.min(original_height), y2.min(original_height));
        if x2 <= x1 || y2 <= y1 {
            return Err(format!(
                "Invalid region [{x1}, {y1}, {x2}, {y2}] for a {original_width}x{original_height} image."
            ));
        }
        image = image.crop_imm(x1, y1, x2 - x1, y2 - y1);
        notes.push(format!(
            "Showing region [{x1}, {y1}, {x2}, {y2}] of the {original_width}x{original_height} original; add ({x1}, {y1}) to map coordinates back."
        ));
    }
    let (width, height) = image.dimensions();
    let keep_original = region.is_none()
        && width.max(height) <= max_dimension
        && base64_len(bytes.len()) <= target_bytes
        && mime_of(format).is_some();
    if keep_original {
        return Ok(PreparedImage {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            mime: mime_of(format).unwrap_or("image/png").to_string(),
            width,
            height,
            note: None,
        });
    }
    let mut scaled = if width.max(height) > max_dimension {
        image.resize(
            max_dimension,
            max_dimension,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        image
    };
    let mut quality = 85u8;
    let mut encoded = encode(&scaled, ImageFormat::Jpeg, quality)?;
    while base64_len(encoded.len()) > target_bytes {
        if quality > 50 {
            quality -= 10;
        } else {
            let (w, h) = scaled.dimensions();
            if w.max(h) <= 256 {
                break;
            }
            scaled = scaled.resize(w * 3 / 4, h * 3 / 4, image::imageops::FilterType::Lanczos3);
        }
        encoded = encode(&scaled, ImageFormat::Jpeg, quality)?;
    }
    if base64_len(encoded.len()) > MAX_BASE64_BYTES {
        return Err("The image is too large for vision even after resizing.".into());
    }
    let (final_width, final_height) = scaled.dimensions();
    if (final_width, final_height) != (width, height) {
        notes.push(format!(
            "Scaled from {width}x{height} to {final_width}x{final_height}; multiply coordinates by {:.3} for the {}.",
            f64::from(width) / f64::from(final_width),
            if region.is_some() { "crop" } else { "original" },
        ));
    }
    Ok(PreparedImage {
        data: base64::engine::general_purpose::STANDARD.encode(&encoded),
        mime: "image/jpeg".into(),
        width: final_width,
        height: final_height,
        note: (!notes.is_empty()).then(|| notes.join(" ")),
    })
}

/// Saves an image under `folder` with a unique name and its format's extension.
pub fn save(folder: &Path, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    let extension = image::guess_format(bytes)
        .ok()
        .and_then(|f| f.extensions_str().first().copied())
        .unwrap_or("img");
    let path = folder.join(format!(
        "image-{}.{extension}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// The file extension for a MIME type of [`PreparedImage`].
pub fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "jpg",
    }
}

/// `_build_native_vision_tool_result`'s text part.
pub fn native_result_text(question: &str, note: Option<&str>) -> String {
    let mut text = "Image loaded into your context — you can see it natively now. Use your built-in vision to answer the user.".to_string();
    if !question.trim().is_empty() {
        text.push_str(&format!("\n\nQuestion: {}", question.trim()));
    }
    if let Some(note) = note {
        text.push_str(&format!("\n\nNote: {note}"));
    }
    text
}

/// What `vision_analyze` says when the session's model can't see images (Hermes would route to
/// an auxiliary vision model, which hermes-rs doesn't have yet).
pub const NO_VISION_MESSAGE: &str = "The current model has no vision support, so the image can't be shown to it. Tell the user to switch to a vision-capable model (one that accepts image input) to look at images.";

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x * y) % 256) as u8])
        }));
        encode(&image, ImageFormat::Png, 0).unwrap()
    }

    #[test]
    fn small_images_pass_through_and_large_ones_shrink() {
        let small = png(64, 32);
        let prepared = prepare(&small, None, EMBED_MAX_DIMENSION, EMBED_TARGET_BYTES).unwrap();
        assert_eq!((prepared.mime.as_str(), prepared.width), ("image/png", 64));
        assert!(prepared.note.is_none());

        let large = png(3000, 1500);
        let prepared = prepare(&large, None, EMBED_MAX_DIMENSION, EMBED_TARGET_BYTES).unwrap();
        assert_eq!(prepared.mime, "image/jpeg");
        assert!(prepared.width <= EMBED_MAX_DIMENSION);
        assert!(prepared.data.len() <= EMBED_TARGET_BYTES);
        assert!(prepared.note.unwrap().starts_with("Scaled from 3000x1500"));
    }

    #[test]
    fn regions_crop_before_scaling() {
        let prepared = prepare(
            &png(400, 300),
            Some([100, 50, 300, 250]),
            EMBED_MAX_DIMENSION,
            EMBED_TARGET_BYTES,
        )
        .unwrap();
        assert_eq!((prepared.width, prepared.height), (200, 200));
        assert!(prepared.note.unwrap().contains("add (100, 50)"));
        assert!(prepare(&png(10, 10), Some([5, 5, 5, 9]), 100, 1 << 20).is_err());
        assert!(prepare(b"not an image", None, 100, 1 << 20).is_err());
    }

    #[tokio::test]
    async fn sources() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), png(4, 4)).unwrap();
        assert!(load_source("a.png", dir.path(), dir.path()).await.is_ok());
        let data = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png(2, 2))
        );
        assert!(load_source(&data, dir.path(), dir.path()).await.is_ok());
        assert!(
            load_source("http://127.0.0.1/x.png", dir.path(), dir.path())
                .await
                .is_err()
        );
        assert!(is_image_path(Path::new("x.JPG")) && !is_image_path(Path::new("x.txt")));
    }
}
