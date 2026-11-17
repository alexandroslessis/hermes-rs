//! Tests of the sandbox's folder labels (`sandbox_labels.rs`). Every label a test sets is on a
//! folder in its own `hermes_testdir` folder (`ensure_low` asserts it under `cfg(test)`), and an
//! [`Unlabel`] guard puts every recorded folder back when the test ends, passed or failed.

use std::path::{Path, PathBuf};

use super::*;

/// Removes every label the test's record lists, on drop (also when the test panics).
struct Unlabel(PathBuf);

impl Drop for Unlabel {
    fn drop(&mut self) {
        if let Err(error) = remove_all(&self.0) {
            eprintln!("could not remove the test's sandbox labels: {error}");
        }
    }
}

/// Puts a label a test set by hand (someone else's label) back to none, on drop.
struct ClearLabel(PathBuf);

impl Drop for ClearLabel {
    fn drop(&mut self) {
        if self.0.exists() {
            let _ = win::set_label(&self.0, "");
        }
    }
}

/// `<temp>/data` (the data folder) and `<temp>/ws` (a workspace), the guard declared after the
/// folder so it runs first.
fn setup() -> (hermes_testdir::TestDir, PathBuf, PathBuf) {
    let dir = hermes_testdir::tempdir().unwrap();
    let data = dir.path().join("data");
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(ws.join("src").join("main.rs"), "fn main() {}").unwrap();
    (dir, data, ws)
}

fn real(path: &Path) -> PathBuf {
    canonical(path).unwrap()
}

/// A descriptor read after removal, compared with the one before: an object that had no SACL
/// at all keeps an empty one (`S:NO_ACCESS_CONTROL`: no label, no audit, the same meaning),
/// which only `SeSecurityPrivilege` could remove (module doc). Nothing else may differ.
fn as_before(after: &str, before: &str) -> String {
    if !before.contains("S:") {
        after.replace("S:NO_ACCESS_CONTROL", "")
    } else {
        after.to_string()
    }
}

/// Every object under `root` (itself included) with its descriptor.
fn descriptors(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = vec![(root.to_path_buf(), win::descriptor(root).unwrap())];
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(descriptors(&path));
        } else {
            out.push((path.clone(), win::descriptor(&path).unwrap()));
        }
    }
    out
}

