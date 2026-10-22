//! The command line against the simulated bench. Every run passes `--simulate`, so no test opens
//! a USB device.

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

fn tool(scenario: &str) -> Command {
    let mut command = Command::cargo_bin("verge").unwrap();
    command
        .args(["--simulate", scenario, "--simulate-pace", "0"])
        .env_remove("VERGE_REPO")
        .env_remove("GITHUB_TOKEN")
        .env_remove("RUST_LOG");
    command
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures")
        .join(name)
}

#[test]
fn list_is_the_default() {
    tool("bench")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "CJ3E7A91C2     ours, build 14d05d4",
        ))
        .stdout(predicate::str::contains("QK7RM2VTX8D41  stock V3.172"))
        .stdout(predicate::str::contains("update: jumper-free (stock)"));
}

#[test]
fn update_installs_the_latest_release() {
    tool("stock")
        .args(["update", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("now runs ours, build v0.3.0"));
}

#[test]
fn update_asks_first_and_will_not_guess_without_a_terminal() {
    tool("ours")
        .arg("update")
        .write_stdin("y\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Pass --yes"));
}

#[test]
fn two_units_need_a_serial() {
    tool("bench")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "2 MCU-Links are attached (CJ3E7A91C2, QK7RM2VTX8D41)",
        ));
}

#[test]
fn old_stock_is_sent_to_the_jumper() {
    tool("stock-old")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Stock V3.108 has no jumper-free update",
        ))
        .stderr(predicate::str::contains("fit the ISP jumper"));
}

#[test]
fn flash_with_the_jumper_still_on() {
    tool("isp-jumper")
        .args(["isp", "flash", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("its ISP pin is still held low"));
}

#[test]
fn flash_refuses_two_bootloaders() {
    tool("two-isp")
        .args(["isp", "flash", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("2 units are in ISP mode"));
}

#[test]
fn an_image_that_never_comes_back_says_what_to_do() {
    // The layout fixture is a valid image the simulator does not recognise, so it boots to
    // nothing, as an unknown image would.
    tool("ours")
        .args(["update", "--yes", "--file"])
        .arg(fixture("layout.s19"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("did not come back"))
        .stderr(predicate::str::contains("verge list"));
}

#[test]
fn release_compares_each_unit() {
    tool("bench")
        .arg("release")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("v0.3.0"))
        .stdout(predicate::str::contains(
            "checked against mculink-verge-v0.3.0.s19.minisig and mculink-verge-v0.3.0.s19.sha256",
        ))
        .stdout(predicate::str::contains(
            "runs 14d05d4, which carries no version",
        ))
        .stdout(predicate::str::contains(
            "runs stock; installing this release replaces it",
        ));
}

#[test]
fn a_missing_release_tag_is_named() {
    tool("ours")
        .args(["update", "--yes", "--release", "v9.9.9"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "has no published release tagged v9.9.9",
        ));
}

/// The directory is optional. Whether this machine has LinkServer decides which of the two
/// answers comes back, and both mean the flag parsed.
#[test]
fn stock_takes_no_directory() {
    tool("ours")
        .args(["update", "--stock"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Pass --yes")
                .or(predicate::str::contains("no MCU-LINK_CMSIS-DAP_V*.s19")),
        );
}

#[test]
fn a_missing_stock_image_says_where_it_looked() {
    tool("ours")
        .args(["update", "--yes", "--stock", "/nonexistent/probe_firmware"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "looked in /nonexistent/probe_firmware",
        ));
}

#[test]
fn inspect_reads_an_elf_without_usb() {
    Command::cargo_bin("verge")
        .unwrap()
        .arg("inspect")
        .arg(fixture("layout.elf"))
        .assert()
        .success()
        .stdout(predicate::str::contains("0x00000000..0x00000010  16 bytes"))
        .stdout(predicate::str::contains(
            "0x00040000..0x00040258  600 bytes",
        ))
        .stdout(predicate::str::contains("fits this part's flash"));
}

#[test]
fn an_unknown_scenario_lists_the_known_ones() {
    tool("nonsense")
        .assert()
        .failure()
        .stderr(predicate::str::contains("there are bench, ours, stock"));
}

#[test]
fn help_has_examples() {
    Command::cargo_bin("verge")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("--simulate").not());
}

#[test]
fn flashing_a_unit_already_in_isp_mode_says_it_may_not_be_an_mculink() {
    let image = sim_image("isp-warning", None);
    for command in [&["isp", "flash"][..], &["update"]] {
        tool("isp")
            .args(command)
            .arg("--file")
            .arg(&image)
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "The tool cannot tell an MCU-Link from any other LPC55S69 board in ISP mode.",
            ));
    }
    std::fs::remove_file(&image).unwrap();
}

