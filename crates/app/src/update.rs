//! Updates from the project's GitHub releases: the latest release's version against this build's,
//! and its `.msi` downloaded and handed to Windows Installer when the user says so.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

const REPOSITORY: &str = "alexandroslessis/hermes-rs";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Automatic checks at most this often.
pub const CHECK_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub page: String,
    /// (file name, download URL) of the installer, when the release has one.
    pub installer: Option<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UpdateStatus {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    Failed(String),
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// `v1.2.3` / `1.2.3-beta` → (1, 2, 3); missing parts are 0.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>());
    let major = parts.next()?.ok()?;
    let minor = parts.next().unwrap_or(Ok(0)).ok()?;
    let patch = parts.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch))
}

/// Whether `latest` is newer than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!("hermes-rs/{CURRENT_VERSION}"))
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

/// The newest published release, when it's newer than this build.
pub async fn check() -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let response = client()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    // No releases published yet.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let release: GithubRelease = response
        .error_for_status()?
        .json()
        .await
        .context("unexpected answer from GitHub")?;
    if release.draft || release.prerelease || !is_newer(&release.tag_name, CURRENT_VERSION) {
        return Ok(None);
    }
    let installer = release
        .assets
        .into_iter()
        .find(|asset| asset.name.to_lowercase().ends_with(".msi"))
        .map(|asset| (asset.name, asset.browser_download_url));
    Ok(Some(Release {
        version: release.tag_name.trim_start_matches(['v', 'V']).to_string(),
        page: release.html_url,
        installer,
    }))
}

/// Downloads the release's installer into the temp folder.
pub async fn download(release: &Release) -> Result<PathBuf> {
    let Some((name, url)) = &release.installer else {
        bail!("this release has no installer");
    };
    // Only a plain file name, so the download can't land outside the folder.
    if name.contains(['/', '\\']) || name.contains("..") {
        bail!("unexpected installer name {name:?}");
    }
    let bytes = client()?
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let dir = std::env::temp_dir().join("hermes-rs-update");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    std::fs::write(&path, &bytes)?;
    Ok(path)
}

/// Starts Windows Installer on the downloaded package (the app quits so it can be replaced).
pub fn run_installer(path: &std::path::Path) -> Result<()> {
    std::process::Command::new("msiexec")
        .arg("/i")
        .arg(path)
        .spawn()
        .context("couldn't start Windows Installer")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("v0.10.0", "0.9.3"));
        assert!(is_newer("1.0", "0.99.99"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-beta", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }
}
