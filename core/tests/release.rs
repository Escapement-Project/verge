//! Release lookup: GitHub's real reply format, the asset convention, and the signature and
//! checksum checks.
//!
//! Each refusal test builds a release in which exactly one check can fail: an image changed after
//! signing gets fresh checksums, and an image whose checksum is wrong is signed properly.

use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};
use verge_core::progress::Quiet;
use verge_core::release::{
    GitHub, Http, HttpError, ReleaseError, Repo, parse_checksum, verify_signature,
};
use verge_core::sim::http::{SimAsset, SimHttp, SimKey, SimRelease};
use verge_core::sim::{SimBus, SimFirmware, firmware_image, firmware_srecord};
use verge_core::source::{self, LoadError, Source};
use verge_core::update::{self, Outcome, Timing};
use verge_core::version::Relation;
use verge_core::{Cancel, Firmware, State, detect};

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
    "someone/mculink-verge".parse().unwrap()
}

const IMAGE: &str = "mculink-verge-v0.3.0.s19";

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

    // The same reply with its assets renamed to follow the convention, and one asset's record
    // copied to stand for the signature.
    let mut json: Value = serde_json::from_slice(&recorded).unwrap();
    json["assets"][0]["name"] = "fw.s19.sha256".into();
    json["assets"][1]["name"] = "fw.s19".into();
    let mut signature = json["assets"][0].clone();
    signature["name"] = "fw.s19.minisig".into();
    json["assets"].as_array_mut().unwrap().push(signature);
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
    assert_eq!(release.signature.name, "fw.s19.minisig");
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

/// A signature the minisign command-line tool made, with its default trusted comment
/// (`timestamp:…\tfile:test\tprehashed`), from the documentation of minisign-verify 0.3.0
/// (`src/lib.rs`, lines 20 to 30). It is not made by the code under test.
#[test]
fn a_signature_made_by_the_minisign_tool_verifies() {
    let key = minisign_verify::PublicKey::from_base64(
        "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3",
    )
    .unwrap();
    let signature = "untrusted comment: signature from minisign secret key
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=
trusted comment: timestamp:1633700835\tfile:test\tprehashed
wLMDjy9FLAuxZ3q4NlEvkgtyhrr0gtTu6KC4KBJdITbbOeAi1zBIYo0v4iTgt8jJpIidRJnp94ABQkJAgAooBQ==
";
    verify_signature(&key, "test.minisig", signature.as_bytes(), "test", b"test").unwrap();
    assert!(matches!(
        verify_signature(&key, "test.minisig", signature.as_bytes(), "test", b"tesT"),
        Err(ReleaseError::BadSignature { .. })
    ));
    assert!(matches!(
        verify_signature(&key, "test.minisig", signature.as_bytes(), "other", b"test"),
        Err(ReleaseError::SignatureNamesAnother { .. })
    ));
}

fn github(http: &SimHttp) -> GitHub<SimHttp> {
    GitHub::new(http.clone(), repo(), None).with_key(Some(http.public_key()))
}

fn image_bytes(build: &str) -> Vec<u8> {
    firmware_srecord(&SimFirmware::Ours {
        build: build.to_owned(),
    })
    .into_bytes()
}

fn sums(bytes: &[u8]) -> SimAsset {
    SimAsset::new(
        &format!("{IMAGE}.sha256"),
        format!("{}  {IMAGE}\n", hex::encode(Sha256::digest(bytes))).into_bytes(),
    )
}

fn signed(key: &SimKey, bytes: &[u8], comment: &str) -> SimAsset {
    key.sign(&format!("{IMAGE}.minisig"), bytes, comment)
}

/// Publishes v0.3.0 with these assets, and returns what downloading it gives.
fn download(http: &SimHttp, assets: Vec<SimAsset>) -> Result<(), ReleaseError> {
    http.publish(SimRelease {
        tag: "v0.3.0".into(),
        prerelease: false,
        assets,
    });
    let github = github(http);
    let release = github.latest()?;
    github.download(&release, &mut Quiet).map(|_| ())
}

