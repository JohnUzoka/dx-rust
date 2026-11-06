//! What GitHub's release listing says, and which releases a check may offer.
use semver::Version;
use serde::Deserialize;
use std::ops::RangeInclusive;

use crate::{NRO_ASSET, TAG_PREFIX};

/// NRO sizes outside these are no build of this game.
const NRO_SIZES: RangeInclusive<u64> = (1 << 20)..=(512 << 20);

/// A release file: where it is, its size and the SHA-256 GitHub computed when it was uploaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asset {
    pub url: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// A published release newer than the running NRO, with the NRO an update needs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub version: Version,
    pub tag: String,
    pub notes: String,
    pub nro: Asset,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<ReleaseFile>,
}

#[derive(Deserialize)]
struct ReleaseFile {
    name: String,
    size: u64,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    digest: Option<String>,
    browser_download_url: String,
}

/// The releases in GitHub's listing (`GET /repos/{repository}/releases`) that are published (not drafts), tagged
/// `switch-v<version>` with a version above `installed` and no pre-release part, and have an uploaded `dxview.nro`
/// with a SHA-256 digest, downloaded from this repository's releases. Newest first. GitHub's own pre-release label
/// doesn't hold a release back (0.1.0 carries it): a build only for testing gets a version with a pre-release part
/// (`switch-v0.1.1-test.1`), or stays a draft.
pub(crate) fn newer(listing: &[u8], repository: &str, installed: &Version) -> Result<Vec<Candidate>, String> {
    let releases: Vec<Release> =
        serde_json::from_slice(listing).map_err(|e| format!("GitHub's release list is unreadable: {e}"))?;
    let downloads = format!("https://github.com/{repository}/releases/download/");
    let mut candidates: Vec<Candidate> = releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let version = Version::parse(release.tag_name.strip_prefix(TAG_PREFIX)?).ok()?;
            if !version.pre.is_empty() || version <= *installed {
                return None;
            }
            let nro = release.assets.iter().find(|file| {
                file.name == NRO_ASSET
                    && file.state.as_deref().is_none_or(|state| state == "uploaded")
                    && file.browser_download_url.starts_with(&downloads)
                    && NRO_SIZES.contains(&file.size)
            })?;
            Some(Candidate {
                nro: Asset {
                    url: nro.browser_download_url.clone(),
                    size: nro.size,
                    sha256: sha256_digest(nro.digest.as_deref()?)?,
                },
                version,
                tag: release.tag_name,
                notes: release.body.unwrap_or_default(),
            })
        })
        .collect();
    candidates.sort_by(|a, b| b.version.cmp(&a.version));
    Ok(candidates)
}

/// GitHub's `digest` for a release file, `sha256:` and 64 hex digits.
fn sha256_digest(digest: &str) -> Option<[u8; 32]> {
    let hex = digest.strip_prefix("sha256:")?.as_bytes();
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0; 32];
    for (byte, pair) in bytes.iter_mut().zip(hex.chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const REPO: &str = "JohnUzoka/dx-rust";

    fn nro(tag: &str) -> Value {
        json!({
            "name": NRO_ASSET, "size": 96 << 20, "state": "uploaded", "digest": format!("sha256:{}", "ab".repeat(32)),
            "browser_download_url": format!("https://github.com/{REPO}/releases/download/{tag}/{NRO_ASSET}"),
        })
    }

    fn release(tag: &str, draft: bool, prerelease: bool, files: Vec<Value>) -> Value {
        json!({ "tag_name": tag, "body": format!("notes for {tag}"), "draft": draft, "prerelease": prerelease, "assets": files })
    }

    fn tags(listing: &Value, installed: &str) -> Vec<String> {
        let candidates = newer(listing.to_string().as_bytes(), REPO, &Version::parse(installed).unwrap()).unwrap();
        candidates.into_iter().map(|c| c.tag).collect()
    }

    #[test]
    fn newer_keeps_published_switch_releases_with_a_checked_nro_newest_first() {
        let mut bad_url = nro("switch-v0.1.9");
        bad_url["browser_download_url"] = json!("https://example.com/dxview.nro");
        let mut no_digest = nro("switch-v0.2.1");
        no_digest["digest"] = Value::Null;
        let mut too_small = nro("switch-v0.2.2");
        too_small["size"] = json!(4096);
        let mut half_uploaded = nro("switch-v0.2.3");
        half_uploaded["state"] = json!("starter");
        let listing = json!([
            release("switch-v0.1.5", false, false, vec![nro("switch-v0.1.5")]),
            release("switch-v0.3.0", true, false, vec![nro("switch-v0.3.0")]),
            release("switch-v0.2.5", false, true, vec![nro("switch-v0.2.5")]),
            release("switch-v0.2.6-beta.1", false, false, vec![nro("switch-v0.2.6-beta.1")]),
            release("v0.11.0", false, false, vec![nro("v0.11.0")]),
            release("switch-v0.1.4", false, false, vec![nro("switch-v0.1.4")]),
            release("switch-v0.1.3", false, false, vec![nro("switch-v0.1.3")]),
            release("switch-v0.2.0", false, false, vec![]),
            release("switch-v0.1.9", false, false, vec![bad_url]),
            release("switch-v0.2.1", false, false, vec![no_digest]),
            release("switch-v0.2.2", false, false, vec![too_small]),
            release("switch-v0.2.3", false, false, vec![half_uploaded]),
            release("switch-v0.1.10", false, false, vec![nro("switch-v0.1.10")]),
        ]);
        // A draft, a pre-release version, another tag, older ones and NROs that can't be checked are left out; a
        // release GitHub labels pre-release (0.2.5) is offered like the rest.
        assert_eq!(tags(&listing, "0.1.4"), ["switch-v0.2.5", "switch-v0.1.10", "switch-v0.1.5"]);
        let first = &newer(listing.to_string().as_bytes(), REPO, &Version::new(0, 1, 4)).unwrap()[0];
        assert_eq!((first.nro.sha256, first.notes.as_str()), ([0xab; 32], "notes for switch-v0.2.5"));
    }

    #[test]
    fn a_test_build_is_offered_its_release() {
        let listing = json!([release("switch-v0.1.0", false, true, vec![nro("switch-v0.1.0")])]);
        assert_eq!(tags(&listing, "0.1.0-otatest"), ["switch-v0.1.0"]);
        assert!(tags(&listing, "0.1.0").is_empty());
    }
}
