//! The questions as a user at a terminal meets them: the command line against the simulated bench
//! under a pseudo-terminal, answered key by key. The other tests run without a terminal, where
//! the first question refuses and says to pass `--yes`, so they cannot see a second question.

#![cfg(unix)]

use rexpect::session::{PtySession, spawn_command};

/// Runs `command` under a pseudo-terminal. indicatif draws no progress when TERM is unset or
/// `dumb`, as in a CI container, so the terminal names one.
fn spawn(mut command: std::process::Command) -> PtySession {
    command.env("TERM", "xterm-256color");
    spawn_command(command, Some(20_000)).unwrap()
}

fn tool(scenario: &str, args: &[&str]) -> PtySession {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_verge"));
    command
        .args(["--simulate", scenario, "--simulate-pace", "0"])
        .args(args)
        .env_remove("VERGE_REPO")
        .env_remove("GITHUB_TOKEN")
        .env_remove("RUST_LOG");
    spawn(command)
}

/// The download question's start: the rest may wrap onto the next row.
const DOWNLOAD: &str = "Download release v0.3.0 of Escapement-Project/mculink-verge from GitHub";

/// Answers the question on screen, once it is whole: dialoguer hides the cursor when it starts
/// reading keys. A key is sent only on `flush`.
fn answer(p: &mut PtySession, key: &str) {
    p.exp_string("[y/N] \u{1b}[?25l").unwrap();
    p.send(key).unwrap();
    p.flush().unwrap();
}

/// `update` asks to download the release, and only then asks to flash it, naming it.
#[test]
fn update_asks_to_download_then_to_flash() {
    let mut p = tool("stock", &["update"]);
    p.exp_string(DOWNLOAD).unwrap();
    answer(&mut p, "y");
    p.exp_string("Flash release v0.3.0 (mculink-verge-v0.3.0.s19) onto QK7RM2VTX8D41")
        .unwrap();
    answer(&mut p, "n");
    p.exp_string("Nothing was flashed.").unwrap();
    p.exp_eof().unwrap();
}

/// A no to the download downloads nothing, and asks nothing more.
#[test]
fn a_no_to_the_download_ends_there() {
    let mut p = tool("stock", &["update"]);
    p.exp_string(DOWNLOAD).unwrap();
    answer(&mut p, "n");
    let rest = p.exp_eof().unwrap();
    assert!(rest.contains("Nothing was flashed."), "{rest}");
    assert!(!rest.contains("Flash release"), "{rest}");
}

/// A batch whose release has to be downloaded asks once, before the download, and arms on that
/// answer.
#[test]
fn a_batch_that_downloads_asks_once() {
    let record =
        std::env::temp_dir().join(format!("verge-terminal-{}-batch.jsonl", std::process::id()));
    let mut p = tool(
        "arrivals",
        &[
            "batch",
            "--count",
            "1",
            "--record",
            record.to_str().unwrap(),
        ],
    );
    p.exp_string(DOWNLOAD).unwrap();
    answer(&mut p, "y");
    let rest = p.exp_eof().unwrap();
    assert!(rest.contains("Armed: release v0.3.0"), "{rest}");
    assert!(rest.contains("Batch done: 1 updated"), "{rest}");
    assert!(
        !rest.contains(DOWNLOAD),
        "the question was drawn again: {rest}"
    );
    std::fs::remove_file(&record).unwrap();
}

/// Several units that fit, at a terminal: the tool lists them and acts on the one picked.
#[test]
fn a_terminal_picks_among_the_units_that_fit() {
    let mut p = tool("bench", &["update"]);
    p.exp_string("Which MCU-Link?").unwrap();
    // dialoguer hides the cursor before it draws the list, so wait for the last item instead.
    p.exp_string("QK7RM2VTX8D41  standalone MCU-Link  NXP's firmware V3.172")
        .unwrap();
    // Down to the second unit, the stock one, and take it.
    p.send("\u{1b}[B").unwrap();
    p.send("\r").unwrap();
    p.flush().unwrap();
    p.exp_string(DOWNLOAD).unwrap();
    answer(&mut p, "y");
    p.exp_string("Flash release v0.3.0 (mculink-verge-v0.3.0.s19) onto QK7RM2VTX8D41")
        .unwrap();
    answer(&mut p, "n");
    p.exp_string("Nothing was flashed.").unwrap();
    p.exp_eof().unwrap();
}

/// A Pro command lists only the Pros, so a lone Pro beside a plain board is taken unasked.
#[test]
fn a_terminal_is_not_asked_when_one_unit_fits() {
    let mut p = tool("ours-pro-and-plain", &["power"]);
    let rest = p.exp_eof().unwrap();
    assert!(
        rest.contains("CJ3E7A91C2's target supply is off."),
        "{rest}"
    );
    assert!(!rest.contains("Which MCU-Link?"), "{rest}");
}

