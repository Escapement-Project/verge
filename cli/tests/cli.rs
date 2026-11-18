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
        .join("tests/fixtures")
        .join(name)
}

#[cfg(unix)]
#[test]
fn closed_stdout_ends_commands_quietly() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::{Command as Process, Stdio};

    for (scenario, args) in [
        ("bench", vec!["--help"]),
        ("bench", vec!["--version"]),
        ("bench", vec!["list", "-v"]),
        ("ours-pro", vec!["dio"]),
        ("ours-pro", vec!["power"]),
        ("ours-pro", vec!["energy", "latest"]),
        ("ours-pro", vec!["energy", "total"]),
        ("ours-pro", vec!["energy", "calibration"]),
        ("ours-pro", vec!["energy", "watch", "--for", "1s"]),
        ("ours", vec!["release"]),
    ] {
        let (out, reader) = UnixStream::pair().unwrap();
        drop(reader);
        let mut process = Process::new(assert_cmd::cargo::cargo_bin!("verge"));
        process
            .args(["--simulate", scenario, "--simulate-pace", "0"])
            .args(&args)
            .env_remove("VERGE_REPO")
            .env_remove("GITHUB_TOKEN")
            .env_remove("RUST_LOG")
            .stdout(Stdio::from(OwnedFd::from(out)));
        let output = process.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{scenario} {args:?}: {stderr}");
        assert!(stderr.is_empty(), "{scenario} {args:?}: {stderr}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn other_stdout_errors_are_reported() {
    let mut process = std::process::Command::new(assert_cmd::cargo::cargo_bin!("verge"));
    process
        .args(["--simulate", "bench", "list", "-v"])
        .env_remove("VERGE_REPO")
        .env_remove("GITHUB_TOKEN")
        .env_remove("RUST_LOG")
        .stdout(
            std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        );
    let output = process.output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stdout failure returned success");
    assert!(stderr.contains("No space left on device"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn closing_stdout_during_a_batch_finishes_and_records_the_current_unit() {
    use std::io::{BufRead, BufReader};
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::{Command as Process, Stdio};

    let directory = tempfile::tempdir().unwrap();
    let record = directory.path().join("batch.jsonl");
    let (out, reader) = UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let child = Process::new(assert_cmd::cargo::cargo_bin!("verge"))
        .args(["--simulate", "arrivals", "--simulate-pace", "0.1"])
        .args(["batch", "--yes", "--count", "3", "--record"])
        .arg(&record)
        .env_remove("VERGE_REPO")
        .env_remove("GITHUB_TOKEN")
        .env_remove("RUST_LOG")
        .stdout(Stdio::from(OwnedFd::from(out)))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(reader);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert!(first.starts_with("Armed:"), "{first}");
    drop(reader);
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        !stderr.contains("ERROR") && !stderr.contains("panicked"),
        "{stderr}"
    );
    assert_eq!(record_results(&record), ["1-1 updated", "1-2 waiting"]);
}

#[test]
fn list_is_the_default() {
    tool("bench")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "CJ3E7A91C2     Verge, build 14d05d4",
        ))
        .stdout(predicate::str::contains(
            "QK7RM2VTX8D41  NXP's firmware V3.172",
        ))
        .stdout(predicate::str::is_match(r"\n +update +jumper-free \(NXP's firmware\)").unwrap());
}

/// Every label sits under the serials and every value under the states, whatever the line.
#[test]
fn list_lines_up_labels_and_values() {
    let output = tool("bench").args(["list", "-v"]).assert().success();
    let text = String::from_utf8_lossy(&output.get_output().stdout).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    let head = lines[0];
    let serial = head.find("CJ3E7A91C2").unwrap();
    let state = head.find("Verge, build").unwrap();
    for line in &lines {
        let (before, after) = line.split_at(state);
        assert!(
            before.ends_with(' ') && !after.starts_with(' '),
            "value off the state column: {line:?}"
        );
        if !line.starts_with(' ') {
            continue;
        }
        assert!(
            line[..serial].trim().is_empty() && !line[serial..].starts_with(' '),
            "label off the serial column: {line:?}"
        );
    }
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("capabilities"))
    );
}

#[test]
fn update_installs_the_latest_release() {
    tool("stock")
        .args(["update", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("now runs Verge, build v0.3.0"));
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
            "2 attached units could be meant: CJ3E7A91C2 and QK7RM2VTX8D41. Pick one with --serial",
        ));
}

#[test]
fn old_stock_is_sent_to_the_jumper() {
    tool("stock-old")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "NXP's firmware V3.108 has no jumper-free update",
        ))
        .stderr(predicate::str::contains("fit the ISP jumper"));
}

/// A flash the unit came back from in ISP mode went in, but the unit does not run it yet: a
/// script sees status 3, not success.
#[test]
fn flash_with_the_jumper_still_on() {
    tool("isp-jumper")
        .args(["isp", "flash", "--yes"])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("its ISP pin is still held low"));
    tool("isp-jumper")
        .args(["update", "--yes"])
        .assert()
        .code(3)
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
        .stderr(predicate::str::contains(
            "did not come back within 15 s of the reset",
        ))
        .stderr(predicate::str::contains("verge list"));
}

/// An image built for another USB ID boots on that ID, where the tool does not look: it says
/// where the unit went rather than that it did not come back.
#[test]
fn an_image_on_another_usb_id_says_where_the_unit_went() {
    use escapement_verge_probe::sim::{SimFirmware, firmware_srecord};
    let image = std::env::temp_dir().join(format!("verge-cli-{}-other-id.s19", std::process::id()));
    std::fs::write(
        &image,
        firmware_srecord(&SimFirmware::OtherId {
            vid: 0x1209,
            pid: 0x0001,
        }),
    )
    .unwrap();
    tool("ours")
        .args(["update", "--yes", "--file"])
        .arg(&image)
        .assert()
        .failure()
        .stderr(predicate::str::contains("came back as 1209:0001 ("))
        .stderr(predicate::str::contains("did not come back").not());
    std::fs::remove_file(&image).unwrap();
}

