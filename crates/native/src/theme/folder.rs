//! The user's theme folder, `<data>/themes/`: every `*.json` in it is a Zed theme family file
//! ([`super::zed`]). It is read when the app starts and again whenever a file is added, changed or
//! removed ([`Watcher`], polled once a second by [`super::watch_user_themes`]), so a person can
//! edit a theme and see it recolour the window.
//!
//! A file that cannot be used is skipped with a reason (a [`Problem`], also logged), never a
//! panic and never a reason to lose the other files' themes. The folder not existing is normal
//! (nothing is created: the app only reads here).
//!
//! Polling instead of a file-system watcher: one `read_dir` and a handful of `stat`s a second is
//! free, needs no dependency, and behaves the same for editors that save by rename.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use super::{Source, ThemeDef, zed};

/// How often the folder is looked at.
pub const POLL_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// The app's data folder: `HERMES_RS_HOME` (tests and our own instances set it), else
/// `%APPDATA%\hermes-rs`, the same rule as the runtime's (`RuntimeConfig::for_current_user`).
pub fn data_dir() -> Option<PathBuf> {
    match std::env::var_os("HERMES_RS_HOME") {
        Some(home) => Some(PathBuf::from(home)),
        None => dirs::data_dir().map(|dir| dir.join("hermes-rs")),
    }
}

/// The theme folder inside a data folder.
pub fn themes_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("themes")
}

/// A file in the folder that was skipped, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// The file's name.
    pub file: String,
    /// A sentence saying what is wrong with it.
    pub reason: String,
}

/// What the folder holds at one moment.
#[derive(Clone, Debug, Default)]
pub struct Scan {
    /// The themes of every usable file, files in name order, themes in file order.
    pub themes: Vec<ThemeDef>,
    /// The files that were skipped.
    pub problems: Vec<Problem>,
    /// What the folder looked like (names, sizes, times), to tell later whether it changed.
    pub fingerprint: Fingerprint,
}

/// The `*.json` files of a folder with their size and modification time, sorted by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fingerprint(Vec<(String, u64, Option<SystemTime>)>);

impl Fingerprint {
    /// Looks at `dir`; a missing or unreadable folder has an empty fingerprint.
    pub fn of(dir: &Path) -> Self {
        let mut files: Vec<_> = json_files(dir)
            .into_iter()
            .filter_map(|path| {
                let meta = fs::metadata(&path).ok()?;
                let name = path.file_name()?.to_string_lossy().into_owned();
                Some((name, meta.len(), meta.modified().ok()))
            })
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Self(files)
    }
}

/// The `*.json` files directly in `dir` (not in sub-folders), unsorted.
fn json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .collect()
}

/// Reads every theme file in `dir`. Problems are logged as they are found.
pub fn scan(dir: &Path) -> Scan {
    let fingerprint = Fingerprint::of(dir);
    let mut scan = Scan {
        fingerprint,
        ..Scan::default()
    };
    let mut files = json_files(dir);
    files.sort();
    for path in files {
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        match read_file(&path) {
            Ok(themes) => scan.themes.extend(themes),
            Err(reason) => {
                tracing::warn!("theme file {file} ignored: {reason}");
                scan.problems.push(Problem { file, reason });
            }
        }
    }
    scan
}

/// The largest theme file read. Zed's own families are under 100 KB; anything this big is not a
/// theme, and reading it would hold the poller up.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// One file's themes, or why it is unusable.
fn read_file(path: &Path) -> Result<Vec<ThemeDef>, String> {
    let size = fs::metadata(path)
        .map_err(|e| format!("cannot be read ({e})"))?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(format!(
            "is too large for a theme file ({size} bytes, at most {MAX_FILE_BYTES})"
        ));
    }
    let json = fs::read_to_string(path).map_err(|e| format!("cannot be read ({e})"))?;
    let family = zed::parse_family(&json).map_err(|e| format!("{e:#}"))?;
    Ok(family
        .themes
        .into_iter()
        .map(|theme| ThemeDef {
            name: theme.name,
            appearance: theme.appearance,
            palette: theme.palette,
            highlight: Some(theme.highlight),
            source: Source::User(path.to_path_buf()),
        })
        .collect())
}

/// Remembers what a folder looked like and says when it changed.
#[derive(Debug)]
pub struct Watcher {
    dir: PathBuf,
    seen: Fingerprint,
}

impl Watcher {
    /// Watches `dir`, taking its current state as already seen (read it with [`scan`] first).
    pub fn new(dir: PathBuf, seen: Fingerprint) -> Self {
        Self { dir, seen }
    }

    /// The folder watched.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Looks at the folder; when anything changed since the last call, reads it again and returns
    /// the new [`Scan`].
    pub fn poll(&mut self) -> Option<Scan> {
        let now = Fingerprint::of(&self.dir);
        if now == self.seen {
            return None;
        }
        let scan = scan(&self.dir);
        // A file written between the two looks is picked up by the next poll.
        self.seen = scan.fingerprint.clone();
        Some(scan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = include_str!("../../assets/themes/one/one.json");
    const SMALL: &str = r##"{"name": "Mine", "themes": [
        {"name": "Mine Dark", "appearance": "dark", "style": {"editor.background": "#101820ff"}}
    ]}"##;

    /// A data folder with a `themes` folder in it; the `TestDir` comes last so it is removed last.
    struct Fixture {
        themes: PathBuf,
        _dir: hermes_testdir::TestDir,
    }

