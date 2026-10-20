//! Our firmware's GitHub releases: finding one, and downloading its image with the checksum
//! checked before anything is flashed.
//!
//! The asset convention a release has to follow:
//!
//! - exactly one asset whose name ends in `.s19`: the image, as `objcopy -O srec` writes it;
//! - an asset named the same with `.sha256` appended, holding the image's SHA-256 in hex, as
//!   `sha256sum` writes it (`<hex>  <name>`). A file name after the hash, if present, must be the
//!   image's.
//!
//! The release's tag is the version: a probe built at that tag reports the same string.
//!
//! GitHub also reports a SHA-256 of every asset it stores (`digest`). Where it does, the image
//! has to match that as well, so a checksum file uploaded alongside a wrong image is caught.

use std::fmt;
use std::str::FromStr;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::image::{Image, ImageError};
use crate::progress::{Progress, Step};

/// Where releases are looked up when nothing says otherwise. A placeholder until the
/// repository is public under its final name.
pub const DEFAULT_REPO: &str = "CordlessCoder/mculink-cjtag-fw";

const API: &str = "https://api.github.com";
/// Generous for a 250 KB image, and small enough that a wrong URL cannot fill memory.
pub const DOWNLOAD_LIMIT: u64 = 8 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    pub owner: String,
    pub name: String,
}

impl Default for Repo {
    fn default() -> Repo {
        DEFAULT_REPO.parse().expect("DEFAULT_REPO is owner/name")
    }
}

impl FromStr for Repo {
    type Err = String;
    fn from_str(text: &str) -> Result<Repo, String> {
        match text.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
                Ok(Repo {
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                })
            }
            _ => Err(format!("expected OWNER/NAME, got {text:?}")),
        }
    }
}

impl fmt::Display for Repo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    #[error("HTTP {status} from {url}")]
    Status { status: u16, url: String },
    #[error("{0}")]
    Transport(String),
}

/// An HTTP GET, so that tests can answer from recorded replies.
pub trait Http {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>, HttpError>;
}

/// HTTPS through `ureq`.
pub struct Ureq(ureq::Agent);

impl Default for Ureq {
    fn default() -> Ureq {
        Ureq(ureq::Agent::new_with_defaults())
    }
}

impl Http for Ureq {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
        let mut request = self.0.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        match request.call() {
            Ok(mut response) => response
                .body_mut()
                .with_config()
                .limit(DOWNLOAD_LIMIT)
                .read_to_vec()
                .map_err(|error| HttpError::Transport(error.to_string())),
            Err(ureq::Error::StatusCode(status)) => Err(HttpError::Status {
                status,
                url: url.to_owned(),
            }),
            Err(error) => Err(HttpError::Transport(error.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReleaseError {
    #[error("{repo} has no published release{}", tag.as_ref().map(|t| format!(" tagged {t}")).unwrap_or_default())]
    NotFound { repo: String, tag: Option<String> },
    #[error(
        "GitHub refused the request (HTTP {0}): the rate limit for unauthenticated use, or a private repository without a token"
    )]
    Refused(u16),
    #[error(transparent)]
    Http(HttpError),
    #[error("GitHub's reply did not parse: {0}")]
    Json(String),
    #[error("release {tag} has {found} .s19 assets; the convention is exactly one")]
    ImageAsset { tag: String, found: usize },
    #[error("release {tag} has no {name}, so its image cannot be checked")]
    NoChecksum { tag: String, name: String },
    #[error("{name} does not start with a SHA-256 in hex")]
    BadChecksumFile { name: String },
    #[error("{name} is for {named}, not {image}")]
    ChecksumNamesAnother {
        name: String,
        named: String,
        image: String,
    },
    #[error("{name} was {got} bytes, and the release says {expected}")]
    WrongSize {
        name: String,
        expected: u64,
        got: usize,
    },
    #[error(
        "{name} has SHA-256 {got}, and the release's checksum says {expected}. Nothing was flashed"
    )]
    ChecksumMismatch {
        name: String,
        expected: String,
        got: String,
    },
    #[error(
        "{name} has SHA-256 {got}, and GitHub's digest of it says {expected}. Nothing was flashed"
    )]
    DigestMismatch {
        name: String,
        expected: String,
        got: String,
    },
    #[error("{name}: {error}")]
    Image { name: String, error: ImageError },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    /// The public download URL.
    pub download_url: String,
    /// The API URL, which serves the bytes to a token for a private repository.
    pub api_url: String,
    /// GitHub's own SHA-256 of the asset, in hex, where the API reports one.
    pub sha256: Option<String>,
}

/// A release that follows the asset convention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub name: Option<String>,
    pub published: Option<String>,
    pub page: String,
    pub prerelease: bool,
    pub image: Asset,
    pub checksum: Asset,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    name: Option<String>,
    published_at: Option<String>,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    url: String,
    #[serde(default)]
    digest: Option<String>,
}

impl From<ApiAsset> for Asset {
    fn from(asset: ApiAsset) -> Asset {
        Asset {
            name: asset.name,
            size: asset.size,
            download_url: asset.browser_download_url,
            api_url: asset.url,
            sha256: asset
                .digest
                .and_then(|d| d.strip_prefix("sha256:").map(str::to_ascii_lowercase)),
        }
    }
}

/// A repository's releases. Public releases need no token.
pub struct GitHub<H> {
    http: H,
    repo: Repo,
    token: Option<String>,
}

impl<H: Http> GitHub<H> {
    pub fn new(http: H, repo: Repo, token: Option<String>) -> GitHub<H> {
        GitHub { http, repo, token }
    }

    #[must_use]
    pub fn repo(&self) -> &Repo {
        &self.repo
    }