/// The warning is for a unit that was in ISP mode already; a jumper-free update knows its unit.
#[test]
fn a_jumper_free_update_does_not_carry_the_isp_warning() {
    let image = sim_image("no-isp-warning", None);
    tool("ours")
        .args(["update", "--file"])
        .arg(&image)
        .assert()
        .failure()
        .stderr(predicate::str::contains("Flash "))
        .stderr(predicate::str::contains("Pass --yes"))
        .stderr(predicate::str::contains("cannot tell an MCU-Link").not());
    std::fs::remove_file(&image).unwrap();
}

/// A release is downloaded only after a question naming the repository, the tag and the size,
/// which `--yes` answers; without either, nothing is downloaded. Then the install's own question.
#[test]
fn a_release_is_downloaded_only_when_agreed() {
    for command in [&["update"][..], &["isp", "flash"]] {
        let scenario = if command[0] == "isp" { "isp" } else { "stock" };
        tool(scenario)
            .args(command)
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "Download release v0.3.0 of Escapement-Project/mculink-verge from GitHub (",
            ))
            .stderr(predicate::str::contains(" KiB)? Pass --yes"))
            .stderr(predicate::str::contains("Flash ").not());
    }
    tool("stock")
        .args(["update", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Download release").not())
        .stdout(predicate::str::contains("now runs ours, build v0.3.0"));
}

#[test]
fn a_mini_in_isp_mode_refuses_the_flash_of_the_other_bootloader() {
    tool("isp-mini")
        .args(["isp", "flash", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unplug the mini first"));
}

fn temp_record(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("verge-cli-{}-{name}.jsonl", std::process::id()))
}

#[test]
fn a_batch_updates_each_unit_as_it_arrives_and_records_it() {
    let record = temp_record("batch");
    tool("arrivals")
        .args(["batch", "--yes", "--count", "3", "--record"])
        .arg(&record)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Batch done: 3 updated, 1 current, 1 skipped, 0 failed, 0 waiting.",
        ))
        .stdout(predicate::str::contains("1-3      CJ"))
        .stdout(predicate::str::contains("1-3      blinking"))
        .stdout(predicate::str::contains("SKIPPED: needs the ISP jumper"))
        // The batch ends with the third update, so the last unit is not left blinking.
        .stdout(predicate::str::contains(
            "1-5      done: unplug it when you like",
        ))
        .stdout(predicate::str::contains("1-5      blinking").not());
    let text = std::fs::read_to_string(&record).unwrap();
    std::fs::remove_file(&record).unwrap();
    let results: Vec<String> = text
        .lines()
        .map(|line| {
            let json: serde_json::Value = serde_json::from_str(line).unwrap();
            format!(
                "{} {}",
                json["port"].as_str().unwrap(),
                json["result"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        results,
        [
            "1-1 updated",
            "1-3 current",
            "1-4 skipped",
            "1-2 updated",
            "1-5 updated"
        ]
    );
}

#[test]
fn a_batch_asks_once_and_says_what_it_takes() {
    tool("arrivals")
        .args(["batch", "--stock-below", "V3.200", "--take", "isp"])
        .assert()
        .failure()
        // The release has to be downloaded first, and the one question says so.
        .stderr(predicate::str::contains(
            "Download release v0.3.0 of Escapement-Project/mculink-verge from GitHub (",
        ))
        .stderr(predicate::str::contains(
            " KiB), and flash it onto every full MCU-Link running stock below V3.200 or in ISP mode, attached from now on?",
        ))
        .stderr(predicate::str::contains(
            "cannot tell an MCU-Link from any other",
        ))
        .stderr(predicate::str::contains("Pass --yes"));
}

#[test]
fn ours_any_takes_ours_as_stock_below_takes_stock() {
    tool("arrivals")
        .args(["batch", "--take", "isp", "--ours-any"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "every full MCU-Link running ours or in ISP mode, attached from now on?",
        ));
}

fn record_results(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| {
            let json: serde_json::Value = serde_json::from_str(line).unwrap();
            format!(
                "{} {}",
                json["port"].as_str().unwrap(),
                json["result"].as_str().unwrap()
            )
        })
        .collect()
}

/// A record file named twice keeps the first batch's lines.
#[test]
fn a_second_batch_adds_to_the_record_file() {
    let record = temp_record("append");
    for _ in 0..2 {
        tool("arrivals")
            .args(["batch", "--yes", "--count", "1", "--record"])
            .arg(&record)
            .assert()
            .success();
    }
    let results = record_results(&record);
    std::fs::remove_file(&record).unwrap();
    assert_eq!(
        results,
        ["1-1 updated", "1-2 waiting", "1-1 updated", "1-2 waiting"]
    );
}

#[test]
fn a_batch_taking_units_in_isp_mode_without_asking_still_warns() {
    let record = temp_record("isp-yes");
    tool("isp")
        .args([
            "batch",
            "--take",
            "isp",
            "--include-attached",
            "--yes",
            "--count",
            "1",
            "--record",
        ])
        .arg(&record)
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "The tool cannot tell an MCU-Link from any other LPC55S69 board in ISP mode.",
        ));
    std::fs::remove_file(&record).unwrap();
}