#[test]
fn release_compares_each_unit() {
    tool("bench")
        .arg("release")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("v0.3.0"))
        .stdout(predicate::str::contains(
            "signed in mculink-verge-v0.3.0.s19.minisig, checksum in mculink-verge-v0.3.0.s19.sha256",
        ))
        .stdout(predicate::str::contains(
            "runs 14d05d4, which carries no version",
        ))
        .stdout(predicate::str::contains(
            "runs NXP's firmware; installing this release replaces it",
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
        .stdout(predicate::str::contains("now runs Verge, build v0.3.0"));
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
fn a_zero_batch_limit_is_rejected_before_loading() {
    tool("arrivals")
        .args(["batch", "--yes", "--count", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value '0'"))
        .stdout(predicate::str::is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn a_failed_record_write_stops_the_batch() {
    tool("arrivals")
        .args(["batch", "--yes", "--count", "3", "--record", "/dev/full"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("writing the batch record file"))
        .stdout(predicate::str::contains("Batch done:").not());
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
        // The serial is padded as `list` pads it, so the columns after it line up.
        .stdout(predicate::str::contains("1-3      CJ3E7AC4F5      Verge"))
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
            " KiB), and flash it onto every standalone MCU-Link running NXP's firmware older than V3.200 or in ISP mode, attached from now on?",
        ))
        .stderr(predicate::str::contains(
            "cannot tell an MCU-Link from any other",
        ))
        .stderr(predicate::str::contains("Pass --yes"));
}

#[test]
fn ours_any_takes_ours_as_stock_below_takes_stock() {
    tool("arrivals")
        .args(["batch", "--take", "isp", "--verge-any"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "every standalone MCU-Link running Verge or in ISP mode, attached from now on?",
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
        .stderr(predicate::str::contains(
            "with --stock, or a Verge build that declares it with --file IMAGE",
        ));
}

/// A simulated image of ours in a file of its own, declaring `boards` where they are given.
fn sim_image(name: &str, boards: Option<u32>) -> PathBuf {
    use escapement_verge_probe::sim::{SimFirmware, firmware_srecord, firmware_srecord_declaring};
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
    use escapement_verge_probe::sim::{SimFirmware, firmware_srecord};
    let dir = std::env::temp_dir().join(format!("verge-cli-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!(
            "MCU-LINK_CMSIS-DAP_V{}.s19",
            version.replace('.', "_")
        )),
        firmware_srecord(&SimFirmware::Stock {
            version: escapement_verge_probe::version::StockVersion::parse(version).unwrap(),
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
            "The unit, a standalone MCU-Link, is in ISP mode at 1-1, where the tool cannot tell its board. Flash it with `verge isp flash` and an image built for a standalone MCU-Link",
        ));
}

#[test]
fn list_names_each_units_board() {
    for (scenario, board) in [
        ("bench", "standalone MCU-Link"),
        ("pro", "MCU-Link Pro"),
        ("isp", "cannot be told in ISP mode"),
        ("unnamed-board", "not reported"),
        ("mini", "MCU-Link mini"),
    ] {
        tool(scenario)
            .assert()
            .success()
            .stdout(predicate::str::is_match(format!(r"\n +board +{board}\n")).unwrap());
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
        .stdout(predicate::str::contains("now runs NXP's firmware V3.172"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `flash` cannot tell the board in ISP mode, so its question says which boards the image is for.
#[test]
fn flash_says_which_boards_the_image_is_built_for() {
    for (name, boards, built_for) in [
        (
            "flash-plain",
            None,
            "The image declares no boards, so verge takes it as a Verge image for the standalone MCU-Link only.",
        ),
        (
            "flash-pro",
            Some(0b10),
            "The image is built for the MCU-Link Pro only.",
        ),
        (
            "flash-both",
            Some(0b11),
            "The image is built for the standalone MCU-Link and the MCU-Link Pro.",
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
            "declares no boards, so verge takes it as a Verge image for the standalone MCU-Link only",
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
            "the image carries no Verge version",
        ))
        .stderr(predicate::str::contains("Pass --verge-any"));
    assert!(!record.exists());
    // Without ours it arms, and asks.
    tool("arrivals")
        .args(["batch", "--take", "stock", "--stock"])
        .arg(&dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "onto every standalone MCU-Link running NXP's firmware, attached from now on?",
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
            "the batch takes the MCU-Link Pro, and the image declares no boards, so verge takes it as a Verge image for the standalone MCU-Link only",
        ));
    assert!(!record.exists());
    let image = sim_image("batch-pro", Some(0b10));
    tool("arrivals")
        .args(["batch", "--board", "mcu-link-pro", "--verge-any", "--file"])
        .arg(&image)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "onto every MCU-Link Pro running NXP's firmware or Verge, attached from now on?",
        ))
        .stderr(predicate::str::contains("Pass --yes"));
    std::fs::remove_file(&image).unwrap();
}

/// The third review's proof: a Pro whose stock has no jumper-free update. The latest release
/// declares nothing, so it is for the standalone MCU-Link only, and `update` refuses it as it would on
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
            "the unit is an MCU-Link Pro, and the image declares no boards, so verge takes it as a Verge image for the standalone MCU-Link only"
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
            "The unit, a standalone MCU-Link, may be in ISP mode with a partial image",
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
            "To put a Verge image built for it on it, run `verge update --file IMAGE` instead, which checks that the image declares this board before anything is flashed",
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
            "the image carries no Verge version",
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
            "This unit cannot be updated without the jumper now. verge downloads the release first, to check that it is built for this unit. Download release v0.3.0 of Escapement-Project/mculink-verge from GitHub (",
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

/// Ours names only the Pro in its product string, so a standalone MCU-Link with its SWD jumper asks
/// about its board, which `--yes` does not answer.
#[test]
fn ours_with_its_swd_jumper_asks_about_its_board_and_stock_is_sent_to_take_it_off() {
    tool("swd-jumper")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("Verge, SWD jumper fitted"));
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
        .stdout(predicate::str::contains("at 1-1 runs Verge, build v0.3.0"));
}

#[test]
fn isp_reset_with_the_jumper_says_what_holds_the_pin() {
    tool("isp-jumper")
        .args(["isp", "reset"])
        .assert()
        .code(3)
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
        .code(3)
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
        .stdout(predicate::str::contains("at 1-2 runs Verge"));
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
            "at 1-2 now runs Verge, build v0.3.0",
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
fn a_cjtag_format_needs_cjtag() {
    tool("ours-pro")
        .args([
            "read",
            "--chip",
            "CC1312R",
            "--cjtag-format",
            "oscan2",
            "b32",
            "0",
            "1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--cjtag-format needs --protocol cjtag",
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
        .stderr(predicate::str::contains("which predates its DIO pins"));
}

#[test]
fn dio_refuses_stock() {
    tool("pro")
        .arg("dio")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "runs NXP's firmware V3.172, not Verge",
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
fn dio_reads_a_standalone_units_j8() {
    tool("ours-plain")
        .arg("dio")
        .assert()
        .success()
        .stdout(predicate::str::contains("DIO pins, on J8 pins 1 to 4:"))
        .stdout(predicate::str::contains("DIO3  input   reads low"))
        .stdout(predicate::str::contains(
            "Direction groups: DIO0; DIO1; DIO2; DIO3.",
        ));
}

#[test]
fn dio_turns_one_j8_pin_on_its_own() {
    tool("ours-plain")
        .args(["dio", "--high", "0"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DIO0  output  driven high"))
        .stdout(predicate::str::contains("DIO1  input   reads low"));
}

/// A standalone unit's DIO pins go to a tickwave recording of those pins alone, as long as the
/// recording ran.
#[test]
fn dio_record_writes_a_standalone_units_pins_alone() {
    use escapement_verge_probe::tickwave::format::decode::Index;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("pins.tickwave");
    tool("ours-plain")
        .args([
            "dio", "record", "--for", "300ms", "--pins", "0,dio2", "--out",
        ])
        .arg(&out)
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "CJ3E7A91C2: DIO0 0 edges, DIO2 0 edges over 0.3",
        ));
    let file = std::fs::read(&out).unwrap();
    let header = Index::of_file(&file).unwrap().footer.header;
    let names: Vec<&str> = header.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["DIO0", "DIO2"]);
    assert!(header.derived.is_empty());
    // The simulator times DIO streams to the microsecond.
    assert!(
        (300_000..400_000).contains(&header.length),
        "{}",
        header.length
    );
}

/// Each pin's edges are counted as the stream reports them: here DIO1 toggling every 50 ms.
#[test]
fn dio_record_counts_each_pins_edges() {
    let dir = tempfile::tempdir().unwrap();
    tool("ours-plain-busy")
        .args(["dio", "record", "--for", "300ms", "--pins", "0,1", "--out"])
        .arg(dir.path().join("pins.tickwave"))
        .assert()
        .success()
        .stderr(predicate::str::is_match("DIO0 0 edges, DIO1 [67] edges over 0\\.3").unwrap());
}

/// Beside a Pro that records its pins with its energy capture, `dio record` picks the unit that
/// streams them alone, without being told which.
#[test]
fn dio_record_picks_the_unit_that_streams_its_pins_alone() {
    let dir = tempfile::tempdir().unwrap();
    tool("ours-pro-and-plain")
        .args(["dio", "record", "--for", "100ms", "--out"])
        .arg(dir.path().join("pins.tickwave"))
        .assert()
        .success()
        .stderr(predicate::str::contains("DIO0 0 edges"));
}

/// Where the probe lost edges, the recording says how often.
#[test]
fn dio_record_counts_the_probes_losses() {
    let dir = tempfile::tempdir().unwrap();
    tool("ours-plain-edge-loss")
        .args(["dio", "record", "--for", "300ms", "--pins", "1", "--out"])
        .arg(dir.path().join("pins.tickwave"))
        .assert()
        .success()
        // Six toggles in 300 ms, the first lost: not an edge.
        .stderr(predicate::str::contains("DIO1 5 edges over 0.3"))
        .stderr(predicate::str::contains(
            "The probe lost edges 1 time: the recording marks those spans unknown.",
        ));
}

/// `verge dio` lists only the pins a bench tool's command marker leaves, says which it holds, and
/// refuses to drive a held one, naming it and the way to give it back.
#[test]
fn dio_names_the_pins_the_bench_marker_holds() {
    tool("ours-plain-marked")
        .arg("dio")
        .assert()
        .success()
        .stdout(predicate::str::contains("DIO1  input"))
        .stdout(predicate::str::contains("DIO2").count(1))
        .stdout(predicate::str::contains(
            "A bench tool's command marker holds DIO2, DIO3 until the unit restarts.",
        ));
    tool("ours-plain-marked")
        .args(["dio", "--high", "0,2"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "'s DIO2 to its command marker. Run `verge restart` to give it back",
        ));
}

/// Pins a bench tool's command marker holds are left out, with a warning, and a recording with
/// none left fails with the way to give them back and leaves no file.
#[test]
fn dio_record_leaves_out_pins_the_bench_marker_holds() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("pins.tickwave");
    tool("ours-plain-marked")
        .args(["dio", "record", "--for", "100ms", "--pins", "2,3", "--out"])
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "reports none of the pins asked for: a bench tool has given them to its command marker. Run `verge restart`",
        ));
    assert!(!out.exists());
    tool("ours-plain-marked")
        .args(["dio", "record", "--for", "100ms", "--out"])
        .arg(&out)
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "reports DIO0, DIO1 of the pins asked for: a bench tool holds the rest",
        ))
        .stderr(predicate::str::contains("DIO0 0 edges, DIO1 0 edges over"));
}

/// A Pro without its energy circuit streams its DIO pins as a standalone unit does, and the
/// recording names the board.
#[test]
fn dio_record_takes_a_pro_without_its_energy_circuit() {
    use escapement_verge_probe::tickwave::format::decode::Index;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("pins.tickwave");
    tool("ours-pro-unfitted")
        .args(["dio", "record", "--for", "100ms", "--out"])
        .arg(&out)
        .assert()
        .success();
    let header = Index::of_file(&std::fs::read(&out).unwrap())
        .unwrap()
        .footer
        .header;
    assert_eq!(
        header.sources[0].instrument.as_deref(),
        Some("MCU-Link Pro")
    );
}

#[test]
fn dio_record_sends_a_pro_to_its_energy_recording() {
    let dir = tempfile::tempdir().unwrap();
    tool("ours-pro")
        .args(["dio", "record", "--for", "100ms", "--out"])
        .arg(dir.path().join("pins.tickwave"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("use `verge energy record --dio`"));
}

#[test]
fn restart_restarts_a_unit_running_verge() {
    tool("ours-plain")
        .arg("restart")
        .assert()
        .success()
        .stdout(predicate::str::contains("CJ3E7A91C2 restarted."));
}

#[test]
fn restart_refuses_stock() {
    tool("stock")
        .arg("restart")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "runs NXP's firmware V3.172, not Verge, so it takes no restart request",
        ));
}

#[test]
fn power_reads_the_supply_off_after_a_start() {
    tool("ours-pro")
        .arg("power")
        .assert()
        .success()
        .stdout(predicate::str::contains("target supply is off."));
}

#[test]
fn power_switches_the_supply() {
    tool("ours-pro")
        .args(["power", "on"])
        .assert()
        .success()
        .stdout(predicate::str::contains("target supply is on."));
}

#[test]
fn power_refuses_a_board_without_a_supply() {
    tool("ours-plain")
        .args(["power", "on"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is not an MCU-Link Pro, so it has no target supply",
        ));
}

#[test]
fn energy_start_reports_the_rate() {
    tool("ours-pro")
        .args(["energy", "start", "--rate", "2000"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "captures 2000 pairs a second, switching current ranges automatically.",
        ));
}

#[test]
fn energy_start_reports_the_high_range_held() {
    tool("ours-pro")
        .args(["energy", "start", "--range", "high"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "captures 25000 pairs a second, holding the high range.",
        ));
}

#[test]
fn energy_start_refuses_a_rate_the_firmware_cannot_take() {
    tool("ours-pro")
        .args(["energy", "start", "--rate", "100001"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--rate takes 1 to 100,000 pairs a second",
        ));
}

#[test]
fn energy_latest_reads_a_running_capture() {
    tool("ours-pro-energy")
        .args(["energy", "latest"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "3.30 V, 5.00 mA (high range, 350 mA setting), pair 100",
        ));
}

#[test]
fn energy_latest_says_when_nothing_captures() {
    tool("ours-pro")
        .args(["energy", "latest"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "is not capturing: start a capture first.",
        ));
}

#[test]
fn energy_total_reads_the_running_total() {
    tool("ours-pro-energy")
        .args(["energy", "total"])
        .assert()
        .success()
        .stdout(predicate::str::contains("17.1 mJ over 100"))
        .stdout(predicate::str::contains("pairs since the start."));
}

#[test]
fn energy_stop_reports_the_total() {
    tool("ours-pro-energy")
        .args(["energy", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("stopped: 17.1 mJ over 100"));
}

#[test]
fn energy_calibration_lists_both_ranges() {
    tool("ours-pro")
        .args(["energy", "calibration"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Low range: code 410 at 0 A, code 31200 at 960 µA, code 62100 at 1.93 mA.",
        ))
        .stdout(predicate::str::contains("High range: code 390 at 0 A"));
}

#[test]
fn energy_watch_prints_a_line_a_read_and_leaves_a_running_capture_running() {
    tool("ours-pro-energy")
        .args(["energy", "watch", "--for", "1s", "--every", "250ms"])
        .assert()
        .success()
        .stdout(
            predicate::str::is_match(
                r"   0\.00 s  3\.30 V  5\.00 mA  \(high range, 350 mA setting\)  pair 100\d\d  interval -",
            )
            .unwrap(),
        )
        .stdout(predicate::str::contains("   1.00 s  3.30 V"))
        // Each interval holds two or three of the target's 10 ms wakes, so its mean lands either
        // side of the 1.71 mW the whole capture averages.
        .stdout(predicate::str::is_match(r"interval 1\.[5-8]\d mW").unwrap())
        .stdout(predicate::str::contains("The capture keeps running."));
}

#[test]
fn energy_watch_stops_the_capture_it_started() {
    tool("ours-pro")
        .args(["energy", "watch", "--rate", "1000", "--for", "200ms"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no complete pair yet"))
        .stdout(predicate::str::contains("stopped: 0 J over"));
}

#[test]
fn energy_watch_refuses_a_zero_interval() {
    tool("ours-pro")
        .args(["energy", "watch", "--every", "0s"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--every has to be longer than nothing",
        ));
}

#[test]
fn energy_watch_leaves_at_its_limit_between_reads() {
    tool("ours-pro-energy")
        .args(["energy", "watch", "--for", "300ms", "--every", "10s"])
        .assert()
        .success()
        .stdout(predicate::str::contains("   0.30 s  3.30 V"))
        .stdout(predicate::str::contains("  10.00 s").not());
}

#[test]
fn energy_watch_starts_a_capture_over_a_fault() {
    tool("ours-pro-energy-failed")
        .args(["energy", "watch", "--for", "200ms"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "the capture failed after 17.1 mJ over 10000 pairs, when the ADC overran, its DMA failed or the probe fell behind reading it. A new capture replaces it.",
        ))
        .stdout(predicate::str::contains("   0.20 s  3.30 V"))
        .stdout(predicate::str::contains("stopped: "))
        .stdout(predicate::str::contains("pairs since the start."));
}

#[test]
fn energy_latest_reports_a_fault() {
    tool("ours-pro-energy-failed")
        .args(["energy", "latest"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "capture failed: the ADC overran, its DMA failed or the probe fell behind reading it. A new start clears the fault.",
        ));
}

#[test]
fn energy_stop_after_a_fault_reports_what_was_kept() {
    tool("ours-pro-energy-failed")
        .args(["energy", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "CJ3E7A91C2's capture had stopped already: the capture failed after 17.1 mJ over 10000 pairs, when the ADC overran, its DMA failed or the probe fell behind reading it.",
        ));
}

#[test]
fn energy_says_when_another_program_holds_the_probe() {
    tool("busy")
        .args(["energy", "total"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "could not ask CJ3E7A91C2 what it runs: another program has it open",
        ));
}

/// Every supply, energy and DIO command reaches a Pro over Verge Aux while a debugger holds
/// CMSIS-DAP, and says what it says over CMSIS-DAP.
#[test]
fn power_energy_and_dio_work_while_a_debugger_holds_the_probe() {
    for args in [
        &["dio"][..],
        &["dio", "--high", "dio0,2", "--low", "1"],
        &["power"],
        &["power", "on"],
        &["energy", "start", "--rate", "2000"],
        &["energy", "latest"],
        &["energy", "total"],
        &["energy", "stop"],
        &["energy", "calibration"],
        &["energy", "watch", "--for", "200ms"],
    ] {
        let free = tool("ours-pro").args(args).assert().success();
        let held = tool("ours-pro-debugger").args(args).assert().success();
        assert_eq!(
            String::from_utf8_lossy(&held.get_output().stdout),
            String::from_utf8_lossy(&free.get_output().stdout),
            "{args:?}"
        );
    }
}

#[test]
fn list_names_what_a_held_pro_runs() {
    tool("ours-pro-debugger")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Verge, build 26ea505, its CMSIS-DAP held by another program",
        ))
        .stdout(predicate::str::is_match(r"\n +board +MCU-Link Pro\n").unwrap());
}

/// Another program on Verge Aux leaves CMSIS-DAP, which the tool falls back to.
#[test]
fn energy_falls_back_to_cmsis_dap_when_verge_aux_is_taken() {
    tool("ours-pro-aux-taken")
        .args(["energy", "start", "--rate", "2000"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "captures 2000 pairs a second, switching current ranges automatically.",
        ));
}

#[test]
fn energy_refuses_an_image_that_predates_it() {
    tool("ours")
        .args(["energy", "total"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "which predates its energy measurement: update it first",
        ));
}

#[test]
fn energy_refuses_a_board_without_it() {
    tool("ours-plain")
        .args(["energy", "start"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is not an MCU-Link Pro, so it has no energy measurement",
        ));
}

#[test]
fn energy_refuses_stock() {
    tool("pro")
        .args(["energy", "latest"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "runs NXP's firmware V3.172, not Verge",
        ));
}

#[test]
fn serial_goes_before_the_command_or_after_it() {
    for args in [
        ["--serial", "CJ3E7A91C2", "power"],
        ["power", "--serial", "CJ3E7A91C2"],
    ] {
        tool("ours-pro-and-plain")
            .args(args)
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "CJ3E7A91C2's target supply is off.",
            ));
    }
}

#[test]
fn a_pro_command_takes_the_only_pro_beside_a_plain_board() {
    tool("ours-pro-and-plain")
        .args(["power"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "CJ3E7A91C2's target supply is off.",
        ));
}

#[test]
fn several_units_that_fit_need_a_serial_off_a_terminal() {
    tool("ours-pro-and-plain")
        .arg("update")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "2 attached units could be meant: CJ3E7A91C2 and CJ3E7AB3E4. Pick one with --serial",
        ));
}

/// An update that cannot pick between a unit and one in ISP mode, which has no serial, names the
/// command that flashes the one in ISP mode.
#[test]
fn an_update_beside_a_unit_in_isp_mode_names_how_to_flash_it() {
    tool("ours-beside-isp")
        .args(["update", "--file", "verge.elf", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "2 attached units could be meant: CJ3E7A91C2 and the unit in ISP mode at 1-2. Pick one with --serial. The unit in ISP mode, at 1-2, has no serial: flash it with `verge isp flash --file verge.elf --port 1-2`",
        ));
}

#[test]
fn list_shows_only_the_unit_serial_names() {
    tool("bench")
        .args(["--serial", "QK7RM2VTX8D41", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "QK7RM2VTX8D41  NXP's firmware V3.172",
        ))
        .stdout(predicate::str::contains("CJ3E7A91C2").not());
    tool("bench")
        .args(["--serial", "NOSUCHUNIT", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no attached MCU-Link has serial NOSUCHUNIT",
        ));
}

#[test]
fn serial_is_refused_where_it_picks_nothing() {
    for (args, why) in [
        (
            &["batch", "--yes"][..],
            "`batch` takes every unit of a kind",
        ),
        (&["inspect", "image.s19"][..], "`inspect` reads a file"),
        (&["isp", "reset"][..], "--port picks it instead"),
    ] {
        tool("bench")
            .arg("--serial")
            .arg("CJ3E7A91C2")
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(why));
    }
}