/// `icacls <root> /t /c` (only reads): every object's ACL and mandatory label as Windows shows
/// them.
fn icacls_listing(root: &Path) -> String {
    use std::os::windows::process::CommandExt as _;
    let out = std::process::Command::new("icacls")
        .arg(root)
        .args(["/t", "/c", "/q"])
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn assert_restored(before: &[(PathBuf, String)], after: &[(PathBuf, String)]) {
    assert_eq!(before.len(), after.len());
    for ((path, b), (_, a)) in before.iter().zip(after) {
        assert_eq!(&as_before(a, b), b, "{}", path.display());
    }
}

#[test]
fn labels_read_from_sddl() {
    assert_eq!(classify(""), Label::None);
    assert_eq!(classify("S:AI"), Label::None);
    assert_eq!(classify(LOW_LABEL_SDDL), Label::LowExplicit);
    assert_eq!(classify("S:(ML;CIOI;NW;;;LW)"), Label::LowExplicit);
    assert_eq!(classify("S:AI(ML;OICIID;NW;;;LW)"), Label::LowInherited);
    // Someone else's choices: another level, other rights, not inherited by both kinds.
    assert_eq!(classify("S:(ML;;NW;;;ME)"), Label::Other);
    assert_eq!(classify("S:(ML;OICI;NW;;;HI)"), Label::Other);
    assert_eq!(classify("S:(ML;OICI;NWNR;;;LW)"), Label::Other);
    assert_eq!(classify("S:(ML;CI;NW;;;LW)"), Label::Other);
    assert_eq!(classify("S:(ML;OICI;NW;;;LW)(ML;OICI;NW;;;LW)"), Label::Other);
}

#[test]
fn roots_profile_system_and_data_folders_are_refused() {
    let (dir, data, ws) = setup();
    let protected = Protected::here(&data);
    let refused = |path: &Path| protected.refusal(path);
    let home = canonical(&dirs::home_dir().unwrap()).unwrap();
    let system_root = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    let program_files = PathBuf::from(std::env::var_os("ProgramFiles").unwrap());
    let drive = PathBuf::from(format!("{}\\", &home.to_string_lossy()[..2]));
    // Pure checks only: nothing is labelled here.
    for path in [
        drive,
        home.clone(),
        home.parent().unwrap().to_path_buf(), // C:\Users: contains the profile
        system_root.clone(),
        system_root.join("Temp"),
        program_files.clone(),
        program_files.join("Git"),
        real(&data),
        real(&data).join("skills"),
        real(&data).join("scratch"), // the scratch root, not a session's
        real(dir.path()),            // contains the data folder
    ] {
        assert!(refused(&path).is_some(), "{} was not refused", path.display());
    }
    assert!(refused(Path::new("relative\\folder")).is_some());
    for path in [
        real(&ws),
        real(&data).join("scratch").join("ses_1"),
        real(&data).join(CACHE_DIR),
        home.join("repos").join("project"),
    ] {
        assert!(refused(&path).is_none(), "{}: {:?}", path.display(), refused(&path));
    }
    // ensure_low refuses before reading or writing anything.
    let error = ensure_low(&data, &protected, &home, "grant").unwrap_err();
    assert!(error.contains("user profile"), "{error}");
    assert!(!data.join(RECORD_FILE).exists());
}

#[test]
fn labelling_records_first_and_removal_restores_the_exact_descriptor() {
    let (_dir, data, ws) = setup();
    let _unlabel = Unlabel(data.clone());
    let file = ws.join("src").join("main.rs");
    std::fs::create_dir_all(ws.join("src").join("deep").join("er")).unwrap();
    std::fs::write(ws.join("src").join("deep").join("er").join("x.txt"), "x").unwrap();
    let before = descriptors(&ws);
    let shown_before = icacls_listing(&ws);
    let original = win::read_label(&ws).unwrap();
    assert_eq!(classify(&original), Label::None, "{original}");

    let protected = Protected::here(&data);
    assert_eq!(ensure_low(&data, &protected, &ws, "workspace"), Ok(Ensured::Labelled));
    assert_eq!(classify(&win::read_label(&ws).unwrap()), Label::LowExplicit);
    // What was inside gets the label too (inherited), so a Low process may change it; and
    // nothing but the label changed anywhere.
    assert_eq!(classify(&win::read_label(&file).unwrap()), Label::LowInherited);
    let deep = ws.join("src").join("deep").join("er").join("x.txt");
    assert_eq!(classify(&win::read_label(&deep).unwrap()), Label::LowInherited);
    for ((path, b), (_, a)) in before.iter().zip(descriptors(&ws)) {
        let strip = |text: &str| text.split("S:").next().unwrap_or_default().to_string();
        assert_eq!(strip(&a), strip(b), "{}", path.display());
    }
    let listed = list(&data).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, real(&ws));
    assert_eq!(listed[0].original, original);
    assert_eq!(listed[0].kind, "workspace");
    assert!(listed[0].complete);

    // Idempotent: no second label, no second record.
    assert_eq!(ensure_low(&data, &protected, &ws, "workspace"), Ok(Ensured::AlreadyOurs));
    assert_eq!(list(&data).unwrap().len(), 1);
    // A folder inside it is Low already: nothing set, nothing recorded.
    assert_eq!(
        ensure_low(&data, &protected, &ws.join("src"), "grant"),
        Ok(Ensured::AlreadyLow)
    );
    assert_eq!(list(&data).unwrap().len(), 1);

    let removal = remove_all(&data).unwrap();
    assert_eq!(removal.restored, vec![real(&ws)]);
    assert!(removal.failed.is_empty() && removal.gone.is_empty() && removal.changed.is_empty());
    assert_restored(&before, &descriptors(&ws));
    assert!(list(&data).unwrap().is_empty());
    // A second opinion from Windows' own tool (read only): every entry's ACL and label as before.
    assert_eq!(icacls_listing(&ws), shown_before);

    // Twice over: the same again.
    assert_eq!(ensure_low(&data, &protected, &ws, "workspace"), Ok(Ensured::Labelled));
    remove_all(&data).unwrap();
    assert_restored(&before, &descriptors(&ws));
}