#[test]
fn ours_is_refused_for_a_pro() {
    tool("pro")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("the unit is an MCU-Link Pro"))
        .stderr(predicate::str::contains("with --stock"));
}

/// A simulated image of ours in a file of its own, declaring `boards` where they are given.
fn sim_image(name: &str, boards: Option<u32>) -> PathBuf {
    use verge_core::sim::{SimFirmware, firmware_srecord, firmware_srecord_declaring};
    let ours = SimFirmware::Ours {
        build: "v0.3.0".into(),
    };
    let text = match boards {
        Some(boards) => firmware_srecord_declaring(&ours, boards),
        None => firmware_srecord(&ours),
    };
    let path = std::env::temp_dir().join(format!("verge-cli-{}-{name}.s19", std::process::id()));
    std::fs::write(&path, text).unwrap();
    path
}

/// A LinkServer-like directory holding the simulator's stock image of `version`.
fn sim_stock_dir(name: &str, version: &str) -> PathBuf {
    use verge_core::sim::{SimFirmware, firmware_srecord};
    let dir = std::env::temp_dir().join(format!("verge-cli-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!(
            "MCU-LINK_CMSIS-DAP_V{}.s19",
            version.replace('.', "_")
        )),
        firmware_srecord(&SimFirmware::Stock {
            version: verge_core::version::StockVersion::parse(version).unwrap(),
        }),
    )
    .unwrap();
    dir
}