/// A RUST_LOG that names only another crate still shows why verge failed.
#[test]
fn the_final_error_survives_a_narrow_rust_log() {
    Command::cargo_bin("verge")
        .unwrap()
        .env("RUST_LOG", "probe_rs::vendor::ti=debug")
        .args(["inspect", "no-such-image.elf"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no-such-image.elf"));
}

/// Each pair's four numbers. The column after them, the range, has to be `low` or `high`.
fn csv_lines(path: &std::path::Path) -> Vec<Vec<u64>> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .skip(1)
        .map(|line| {
            let fields: Vec<&str> = line.split(',').collect();
            assert!(matches!(fields[4], "low" | "high" | "under"), "{line}");
            fields[..4].iter().map(|v| v.parse().unwrap()).collect()
        })
        .collect()
}

/// A recording of a running capture holds every pair it streamed, in order, and leaves the
/// capture running.
#[test]
fn energy_record_writes_every_pair_and_leaves_a_running_capture() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("run.csv");
    let output = tool("ours-pro-energy")
        .args(["energy", "record", "--csv", "--for", "1s", "--out"])
        .arg(&out)
        .assert()
        .success()
        .stderr(predicate::str::contains("the capture keeps running."))
        .get_output()
        .stderr
        .clone();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.starts_with("sequence,time_ns,millivolts,nanoamps,range\n"));
    let lines = csv_lines(&out);
    assert!(lines.len() >= 1000, "{} pairs", lines.len());
    assert!(
        lines
            .windows(2)
            .all(|w| w[1][0] == w[0][0] + 1 && w[1][1] == w[0][1] + 1_000_000)
    );
    assert!(
        String::from_utf8_lossy(&output).contains(&format!("{} pairs to", lines.len())),
        "{}",
        String::from_utf8_lossy(&output)
    );
}

