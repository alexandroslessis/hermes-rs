//! Skill bundles, after Hermes's `agent/skill_bundles.py` (MIT): named groups of skills that one
//! slash command loads together. Each bundle is a YAML file in `skill-bundles/` next to the
//! skills folder (`name`, `description`, `skills: [...]`, optional `instruction`; the file stem is
//! the fallback name). `/<bundle>` loads every member skill into one message; a bundle wins over
//! a skill with the same slug.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::skills::SkillStore;

/// One bundle.
#[derive(Clone, Debug, PartialEq)]
pub struct Bundle {
    pub name: String,
    /// The slash command: `/<slug>`.
    pub slug: String,
    pub description: String,
    pub skills: Vec<String>,
    /// Extra guidance the bundle adds to every invocation.
    pub instruction: String,
    pub path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct BundleFile {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,
    #[serde(default)]
    skills: Vec<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    instruction: String,
}

/// Hermes's `slugify_skill_name`: lowercase, runs of anything but letters and digits become `-`.
pub fn slugify(name: &str) -> String {
    name.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Hermes's `_scaffold_header`: what a multi-skill message says before the skill blocks.
fn scaffold_header(
    bundle: &str,
    loaded: &[String],
    missing: &[String],
    bundle_instruction: &str,
    user_instruction: &str,
) -> String {
    let mut lines = vec![
        format!(
            "[IMPORTANT: The user has invoked the \"{bundle}\" skill bundle, loading {} skills together. Treat every skill below as active guidance for this turn.]",
            loaded.len()
        ),
        String::new(),
        format!("Bundle: {bundle}"),
        format!("Skills loaded: {}", loaded.join(", ")),
    ];
    if !missing.is_empty() {
        lines.push(format!("Skills missing (skipped): {}", missing.join(", ")));
    }
    if !bundle_instruction.is_empty() {
        lines.push(String::new());
        lines.push(format!("Bundle instruction: {bundle_instruction}"));
    }
    if !user_instruction.is_empty() {
        lines.push(String::new());
        lines.push(format!("User instruction: {user_instruction}"));
    }
    lines.join("\n")
}

impl SkillStore {
    fn bundles_dir(&self) -> PathBuf {
        self.root()
            .parent()
            .unwrap_or(self.root())
            .join("skill-bundles")
    }

    /// Every bundle, by slug; of two with the same slug the first file (alphabetically) wins.
    pub fn bundles(&self) -> Vec<Bundle> {
        let Ok(dir) = std::fs::read_dir(self.bundles_dir()) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = dir
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                path.extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
                })
            })
            .collect();
        files.sort();
        let mut bundles: Vec<Bundle> = Vec::new();
        for path in files {
            let Some(bundle) = load_bundle(&path) else {
                continue;
            };
            if bundles.iter().any(|b| b.slug == bundle.slug) {
                tracing::warn!(
                    "duplicate bundle /{} in {}; skipped",
                    bundle.slug,
                    path.display()
                );
                continue;
            }
            bundles.push(bundle);
        }
        bundles.sort_by(|a, b| a.slug.cmp(&b.slug));
        bundles
    }

    pub fn bundle(&self, name: &str) -> Option<Bundle> {
        let slug = slugify(name);
        self.bundles().into_iter().find(|b| b.slug == slug)
    }

    /// The message `/<bundle> instruction` sends (Hermes's `build_bundle_invocation_message`):
    /// every member skill, with missing ones noted. `None` when there's no such bundle or none of
    /// its skills exist.
    pub fn bundle_message(&self, name: &str, instruction: &str) -> Option<String> {
        let bundle = self.bundle(name)?;
        let mut loaded = Vec::new();
        let mut missing = Vec::new();
        let mut blocks = Vec::new();
        for member in &bundle.skills {
            if loaded.contains(member) || missing.contains(member) {
                continue;
            }
            let block = self.find(member).and_then(|skill| {
                let note = format!("[Loaded as part of the \"{}\" skill bundle.]", bundle.name);
                self.skill_block(&skill, &note, "")
                    .map(|block| (skill.name, block))
            });
            match block {
                Some((name, block)) => {
                    loaded.push(name);
                    blocks.push(block);
                }
                None => missing.push(member.clone()),
            }
        }
        if blocks.is_empty() {
            return None;
        }
        let header = scaffold_header(
            &bundle.name,
            &loaded,
            &missing,
            &bundle.instruction,
            instruction.trim(),
        );
        Some(
            std::iter::once(header)
                .chain(blocks)
                .collect::<Vec<_>>()
                .join("\n\n"),
        )
    }

    /// Writes a bundle (Hermes's `save_bundle`). `replacing` names the bundle being edited, whose
    /// file goes when the name changes.
    pub fn save_bundle(
        &self,
        name: &str,
        description: &str,
        skills: &[String],
        instruction: &str,
        replacing: Option<&str>,
    ) -> Result<Bundle, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Bundle name is required.".into());
        }
        let slug = slugify(name);
        if slug.is_empty() {
            return Err(format!("Bundle name '{name}' has no letters or digits."));
        }
        let skills: Vec<String> = skills
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if skills.is_empty() {
            return Err("A bundle needs at least one skill.".into());
        }
        let existing = self.bundle(&slug);
        let replacing = replacing.and_then(|name| self.bundle(name));
        if let Some(existing) = &existing {
            if replacing.as_ref().is_none_or(|r| r.path != existing.path) {
                return Err(format!("A bundle named /{slug} already exists."));
            }
        }
        if self.find(&slug).is_some() {
            tracing::info!("bundle /{slug} shadows the skill of the same name");
        }
        let dir = self.bundles_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{slug}.yaml"));
        let file = BundleFile {
            name: name.to_string(),
            description: description.trim().to_string(),
            skills: skills
                .iter()
                .cloned()
                .map(serde_yaml::Value::String)
                .collect(),
            instruction: instruction.trim().to_string(),
        };
        let text = serde_yaml::to_string(&file).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| e.to_string())?;
        if let Some(old) = replacing.filter(|old| old.path != path) {
            let _ = std::fs::remove_file(old.path);
        }
        load_bundle(&path).ok_or_else(|| "The bundle couldn't be read back.".to_string())
    }

    pub fn delete_bundle(&self, name: &str) -> Result<(), String> {
        let bundle = self
            .bundle(name)
            .ok_or_else(|| format!("No bundle named '{name}'."))?;
        std::fs::remove_file(&bundle.path).map_err(|e| e.to_string())
    }
}