/// The second review's proof, at the command line: `verge isp enter` on a Pro, then `verge
/// flash` as it advised, put ours on the Pro, since a unit in ISP mode cannot say which board it
/// is. The check is made before the unit goes into ISP mode.
#[test]
fn bootloader_sends_a_pro_into_isp_mode_only_for_stock() {
    tool("pro")
        .args(["isp", "enter"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("the unit is an MCU-Link Pro"))
        .stderr(predicate::str::contains(
            "run `verge isp enter --stock`, then `verge isp flash --stock`",
        ));
    tool("pro")
        .args(["isp", "enter", "--stock"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "The unit, an MCU-Link Pro, is in ISP mode at 1-1. Flash it with `verge isp flash --stock`.",
        ));
    tool("stock")
        .args(["isp", "enter"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "The unit, a full MCU-Link, is in ISP mode at 1-1, where the tool cannot tell its board. Flash it with `verge isp flash` and an image built for a full MCU-Link",
        ));
}

#[test]
fn list_names_each_units_board() {
    for (scenario, board) in [
        ("bench", "board: a full MCU-Link"),
        ("pro", "board: an MCU-Link Pro"),
        ("isp", "board: cannot be told in ISP mode"),
        ("unnamed-board", "board: not reported"),
        ("mini", "board: an MCU-Link mini"),
    ] {
        tool(scenario)
            .assert()
            .success()
            .stdout(predicate::str::contains(board));
    }
}

/// A running unit that does not say which board it is gets ours only after a question that says
/// so, answered at a terminal. Stock's own image needs no such answer.
#[test]
fn a_unit_that_names_no_board_needs_an_answer_at_a_terminal() {
    tool("unnamed-board")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "This unit's firmware does not say which board it is",
        ))
        .stderr(predicate::str::contains("--yes does not answer that"));
    tool("unnamed-board")
        .arg("update")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "so the tool cannot check that the image is built for it.",
        ))
        .stderr(predicate::str::contains("Only an answer at a terminal"));
    let dir = sim_stock_dir("unnamed-stock", "3.172");
    tool("unnamed-board")
        .args(["update", "--yes", "--stock"])
        .arg(&dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("now runs stock V3.172"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `flash` cannot tell the board in ISP mode, so its question says which boards the image is for.
#[test]
fn flash_says_which_boards_the_image_is_built_for() {
    for (name, boards, built_for) in [
        (
            "flash-plain",
            None,
            "The image is built for the full MCU-Link only.",
        ),
        (
            "flash-pro",
            Some(0b10),
            "The image is built for the MCU-Link Pro only.",
        ),
        (
            "flash-both",
            Some(0b11),
            "The image is built for the full MCU-Link and the MCU-Link Pro.",
        ),
    ] {
        let image = sim_image(name, boards);
        tool("isp")
            .args(["isp", "flash", "--file"])
            .arg(&image)
            .assert()
            .failure()
            .stderr(predicate::str::contains(built_for))
            .stderr(predicate::str::contains("Pass --yes"));
        std::fs::remove_file(&image).unwrap();
    }
}

#[test]
fn inspect_says_which_boards_an_image_declares() {
    let image = sim_image("inspect-pro", Some(0b10));
    Command::cargo_bin("verge")
        .unwrap()
        .arg("inspect")
        .arg(&image)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "declares, at 0x00000130 in format 1, that it runs on the MCU-Link Pro only (boards 0x00000002)",
        ));
    std::fs::remove_file(&image).unwrap();
    Command::cargo_bin("verge")
        .unwrap()
        .arg("inspect")
        .arg(fixture("layout.elf"))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "declares no boards: as an image of ours, it goes on the full MCU-Link only",
        ));
}

