//! A simulated GitHub, serving releases the way the REST API does.
//!
//! The JSON carries the fields the API returns that the tool reads, plus a few it ignores. The
//! tests hold the parser to a recorded reply as well (`tests/fixtures/github-release-probe-rs.json`).

use std::sync::{Arc, Mutex};

use serde_json::json;
use sha2::{Digest, Sha256};

use super::{SimFirmware, firmware_srecord};
use crate::release::{Http, HttpError, Repo};

#[derive(Clone, Debug)]
pub struct SimAsset {
    pub name: String,
    pub bytes: Vec<u8>,
    /// What GitHub reports as the asset's digest; `None` leaves the field out.
    pub digest: Option<String>,
}

impl SimAsset {
    #[must_use]
    pub fn new(name: &str, bytes: Vec<u8>) -> SimAsset {
        let digest = Some(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))));
        SimAsset {
            name: name.to_owned(),
            bytes,
            digest,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SimRelease {
    pub tag: String,
    pub prerelease: bool,
    pub assets: Vec<SimAsset>,
}

struct Inner {
    repo: Repo,
    private: bool,
    releases: Vec<SimRelease>,
    requests: Vec<String>,
}

#[derive(Clone)]
pub struct SimHttp(Arc<Mutex<Inner>>);

/// The image and checksum assets a release of ours carries, by the convention in
/// [`crate::release`].
#[must_use]
pub fn ours_assets(tag: &str) -> Vec<SimAsset> {
    let image = firmware_srecord(&SimFirmware::Ours {
        build: tag.to_owned(),
    })
    .into_bytes();
    let name = format!("mculink-cjtag-fw-{tag}.s19");
    let sums = format!("{}  {name}\n", hex::encode(Sha256::digest(&image)));
    vec![
        SimAsset::new(&name, image),
        SimAsset::new(&format!("{name}.sha256"), sums.into_bytes()),
    ]
}

impl SimHttp {
    #[must_use]
    pub fn new(repo: Repo) -> SimHttp {
        SimHttp(Arc::new(Mutex::new(Inner {
            repo,
            private: false,
            releases: Vec::new(),
            requests: Vec::new(),
        })))
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0
            .lock()
            .expect("the simulated GitHub lock is not poisoned")
    }

    /// A repository that answers only requests carrying a token.
    pub fn set_private(&self, private: bool) {
        self.inner().private = private;
    }

    pub fn publish(&self, release: SimRelease) {
        self.inner().releases.push(release);
    }

    /// Publishes a release of ours that follows the convention.
    pub fn publish_ours(&self, tag: &str) {
        self.publish(SimRelease {
            tag: tag.to_owned(),
            prerelease: false,
            assets: ours_assets(tag),
        });
    }

    /// Every URL asked for, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.inner().requests.clone()
    }

    fn release_json(repo: &Repo, release: &SimRelease, index: usize) -> serde_json::Value {
        let base = format!("https://api.github.com/repos/{}/{}", repo.owner, repo.name);
        let assets: Vec<serde_json::Value> = release
            .assets
            .iter()
            .enumerate()
            .map(|(n, asset)| {
                json!({
                    "url": format!("{base}/releases/assets/{}", index * 100 + n),
                    "id": index * 100 + n,
                    "name": asset.name,
                    "label": "",
                    "content_type": "application/octet-stream",
                    "state": "uploaded",
                    "size": asset.bytes.len(),
                    "digest": asset.digest,
                    "browser_download_url": format!(
                        "https://github.com/{}/{}/releases/download/{}/{}",
                        repo.owner, repo.name, release.tag, asset.name
                    ),
                })
            })
            .collect();
        json!({
            "url": format!("{base}/releases/{index}"),
            "html_url": format!("https://github.com/{}/{}/releases/tag/{}", repo.owner, repo.name, release.tag),
            "id": index,
            "tag_name": release.tag,
            "name": format!("Release {}", release.tag),
            "draft": false,
            "prerelease": release.prerelease,
            "created_at": "2026-10-20T12:00:00Z",
            "published_at": "2026-10-20T12:00:00Z",
            "assets": assets,
        })
    }
}

impl Http for SimHttp {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
        let mut inner = self.inner();
        inner.requests.push(url.to_owned());
        let not_found = || HttpError::Status {
            status: 404,
            url: url.to_owned(),
        };
        let authorised = headers.iter().any(|(name, _)| *name == "Authorization");
        let repo = inner.repo.clone();
        let api = format!(
            "https://api.github.com/repos/{}/{}/releases/",
            repo.owner, repo.name
        );
        let download = format!(
            "https://github.com/{}/{}/releases/download/",
            repo.owner, repo.name
        );
        if let Some(rest) = url.strip_prefix(&api) {
            if inner.private && !authorised {
                return Err(not_found());
            }
            let found = if rest == "latest" {
                inner
                    .releases
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, r)| !r.prerelease)
            } else if let Some(tag) = rest.strip_prefix("tags/") {
                inner
                    .releases
                    .iter()
                    .enumerate()
                    .find(|(_, r)| r.tag == tag)
            } else if let Some(id) = rest.strip_prefix("assets/") {
                let id: usize = id.parse().map_err(|_| not_found())?;
                let asset = inner
                    .releases
                    .get(id / 100)
                    .and_then(|r| r.assets.get(id % 100))
                    .ok_or_else(not_found)?;
                return Ok(asset.bytes.clone());
            } else {
                None
            };
            let (index, release) = found.ok_or_else(not_found)?;
            return Ok(Self::release_json(&repo, release, index)
                .to_string()
                .into_bytes());
        }
        if let Some(rest) = url.strip_prefix(&download) {
            if inner.private {
                return Err(not_found());
            }
            let (tag, name) = rest.split_once('/').ok_or_else(not_found)?;
            return inner
                .releases
                .iter()
                .find(|r| r.tag == tag)
                .and_then(|r| r.assets.iter().find(|a| a.name == name))
                .map(|a| a.bytes.clone())
                .ok_or_else(not_found);
        }
        Err(not_found())
    }
}