/// A junction (or symbolic link) in a labelled folder is not followed: what it points to, outside
/// the folder, keeps its label.
#[test]
fn links_inside_a_labelled_folder_are_not_followed() {
    use std::os::windows::process::CommandExt as _;
    let (dir, data, ws) = setup();
    let _unlabel = Unlabel(data.clone());
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("f.txt"), "x").unwrap();
    let made = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(ws.join("link"))
        .arg(&outside)
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    let before = descriptors(&outside);
    assert_eq!(
        ensure_low(&data, &Protected::here(&data), &ws, "workspace"),
        Ok(Ensured::Labelled)
    );
    assert_eq!(descriptors(&outside), before);
    remove_all(&data).unwrap();
    assert_eq!(descriptors(&outside), before);
    std::fs::remove_dir(ws.join("link")).unwrap();
}

/// A labelling cut short (the app stopped in the middle of a large folder) is finished by the
/// next command: the record says it isn't complete.
#[test]
fn an_unfinished_labelling_is_finished_next_time() {
    let (_dir, data, ws) = setup();
    let _unlabel = Unlabel(data.clone());
    // What a stop right after the folder's own label leaves: recorded, labelled, nothing below.
    let record = Record {
        folders: vec![Labelled {
            path: real(&ws),
            original: win::read_label(&ws).unwrap(),
            kind: "workspace".into(),
            labelled_at: 0,
            complete: false,
        }],
    };
    save(&data, &record).unwrap();
    win::set_label(&ws, LOW_LABEL_SDDL).unwrap();
    let file = ws.join("src").join("main.rs");
    assert_eq!(classify(&win::read_label(&file).unwrap()), Label::None);
    assert_eq!(
        ensure_low(&data, &Protected::here(&data), &ws, "workspace"),
        Ok(Ensured::Labelled)
    );
    assert_eq!(classify(&win::read_label(&file).unwrap()), Label::LowInherited);
    assert!(list(&data).unwrap()[0].complete);
}

#[test]
fn folders_with_a_label_we_did_not_set_are_refused_and_left_alone() {
    let (_dir, data, ws) = setup();
    let _unlabel = Unlabel(data.clone());
    let protected = Protected::here(&data);
    let medium = ws.join("medium");
    let low = ws.join("low");
    std::fs::create_dir_all(&medium).unwrap();
    std::fs::create_dir_all(&low).unwrap();
    let _clear = (ClearLabel(medium.clone()), ClearLabel(low.clone()));
    win::set_label(&medium, "S:(ML;;NW;;;ME)").unwrap();
    // Our label's exact shape, but set by someone else (icacls): not in the record, not ours.
    win::set_label(&low, LOW_LABEL_SDDL).unwrap();
    for folder in [&medium, &low] {
        let before = win::descriptor(folder).unwrap();
        let error = ensure_low(&data, &protected, folder, "grant").unwrap_err();
        assert!(error.contains("did not set"), "{error}");
        assert_eq!(win::descriptor(folder).unwrap(), before);
    }
    assert!(list(&data).unwrap().is_empty());
}

#[test]
fn removal_tolerates_deleted_folders_and_leaves_changed_labels() {
    let (_dir, data, ws) = setup();
    let _unlabel = Unlabel(data.clone());
    let protected = Protected::here(&data);
    let gone = ws.join("gone");
    let changed = ws.join("changed");
    let kept = ws.join("kept");
    for folder in [&gone, &changed, &kept] {
        std::fs::create_dir_all(folder).unwrap();
        assert_eq!(ensure_low(&data, &protected, folder, "grant"), Ok(Ensured::Labelled));
    }
    let (gone_real, changed_real, kept_real) = (real(&gone), real(&changed), real(&kept));
    std::fs::remove_dir_all(&gone).unwrap();
    let _clear = ClearLabel(changed.clone());
    win::set_label(&changed, "S:(ML;;NW;;;ME)").unwrap();

    let removal = remove_all(&data).unwrap();
    assert_eq!(removal.gone, vec![gone_real]);
    assert_eq!(removal.changed, vec![changed_real]);
    assert_eq!(removal.restored, vec![kept_real]);
    assert!(removal.failed.is_empty());
    // Someone else's label stays as they set it.
    assert_eq!(classify(&win::read_label(&changed).unwrap()), Label::Other);
    assert_eq!(classify(&win::read_label(&kept).unwrap()), Label::None);
    assert!(list(&data).unwrap().is_empty());
}