/// Stock's image or a file carries no version of ours, so a batch that takes ours older than the
/// image does not arm with one. A newer stock than the units run, and a count, end the batch if
/// it arms anyway, rather than leave it waiting for units forever.
#[test]
fn a_batch_that_cannot_date_ours_does_not_arm() {
    let dir = sim_stock_dir("batch-stock", "3.200");
    let record = temp_record("undated");
    tool("arrivals")
        .args(["batch", "--yes", "--count", "1", "--stock"])
        .arg(&dir)
        .arg("--record")
        .arg(&record)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "the image carries no version of ours",
        ))
        .stderr(predicate::str::contains("Pass --ours-any"));
    assert!(!record.exists());
    // Without ours it arms, and asks.
    tool("arrivals")
        .args(["batch", "--take", "stock", "--stock"])
        .arg(&dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "onto every full MCU-Link running stock, attached from now on?",
        ))
        .stderr(predicate::str::contains("Pass --yes"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A Pro attached and a count end the batch if it arms anyway, with ours on the Pro.
#[test]
fn a_batch_for_the_pro_needs_an_image_built_for_it() {
    let record = temp_record("pro");
    tool("pro")
        .args([
            "batch",
            "--yes",
            "--board",
            "mcu-link-pro",
            "--include-attached",
            "--count",
            "1",
            "--record",
        ])
        .arg(&record)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "the batch takes the MCU-Link Pro, and the image is built for the full MCU-Link only",
        ));
    assert!(!record.exists());
    let image = sim_image("batch-pro", Some(0b10));
    tool("arrivals")
        .args(["batch", "--board", "mcu-link-pro", "--ours-any", "--file"])
        .arg(&image)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "onto every MCU-Link Pro running stock or ours, attached from now on?",
        ))
        .stderr(predicate::str::contains("Pass --yes"));
    std::fs::remove_file(&image).unwrap();
}

/// The third review's proof: a Pro whose stock has no jumper-free update. The latest release
/// declares nothing, so it is for the full MCU-Link only, and `update` refuses it as it would on
/// the jumper-free path, rather than send the user to a plain `verge isp flash`.
#[test]
fn update_on_a_pro_needing_the_jumper_does_not_advise_a_plain_flash() {
    let output = tool("pro-old").args(["update", "--yes"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        !stderr.contains("run `verge isp flash`"),
        "advises a flash of ours on a known Pro: {stderr}"
    );
    assert!(
        stderr.contains(
            "the unit is an MCU-Link Pro, and this image is built for the full MCU-Link only"
        ),
        "{stderr}"
    );
}

/// For an image that is built for the board, the jumper advice names it and the board, since
/// nothing can check the board once the unit is in ISP mode.
#[test]
fn the_jumper_advice_names_the_image_and_the_board() {
    let image = sim_image("jumper-pro", Some(0b10));
    tool("pro-old")
        .args(["update", "--yes", "--file"])
        .arg(&image)
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!(
            "run `verge isp flash --file {}`",
            image.display()
        )))
        .stderr(predicate::str::contains(
            "that image is the one checked for this unit, an MCU-Link Pro",
        ));
    std::fs::remove_file(&image).unwrap();
    let dir = sim_stock_dir("jumper-stock", "3.172");
    tool("pro-old")
        .args(["update", "--yes", "--stock"])
        .arg(&dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!(
            "run `verge isp flash --stock {}`",
            dir.display()
        )));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// After an update fails mid-flash, the advice flashes the same image again, and names the
/// board the unit said it was before it went into ISP mode.
#[test]
fn a_failed_update_says_how_to_flash_the_same_image_again() {
    tool("fails-writes")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "The unit, a full MCU-Link, may be in ISP mode with a partial image",
        ))
        .stderr(predicate::str::contains(
            "`verge isp flash --release v0.3.0`",
        ));
}

/// `verge isp enter` refused on another board says the whole of what to do instead.
#[test]
fn bootloader_on_a_pro_says_how_to_put_ours_on_it() {
    tool("pro")
        .args(["isp", "enter"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "To put an image of ours built for it on it, run `verge update --file IMAGE` instead, which checks that the image declares this board before anything is flashed",
        ));
}

/// A batch that cannot date ours is refused before its image is looked for.
#[test]
fn a_batch_that_cannot_date_ours_is_refused_before_loading() {
    tool("arrivals")
        .args(["batch", "--yes", "--stock", "/nonexistent/probe_firmware"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "the image carries no version of ours",
        ))
        .stderr(predicate::str::contains("no MCU-LINK_CMSIS-DAP_V*.s19").not());
}

