//! Release lookup: GitHub's real reply format, the asset convention, and the checksum checks.

use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};
use updater_core::progress::Quiet;
use updater_core::release::{GitHub, Http, HttpError, ReleaseError, Repo, parse_checksum};
use updater_core::sim::http::{SimAsset, SimHttp, SimRelease, ours_assets};
use updater_core::sim::{SimBus, SimFirmware, firmware_image};
use updater_core::update::{self, Outcome, Timing};
use updater_core::version::Relation;
use updater_core::{Cancel, Firmware, State, detect};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

/// Answers every request with one body.
struct Fixed(Vec<u8>);

impl Http for Fixed {
    fn get(&self, _: &str, _: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
        Ok(self.0.clone())
    }
}

fn repo() -> Repo {
    "someone/mculink-cjtag-fw".parse().unwrap()
}

#[test]
fn a_recorded_github_reply_parses() {
    let recorded = fixture("github-release-probe-rs.json");
    // probe-rs's release has no .s19, so it parses and then fails the convention.
    let github = GitHub::new(Fixed(recorded.clone()), repo(), None);
    assert_eq!(
        github.latest(),
        Err(ReleaseError::ImageAsset {
            tag: "v0.32.0".into(),
            found: 0
        })
    );

    // The same reply with its two assets renamed to follow the convention.
    let mut json: Value = serde_json::from_slice(&recorded).unwrap();
    json["assets"][0]["name"] = "fw.s19.sha256".into();
    json["assets"][1]["name"] = "fw.s19".into();
    let github = GitHub::new(Fixed(json.to_string().into_bytes()), repo(), None);
    let release = github.latest().unwrap();
    let original: Value = serde_json::from_slice(&recorded).unwrap();
    let asset = &original["assets"][1];
    assert_eq!(release.tag, "v0.32.0");
    assert_eq!(release.page, original["html_url"].as_str().unwrap());
    assert_eq!(release.image.size, asset["size"].as_u64().unwrap());
    assert_eq!(
        release.image.download_url,
        asset["browser_download_url"].as_str().unwrap()
    );
    assert_eq!(release.image.api_url, asset["url"].as_str().unwrap());
    assert_eq!(
        release.image.sha256.as_deref(),
        asset["digest"].as_str().unwrap().strip_prefix("sha256:")
    );
}

