use std::path::{Component, Path, PathBuf};

/// Resolves a path the model wrote: `~/x`, MSYS-style `/c/Users/x`, absolute Windows paths and
/// paths relative to the session's working directory all work.
pub fn resolve(raw: &str, cwd: &Path, home: &Path) -> PathBuf {
    let raw = raw.trim();
    let path = if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        home.join(rest)
    } else if let Some(windows) = msys_to_windows(raw) {
        PathBuf::from(windows)
    } else {
        let path = PathBuf::from(raw);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    };
    normalize(&path)
}

/// `/c/Users/x` → `C:/Users/x`.
fn msys_to_windows(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix('/')?;
    let mut chars = rest.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    let after = chars.as_str();
    (after.is_empty() || after.starts_with('/')).then(|| {
        format!(
            "{}:/{}",
            drive.to_ascii_uppercase(),
            after.trim_start_matches('/')
        )
    })
}

/// Removes `.` and `..` lexically (without touching the filesystem).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_shapes_models_write() {
        let cwd = Path::new("C:/work/project");
        let home = Path::new("C:/Users/me");
        assert_eq!(
            resolve("src/main.rs", cwd, home),
            PathBuf::from("C:/work/project/src/main.rs")
        );
        assert_eq!(
            resolve("../other/a.txt", cwd, home),
            PathBuf::from("C:/work/other/a.txt")
        );
        assert_eq!(
            resolve("~/notes.md", cwd, home),
            PathBuf::from("C:/Users/me/notes.md")
        );
        assert_eq!(
            resolve("/c/Users/me/x.txt", cwd, home),
            PathBuf::from("C:/Users/me/x.txt")
        );
        assert_eq!(
            resolve("D:\\data\\y.csv", cwd, home),
            PathBuf::from("D:\\data\\y.csv")
        );
    }
}