/// The tool under a shell that prints its exit status once it ends: rexpect does not give it.
fn tool_status(scenario: &str, args: &[&str]) -> PtySession {
    let mut command = std::process::Command::new("sh");
    command
        .arg("-c")
        .arg(r#""$0" "$@"; echo "[exit $?]""#)
        .arg(env!("CARGO_BIN_EXE_verge"))
        .args(["--simulate", scenario, "--simulate-pace", "0"])
        .args(args)
        .env_remove("VERGE_REPO")
        .env_remove("GITHUB_TOKEN")
        .env_remove("RUST_LOG");
    spawn(command)
}

/// Ctrl-C at a question stops the run: the cursor dialoguer hid comes back, a line says nothing
/// was changed, there is no Rust error and no warning about an operation, and the status is 130.
/// The same at a yes or no question and at the unit picker.
#[test]
fn ctrl_c_at_a_question_stops_the_run() {
    for (scenario, shown) in [
        ("stock", "[y/N] \u{1b}[?25l"),
        ("bench", "NXP's firmware V3.172"),
    ] {
        let mut p = tool_status(scenario, &["update"]);
        p.exp_string(shown).unwrap();
        p.send_control('c').unwrap();
        let rest = p.exp_string("[exit 130]").unwrap();
        assert!(
            rest.contains("Stopped at the question. Nothing was changed."),
            "{scenario}: {rest}"
        );
        assert!(
            rest.contains("\u{1b}[?25h"),
            "{scenario}: the cursor stays hidden: {rest}"
        );
        assert!(!rest.contains("IO error"), "{scenario}: {rest}");
        assert!(!rest.contains("next safe point"), "{scenario}: {rest}");
    }
}

/// A question longer than the terminal is drawn once: after the answer, dialoguer redraws only its
/// last row, so the start of the question does not come again.
#[test]
fn a_long_question_is_drawn_once() {
    let mut p = tool("stock", &["update"]);
    p.exp_string(DOWNLOAD).unwrap();
    answer(&mut p, "y");
    p.exp_string("Flash release v0.3.0 (mculink-verge-v0.3.0.s19) onto")
        .unwrap();
    answer(&mut p, "n");
    let rest = p.exp_eof().unwrap();
    assert!(rest.contains("Nothing was flashed."), "{rest}");
    assert!(
        !rest.contains("Flash release"),
        "the question was drawn again: {rest}"
    );
}

/// A batch stopped with a unit failed ends with its tally all the same, then the failure, counted
/// in words that agree with the number. The waiting shows on the terminal.
#[test]
fn a_stopped_batch_tallies_its_failures() {
    let record =
        std::env::temp_dir().join(format!("verge-terminal-{}-tally.jsonl", std::process::id()));
    let mut p = tool_status(
        "fails-writes",
        &[
            "batch",
            "--yes",
            "--include-attached",
            "--record",
            record.to_str().unwrap(),
        ],
    );
    p.exp_string("FAILED").unwrap();
    p.exp_string("waiting for probes: 1 failed").unwrap();
    p.send_control('c').unwrap();
    let rest = p.exp_string("[exit 1]").unwrap();
    assert!(
        rest.contains("Batch stopped: 0 updated, 0 current, 0 skipped, 1 failed, 0 waiting."),
        "{rest}"
    );
    assert!(rest.contains("1 probe failed; the record file"), "{rest}");
    std::fs::remove_file(&record).unwrap();
}

/// A no to the jumper path's download still says how to use the jumper, and nothing more.
#[test]
fn a_no_to_the_jumper_download_still_says_how() {
    let mut p = tool_status("stock-old", &["update"]);
    p.exp_string("[y/N] \u{1b}[?25l").unwrap();
    p.send("n").unwrap();
    p.flush().unwrap();
    let rest = p.exp_string("[exit 1]").unwrap();
    assert!(rest.contains("Nothing was downloaded."), "{rest}");
    assert!(
        rest.contains("To use the jumper: unplug the probe, fit the ISP jumper, plug it back in, and run `verge isp flash` with an image built for a standalone MCU-Link."),
        "{rest}"
    );
    assert!(!rest.contains("ERROR"), "{rest}");
}

/// Ctrl-C is how a recording ends, so it is said plainly, not as a warning, and the recording
/// finishes its file.
#[test]
fn ctrl_c_ends_a_recording_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("run.csv");
    let mut command = std::process::Command::new("sh");
    command
        .arg("-c")
        .arg(r#""$0" "$@"; echo "[exit $?]""#)
        .arg(env!("CARGO_BIN_EXE_verge"))
        // Paced, so the recording waits for Ctrl-C at the rate a probe sends.
        .args(["--simulate", "ours-pro-energy", "--simulate-pace", "1"])
        .args(["energy", "record", "--out"])
        .arg(&out)
        .env_remove("RUST_LOG");
    let mut p = spawn(command);
    p.exp_string("Ctrl-C ends it.").unwrap();
    p.send_control('c').unwrap();
    let rest = p.exp_string("[exit 0]").unwrap();
    assert!(
        rest.contains(
            "Ending the recording; press Ctrl-C again to quit without finishing the file."
        ),
        "{rest}"
    );
    assert!(!rest.contains("WARN"), "{rest}");
    assert!(rest.contains("samples to"), "{rest}");
}

/// NO_COLOR leaves verge's own lines uncoloured on a terminal.
#[test]
fn no_color_is_honoured() {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_verge"));
    command
        .args(["--simulate", "ours", "--simulate-pace", "0", "isp", "info"])
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG");
    let mut p = spawn(command);
    let rest = p.exp_eof().unwrap();
    assert!(rest.contains("ERROR no probe is in ISP mode"), "{rest}");
    assert!(!rest.contains('\u{1b}'), "{rest:?}");
}
