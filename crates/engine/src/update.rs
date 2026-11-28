//! `update.check` and `update.download`: ask GitHub for the newest PR Craft Release and save its zip, checked against `SHA256SUMS.txt`.
//! Nothing here replaces the running app.

use std::path::{Path, PathBuf};

use craft::update::http::{Http, Stop};
use craft::update::protocol::LatestRelease;
use craft::update::release::{Asset, Release};
use craft::update::source::{GitHubSource, ReleaseSource};
use craft::update::version;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CommandError, params, store};

pub const COMMANDS: [&str; 2] = ["update.check", "update.download"];

const REPO: &str = "jirayusueb/prcraft";
const RUNNING: &str = env!("CARGO_PKG_VERSION");

/// The zip a Release carries for one build: `PR-Craft-0.2.0-macos-arm64.zip`. `xtask bundle` names its zip the same way.
pub fn zip_name(version: &str, arch: &str) -> String {
    format!("PR-Craft-{version}-macos-{arch}.zip")
}

#[derive(Debug, Serialize)]
pub struct AssetInfo {
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub running: String,
    pub latest: Option<String>,
    pub newer: bool,
    pub asset: Option<AssetInfo>,
    pub source: &'static str,
}

#[derive(Deserialize)]
struct Download {
    #[serde(default)]
    replace: bool,
}

/// The zip for this Mac: the `arm64` build on Apple silicon, else the `universal` one.
pub fn pick<'a>(release: &'a Release, running_arch: &str) -> Option<&'a Asset> {
    let label = release.version_label();
    let arch = if running_arch == "aarch64" { "arm64" } else { running_arch };
    [arch, "universal"].iter().find_map(|arch| {
        let name = zip_name(&label, arch);
        release.assets.iter().find(|asset| asset.name == name)
    })
}

/// Whether `latest` is worth offering to the copy running `running`.
pub fn newer(release: &Release, running: &str) -> bool {
    match (&release.version, version::parse(running)) {
        (Some(latest), Some(running)) => version::offers_update(latest, &running),
        _ => false,
    }
}

/// Where the zip goes, or why it must not be fetched: no checksum to verify it against, or a file already there.
fn destination(asset: &Asset, dir: &Path, replace: bool) -> Result<PathBuf, CommandError> {
    if asset.sha256.is_none() {
        return Err(CommandError::Invalid(format!("the release publishes no checksum for {}, so it was not downloaded", asset.name)));
    }
    let dest = dir.join(&asset.name);
    if dest.exists() && !replace {
        return Err(CommandError::Invalid(format!("{} already exists; pass replace: true to download it again", dest.display())));
    }
    Ok(dest)
}

fn latest() -> Result<(Http, Option<Release>, &'static str), CommandError> {
    let http = Http::new(&format!("pr-craft/{RUNNING}")).map_err(CommandError::Update)?;
    let cache = store::data_dir().map_err(CommandError::read)?.join("update-cache.json");
    let source = GitHubSource::new(http.clone(), cache);
    let answer = source.latest(REPO, false);
    source.save();
    let name = answer.source.label();
    match answer.latest {
        LatestRelease::Found(release) => Ok((http, Some(release), name)),
        LatestRelease::None => Ok((http, None, name)),
        LatestRelease::RateLimited => Err(CommandError::Update("GitHub's request limit was reached; try again later".into())),
        LatestRelease::Failed(reason) => Err(CommandError::Update(reason)),
    }
}

pub fn command(id: &str, value: &Value) -> Option<Result<Value, CommandError>> {
    Some(match id {
        "update.check" => check().and_then(|c| Ok(serde_json::to_value(c)?)),
        "update.download" => params::<Download>(value).and_then(|p| download(p.replace)).map(|path| serde_json::json!({ "path": path })),
        _ => return None,
    })
}

fn check() -> Result<Check, CommandError> {
    let (_, release, source) = latest()?;
    let asset = release.as_ref().and_then(|r| pick(r, std::env::consts::ARCH));
    Ok(Check {
        running: RUNNING.into(),
        latest: release.as_ref().map(Release::version_label),
        newer: release.as_ref().is_some_and(|r| newer(r, RUNNING)),
        asset: asset.map(|a| AssetInfo { name: a.name.clone(), size: a.size }),
        source,
    })
}

