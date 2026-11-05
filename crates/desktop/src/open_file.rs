//! Opening a file the page links to (`file:` links: the Artifacts gallery's files, a reply's
//! file links). Hermes's browser build drops them ("this browser cannot open it",
//! `lib/browser-desktop-bridge.ts`); here the backend runs on the user's own machine, so
//! `patches/0013` sends them to `POST /api/hermes-rs/open-file`, which opens the file with its
//! default app, as Hermes's Electron app does (`shell.openExternal`).
//!
//! A link never runs a program: an executable or script (anything Windows would run rather than
//! open) is shown selected in Explorer instead, and a folder opens in Explorer.
//!
//! Hermes: `apps/desktop/electron/` opener rule (`openExternal`), `apps/shared/src/external-url.ts`.

use std::path::{Path, PathBuf};

/// Extensions Windows runs instead of opening (`PATHEXT` and the other shell-executable types).
const RUNS: [&str; 34] = [
    "exe", "com", "bat", "cmd", "ps1", "psm1", "psd1", "ps1xml", "vbs", "vbe", "js", "jse", "wsf",
    "wsh", "msi", "msp", "mst", "scr", "hta", "cpl", "lnk", "url", "reg", "jar", "pif", "appx",
    "msix", "appxbundle", "application", "gadget", "inf", "scf", "sys", "dll",
];

#[derive(Debug, PartialEq, Eq)]
pub enum Open {
    /// Open with its default app (or a folder in Explorer).
    Launch(PathBuf),
    /// Show it selected in Explorer: it would run if opened.
    Reveal(PathBuf),
}

/// What to do for a `file:` link (or a plain absolute path); an error for anything else or a
/// file that isn't there.
pub fn plan(link: &str) -> Result<Open, String> {
    let path = file_path(link).ok_or_else(|| format!("Not a local file: {link}"))?;
    if !path.exists() {
        return Err(format!("{} doesn't exist", path.display()));
    }
    let runs = path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| RUNS.contains(&e.to_ascii_lowercase().as_str()));
    Ok(if runs { Open::Reveal(path) } else { Open::Launch(path) })
}

/// `file:///C:/a%20b/c.md`, `file://C:/x`, `file:///c:\x` or `C:\x` → `C:\a b\c.md`. Only
/// absolute paths on this machine (no `file://server/share` hosts other than `localhost`).
fn file_path(link: &str) -> Option<PathBuf> {
    let link = link.trim();
    let rest = match link.get(..5) {
        Some(scheme) if scheme.eq_ignore_ascii_case("file:") => &link[5..],
        _ => {
            let plain = PathBuf::from(link);
            return plain.is_absolute().then_some(plain);
        }
    };
    let rest = rest.trim_start_matches('/');
    let rest = rest.strip_prefix("localhost/").unwrap_or(rest);
    let decoded = crate::rest::percent_decode(rest);
    let path = PathBuf::from(decoded.replace('/', "\\"));
    path.is_absolute().then_some(path)
}

/// Does it (Explorer runs the default app for a file, opens a folder, or selects with
/// `/select,`). Explorer's exit code doesn't say whether it worked, so only a failure to start
/// it is an error.
pub fn run(open: &Open) -> Result<(), String> {
    let mut command = std::process::Command::new("explorer.exe");
    match open {
        Open::Launch(path) => command.arg(path),
        Open::Reveal(path) => command.arg(format!("/select,{}", path.display())),
    };
    command.spawn().map(drop).map_err(|e| format!("Couldn't start Explorer: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link_to(path: &Path) -> String {
        format!("file:///{}", path.display().to_string().replace('\\', "/").replace(' ', "%20"))
    }

    #[test]
    fn documents_open_and_programs_are_only_shown() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("my report.md");
        let script = dir.path().join("setup.PS1");
        std::fs::write(&doc, "# hi").unwrap();
        std::fs::write(&script, "rm -r ~").unwrap();
        assert_eq!(plan(&link_to(&doc)), Ok(Open::Launch(doc.clone())));
        assert_eq!(plan(&link_to(&script)), Ok(Open::Reveal(script.clone())));
        assert_eq!(plan(&doc.display().to_string()), Ok(Open::Launch(doc)));
        assert_eq!(plan(&link_to(dir.path())), Ok(Open::Launch(dir.path().to_path_buf())));
    }

    #[test]
    fn other_links_are_refused() {
        assert!(plan("https://example.com/a.md").is_err());
        assert!(plan("file://server/share/a.md").is_err());
        assert!(plan("relative/a.md").is_err());
        assert!(plan("file:///C:/definitely/not/here.md").is_err());
    }

    #[test]
    fn link_forms() {
        assert_eq!(file_path("file:///C:/a%20b/c.md"), Some(PathBuf::from(r"C:\a b\c.md")));
        assert_eq!(file_path("FILE://C:/x"), Some(PathBuf::from(r"C:\x")));
        assert_eq!(file_path("file://localhost/C:/x"), Some(PathBuf::from(r"C:\x")));
        assert_eq!(file_path(r"C:\x\y.txt"), Some(PathBuf::from(r"C:\x\y.txt")));
    }
}
