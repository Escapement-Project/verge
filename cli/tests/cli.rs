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
        .args(["flash", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("the jumper is still fitted"));
}

#[test]
fn flash_refuses_two_bootloaders() {
    tool("two-isp")
        .args(["flash", "--yes"])
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
    for command in ["flash", "update"] {
        tool("isp")
            .arg(command)
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "The tool cannot tell an MCU-Link from any other LPC55S69 board in ISP mode.",
            ));
    }
}

/// The warning is for a unit that was in ISP mode already; a jumper-free update knows its unit.
#[test]
fn a_jumper_free_update_does_not_carry_the_isp_warning() {
    tool("ours")
        .arg("update")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Pass --yes"))
        .stderr(predicate::str::contains("cannot tell an MCU-Link").not());
}

#[test]
fn a_mini_in_isp_mode_refuses_the_flash_of_the_other_bootloader() {
    tool("isp-mini")
        .args(["flash", "--yes"])
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
        .stdout(predicate::str::contains("SKIPPED: needs the ISP jumper"));
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
        .stderr(predicate::str::contains(
            "every full MCU-Link running stock below V3.200 or in ISP mode, attached from now on?",
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
