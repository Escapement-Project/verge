//! The questions as a user at a terminal meets them: the command line against the simulated bench
//! under a pseudo-terminal, answered key by key. The other tests run without a terminal, where
//! the first question refuses and says to pass `--yes`, so they cannot see a second question.

#![cfg(unix)]

use rexpect::session::{PtySession, spawn_command};

fn tool(scenario: &str, args: &[&str]) -> PtySession {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_verge"));
    command
        .args(["--simulate", scenario, "--simulate-pace", "0"])
        .args(args)
        .env_remove("VERGE_REPO")
        .env_remove("GITHUB_TOKEN")
        .env_remove("RUST_LOG");
    spawn_command(command, Some(20_000)).unwrap()
}

const DOWNLOAD: &str = "Download release v0.3.0 of Escapement-Project/mculink-verge from GitHub (";

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
    p.exp_string("nothing was flashed").unwrap();
    p.exp_eof().unwrap();
}

/// A no to the download downloads nothing, and asks nothing more.
#[test]
fn a_no_to_the_download_ends_there() {
    let mut p = tool("stock", &["update"]);
    p.exp_string(DOWNLOAD).unwrap();
    answer(&mut p, "n");
    let rest = p.exp_eof().unwrap();
    assert!(rest.contains("nothing was flashed"), "{rest}");
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
    p.exp_string(" KiB), and flash it onto every full MCU-Link")
        .unwrap();
    answer(&mut p, "y");
    p.exp_string("Armed: release v0.3.0").unwrap();
    let rest = p.exp_eof().unwrap();
    assert!(rest.contains("Batch done: 1 updated"), "{rest}");
    assert!(!rest.contains("without being asked again"), "{rest}");
    std::fs::remove_file(&record).unwrap();
}

/// Several units that fit, at a terminal: the tool lists them and acts on the one picked.
#[test]
fn a_terminal_picks_among_the_units_that_fit() {
    let mut p = tool("bench", &["update"]);
    p.exp_string("Which MCU-Link?").unwrap();
    // dialoguer hides the cursor before it draws the list, so wait for the last item instead.
    p.exp_string("QK7RM2VTX8D41  stock V3.172").unwrap();
    // Down to the second unit, the stock one, and take it.
    p.send("\u{1b}[B").unwrap();
    p.send("\r").unwrap();
    p.flush().unwrap();
    p.exp_string(DOWNLOAD).unwrap();
    answer(&mut p, "y");
    p.exp_string("Flash release v0.3.0 (mculink-verge-v0.3.0.s19) onto QK7RM2VTX8D41")
        .unwrap();
    answer(&mut p, "n");
    p.exp_string("nothing was flashed").unwrap();
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
