//! Notes: long-form memory in `<memories>/notes/<slug>.md` (Backend v2 Phase 9). An entry of the
//! `project` or `archive` tier stays a one-line summary; its `note` (the body) is a file, and the
//! entry's `links` carry `file:notes/<slug>.md`.
//!
//! Not from Hermes (see `PORTING.md`, `diverged`). The rules this module keeps:
//! - A slug is built here and only here: lowercase ASCII letters, digits and `-`, at most 48
//!   characters, never a Windows reserved device name, never empty. A slug read back from a link
//!   must pass the same test, so no path (`..`, a drive, a separator) can come out of a link.
//! - Only links of the exact form `file:notes/<valid slug>.md` are trusted by `note_read`, and the
//!   store is the only writer of them: a link the model supplies that starts with `notes/` is
//!   refused (`reserved_link`), so a project's entry can't be made to point at another project's
//!   note.
//! - Files are written under the database's write lock: a temporary file in the same folder,
//!   flushed, then renamed to a name that is free (a `-2`, `-3` ... suffix otherwise); an existing
//!   note is never overwritten (a rewrite is a new file; the old one belongs to the entry's
//!   history until a user removal deletes it).
//! - Reads and deletes go through `symlink_metadata`: a note that is a link or not a regular file
//!   is not read.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The most characters a note's body may hold.
pub(crate) const NOTE_MAX_CHARS: usize = 20_000;
/// The longest slug the store makes (a collision suffix may add up to three more characters).
const SLUG_MAX: usize = 48;
const LINK_PREFIX: &str = "file:notes/";
const LINK_SUFFIX: &str = ".md";

/// Device names Windows refuses as file names (with or without an extension).
fn reserved_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || ["com", "lpt"].iter().any(|prefix| {
            stem.strip_prefix(prefix)
                .is_some_and(|n| n.len() == 1 && matches!(n.as_bytes()[0], b'0'..=b'9'))
        })
}

/// A file-name-safe slug from an entry's text (never empty, never a reserved name).
pub(crate) fn slug_for(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= SLUG_MAX {
            break;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        return "note".into();
    }
    if reserved_device(&slug) {
        return format!("n-{slug}");
    }
    slug
}

/// Whether `slug` is one this module could have made (with a collision suffix).
pub(crate) fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= SLUG_MAX + 4
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !slug.starts_with('-')
        && !reserved_device(slug)
}

/// The link an entry carries for the note `slug`.
pub(crate) fn link_for(slug: &str) -> String {
    format!("{LINK_PREFIX}{slug}{LINK_SUFFIX}")
}

/// The slug a store-made note link names; `None` for any other link.
pub(crate) fn slug_of_link(link: &str) -> Option<String> {
    let slug = link.strip_prefix(LINK_PREFIX)?.strip_suffix(LINK_SUFFIX)?;
    valid_slug(slug).then(|| slug.to_string())
}

/// Whether a `file:` link names something in the notes folder as the store writes them
/// (`file:notes/...`, however the slashes and case are spelled): those are the store's to make.
pub(crate) fn reserved_link(link: &str) -> bool {
    let Some(path) = link.strip_prefix("file:") else {
        return false;
    };
    let path = path.trim().replace('\\', "/").to_ascii_lowercase();
    let path = path.trim_start_matches("./");
    path.starts_with("notes/") || path == "notes"
}