/// The simulated target holds DIO3 high for its 10 ms awake every 100 ms. A CSV recording of it
/// puts each edge on a line of its own, in time order among the pairs, and each pair gives the
/// level it saw.
#[test]
fn energy_record_lines_up_dio_edges_with_the_pairs() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("dio.csv");
    let output = tool("ours-pro-energy")
        .args([
            "energy", "record", "--csv", "--dio", "3", "--for", "1s", "--out",
        ])
        .arg(&out)
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    let said = String::from_utf8_lossy(&output);
    let text = std::fs::read_to_string(&out).unwrap();
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("sequence,time_ns,millivolts,nanoamps,range,dio3,dio_gap")
    );
    let rows: Vec<Vec<&str>> = lines.map(|line| line.split(',').collect()).collect();
    assert!(rows.iter().all(|row| row[6] == "0"), "a gap with no loss");
    let times: Vec<u64> = rows.iter().map(|row| row[1].parse().unwrap()).collect();
    assert!(times.is_sorted(), "lines out of time order");
    let edges: Vec<(u64, &str)> = rows
        .iter()
        .filter(|row| row[0].is_empty())
        .map(|row| (row[1].parse().unwrap(), row[5]))
        .collect();
    assert!(edges.len() >= 18, "{} edges", edges.len());
    for (time, level) in &edges {
        let into = time % 100_000_000;
        assert!(
            (into == 0 && *level == "1") || (into == 10_000_000 && *level == "0"),
            "an edge at {time} to {level}"
        );
    }
    // A pair reads the level the edges before it left.
    let mut level = None;
    for row in &rows {
        if row[0].is_empty() {
            level = Some(row[5]);
        } else if let Some(level) = level {
            assert_eq!(row[5], level, "{row:?}");
        }
    }
    assert!(
        said.contains(&format!("{} edges on DIO3.\n", edges.len())),
        "{said}"
    );
}