#[test]
fn an_unreadable_record_stops_labelling() {
    let (_dir, data, ws) = setup();
    let _unlabel = Unlabel(data.clone());
    std::fs::write(data.join(RECORD_FILE), "not json").unwrap();
    let error = ensure_low(&data, &Protected::here(&data), &ws, "workspace").unwrap_err();
    assert!(error.contains("not valid"), "{error}");
    assert_eq!(classify(&win::read_label(&ws).unwrap()), Label::None);
    assert!(remove_all(&data).is_err());
    std::fs::remove_file(data.join(RECORD_FILE)).unwrap();
}

#[test]
fn prepare_labels_workspace_scratch_cache_and_grants_and_sets_the_cache_variables() {
    let (dir, data, ws) = setup();
    let _unlabel = Unlabel(data.clone());
    let scratch = data.join("scratch").join("ses_1");
    let grant = dir.path().join("granted");
    std::fs::create_dir_all(&grant).unwrap();
    let missing = dir.path().join("missing");
    let prepared = prepare(
        &data,
        &ws,
        &scratch,
        &[grant.clone(), missing, PathBuf::from("relative")],
    );
    for folder in [&ws, &scratch, &data.join(CACHE_DIR), &grant] {
        assert_eq!(
            classify(&win::read_label(folder).unwrap()),
            Label::LowExplicit,
            "{}",
            folder.display()
        );
        assert!(prepared.writable.contains(&real(folder)), "{}", folder.display());
    }
    let kinds: Vec<String> = list(&data).unwrap().into_iter().map(|f| f.kind).collect();
    assert_eq!(kinds, ["workspace", "scratch", "cache", "grant"]);
    // The missing and the relative grant are said, not labelled.
    assert_eq!(prepared.notes.len(), 2, "{:?}", prepared.notes);
    assert!(prepared.notes[0].contains("does not exist"), "{:?}", prepared.notes);
    assert!(prepared.notes[1].contains("not an absolute path"), "{:?}", prepared.notes);
    let env: std::collections::HashMap<_, _> = prepared.env.into_iter().collect();
    assert_eq!(env["CARGO_HOME"], data.join(CACHE_DIR).join("cargo"));
    assert_eq!(env["npm_config_cache"], data.join(CACHE_DIR).join("npm"));
    assert_eq!(env["UV_CACHE_DIR"], data.join(CACHE_DIR).join("uv"));

    // A session's workspace that contains the data folder is refused (said, not labelled).
    let refused = prepare(&data, dir.path(), &scratch, &[]);
    assert!(
        refused.notes.iter().any(|n| n.contains("workspace") && n.contains("data folder")),
        "{:?}",
        refused.notes
    );
}

#[test]
fn a_blocked_write_is_found_in_the_output_with_its_path() {
    let writable = vec![PathBuf::from(r"C:\work\repo")];
    let hint = |text: &str| blocked_hint(text, &writable);
    let expected = |path: &str| {
        Some(format!(
            "blocked by sandbox: {path}; grant the folder in Settings (terminal.sandbox_grants in \
             desktop-config.json) or move the work into the workspace"
        ))
    };
    // PowerShell
    assert_eq!(
        hint("Set-Content : Access to the path 'C:\\Users\\me\\x.txt' is denied."),
        expected(r"C:\Users\me\x.txt")
    );
    // cmd
    assert_eq!(
        hint("Access is denied.").as_deref(),
        expected("(a path the output does not show)").as_deref()
    );
    // git, cargo, Python
    assert_eq!(
        hint("error: could not lock config file C:/Users/me/.gitconfig: Permission denied"),
        expected("C:/Users/me/.gitconfig")
    );
    assert_eq!(
        hint("warning: failed to write cache, path: C:\\Users\\me\\.cargo\\registry\\x, error: Access is denied. (os error 5)"),
        expected(r"C:\Users\me\.cargo\registry\x")
    );
    assert_eq!(
        hint("PermissionError: [Errno 13] Permission denied: 'C:\\Program Files\\x'"),
        expected(r"C:\Program Files\x")
    );
    assert_eq!(
        hint("npm error path C:\\Users\\me\\AppData\\Local\\npm-cache\\_cacache\nnpm error code EPERM"),
        expected("(a path the output does not show)")
    );
    // Inside the workspace (a locked file): not the sandbox.
    assert_eq!(hint("error: failed to remove C:\\work\\repo\\target\\a.exe: Access is denied. (os error 5)"), None);
    assert_eq!(hint("all fine\nno errors"), None);
}