/// Whether `meta` is a symlink or a junction / other reparse point.
fn is_link(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// The notes folder must be a real folder (or not exist yet): a junction or symlink planted in
/// its place would send note reads, writes and deletes somewhere else.
fn check_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() && !is_link(&meta) => Ok(()),
        Ok(_) => Err(std::io::Error::other(
            "the notes folder is a link or not a folder",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// `<notes>/<slug>.md`, for a slug that passes [`valid_slug`].
fn path_of(dir: &Path, slug: &str) -> Option<PathBuf> {
    valid_slug(slug).then(|| dir.join(format!("{slug}{LINK_SUFFIX}")))
}

/// Writes `body` as a new note, named after `base` (a slug from [`slug_for`]); the slug used.
/// Atomic (temporary file, flushed, renamed) and never over an existing note. The caller holds
/// the database's write lock, so two writers can't pick the same name.
pub(crate) fn write_new(dir: &Path, base: &str, body: &str) -> std::io::Result<String> {
    check_dir(dir)?;
    std::fs::create_dir_all(dir)?;
    check_dir(dir)?;
    let mut slug = base.to_string();
    let mut n = 1;
    while path_of(dir, &slug).is_none_or(|p| std::fs::symlink_metadata(p).is_ok()) {
        n += 1;
        if n > 99 {
            return Err(std::io::Error::other("no free note name"));
        }
        slug = format!("{base}-{n}");
    }
    let path = path_of(dir, &slug).ok_or_else(|| std::io::Error::other("bad note name"))?;
    let tmp = dir.join(format!(".note_{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &path)
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(slug)
}

/// A note's body. Only a regular file (not a link) of a sane size inside `dir`.
pub(crate) fn read(dir: &Path, slug: &str) -> Result<String, String> {
    let path = path_of(dir, slug).ok_or("that note's name is not valid")?;
    check_dir(dir).map_err(|_| "the notes folder is a link or not a folder")?;
    let meta = std::fs::symlink_metadata(&path).map_err(|_| "the note's file is missing")?;
    if !meta.is_file() || is_link(&meta) {
        return Err("the note's file is not a regular file".into());
    }
    if meta.len() as usize > NOTE_MAX_CHARS * 4 {
        return Err("the note's file is larger than a note may be".into());
    }
    std::fs::read_to_string(&path).map_err(|_| "the note's file could not be read as text".into())
}

/// Deletes a note's file (the entry was removed by the user). A missing file is fine.
pub(crate) fn delete(dir: &Path, slug: &str) {
    let Some(path) = path_of(dir, slug) else {
        return;
    };
    if check_dir(dir).is_err() {
        tracing::warn!("memory: notes folder {} is a link; note {slug} not deleted", dir.display());
        return;
    }
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() && !is_link(&meta) => {
            if let Err(error) = std::fs::remove_file(&path) {
                tracing::warn!("memory: could not delete note {}: {error}", path.display());
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_file_name_safe() {
        assert_eq!(slug_for("Release checklist for v2"), "release-checklist-for-v2");
        assert_eq!(slug_for("  ../../etc/passwd  "), "etc-passwd");
        assert_eq!(slug_for("C:\\Windows\\System32"), "c-windows-system32");
        assert_eq!(slug_for("日本語"), "note");
        assert_eq!(slug_for(""), "note");
        assert_eq!(slug_for("CON"), "n-con");
        assert_eq!(slug_for("com1"), "n-com1");
        assert_eq!(slug_for("lpt9"), "n-lpt9");
        assert_eq!(slug_for("nul"), "n-nul");
        assert_eq!(slug_for("console"), "console");
        let long = slug_for(&"word ".repeat(100));
        assert!(long.len() <= SLUG_MAX, "{long}");
        assert!(!long.ends_with('-'));
        for text in ["a/b", "x:y", "..", "a.b", "NUL.txt", "trailing dot.", "tab\there"] {
            assert!(valid_slug(&slug_for(text)), "{text}");
        }
    }

    #[test]
    fn links_name_only_valid_slugs() {
        assert_eq!(slug_of_link("file:notes/abc-def.md").as_deref(), Some("abc-def"));
        for bad in [
            "file:notes/../x.md",
            "file:notes/a/b.md",
            "file:notes/A.md",
            "file:notes/con.md",
            "file:notes/.md",
            "file:notes\\a.md",
            "file:/notes/a.md",
            "skill:notes/a.md",
            "file:notes/a.txt",
            "file:notes/-a.md",
        ] {
            assert_eq!(slug_of_link(bad), None, "{bad}");
        }
        assert!(reserved_link("file:notes/anything.md"));
        assert!(reserved_link("file:Notes\\x.md"));
        assert!(reserved_link("file:./notes/x.md"));
        assert!(!reserved_link("file:C:/work/notes/x.md"));
        assert!(!reserved_link("skill:notes"));
    }

    #[test]
    fn writes_are_atomic_exclusive_and_confined() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes");
        let a = write_new(&notes, "topic", "first").unwrap();
        let b = write_new(&notes, "topic", "second").unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("topic", "topic-2"));
        assert_eq!(read(&notes, "topic").unwrap(), "first");
        assert_eq!(read(&notes, "topic-2").unwrap(), "second");
        // No temporary file is left behind.
        let names: Vec<String> = std::fs::read_dir(&notes)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        // A bad slug reads and deletes nothing, and never leaves the folder.
        std::fs::write(dir.path().join("secret.md"), "secret").unwrap();
        assert!(read(&notes, "../secret").is_err());
        assert!(read(&notes, "missing").is_err());
        delete(&notes, "../secret");
        assert!(dir.path().join("secret.md").exists());
        delete(&notes, "topic");
        assert!(read(&notes, "topic").is_err());
        assert!(write_new(&notes, "../escape", "x").is_err());
    }

    /// Review (CP9.2): a junction planted as the notes folder is never read, written or deleted
    /// through.
    #[cfg(windows)]
    #[test]
    fn a_junction_in_place_of_the_notes_folder_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("topic.md"), "outside").unwrap();
        let notes = dir.path().join("notes");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&notes)
            .arg(&elsewhere)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        assert!(read(&notes, "topic").is_err());
        assert!(write_new(&notes, "fresh", "body").is_err());
        delete(&notes, "topic");
        assert_eq!(std::fs::read_to_string(elsewhere.join("topic.md")).unwrap(), "outside");
        assert!(!elsewhere.join("fresh.md").exists());
        // Remove the junction itself (never what it points at) before the folder is cleaned up.
        std::fs::remove_dir(&notes).unwrap();
        assert!(elsewhere.join("topic.md").exists());
    }
}