    fn fixture() -> Fixture {
        let dir = hermes_testdir::tempdir().expect("temp folder");
        let themes = themes_dir(dir.path());
        fs::create_dir_all(&themes).expect("themes folder");
        Fixture { themes, _dir: dir }
    }

    #[test]
    fn a_missing_folder_is_just_empty() {
        let dir = hermes_testdir::tempdir().expect("temp folder");
        let scan = scan(&themes_dir(dir.path()));
        assert!(scan.themes.is_empty() && scan.problems.is_empty());
        assert!(!themes_dir(dir.path()).exists(), "the app only reads here");
    }

    #[test]
    fn files_are_read_in_name_order_and_tagged_with_their_path() {
        let fx = fixture();
        fs::write(fx.themes.join("b-mine.json"), SMALL).unwrap();
        fs::write(fx.themes.join("a-one.json"), ONE).unwrap();
        fs::write(fx.themes.join("notes.txt"), "not a theme").unwrap();
        fs::create_dir(fx.themes.join("sub.json")).unwrap(); // a folder, not a file
        let scan = scan(&fx.themes);
        let names: Vec<_> = scan.themes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["One Dark", "One Light", "Mine Dark"]);
        assert!(scan.problems.is_empty(), "{:?}", scan.problems);
        let mine = scan.themes.last().unwrap();
        assert_eq!(mine.palette.background.hex(), "#101820");
        assert_eq!(mine.source, Source::User(fx.themes.join("b-mine.json")));
    }

    #[test]
    fn a_malformed_file_is_skipped_with_a_reason_and_the_rest_still_load() {
        let fx = fixture();
        fs::write(fx.themes.join("broken.json"), "{ this is not json").unwrap();
        fs::write(fx.themes.join("empty.json"), "").unwrap();
        fs::write(
            fx.themes.join("notathemefile.json"),
            r#"{"hello": "world"}"#,
        )
        .unwrap();
        fs::write(
            fx.themes.join("bad-appearance.json"),
            r#"{"themes":[{"name":"X","appearance":"sepia"}]}"#,
        )
        .unwrap();
        fs::write(fx.themes.join("good.json"), SMALL).unwrap();
        let scan = scan(&fx.themes);
        assert_eq!(scan.themes.len(), 1, "the good file's theme loads");
        let mut files: Vec<_> = scan.problems.iter().map(|p| p.file.as_str()).collect();
        files.sort_unstable();
        assert_eq!(
            files,
            [
                "bad-appearance.json",
                "broken.json",
                "empty.json",
                "notathemefile.json"
            ]
        );
        for problem in &scan.problems {
            assert!(!problem.reason.is_empty(), "{}: no reason", problem.file);
        }
        assert!(
            scan.problems
                .iter()
                .any(|p| p.file == "bad-appearance.json" && p.reason.contains("sepia"))
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_as_text_is_a_problem_not_a_panic() {
        let fx = fixture();
        fs::write(
            fx.themes.join("binary.json"),
            [0xff, 0xfe, 0x00, 0x80, 0x81],
        )
        .unwrap();
        let scan = scan(&fx.themes);
        assert!(scan.themes.is_empty());
        assert_eq!(scan.problems.len(), 1);
        assert!(scan.problems[0].reason.contains("cannot be read"));
    }

    #[test]
    fn a_huge_file_is_a_problem_and_is_not_read() {
        let fx = fixture();
        let file = fs::File::create(fx.themes.join("huge.json")).unwrap();
        // Extended with `set_len`: the test writes no 4 MB itself.
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        drop(file);
        fs::write(fx.themes.join("good.json"), SMALL).unwrap();
        let scan = scan(&fx.themes);
        assert_eq!(scan.themes.len(), 1);
        assert_eq!(scan.problems.len(), 1);
        assert!(scan.problems[0].reason.contains("too large"));
    }

    #[test]
    fn the_watcher_sees_added_changed_and_removed_files() {
        let fx = fixture();
        let first = scan(&fx.themes);
        let mut watcher = Watcher::new(fx.themes.clone(), first.fingerprint);
        assert!(watcher.poll().is_none(), "nothing changed yet");

        // Added.
        fs::write(fx.themes.join("mine.json"), SMALL).unwrap();
        let added = watcher.poll().expect("a new file is a change");
        assert_eq!(added.themes.len(), 1);
        assert!(watcher.poll().is_none(), "the change is reported once");

        // Changed: a different size is a change whatever the file system's time resolution.
        fs::write(
            fx.themes.join("mine.json"),
            SMALL
                .replace("101820", "202830")
                .replace("Mine Dark", "Mine Darker"),
        )
        .unwrap();
        let changed = watcher.poll().expect("an edited file is a change");
        assert_eq!(changed.themes[0].name, "Mine Darker");
        assert_eq!(changed.themes[0].palette.background.hex(), "#202830");

        // Becomes malformed: reported as a problem, the theme is gone.
        fs::write(fx.themes.join("mine.json"), "{").unwrap();
        let broken = watcher.poll().expect("breaking a file is a change");
        assert!(broken.themes.is_empty());
        assert_eq!(broken.problems.len(), 1);

        // Removed.
        fs::remove_file(fx.themes.join("mine.json")).unwrap();
        let removed = watcher.poll().expect("removing a file is a change");
        assert!(removed.themes.is_empty() && removed.problems.is_empty());
        assert!(watcher.poll().is_none());
    }

    #[test]
    fn the_themes_folder_sits_in_the_data_folder() {
        assert_eq!(
            themes_dir(Path::new("C:/data")),
            PathBuf::from("C:/data").join("themes")
        );
    }
}
