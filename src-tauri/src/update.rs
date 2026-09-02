use crate::models::AppUpdate;
use anyhow::{bail, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const GITHUB_REPO: &str = "brandonrstone/league-of-legends-counterpicker";

#[derive(Debug, Deserialize)]
pub struct GithubRelease {
    pub tag_name: String,
    pub assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
pub struct GithubAsset {
    pub name: String,
    pub browser_download_url: String,
}

pub fn normalize_version(raw: &str) -> String {
    raw.trim().trim_start_matches('v').trim_start_matches('V').to_string()
}

pub fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let mut parts = normalize_version(raw).split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

pub fn version_is_newer(remote: &str, current: &str) -> bool {
    match (parse_version(remote), parse_version(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

pub fn pick_setup_asset(assets: &[GithubAsset]) -> Option<&GithubAsset> {
    let setup = assets.iter().find(|a| {
        let n = a.name.to_ascii_lowercase();
        n.ends_with(".exe") && n.contains("setup")
    });
    setup.or_else(|| {
        assets
            .iter()
            .find(|a| a.name.to_ascii_lowercase().ends_with(".exe"))
    })
}

pub fn offer_from_release(release: &GithubRelease, current: &str) -> Option<AppUpdate> {
    if !version_is_newer(&release.tag_name, current) {
        return None;
    }
    let asset = pick_setup_asset(&release.assets)?;
    let version = normalize_version(&release.tag_name);
    Some(AppUpdate {
        version: version.clone(),
        download_url: asset.browser_download_url.clone(),
        asset_name: safe_filename(&asset.name),
        status: "available".into(),
        progress: 0.0,
        message: format!("Version {version} is available"),
    })
}

pub fn safe_filename(name: &str) -> String {
    Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .unwrap_or("RiftCounterpick-setup.exe")
        .to_string()
}

pub async fn fetch_latest_release(client: &reqwest::Client) -> Result<GithubRelease> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let res = client
        .get(url)
        .header("User-Agent", "Rift-Counterpick")
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(12))
        .send()
        .await?;
    if !res.status().is_success() {
        bail!("GitHub release check failed ({})", res.status());
    }
    Ok(res.json().await?)
}

pub async fn download_installer(
    client: &reqwest::Client,
    url: &str,
    dest: &PathBuf,
) -> Result<()> {
    let res = client
        .get(url)
        .header("User-Agent", "Rift-Counterpick")
        .timeout(Duration::from_secs(180))
        .send()
        .await?;
    if !res.status().is_success() {
        bail!("download failed ({})", res.status());
    }
    let bytes = res.bytes().await?;
    if bytes.len() < 1024 {
        bail!("download was too small to be an installer");
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_compare_numerically() {
        assert!(version_is_newer("1.0.2", "1.0.1"));
        assert!(version_is_newer("v1.1.0", "1.0.9"));
        assert!(!version_is_newer("1.0.1", "1.0.1"));
        assert!(!version_is_newer("1.0.0", "1.0.1"));
    }

    #[test]
    fn picks_nsis_setup_exe() {
        let assets = vec![
            GithubAsset {
                name: "notes.txt".into(),
                browser_download_url: "https://example.com/notes".into(),
            },
            GithubAsset {
                name: "Rift.Counterpick_1.0.2_x64-setup.exe".into(),
                browser_download_url: "https://example.com/setup.exe".into(),
            },
        ];
        let picked = pick_setup_asset(&assets).unwrap();
        assert!(picked.name.ends_with("setup.exe"));
    }

    #[test]
    fn offer_skips_same_or_older_releases() {
        let release = GithubRelease {
            tag_name: "v1.0.1".into(),
            assets: vec![GithubAsset {
                name: "Rift.Counterpick_1.0.1_x64-setup.exe".into(),
                browser_download_url: "https://example.com/setup.exe".into(),
            }],
        };
        assert!(offer_from_release(&release, "1.0.1").is_none());
        assert!(offer_from_release(&release, "1.0.2").is_none());
        let offer = offer_from_release(&release, "1.0.0").unwrap();
        assert_eq!(offer.version, "1.0.1");
        assert_eq!(offer.status, "available");
    }

    #[test]
    fn rejects_path_traversal_in_asset_names() {
        assert_eq!(safe_filename("../../evil.exe"), "evil.exe");
        assert_eq!(safe_filename("Rift.Counterpick_1.0.1_x64-setup.exe"), "Rift.Counterpick_1.0.1_x64-setup.exe");
    }
}