#[test]
fn the_latest_release_downloads_and_checks_out() {
    let http = SimHttp::new(repo());
    http.publish_ours("v0.2.0");
    http.publish_ours("v0.3.0");
    http.publish(SimRelease {
        tag: "v0.4.0-rc1".into(),
        prerelease: true,
        assets: http.ours_assets("v0.4.0-rc1"),
    });
    let github = github(&http);
    let release = github.latest().unwrap();
    assert_eq!(release.tag, "v0.3.0");
    assert_eq!(release.image.name, IMAGE);
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

#[test]
fn an_unsigned_release_is_refused_when_it_is_looked_up() {
    let http = SimHttp::new(repo());
    let bytes = image_bytes("v0.3.0");
    http.publish(SimRelease {
        tag: "v0.3.0".into(),
        prerelease: false,
        assets: vec![SimAsset::new(IMAGE, bytes.clone()), sums(&bytes)],
    });
    let error = github(&http).latest().unwrap_err();
    assert_eq!(
        error,
        ReleaseError::Unsigned {
            tag: "v0.3.0".into(),
            name: format!("{IMAGE}.minisig")
        }
    );
    assert!(error.to_string().contains("it is not signed"), "{error}");
}

/// The case the signature exists for: the image, its checksum file and GitHub's digest all agree,
/// and only the signature knows the image is not what was signed.
#[test]
fn an_image_changed_after_signing_is_refused_though_its_checksums_agree() {
    let http = SimHttp::new(repo());
    let signed_bytes = image_bytes("v0.3.0");
    let other = image_bytes("v9.9.9");
    let signature = signed(http.key(), &signed_bytes, &format!("file:{IMAGE}"));
    let error = download(
        &http,
        vec![SimAsset::new(IMAGE, other.clone()), sums(&other), signature],
    )
    .unwrap_err();
    let ReleaseError::BadSignature { reason, .. } = &error else {
        panic!("{error:?}");
    };
    assert!(reason.contains("changed after it was signed"), "{reason}");
}

#[test]
fn a_signature_by_another_key_is_refused() {
    let http = SimHttp::new(repo());
    let bytes = image_bytes("v0.3.0");
    let stranger = SimKey::generate();
    let error = download(
        &http,
        vec![
            SimAsset::new(IMAGE, bytes.clone()),
            sums(&bytes),
            signed(&stranger, &bytes, &format!("file:{IMAGE}")),
        ],
    )
    .unwrap_err();
    let ReleaseError::BadSignature { reason, .. } = &error else {
        panic!("{error:?}");
    };
    assert!(
        reason.contains("a key other than the one built into verge"),
        "{reason}"
    );
}

/// A validly signed image of one release, published again as another.
#[test]
fn a_signature_of_another_file_is_refused() {
    let http = SimHttp::new(repo());
    let bytes = image_bytes("v0.3.0");
    let error = download(
        &http,
        vec![
            SimAsset::new(IMAGE, bytes.clone()),
            sums(&bytes),
            signed(http.key(), &bytes, "file:mculink-verge-v0.2.0.s19"),
        ],
    )
    .unwrap_err();
    assert_eq!(
        error,
        ReleaseError::SignatureNamesAnother {
            name: format!("{IMAGE}.minisig"),
            named: "mculink-verge-v0.2.0.s19".into(),
            image: IMAGE.into()
        }
    );

    let http = SimHttp::new(repo());
    let error = download(
        &http,
        vec![
            SimAsset::new(IMAGE, bytes.clone()),
            sums(&bytes),
            signed(http.key(), &bytes, "timestamp:1"),
        ],
    )
    .unwrap_err();
    assert!(
        matches!(error, ReleaseError::SignatureNamesNothing { .. }),
        "{error:?}"
    );
}

#[test]
fn a_signature_file_that_is_not_one_is_refused() {
    let http = SimHttp::new(repo());
    let bytes = image_bytes("v0.3.0");
    let error = download(
        &http,
        vec![
            SimAsset::new(IMAGE, bytes.clone()),
            sums(&bytes),
            SimAsset::new(&format!("{IMAGE}.minisig"), b"not a signature\n".to_vec()),
        ],
    )
    .unwrap_err();
    assert!(
        matches!(error, ReleaseError::BadSignatureFile { .. }),
        "{error:?}"
    );
}

#[test]
fn a_build_without_a_release_key_installs_no_release() {
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let github = GitHub::new(http.clone(), repo(), None).with_key(None);
    let release = github.latest().unwrap();
    assert_eq!(
        github.download(&release, &mut Quiet).unwrap_err(),
        ReleaseError::NoKey
    );
    // Refused before any asset was fetched.
    assert!(http.requests().iter().all(|u| !u.contains("/download/")));
}

#[test]
fn an_image_that_does_not_match_its_checksum_is_refused() {
    let http = SimHttp::new(repo());
    let bytes = image_bytes("v0.3.0");
    let error = download(
        &http,
        vec![
            SimAsset::new(IMAGE, bytes.clone()),
            sums(&image_bytes("v0.2.0")),
            signed(http.key(), &bytes, &format!("file:{IMAGE}")),
        ],
    )
    .unwrap_err();
    assert!(
        matches!(error, ReleaseError::ChecksumMismatch { .. }),
        "{error:?}"
    );
}

#[test]
fn an_image_that_does_not_match_githubs_digest_is_refused() {
    let http = SimHttp::new(repo());
    let bytes = image_bytes("v0.3.0");
    let image = SimAsset {
        digest: SimAsset::new(IMAGE, image_bytes("v0.2.0")).digest,
        ..SimAsset::new(IMAGE, bytes.clone())
    };
    let error = download(
        &http,
        vec![
            image,
            sums(&bytes),
            signed(http.key(), &bytes, &format!("file:{IMAGE}")),
        ],
    )
    .unwrap_err();
    assert!(
        matches!(error, ReleaseError::DigestMismatch { .. }),
        "{error:?}"
    );
}

#[test]
fn releases_that_break_the_convention_are_refused() {
    let http = SimHttp::new(repo());
    let mut no_sums = http.ours_assets("v0.3.0");
    no_sums.retain(|a| !a.name.ends_with(".sha256"));
    http.publish(SimRelease {
        tag: "v0.3.0".into(),
        prerelease: false,
        assets: no_sums,
    });
    assert!(matches!(
        github(&http).latest(),
        Err(ReleaseError::NoChecksum { .. })
    ));

    let http = SimHttp::new(repo());
    let mut two_images = http.ours_assets("v0.3.0");
    two_images.push(SimAsset::new("armwire.s19", b"S0".to_vec()));
    http.publish(SimRelease {
        tag: "v0.3.0".into(),
        prerelease: false,
        assets: two_images,
    });
    assert_eq!(
        github(&http).latest(),
        Err(ReleaseError::ImageAsset {
            tag: "v0.3.0".into(),
            found: 2
        })
    );

    let http = SimHttp::new(repo());
    let bytes = image_bytes("v0.3.0");
    let digest = hex::encode(Sha256::digest(&bytes));
    let error = download(
        &http,
        vec![
            SimAsset::new(IMAGE, bytes.clone()),
            SimAsset::new(
                &format!("{IMAGE}.sha256"),
                format!("{digest}  something-else.s19\n").into_bytes(),
            ),
            signed(http.key(), &bytes, &format!("file:{IMAGE}")),
        ],
    )
    .unwrap_err();
    assert!(
        matches!(error, ReleaseError::ChecksumNamesAnother { .. }),
        "{error:?}"
    );
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
    let with_token =
        GitHub::new(http.clone(), repo(), Some("t".into())).with_key(Some(http.public_key()));
    let release = with_token.latest().unwrap();
    with_token.download(&release, &mut Quiet).unwrap();
    assert!(
        http.requests()
            .iter()
            .any(|u| u.contains("/releases/assets/"))
    );
}

/// Nothing reaches the bus when the release fails its checks: loading the image comes first.
#[test]
fn a_bad_signature_flashes_nothing() {
    let http = SimHttp::new(repo());
    let bytes = image_bytes("v0.3.0");
    http.publish(SimRelease {
        tag: "v0.3.0".into(),
        prerelease: false,
        assets: vec![
            SimAsset::new(IMAGE, bytes.clone()),
            sums(&bytes),
            signed(&SimKey::generate(), &bytes, &format!("file:{IMAGE}")),
        ],
    });
    let bus = SimBus::scenario("ours").unwrap();
    let units = detect(&bus).unwrap();
    let before = bus.log().len();
    let result = source::load(
        &Source::Release(None),
        &github(&http),
        Some(&units[0]),
        false,
        &mut Quiet,
    );
    assert!(
        matches!(
            result,
            Err(LoadError::Release(ReleaseError::BadSignature { .. }))
        ),
        "{result:?}"
    );
    assert_eq!(bus.log().len(), before, "{:?}", &bus.log()[before..]);
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