/// A recording that starts its own capture stops it.
#[test]
fn energy_record_stops_the_capture_it_started() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("own.sr");
    tool("ours-pro")
        .args([
            "energy", "record", "--rate", "2000", "--for", "500ms", "--out",
        ])
        .arg(&out)
        .assert()
        .success()
        .stderr(predicate::str::contains("the capture stopped."));
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let mut metadata = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("metadata").unwrap(), &mut metadata).unwrap();
    assert!(metadata.contains("samplerate=2 kHz\n"), "{metadata}");
    assert!(metadata.contains("analog4=mA\n"), "{metadata}");
}

/// `--range high` holds the high range for the capture the recording starts: no pair is read on
/// the low range, and the simulated target's sleep, under the floor, is marked `under`.
/// Automatic, the default, reads the sleep on the low range.
#[test]
fn energy_record_holds_the_high_range_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    for (range, low, under) in [("auto", true, false), ("high", false, true)] {
        let out = dir.path().join(format!("{range}.csv"));
        let output = tool("ours-pro")
            .args([
                "energy", "record", "--csv", "--for", "500ms", "--range", range, "--out",
            ])
            .arg(&out)
            .assert()
            .success()
            .get_output()
            .stderr
            .clone();
        let output = String::from_utf8_lossy(&output);
        assert_eq!(
            output.contains("The capture held the high range."),
            !low,
            "{range}: {output}"
        );
        assert_eq!(
            output.contains("pairs were read under range"),
            under,
            "{range}: {output}"
        );
        let csv = std::fs::read_to_string(&out).unwrap();
        assert_eq!(
            csv.contains(",low"),
            low,
            "{range}: {}",
            &csv[..200.min(csv.len())]
        );
        assert_eq!(csv.contains(",under"), under, "{range}");
    }
}