/// cargo-dist's checksum file for a probe-rs asset, against GitHub's own digest of that asset:
/// two sources that do not share a computation.
#[test]
fn a_recorded_checksum_file_matches_githubs_digest() {
    let name = "probe-rs-tools-aarch64-apple-darwin.tar.xz";
    let sums = fixture(&format!("{name}.sha256"));
    let digest = parse_checksum(&sums, &format!("{name}.sha256"), name).unwrap();
    let json: Value = serde_json::from_slice(&fixture("github-release-probe-rs.json")).unwrap();
    let asset = json["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == name)
        .unwrap();
    assert_eq!(
        Some(format!("sha256:{digest}").as_str()),
        asset["digest"].as_str()
    );
    assert!(matches!(
        parse_checksum(&sums, "x.sha256", "another.tar.xz"),
        Err(ReleaseError::ChecksumNamesAnother { .. })
    ));
}

fn github(http: &SimHttp) -> GitHub<SimHttp> {
    GitHub::new(http.clone(), repo(), None)
}

#[test]
fn the_latest_release_downloads_and_checks_out() {
    let http = SimHttp::new(repo());
    http.publish_ours("v0.2.0");
    http.publish_ours("v0.3.0");
    http.publish(SimRelease {
        tag: "v0.4.0-rc1".into(),
        prerelease: true,
        assets: ours_assets("v0.4.0-rc1"),
    });
    let github = github(&http);
    let release = github.latest().unwrap();
    assert_eq!(release.tag, "v0.3.0");
    assert_eq!(release.image.name, "mculink-cjtag-fw-v0.3.0.s19");
    let image = github.download(&release, &mut Quiet).unwrap();
    assert_eq!(
        image.chunks,
        firmware_image(&SimFirmware::Ours {
            build: "v0.3.0".into()
        })
        .chunks
    );
    // Public downloads go to the public URLs and carry no token.
    assert!(http.requests().iter().all(|u| !u.contains("/assets/")));
    assert_eq!(github.tagged("v0.4.0-rc1").unwrap().tag, "v0.4.0-rc1");
}

fn published(assets: Vec<SimAsset>) -> SimHttp {
    let http = SimHttp::new(repo());
    http.publish(SimRelease {
        tag: "v0.3.0".into(),
        prerelease: false,
        assets,
    });
    http
}

#[test]
fn an_image_that_does_not_match_its_checksum_is_refused() {
    let mut assets = ours_assets("v0.3.0");
    // Change one byte of the image after the checksum file and the digest were made.
    let bytes = &mut assets[0].bytes;
    let at = bytes.len() / 2;
    bytes[at] = if bytes[at] == b'0' { b'1' } else { b'0' };
    let http = published(assets);
    let github = github(&http);
    let release = github.latest().unwrap();
    assert!(matches!(
        github.download(&release, &mut Quiet),
        Err(ReleaseError::ChecksumMismatch { .. })
    ));
}

#[test]
fn a_checksum_file_uploaded_for_a_wrong_image_is_caught_by_githubs_digest() {
    let mut assets = ours_assets("v0.3.0");
    let original_digest = assets[0].digest.clone();
    // A different image, with a checksum file made for it, under the first image's digest.
    let other = updater_core::sim::firmware_srecord(&SimFirmware::Ours {
        build: "v9.9.9".into(),
    })
    .into_bytes();
    let name = assets[0].name.clone();
    assets[0] = SimAsset {
        digest: original_digest,
        ..SimAsset::new(&name, other.clone())
    };
    assets[1] = SimAsset::new(
        &format!("{name}.sha256"),
        format!("{}  {name}\n", hex::encode(Sha256::digest(&other))).into_bytes(),
    );
    let http = published(assets);
    let github = github(&http);
    let release = github.latest().unwrap();
    assert!(matches!(
        github.download(&release, &mut Quiet),
        Err(ReleaseError::DigestMismatch { .. })
    ));
}

#[test]
fn releases_that_break_the_convention_are_refused() {
    let mut no_sums = ours_assets("v0.3.0");
    no_sums.pop();
    assert!(matches!(
        github(&published(no_sums)).latest(),
        Err(ReleaseError::NoChecksum { .. })
    ));

    let mut two_images = ours_assets("v0.3.0");
    two_images.push(SimAsset::new("armwire.s19", b"S0".to_vec()));
    assert_eq!(
        github(&published(two_images)).latest(),
        Err(ReleaseError::ImageAsset {
            tag: "v0.3.0".into(),
            found: 2
        })
    );

    let mut wrong_name = ours_assets("v0.3.0");
    let digest = hex::encode(Sha256::digest(&wrong_name[0].bytes));
    wrong_name[1] = SimAsset::new(
        &wrong_name[1].name.clone(),
        format!("{digest}  something-else.s19\n").into_bytes(),
    );
    let http = published(wrong_name);
    let release = github(&http).latest().unwrap();
    assert!(matches!(
        github(&http).download(&release, &mut Quiet),
        Err(ReleaseError::ChecksumNamesAnother { .. })
    ));
}

#[test]
fn a_missing_release_and_a_private_repository() {
    let http = SimHttp::new(repo());
    assert_eq!(
        github(&http).latest(),
        Err(ReleaseError::NotFound {
            repo: repo().to_string(),
            tag: None
        })
    );
    http.publish_ours("v0.3.0");
    http.set_private(true);
    assert!(matches!(
        github(&http).latest(),
        Err(ReleaseError::NotFound { .. })
    ));
    let with_token = GitHub::new(http.clone(), repo(), Some("t".into()));
    let release = with_token.latest().unwrap();
    with_token.download(&release, &mut Quiet).unwrap();
    assert!(
        http.requests()
            .iter()
            .any(|u| u.contains("/releases/assets/"))
    );
}

#[test]
fn from_release_to_running() {
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let bus = SimBus::scenario("bench").unwrap();
    let units = detect(&bus).unwrap();
    let unit = update::select(&units, Some("CJ3E7A91C2")).unwrap();

    let github = github(&http);
    let release = github.latest().unwrap();
    assert_eq!(
        unit.build().unwrap().compare(&release.tag),
        Relation::Unknown
    );
    let image = github.download(&release, &mut Quiet).unwrap();
    let outcome = update::update(
        &bus,
        unit,
        &image,
        &Timing::default(),
        &mut Quiet,
        &Cancel::new(),
    )
    .unwrap();
    let Outcome::Running(back) = outcome else {
        panic!("{outcome:?}");
    };
    let State::Probe {
        firmware: Firmware::Ours(ours),
        ..
    } = &back.state
    else {
        panic!("{:?}", back.state);
    };
    assert_eq!(ours.build, release.tag);
    assert_eq!(
        back.build().unwrap().compare(&release.tag),
        Relation::UpToDate
    );
}