/// The profile spelled every way Windows accepts (8.3 short names, `\\?\`, another case) is the
/// profile: `ensure_low` reads the real path before the refusal check, and refuses before it
/// reads or writes anything.
#[test]
fn other_spellings_of_the_profile_are_refused_too() {
    let (_dir, data, _ws) = setup();
    let protected = Protected::here(&data);
    let home = canonical(&dirs::home_dir().unwrap()).unwrap();
    let mut spellings = vec![
        PathBuf::from(format!(r"\\?\{}", home.display())),
        PathBuf::from(home.display().to_string().to_uppercase()),
        PathBuf::from(format!(r"{}\.", home.display())),
        home.join("repos").join(".."),
    ];
    // `%TEMP%` is often spelled with the profile's short name (`C:\Users\ABCDEF~1\...`).
    spellings.extend(
        std::env::temp_dir()
            .ancestors()
            .filter(|a| canonical(a).is_some_and(|r| r == home))
            .map(Path::to_path_buf),
    );
    for spelling in spellings {
        let error = ensure_low(&data, &protected, &spelling, "grant").unwrap_err();
        assert!(error.contains("not labelled"), "{}: {error}", spelling.display());
    }
    assert!(!data.join(RECORD_FILE).exists());
}

/// Places a planted file would run from later at the user's own level, and other users' folders.
#[test]
fn appdata_path_folders_startup_and_other_profiles_are_refused() {
    let (_dir, data, _ws) = setup();
    let protected = Protected::here(&data);
    let home = canonical(&dirs::home_dir().unwrap()).unwrap();
    let env = |name: &str| PathBuf::from(std::env::var_os(name).unwrap());
    let users = home.parent().unwrap().to_path_buf();
    let mut refused = vec![
        env("APPDATA"),
        env("LOCALAPPDATA"),
        home.join("AppData"),
        env("APPDATA").join(r"Microsoft\Windows\Start Menu\Programs\Startup"),
        home.join(".ssh"),
        home.join(".cargo"),
        home.join(".cargo").join("bin"),
        home.join(".rustup").join("toolchains"),
        users.join("Public"),
        users.join("Default"),
        users.join("someone-else").join("repo"),
    ];
    if let Some(documents) = dirs::document_dir() {
        refused.push(documents.join("WindowsPowerShell"));
        refused.push(documents.join("PowerShell").join("Modules"));
    }
    // A folder on PATH, and the folder holding it.
    let on_path = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .find(|p| p.is_absolute() && p.parent().is_some_and(|q| q.parent().is_some()))
        .unwrap();
    refused.push(on_path.clone());
    refused.push(on_path.parent().unwrap().to_path_buf());
    for path in refused {
        let real = canonical(&path).unwrap_or(path.clone());
        assert!(
            protected.refusal(&real).is_some(),
            "{} was not refused",
            real.display()
        );
    }
    // The session folders inside the data folder (itself inside APPDATA in the app) stay allowed.
    assert!(protected.refusal(&real(&data).join("scratch").join("s")).is_none());
}