/// Parses one bundle file; `None` (logged) when it's unusable, so one broken file can't hide
/// the others.
fn load_bundle(path: &Path) -> Option<Bundle> {
    let text = std::fs::read_to_string(path).ok()?;
    let file: BundleFile = match serde_yaml::from_str(text.trim_start_matches('\u{feff}')) {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!("invalid bundle {}: {error}", path.display());
            return None;
        }
    };
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let name = Some(file.name.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or(stem);
    let skills: Vec<String> = file
        .skills
        .iter()
        .filter_map(|s| match s {
            serde_yaml::Value::String(s) => Some(s.trim().to_string()),
            serde_yaml::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty())
        .collect();
    let slug = slugify(&name);
    if skills.is_empty() || slug.is_empty() {
        tracing::warn!(
            "bundle {} has no skills or no usable name; skipped",
            path.display()
        );
        return None;
    }
    let description = Some(file.description.trim().to_string())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| format!("Load {} skills as a bundle", skills.len()));
    Some(Bundle {
        name,
        slug,
        description,
        skills,
        instruction: file.instruction.trim().to_string(),
        path: path.to_path_buf(),
    })
}

/// The bundle and the user's instruction behind a bundle message, for showing it short.
pub fn describe_bundle_invocation(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix("[IMPORTANT: The user has invoked the \"")?;
    let (name, rest) = rest.split_once("\" skill bundle")?;
    let header = rest.split("\n\n[").next().unwrap_or_default();
    let instruction = header
        .lines()
        .find_map(|line| line.strip_prefix("User instruction: "))
        .unwrap_or_default()
        .to_string();
    Some((slugify(name), instruction))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SkillsConfig;
    use serde_json::json;

    fn store() -> (tempfile::TempDir, SkillStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SkillStore::new(dir.path().join("skills"), SkillsConfig::default());
        for (name, body) in [("alpha", "Do A."), ("beta", "Do B.")] {
            let content = format!(
                "---\nname: {name}\ndescription: Skill {name}.\n---\n\n# {name}\n\n{body}\n"
            );
            let created = store.manage_tool(
                &json!({"operations": [{"action": "create", "name": name, "content": content}]}),
            );
            assert_eq!(created["success"], true, "{created}");
        }
        (dir, store)
    }

    #[test]
    fn a_bundle_loads_its_skills_and_notes_the_missing_ones() {
        let (_dir, store) = store();
        let bundle = store
            .save_bundle(
                "Release Day",
                "Ship it.",
                &["alpha".into(), "gamma".into(), "beta".into()],
                "Check CI first.",
                None,
            )
            .unwrap();
        assert_eq!(bundle.slug, "release-day");
        let message = store.bundle_message("release-day", "go").unwrap();
        assert!(message.starts_with("[IMPORTANT: The user has invoked the \"Release Day\" skill bundle, loading 2 skills together."));
        assert!(message.contains("Skills loaded: alpha, beta"));
        assert!(message.contains("Skills missing (skipped): gamma"));
        assert!(message.contains("Bundle instruction: Check CI first."));
        assert!(message.contains("[Loaded as part of the \"Release Day\" skill bundle.]"));
        assert!(message.contains("Do A.") && message.contains("Do B."));
        assert_eq!(
            describe_bundle_invocation(&message),
            Some(("release-day".to_string(), "go".to_string()))
        );
    }

    #[test]
    fn bundles_are_saved_renamed_and_deleted() {
        let (_dir, store) = store();
        store
            .save_bundle("one", "", &["alpha".into()], "", None)
            .unwrap();
        assert!(
            store
                .save_bundle("One", "", &["beta".into()], "", None)
                .is_err()
        );
        assert!(store.save_bundle("x", "", &[], "", None).is_err());
        let renamed = store
            .save_bundle("two", "", &["alpha".into(), "beta".into()], "", Some("one"))
            .unwrap();
        let slugs: Vec<String> = store.bundles().into_iter().map(|b| b.slug).collect();
        assert_eq!(slugs, vec!["two"]);
        assert_eq!(renamed.description, "Load 2 skills as a bundle");
        store.delete_bundle("two").unwrap();
        assert!(store.bundles().is_empty());
    }
}