    fn get(&self, url: &str, accept: &str) -> Result<Vec<u8>, HttpError> {
        let auth = self.token.as_ref().map(|token| format!("Bearer {token}"));
        let mut headers = vec![
            ("Accept", accept),
            ("X-GitHub-Api-Version", "2022-11-28"),
            (
                "User-Agent",
                concat!("mculink-updater/", env!("CARGO_PKG_VERSION")),
            ),
        ];
        if let Some(auth) = &auth {
            headers.push(("Authorization", auth));
        }
        self.http.get(url, &headers)
    }

    fn release(&self, url: &str, tag: Option<&str>) -> Result<Release, ReleaseError> {
        let body = self
            .get(url, "application/vnd.github+json")
            .map_err(|error| match error {
                HttpError::Status { status: 404, .. } => ReleaseError::NotFound {
                    repo: self.repo.to_string(),
                    tag: tag.map(str::to_owned),
                },
                HttpError::Status {
                    status: status @ (401 | 403 | 429),
                    ..
                } => ReleaseError::Refused(status),
                other => ReleaseError::Http(other),
            })?;
        let release: ApiRelease =
            serde_json::from_slice(&body).map_err(|error| ReleaseError::Json(error.to_string()))?;
        conventional(release)
    }

    /// The latest release: GitHub's choice, which skips drafts and prereleases.
    pub fn latest(&self) -> Result<Release, ReleaseError> {
        let url = format!(
            "{API}/repos/{}/{}/releases/latest",
            self.repo.owner, self.repo.name
        );
        self.release(&url, None)
    }

    pub fn tagged(&self, tag: &str) -> Result<Release, ReleaseError> {
        let url = format!(
            "{API}/repos/{}/{}/releases/tags/{tag}",
            self.repo.owner, self.repo.name
        );
        self.release(&url, Some(tag))
    }

    fn fetch(&self, asset: &Asset) -> Result<Vec<u8>, ReleaseError> {
        let bytes = match &self.token {
            Some(_) => self.get(&asset.api_url, "application/octet-stream"),
            None => self.get(&asset.download_url, "application/octet-stream"),
        }
        .map_err(ReleaseError::Http)?;
        if bytes.len() as u64 != asset.size {
            return Err(ReleaseError::WrongSize {
                name: asset.name.clone(),
                expected: asset.size,
                got: bytes.len(),
            });
        }
        Ok(bytes)
    }

    /// Downloads the release's image and its checksum, and parses the image only once the
    /// checksum matches.
    pub fn download(
        &self,
        release: &Release,
        progress: &mut dyn Progress,
    ) -> Result<Image, ReleaseError> {
        progress.start(Step::Download, release.image.size + release.checksum.size);
        let sums = self.fetch(&release.checksum)?;
        progress.advance(release.checksum.size);
        let expected = parse_checksum(&sums, &release.checksum.name, &release.image.name)?;
        let bytes = self.fetch(&release.image)?;
        progress.advance(release.image.size + release.checksum.size);
        let got = hex::encode(Sha256::digest(&bytes));
        if got != expected {
            return Err(ReleaseError::ChecksumMismatch {
                name: release.image.name.clone(),
                expected,
                got,
            });
        }
        if let Some(digest) = &release.image.sha256
            && *digest != got
        {
            return Err(ReleaseError::DigestMismatch {
                name: release.image.name.clone(),
                expected: digest.clone(),
                got,
            });
        }
        let image = Image::parse(&bytes).map_err(|error| ReleaseError::Image {
            name: release.image.name.clone(),
            error,
        })?;
        image
            .check_for_mculink()
            .map_err(|error| ReleaseError::Image {
                name: release.image.name.clone(),
                error,
            })?;
        Ok(image)
    }
}

fn conventional(release: ApiRelease) -> Result<Release, ReleaseError> {
    let ApiRelease {
        tag_name,
        name,
        published_at,
        html_url,
        prerelease,
        assets,
    } = release;
    let (images, others): (Vec<ApiAsset>, Vec<ApiAsset>) = assets
        .into_iter()
        .partition(|asset| asset.name.ends_with(".s19"));
    let [image] = <[ApiAsset; 1]>::try_from(images).map_err(|images| ReleaseError::ImageAsset {
        tag: tag_name.clone(),
        found: images.len(),
    })?;
    let checksum_name = format!("{}.sha256", image.name);
    let checksum = others
        .into_iter()
        .find(|asset| asset.name == checksum_name)
        .ok_or_else(|| ReleaseError::NoChecksum {
            tag: tag_name.clone(),
            name: checksum_name,
        })?;
    Ok(Release {
        tag: tag_name,
        name,
        published: published_at,
        page: html_url,
        prerelease,
        image: image.into(),
        checksum: checksum.into(),
    })
}

/// The hex digest from a `sha256sum`-style file, lowercased.
pub fn parse_checksum(bytes: &[u8], name: &str, image: &str) -> Result<String, ReleaseError> {
    let bad = || ReleaseError::BadChecksumFile {
        name: name.to_owned(),
    };
    let text = std::str::from_utf8(bytes).map_err(|_| bad())?;
    let mut words = text.split_whitespace();
    let digest = words.next().ok_or_else(bad)?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    if let Some(named) = words.next() {
        // `sha256sum -b` marks the name with '*'.
        let named = named.strip_prefix('*').unwrap_or(named);
        if named != image {
            return Err(ReleaseError::ChecksumNamesAnother {
                name: name.to_owned(),
                named: named.to_owned(),
                image: image.to_owned(),
            });
        }
    }
    Ok(digest.to_ascii_lowercase())
}