/// On a unit that needs the jumper, `update` still loads the release, to check it is built for the
/// unit, and asks first. The question says why, so a no still tells the user about the jumper.
#[test]
fn the_jumper_path_asks_before_it_downloads_and_says_why() {
    tool("stock-old")
        .arg("update")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "This unit cannot be updated without the jumper now. Download release v0.3.0 of Escapement-Project/mculink-verge from GitHub (",
        ))
        .stderr(predicate::str::contains("Pass --yes"));
}

/// The advised `verge isp flash` names the repository the release came from where it is not the
/// default, so that it finds the same release, and the copy `update` kept of it.
#[test]
fn the_advice_names_a_repository_given_on_the_command_line() {
    tool("stock-old")
        .args(["--repo", "someone/fork", "update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "run `verge isp flash --release v0.3.0 --repo someone/fork`",
        ));
    tool("stock-old")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "run `verge isp flash --release v0.3.0`",
        ));
}

/// Ours names only the Pro in its product string, so a full MCU-Link with its SWD jumper asks
/// about its board, which `--yes` does not answer.
#[test]
fn ours_with_its_swd_jumper_asks_about_its_board_and_stock_is_sent_to_take_it_off() {
    tool("swd-jumper")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("ours, SWD jumper fitted"));
    // On Windows the tool offers ours only the ISP jumper, and says so before any board question.
    let refusal = if cfg!(windows) {
        "cannot be updated without the jumper"
    } else {
        "does not say which board it is"
    };
    tool("swd-jumper")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(refusal));
    tool("stock-swd-jumper")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("take the SWD jumper off"));
}

#[test]
fn isp_reset_starts_the_image_the_unit_holds() {
    tool("isp-ours")
        .args(["isp", "reset"])
        .assert()
        .success()
        .stdout(predicate::str::contains("at 1-1 runs ours, build v0.3.0"));
}

#[test]
fn isp_reset_with_the_jumper_says_what_holds_the_pin() {
    tool("isp-jumper")
        .args(["isp", "reset"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "its ISP pin is still held low, by the ISP jumper or by a probe on its debug connector",
        ));
}

/// A unit whose flash holds nothing comes back in ISP mode too, and the advice says so.
#[test]
fn isp_reset_of_a_blank_unit_says_it_may_hold_no_image() {
    tool("isp")
        .args(["isp", "reset"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "or its flash holds no image the ROM starts",
        ));
}