#[test]
fn energy_record_works_while_a_debugger_holds_the_probe() {
    let dir = tempfile::tempdir().unwrap();
    tool("ours-pro-debugger")
        .args(["energy", "record", "--for", "200ms", "--out"])
        .arg(dir.path().join("held.sr"))
        .assert()
        .success()
        .stderr(predicate::str::contains("pairs to"));
}

#[test]
fn energy_record_needs_verge_aux() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("none.sr");
    tool("ours-pro-aux-taken")
        .args(["energy", "record", "--out"])
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "another program holds CJ3E7A91C2's Verge Aux, which a recording needs",
        ));
    assert!(!out.exists(), "a file was left behind");
    tool("ours-plain")
        .args(["energy", "record", "--out"])
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not an MCU-Link Pro"));
}

/// The file's extension picks a recording's format; a flag the name contradicts is refused before
/// any unit is touched, and a name that says nothing gets a tickwave recording, named as such.
#[test]
fn a_recordings_name_picks_its_format() {
    use escapement_verge_probe::tickwave::format::decode::Index;
    let dir = tempfile::tempdir().unwrap();
    let csv = dir.path().join("run.csv");
    tool("ours-pro-energy")
        .args(["energy", "record", "--for", "200ms", "--out"])
        .arg(&csv)
        .assert()
        .success();
    assert!(
        std::fs::read_to_string(&csv)
            .unwrap()
            .starts_with("sequence,time_ns")
    );
    let named = dir.path().join("run.tickwave");
    tool("ours-pro-energy")
        .args(["energy", "record", "--for", "200ms", "--out"])
        .arg(&named)
        .assert()
        .success();
    Index::of_file(&std::fs::read(&named).unwrap()).unwrap();
    let unnamed = dir.path().join("run.dat");
    tool("ours-pro-energy")
        .args(["energy", "record", "--for", "200ms", "--out"])
        .arg(&unnamed)
        .assert()
        .success()
        .stderr(predicate::str::contains("run.dat (a tickwave recording)"));
    Index::of_file(&std::fs::read(&unnamed).unwrap()).unwrap();
    let wrong = dir.path().join("wrong.sr");
    tool("ours-pro-energy")
        .args(["energy", "record", "--tickwave", "--for", "200ms", "--out"])
        .arg(&wrong)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--tickwave writes a tickwave recording, and",
        ))
        .stderr(predicate::str::contains("name the file .tickwave"));
    assert!(!wrong.exists());
    let pins = dir.path().join("pins.csv");
    tool("ours-plain")
        .args(["dio", "record", "--for", "100ms", "--out"])
        .arg(&pins)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "`dio record` writes a tickwave recording: name it .tickwave",
        ));
    assert!(!pins.exists());
}

/// `--out -` is stdout, for CSV only: stdout then holds the CSV and nothing else, and the summary
/// goes to stderr. Nothing named `-` is made.
#[test]
fn csv_to_stdout_holds_only_the_csv() {
    let dir = tempfile::tempdir().unwrap();
    let output = tool("ours-pro-energy")
        .current_dir(dir.path())
        .args(["energy", "record", "--csv", "--for", "200ms", "--out", "-"])
        .assert()
        .success()
        .stderr(predicate::str::contains("pairs to stdout"))
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("sequence,time_ns,millivolts,nanoamps,range")
    );
    assert!(
        lines.all(|line| line.split(',').count() == 5),
        "a line that is not CSV: {text}"
    );
    tool("ours-pro-energy")
        .current_dir(dir.path())
        .args(["energy", "record", "--for", "200ms", "--out", "-"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--out - writes to stdout, which takes CSV only: add --csv",
        ));
    assert!(!dir.path().join("-").exists());
}