/// A file with a second name outside the folder is not labelled: the label is the file's, so it
/// would reach the outside name too.
#[test]
fn hard_linked_files_are_left_alone() {
    let (dir, data, ws) = setup();
    let _unlabel = Unlabel(data.clone());
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, "x").unwrap();
    std::fs::hard_link(&outside, ws.join("linked.txt")).unwrap();
    let before = win::descriptor(&outside).unwrap();
    let ensured = ensure_low(&data, &Protected::here(&data), &ws, "workspace").unwrap();
    let Ensured::Partly(why) = ensured else {
        panic!("{ensured:?}")
    };
    assert!(why.contains("1 hard-linked file"), "{why}");
    assert_eq!(win::descriptor(&outside).unwrap(), before);
    // Everything else inside is labelled.
    let main = ws.join("src").join("main.rs");
    assert_eq!(classify(&win::read_label(&main).unwrap()), Label::LowInherited);
    remove_all(&data).unwrap();
    assert_eq!(win::descriptor(&outside).unwrap(), before);
}

/// A walk checks where each handle really is: an entry reached through a junction (a folder a
/// command swapped for one while the walk ran) is outside the root and skipped.
#[test]
fn an_entry_reached_through_a_junction_is_skipped() {
    let (dir, _data, ws) = setup();
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("f.txt"), "x").unwrap();
    junction(&ws.join("j"), &outside);
    let root = real(&ws);
    let (_, link) = visit(&ws.join("j"), &root).unwrap();
    assert!(matches!(link, Visit::Skip));
    let (_, through) = visit(&ws.join("j").join("f.txt"), &root).unwrap();
    assert!(matches!(through, Visit::Skip));
    let (_, inside) = visit(&ws.join("src").join("main.rs"), &root).unwrap();
    assert!(matches!(inside, Visit::Unlabelled));
    std::fs::remove_dir(ws.join("j")).unwrap();
}

/// A recorded folder replaced by a junction to somewhere else: removal neither follows it nor
/// restores anything through it; the entry is reported as changed and forgotten.
#[test]
fn removal_does_not_follow_a_recorded_folder_replaced_by_a_junction() {
    let (dir, data, ws) = setup();
    let _unlabel = Unlabel(data.clone());
    let granted = ws.join("granted");
    std::fs::create_dir_all(&granted).unwrap();
    assert_eq!(
        ensure_low(&data, &Protected::here(&data), &granted, "grant"),
        Ok(Ensured::Labelled)
    );
    let moved = ws.join("granted-moved");
    std::fs::rename(&granted, &moved).unwrap();
    let _clear = ClearLabel(moved.clone());
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("f.txt"), "x").unwrap();
    junction(&granted, &outside);
    let before = descriptors(&outside);
    let removal = remove_all(&data).unwrap();
    assert_eq!(removal.changed, vec![real(&ws).join("granted")]);
    assert!(removal.restored.is_empty() && removal.failed.is_empty());
    assert_eq!(descriptors(&outside), before);
    assert!(list(&data).unwrap().is_empty());
    std::fs::remove_dir(&granted).unwrap();
}

fn junction(link: &Path, target: &Path) {
    use std::os::windows::process::CommandExt as _;
    let made = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
}

#[test]
fn the_cargo_config_copy_has_no_credentials() {
    let text = "
[registry]
token = \"cio_secret\"
default = \"mine\"
global-credential-providers = [\"cargo:token\"]

[registries.mine]
index = \"sparse+https://example.com/index/\"
token = \"mine_secret\"
secret-key = \"k3.secret.abc\"
secret-key-subject = \"subj\"

[registries.inline]
index = \"https://example.org/git\"

[build]
jobs = 4
";
    let copied = without_credentials(text).unwrap();
    for secret in ["cio_secret", "mine_secret", "k3.secret", "subj"] {
        assert!(!copied.contains(secret), "{copied}");
    }
    for kept in [
        "sparse+https://example.com/index/",
        "https://example.org/git",
        "jobs = 4",
        "default = \"mine\"",
    ] {
        assert!(copied.contains(kept), "{kept}: {copied}");
    }
    // An inline table's token goes too.
    let inline = without_credentials("[registries]\nx = { index = \"i\", token = \"t0k\" }\n").unwrap();
    assert!(!inline.contains("t0k") && inline.contains("index"), "{inline}");
    assert!(without_credentials("not = [toml").is_none());
}
