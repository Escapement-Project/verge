//! Where an image comes from: a release of ours, a file, or the user's LinkServer install.

use std::path::PathBuf;

use crate::detect::{Hardware, Unit};
use crate::image::{Image, ImageError};
use crate::progress::Progress;
use crate::release::{GitHub, Http, Release, ReleaseError};
use crate::stock;
use crate::version::Relation;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A release of ours: the latest, or the one with this tag.
    Release(Option<String>),
    /// An ELF or S-record file.
    File(PathBuf),
    /// The newest stock image in LinkServer's usual places, or in this directory.
    Stock(Option<PathBuf>),
}

/// What an image is, as far as telling a unit that runs it already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageVersion {
    /// A release of ours, by its tag.
    Ours(String),
    Stock(crate::version::StockVersion),
    /// A file: nothing says which build it is.
    Unknown,
}

/// An image ready to flash, and how to name it to the user.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub image: Image,
    pub name: String,
    pub release: Option<Release>,
    pub version: ImageVersion,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{path}: {error}")]
    File { path: String, error: ImageError },
    #[error(transparent)]
    Release(#[from] ReleaseError),
    #[error(
        "no MCU-LINK_CMSIS-DAP_V*.s19 ({searched}). Install NXP's LinkServer, or name its probe_firmware directory"
    )]
    NoStock { searched: String },
    #[error("the unit already runs {tag}")]
    AlreadyRuns { tag: String },
    #[error("the unit runs {build}, which is newer than release {tag}")]
    Newer { build: String, tag: String },
    #[error(
        "the unit is {board}, and this firmware is built for the plain MCU-Link only. NXP's stock image can go on it"
    )]
    OtherBoard { board: String },
}

fn from_file(path: &std::path::Path) -> Result<Image, LoadError> {
    Image::from_path(path).map_err(|error| LoadError::File {
        path: path.display().to_string(),
        error,
    })
}

/// Finds the release a [`Source::Release`] names, without downloading it.
pub fn release<H: Http>(github: &GitHub<H>, tag: Option<&str>) -> Result<Release, ReleaseError> {
    match tag {
        None | Some("latest") => github.latest(),
        Some(tag) => github.tagged(tag),
    }
}

/// Whether installing `release` on `unit` is an update. A unit not running ours always takes it.
pub fn check_release(unit: Option<&Unit>, release: &Release, force: bool) -> Result<(), LoadError> {
    let Some(build) = unit.and_then(Unit::build) else {
        return Ok(());
    };
    match build.compare(&release.tag) {
        Relation::UpToDate if !force => Err(LoadError::AlreadyRuns {
            tag: release.tag.clone(),
        }),
        Relation::Ahead if !force => Err(LoadError::Newer {
            build: build.raw,
            tag: release.tag.clone(),
        }),
        _ => Ok(()),
    }
}

/// Whether `source` may go on `unit`. Stock's image goes anywhere stock runs; anything else is
/// ours, a release or a file, and goes only on the plain MCU-Link. A unit that does not say which
/// board it is, such as one in ISP mode, is not refused.
pub fn check_board(unit: Option<&Unit>, source: &Source) -> Result<(), LoadError> {
    if matches!(source, Source::Stock(_)) {
        return Ok(());
    }
    match unit.map(Unit::hardware) {
        Some(Hardware::Other(board)) => Err(LoadError::OtherBoard { board }),
        Some(Hardware::McuLink | Hardware::Unknown) | None => Ok(()),
    }
}

/// Loads the image `source` names. Refuses an image of ours for a unit known to be another
/// board. For a release, refuses one `unit` already runs or is ahead of unless `force`, and
/// downloads it with its checksums checked.
pub fn load<H: Http>(
    source: &Source,
    github: &GitHub<H>,
    unit: Option<&Unit>,
    force: bool,
    progress: &mut dyn Progress,
) -> Result<Loaded, LoadError> {
    check_board(unit, source)?;
    match source {
        Source::File(path) => {
            let image = from_file(path)?;
            let name = format!("{} ({})", path.display(), image.format);
            Ok(Loaded {
                image,
                name,
                release: None,
                version: ImageVersion::Unknown,
            })
        }
        Source::Stock(dir) => {
            let dirs = match dir {
                Some(dir) => vec![dir.clone()],
                None => stock::search_dirs(),
            };
            let Some(found) = stock::find(&dirs).into_iter().next() else {
                let searched = if dirs.is_empty() {
                    "found no LinkServer install".to_owned()
                } else {
                    let names: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
                    format!("looked in {}", names.join(", "))
                };
                return Err(LoadError::NoStock { searched });
            };
            let image = from_file(&found.path)?;
            Ok(Loaded {
                image,
                name: format!("stock V{} from {}", found.version, found.path.display()),
                release: None,
                version: ImageVersion::Stock(found.version),
            })
        }
        Source::Release(tag) => {
            let release = release(github, tag.as_deref())?;
            check_release(unit, &release, force)?;
            let image = github.download(&release, progress)?;
            Ok(Loaded {
                image,
                name: format!("release {} ({})", release.tag, release.image.name),
                version: ImageVersion::Ours(release.tag.clone()),
                release: Some(release),
            })
        }
    }
}