/// Where no attached unit can do what was asked, the refusal says what each one is, and never
/// asks for a serial nobody could give.
#[test]
fn none_of_several_units_fits_says_why() {
    tool("bench")
        .args(["energy", "latest"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no attached unit measures energy: CJ3E7A91C2 is a standalone MCU-Link running Verge, build 14d05d4, and QK7RM2VTX8D41 is a standalone MCU-Link running NXP's firmware V3.172. Energy measurement needs an MCU-Link Pro running Verge.",
        ))
        .stderr(predicate::str::contains("serial").not());
    tool("two-isp")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("none reports a serial"))
        .stderr(predicate::str::contains("?").not());
    tool("two-isp")
        .args(["isp", "enter"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "2 units are in ISP mode already, at 1-1 and 1-2, and report no serial. `verge isp flash --port 1-1` flashes one",
        ));
}

/// A unit a debugger holds is offered no update until the debugger closes, not the ISP jumper.
#[test]
fn a_unit_a_debugger_holds_waits_for_it() {
    tool("ours-pro-debugger")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"\n +update +Close the debugger first\. ").unwrap())
        .stdout(predicate::str::contains("ISP jumper").not());
    tool("ours-pro-debugger")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "CJ3E7A91C2 cannot be updated as it is. Close the debugger first. Another program holds its CMSIS-DAP interface, and the tool does not update a unit in use.",
        ));
}

/// Beside a unit in ISP mode, a running unit is not sent to the jumper, which would make two:
/// it waits for the other one, and `update` says how to flash that.
#[test]
fn beside_a_unit_in_isp_mode_the_other_waits() {
    tool("ours-beside-isp")
        .assert()
        .success()
        .stdout(predicate::str::is_match(
            r"\n +update +Another unit is in ISP mode, at 1-2\. Flash that one or unplug it, then update this one",
        )
        .unwrap())
        .stdout(predicate::str::contains("ISP jumper").not());
    tool("ours-beside-isp")
        .args(["--serial", "CJ3E7A91C2", "update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Another unit is in ISP mode, at 1-2.",
        ))
        .stderr(predicate::str::contains(
            "flash it with `verge isp flash --release --port 1-2`",
        ))
        .stderr(predicate::str::contains("fit the ISP jumper").not());
}

/// The ISP commands point at `--port` where it picks one of several bootloaders, at `isp enter`
/// where a running unit can go to ISP mode without the jumper, and refuse a mini in its own words.
#[test]
fn the_isp_commands_say_what_works() {
    tool("two-isp")
        .args(["isp", "info"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "2 units are in ISP mode, at 1-1 and 1-2. Pick one with --port, as --port 1-1, or unplug all but one",
        ));
    tool("ours")
        .args(["isp", "info"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no unit is in ISP mode. `verge isp enter` puts CJ3E7A91C2 there without the jumper",
        ));
    tool("isp")
        .args(["isp", "enter"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "the unit at 1-1 is in ISP mode already. `verge isp flash` flashes it; `verge isp reset` starts the image it holds",
        ));
    tool("mini")
        .args(["isp", "info"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "verge does not work with the mini",
        ))
        .stderr(predicate::str::contains("firmware").not());
    tool("isp-mini")
        .args(["isp", "reset"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "the tool works with a bootloader only while no other is attached: unplug the mini first",
        ))
        .stderr(predicate::str::contains("flashes").not());
}

/// NXP's own file, given with --file, goes on any board as --stock's would, and the mini's file
/// is refused by its name.
#[test]
fn linkservers_files_are_known_by_their_names() {
    let dir = sim_stock_dir("by-name", "3.172");
    let file = dir.join("MCU-LINK_CMSIS-DAP_V3_172.s19");
    tool("ours-pro")
        .args(["update", "--yes", "--file"])
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("now runs NXP's firmware V3.172"));
    tool("isp")
        .arg("inspect")
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "is NXP's firmware V3.172, by its name: it goes on any board NXP's firmware supports",
        ));
    let mini = dir.join("MCU-MINI_CMSIS-DAP_V3_172.s19");
    std::fs::copy(&file, &mini).unwrap();
    tool("isp")
        .args(["isp", "flash", "--yes", "--file"])
        .arg(&mini)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is NXP's firmware for the MCU-Link mini, and verge does not work with the mini",
        ));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `--take` takes a list as the docs write it, with commas.
#[test]
fn take_takes_commas() {
    tool("arrivals")
        .args(["batch", "--take", "stock,isp"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "every standalone MCU-Link running NXP's firmware or in ISP mode",
        ));
}

/// A tag given without its "v" finds the release.
#[test]
fn a_release_tag_without_its_v_is_found() {
    tool("stock")
        .args(["update", "--yes", "--release", "0.3.0"])
        .assert()
        .success()
        .stdout(predicate::str::contains("now runs Verge, build v0.3.0"));
}

/// A firmware too old for the capture commands is named as such, and a start that was never
/// sent is not called uncertain.
#[test]
fn an_old_build_is_named_without_uncertainty() {
    for args in [
        &["energy", "start"][..],
        &["energy", "watch", "--for", "100ms"][..],
        &["energy", "stop"][..],
    ] {
        tool("ours-pro-old-aux")
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "CJ3E7A91C2 runs build 26ea505, which predates the capture commands this tool uses: update it first",
            ))
            .stderr(predicate::str::contains("uncertain").not());
    }
}