fn download(replace: bool) -> Result<PathBuf, CommandError> {
    let (http, release, _) = latest()?;
    let release = release.ok_or_else(|| CommandError::Invalid("no PR Craft release is published yet".into()))?;
    let asset = pick(&release, std::env::consts::ARCH)
        .ok_or_else(|| CommandError::Invalid(format!("release {} has no zip for this Mac", release.version_label())))?;
    let home = std::env::var_os("HOME").ok_or_else(|| CommandError::Invalid("HOME is not set, so there is no Downloads folder".into()))?;
    let dest = destination(asset, &PathBuf::from(home).join("Downloads"), replace)?;
    let sha256 = asset.sha256.as_deref().unwrap_or_default();
    http.download(&asset.url, &dest, asset.size, sha256, &Stop::default(), &mut |_| {}).map_err(CommandError::Update)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use craft::update::release::{parse_github_release, parse_sha256sums, release_from_sums};

    use super::*;

    const SUMS: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  PR-Craft-0.2.0-macos-arm64.zip\n\
                        bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  PR-Craft-0.2.0-macos-universal.zip\n";

    fn api_json() -> String {
        let base = "https://github.com/jirayusueb/prcraft/releases/download/v0.2.0";
        let asset = |name: &str| format!(r#"{{"name":"{name}","size":1234,"browser_download_url":"{base}/{name}"}}"#);
        format!(
            r#"{{"tag_name":"v0.2.0","name":"PR Craft v0.2.0","html_url":"https://github.com/jirayusueb/prcraft/releases/tag/v0.2.0","prerelease":false,"assets":[{},{},{}]}}"#,
            asset("PR-Craft-0.2.0-macos-universal.zip"),
            asset("PR-Craft-0.2.0-macos-arm64.zip"),
            asset("SHA256SUMS.txt")
        )
    }

    fn api_release() -> Release {
        let mut release = parse_github_release(&api_json(), REPO).unwrap();
        release.apply_sums(&parse_sha256sums(SUMS));
        release
    }

    #[test]
    fn apple_silicon_takes_its_own_zip_and_others_the_universal_one() {
        let release = api_release();
        assert_eq!(pick(&release, "aarch64").unwrap().name, "PR-Craft-0.2.0-macos-arm64.zip");
        assert_eq!(pick(&release, "x86_64").unwrap().name, "PR-Craft-0.2.0-macos-universal.zip");
        assert!(pick(&release, "aarch64").unwrap().sha256.is_some());
    }

    #[test]
    fn a_release_known_only_from_its_sums_has_the_same_choice() {
        let release = release_from_sums(REPO, "v0.2.0", &parse_sha256sums(SUMS));
        let asset = pick(&release, "aarch64").unwrap();
        assert_eq!(asset.name, "PR-Craft-0.2.0-macos-arm64.zip");
        assert!(asset.sha256.is_some());
    }

    #[test]
    fn a_release_without_a_zip_for_this_mac_offers_none() {
        let release = release_from_sums(REPO, "v0.2.0", &[("c".repeat(64), "notes.txt".into())]);
        assert!(pick(&release, "aarch64").is_none());
    }

    #[test]
    fn only_a_higher_version_is_newer() {
        let release = api_release();
        assert!(newer(&release, "0.1.0"));
        assert!(!newer(&release, "0.2.0"));
        assert!(!newer(&release, "0.3.0"));
    }

    #[test]
    fn an_asset_without_a_checksum_is_refused() {
        let asset = Asset { name: "PR-Craft-0.2.0-macos-arm64.zip".into(), size: 1, url: String::new(), sha256: None };
        let err = destination(&asset, Path::new("/nonexistent"), true).unwrap_err();
        assert!(err.to_string().contains("no checksum"), "{err}");
    }

    #[test]
    fn an_existing_file_is_kept_unless_replace_is_set() {
        let dir = std::env::temp_dir().join(format!("pr-craft-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let asset = pick(&api_release(), "aarch64").unwrap().clone();
        let dest = dir.join(&asset.name);
        assert_eq!(destination(&asset, &dir, false).unwrap(), dest);
        std::fs::write(&dest, b"old").unwrap();
        assert!(destination(&asset, &dir, false).is_err());
        assert_eq!(destination(&asset, &dir, true).unwrap(), dest);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