/// Two units in ISP mode: without a port the tool refuses, with one it resets that unit only.
#[test]
fn isp_reset_takes_a_port_among_two_bootloaders() {
    tool("two-isp-ours")
        .args(["isp", "reset"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("2 units are in ISP mode"));
    tool("two-isp-ours")
        .args(["isp", "reset", "--port", "1-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("at 1-2 runs ours"));
}

#[test]
fn isp_flash_takes_a_port_among_two_bootloaders() {
    let image = sim_image("isp-flash-port", None);
    tool("two-isp")
        .args(["isp", "flash", "--yes", "--port", "1-2", "--file"])
        .arg(&image)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "at 1-2 now runs ours, build v0.3.0",
        ));
    std::fs::remove_file(&image).unwrap();
}

#[test]
fn isp_info_names_the_bootloader() {
    tool("isp")
        .args(["isp", "info"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1-1: bootloader K"));
}

/// The old top-level commands are gone: they live under `verge isp` now.
#[test]
fn flash_and_bootloader_are_isp_subcommands() {
    for command in ["flash", "bootloader"] {
        tool("isp").arg(command).assert().failure();
    }
}

/// Beside another unit in ISP mode, `isp enter` still sends a unit into its bootloader where the
/// bus tells bootloaders apart, and says which port to flash; an update still waits.
#[test]
fn isp_enter_works_beside_another_bootloader() {
    tool("ours-beside-isp")
        .args(["update", "--yes", "--serial", "CJ3E7A91C2"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Another unit is in ISP mode"));
    tool("ours-beside-isp")
        .args(["isp", "enter", "--serial", "CJ3E7A91C2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("`verge isp flash --port 1-1`"));
}

/// A mini beside the bootloader a port names refuses the flash before any question.
#[test]
fn isp_flash_by_port_beside_a_mini_refuses_first() {
    tool("isp-mini")
        .args(["isp", "flash", "--port", "1-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unplug the mini first"))
        .stderr(predicate::str::contains("Download").not());
}

#[test]
fn cjtag_limits_the_formats_to_oscan1() {
    tool("ours-pro")
        .args(["cjtag", "--oscan1-only"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "offers two-pin OScan1, until it restarts.",
        ));
}

#[test]
fn cjtag_shows_the_formats() {
    tool("ours-pro")
        .arg("cjtag")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "offers two-pin OScan1, OScan2, OScan5.",
        ));
}

#[test]
fn cjtag_limits_the_formats_to_a_set() {
    tool("ours-pro")
        .args(["cjtag", "--formats", "oscan5,9"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "offers two-pin OScan1, OScan5, until it restarts.",
        ));
}

#[test]
fn cjtag_refuses_an_unknown_format() {
    tool("ours-pro")
        .args(["cjtag", "--formats", "oscan9"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("oscan9 is not a scan format"));
}

#[test]
fn cjtag_refuses_stock() {
    tool("pro")
        .args(["cjtag", "--oscan1-only"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is stock V3.172, not running ours",
        ));
}

#[test]
fn dio_reads_the_pins() {
    tool("ours-pro")
        .arg("dio")
        .assert()
        .success()
        .stdout(predicate::str::contains("DIO0  input   reads low"))
        .stdout(predicate::str::contains("DIO1  input   reads low"))
        .stdout(predicate::str::contains("DIO2  input   reads low"))
        .stdout(predicate::str::contains("DIO3  input   reads low"))
        .stdout(predicate::str::contains(
            "Direction groups: DIO0, DIO1, DIO2; DIO3.",
        ));
}

#[test]
fn dio_drives_a_group() {
    tool("ours-pro")
        .args(["dio", "--high", "dio0,2", "--low", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DIO0  output  driven high"))
        .stdout(predicate::str::contains("DIO1  output  driven low"))
        .stdout(predicate::str::contains("DIO2  output  driven high"))
        .stdout(predicate::str::contains("DIO3  input   reads low"));
}

#[test]
fn dio_refuses_to_split_a_group() {
    tool("ours-pro")
        .args(["dio", "--high", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "DIO0, DIO1, DIO2 change direction together",
        ));
}

#[test]
fn dio_refuses_a_pin_named_twice() {
    tool("ours-pro")
        .args(["dio", "--high", "all", "--input", "3"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "DIO3 named in both --high and --input",
        ));
}

#[test]
fn dio_refuses_an_image_that_predates_it() {
    tool("ours")
        .arg("dio")
        .assert()
        .failure()
        .stderr(predicate::str::contains("which predates the DIO pins"));
}

#[test]
fn dio_refuses_stock() {
    tool("pro")
        .arg("dio")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is stock V3.172, not running ours",
        ));
}

#[test]
fn dio_reads_what_holds_an_input() {
    tool("ours-pro-held")
        .arg("dio")
        .assert()
        .success()
        .stdout(predicate::str::contains("DIO0  input   reads low"))
        .stdout(predicate::str::contains("DIO3  input   reads high"));
}

#[test]
fn dio_turns_a_group_to_input() {
    tool("ours-pro")
        .args(["dio", "--low", "3", "--input", "0,1,2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DIO2  input   reads low"))
        .stdout(predicate::str::contains("DIO3  output  driven low"));
}

#[test]
fn dio_refuses_a_board_without_the_pins() {
    tool("ours-plain")
        .arg("dio")
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not an MCU-Link Pro"));
}