/// What ends or cuts off a running capture says so.
#[test]
fn ending_a_running_capture_says_so() {
    tool("ours-pro-energy")
        .args(["power", "off"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "A capture is running; the target now has no supply from the Pro.",
        ));
    tool("ours-pro-energy")
        .args(["energy", "start", "--rate", "1k"])
        .assert()
        .success()
        .stdout(predicate::str::contains("captures 1000 pairs a second"))
        .stdout(predicate::str::contains(
            "This replaced the capture that was running, and its total: 17.1 mJ over 10003 pairs",
        ));
    tool("ours-pro-energy")
        .arg("restart")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "restarted. Its energy capture ended.",
        ));
    tool("ours-pro")
        .args(["energy", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CJ3E7A91C2 was not capturing"));
}

/// Argument errors say what the option takes, in the tool's words.
#[test]
fn argument_errors_say_what_is_taken() {
    tool("arrivals")
        .args(["batch", "--count", "0"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--count has to be at least 1"));
    tool("ours-pro")
        .args(["energy", "start", "--rate", "25k"])
        .assert()
        .success()
        .stdout(predicate::str::contains("captures 25000 pairs a second"));
    tool("ours-plain")
        .args(["dio", "--high", "0,"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "an empty pin name: name pins as 0 to 3, dio0 to dio3, or all",
        ));
    tool("ours-pro")
        .args(["energy", "watch", "--window", "0s"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--window has to be longer than nothing",
        ));
    tool("ours-pro").args(["power", "On"]).assert().success();
}

/// `read` goes to real probes, so a simulated bench refuses it rather than touch one.
#[test]
fn read_refuses_a_simulated_bench() {
    tool("bench")
        .args(["read", "--chip", "CC1312R", "b32", "0", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--simulate has no target to read"));
}

/// A bad VERGE_REPO stops only what uses releases, and names itself.
#[test]
fn a_bad_verge_repo_stops_only_releases() {
    tool("ours").env("VERGE_REPO", "bad").assert().success();
    tool("ours")
        .env("VERGE_REPO", "bad")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "VERGE_REPO is \"bad\"; it has to be OWNER/NAME",
        ));
}

/// `release --serial` checks the serial before it prints anything.
#[test]
fn release_checks_the_serial_first() {
    tool("bench")
        .args(["--serial", "NOPE", "release"])
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "no attached MCU-Link has serial NOPE",
        ));
}

/// The line after an update names the unit as the user picked it, and the serial it reports now.
#[test]
fn done_names_the_serial_that_changed() {
    tool("bench")
        .args(["--serial", "QK7RM2VTX8D41", "update", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(
            r"Done: QK7RM2VTX8D41 at 1-2 now runs Verge, build v0\.3\.0, and reports serial CJ\w+\.",
        )
        .unwrap());
}

/// `list -v` shows what Verge Aux said about a unit a debugger holds.
#[test]
fn list_verbose_shows_a_held_units_build() {
    tool("ours-pro-debugger")
        .args(["list", "-v"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"\n +capabilities +.*energy capture").unwrap());
}

/// `-v` lines carry no span names, and the waits are said in seconds as `restart` says them.
#[test]
fn verbose_lines_carry_no_spans() {
    tool("ours")
        .args(["-v", "update", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains(" INFO bootloader K2.1.0 answers"))
        .stderr(predicate::str::contains("step{").not());
}

/// A unit on the SWD jumper is refused for what is true of it. One running Verge that another
/// program holds is restarted all the same: the request goes to its USB core, as the docs say.
#[test]
fn the_jumper_is_named_and_a_held_unit_restarts() {
    tool("swd-jumper")
        .arg("dio")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "CJ3E7A91C2 has its SWD jumper fitted, which takes away the interfaces the tool reaches its DIO pins through",
        ))
        .stderr(predicate::str::contains("not running Verge").not());
    tool("busy")
        .arg("restart")
        .assert()
        .success()
        .stdout(predicate::str::contains("CJ3E7A91C2 restarted."));
}

/// A unit the system does not let this user open is listed with the way to allow it on Linux,
/// and offered no update until it may.
#[cfg(target_os = "linux")]
#[test]
fn a_unit_this_user_may_not_open_points_at_the_udev_rules() {
    tool("denied")
        .assert()
        .success()
        .stdout(predicate::str::contains("#usb-permissions-on-linux"))
        .stdout(predicate::str::contains("ISP jumper").not());
}

/// A missing file is named once.
#[test]
fn a_missing_file_is_named_once() {
    tool("isp")
        .args(["isp", "flash", "--yes", "--file", "/nonexistent.elf"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "reading /nonexistent.elf: No such file or directory",
        ))
        .stderr(predicate::str::contains("ERROR /nonexistent.elf").not())
        .stderr(predicate::str::contains("os error").not());
}

/// A mini that cannot be updated is refused in a sentence, not a table row.
#[test]
fn a_mini_is_refused_in_a_sentence() {
    tool("mini")
        .args(["update", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "the MCU-Link mini in ISP mode at 1-1 cannot be updated as it is. verge does not work with the MCU-Link mini.",
        ));
}

/// Counts read as words: one run is "1 run", one page "1 page".
#[test]
fn inspect_counts_in_words() {
    let path = std::env::temp_dir().join(format!("verge-cli-{}-one.s19", std::process::id()));
    std::fs::write(&path, "S1070100AABBCCDDE9\n").unwrap();
    Command::cargo_bin("verge")
        .unwrap()
        .arg("inspect")
        .arg(&path)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "4 bytes into flash: 1 page of 512 bytes to erase and write, in 1 run",
        ));
    std::fs::remove_file(&path).unwrap();
}

/// A reset with nothing in ISP mode says so, and sends the user nowhere: entering ISP mode to leave
/// it again, or fitting the jumper, would be pointless.
#[test]
fn isp_reset_with_nothing_in_isp_mode_says_so() {
    tool("ours")
        .args(["isp", "reset"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no unit is in ISP mode, so there is nothing to reset",
        ))
        .stderr(predicate::str::contains("isp enter").not())
        .stderr(predicate::str::contains("jumper").not());
}

/// A reader that closes the pipe early ends a recording to stdout quietly, as `head` expects.
#[test]
fn csv_to_a_pipe_that_closes_early_ends_quietly() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_verge"))
        .args(["--simulate", "ours-pro-energy", "--simulate-pace", "1"])
        .args(["energy", "record", "--csv", "--for", "5s", "--out", "-"])
        .env_remove("RUST_LOG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    assert!(first.starts_with("sequence,time_ns"), "{first}");
    // The reader goes away: the next write finds the pipe closed.
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{:?}: {stderr}", output.status);
    assert!(!stderr.contains("Broken pipe"), "{stderr}");
    assert!(!stderr.contains("ERROR"), "{stderr}");
}
