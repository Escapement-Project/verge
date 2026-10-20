//! The window, headless, against the simulated bench or a hand-driven backend. No test opens a
//! USB device.
//!
//! A test that waits on the worker is bounded in frames, the window's own unit: the simulated
//! bench runs an update in a few milliseconds of real time, so 400 frames is generous.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{Role, ViewportCommand, ViewportEvent, ViewportId, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use verge_core::Unit;
use verge_core::release::{GitHub, Repo};
use verge_core::sim::http::SimHttp;
use verge_core::sim::{SimBus, SimFirmware, firmware_srecord};
use verge_core::update::Outcome;
use verge_core::version::StockVersion;

use crate::app::{Config, VergeApp};
use crate::log::Log;
use crate::style::{self, color};
use crate::worker::{Event, Manual, Shared, Worker};

fn repo() -> Repo {
    "someone/mculink-verge".parse().unwrap()
}

fn sim_app(bus: SimBus, stock_dirs: Vec<PathBuf>) -> VergeApp {
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let key = http.public_key();
    app_on(
        bus,
        GitHub::new(http, repo(), None).with_key(Some(key)),
        stock_dirs,
    )
}

/// A window on `bus`, with releases from `github`.
fn app_on<H>(bus: SimBus, github: GitHub<H>, stock_dirs: Vec<PathBuf>) -> VergeApp
where
    H: verge_core::release::Http + Clone + Send + 'static,
{
    let worker = Worker::spawn(bus, github, || {});
    VergeApp::new(
        Box::new(worker),
        Log::default(),
        Config {
            repo: repo(),
            simulated: Some("test".into()),
            stock_dirs,
            record_dir: std::env::temp_dir(),
        },
    )
}

fn harness(app: VergeApp) -> Harness<'static, VergeApp> {
    Harness::builder()
        .with_size(vec2(1080.0, 700.0))
        .with_context_setup(style::install)
        .build_ui_state(|ui, app: &mut VergeApp| app.show(ui), app)
}

/// Steps until a widget labelled `label` shows, or fails after 400 frames.
fn until(h: &mut Harness<'_, VergeApp>, label: &str) {
    for _ in 0..400 {
        h.step();
        if h.query_by_label(label).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no {label:?} after 400 frames");
}

fn until_contains(h: &mut Harness<'_, VergeApp>, label: &str) {
    for _ in 0..400 {
        h.step();
        if h.query_by_label_contains(label).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("nothing containing {label:?} after 400 frames");
}

fn press(h: &mut Harness<'_, VergeApp>, label: &str) {
    // Scrolling animates; a press and release that land on a moving widget are no click.
    h.get_by_label(label).scroll_to_me();
    h.run_steps(20);
    h.get_by_label(label).click();
    h.step();
    h.step();
}

#[test]
fn the_bench_is_listed_with_the_release_to_install() {
    let mut h = harness(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "UPDATE TO V0.3.0");
    h.get_by_role_and_label(Role::ListBoxOption, "CJ3E7A91C2");
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41");
    h.get_by_label_contains("runs 14d05d4, which carries no version");
    h.get_by_label("SIMULATED BENCH: TEST");
}

#[test]
fn an_update_is_confirmed_runs_and_reports_the_unit_back() {
    let mut h = harness(sim_app(SimBus::scenario("stock").unwrap(), vec![]));
    until(&mut h, "INSTALL V0.3.0");
    press(&mut h, "INSTALL V0.3.0");
    // Nothing runs until the user confirms.
    h.get_by_label_contains("Flash release v0.3.0 onto QK7RM2VTX8D41 at 1-1?");
    assert!(!h.state().busy());
    press(&mut h, "CONFIRM");
    assert!(h.state().busy());
    until(&mut h, "DONE");
    h.get_by_label_contains("runs ours, build v0.3.0");
    assert!(!h.state().busy());
    press(&mut h, "BACK");
    // The rescan after the update finds the unit by its port and selects it again.
    until(&mut h, "OURS v0.3.0 · 1-1");
}

/// The second of two units, updated, comes back as a new enumeration with a new serial; the
/// window keeps it selected by its port rather than falling back to the first unit.
#[test]
fn the_updated_unit_stays_selected_when_it_returns() {
    let mut h = harness(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "UPDATE TO V0.3.0");
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41")
        .click();
    h.step();
    h.step();
    press(&mut h, "INSTALL V0.3.0");
    press(&mut h, "CONFIRM");
    until(&mut h, "DONE");
    press(&mut h, "BACK");
    until_contains(&mut h, "CJ3E7B02F9 · 1-2 · 1fc9:0143");
}

#[test]
fn confirming_a_unit_already_in_isp_mode_says_it_may_not_be_an_mculink() {
    let mut h = harness(sim_app(SimBus::scenario("isp").unwrap(), vec![]));
    until(&mut h, "INSTALL V0.3.0");
    press(&mut h, "INSTALL V0.3.0");
    h.get_by_label_contains(
        "The tool cannot tell an MCU-Link from any other LPC55S69 board in ISP mode. Flash release v0.3.0",
    );
    // A jumper-free update restarts a unit it knows, so its confirmation does not say so.
    let mut h = harness(sim_app(SimBus::scenario("ours").unwrap(), vec![]));
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "UPDATE TO V0.3.0");
    h.get_by_label_contains("Flash release v0.3.0 onto CJ3E7A91C2");
    assert!(
        h.query_by_label_contains("cannot tell an MCU-Link")
            .is_none()
    );
}

#[test]
fn a_mini_in_isp_mode_leaves_the_other_bootloader_nothing_to_press() {
    let mut h = harness(sim_app(SimBus::scenario("isp-mini").unwrap(), vec![]));
    until(&mut h, "CANNOT UPDATE");
    h.get_by_label_contains("unplug the mini first");
}

#[test]
fn two_units_in_isp_mode_leave_nothing_to_press() {
    let mut h = harness(sim_app(SimBus::scenario("two-isp").unwrap(), vec![]));
    until(&mut h, "CANNOT UPDATE");
    assert!(
        h.get_by_label("CANNOT UPDATE")
            .accesskit_node()
            .is_disabled()
    );
    h.get_by_label_contains("2 units are in ISP mode");
}

#[test]
fn old_stock_is_sent_to_the_jumper() {
    let mut h = harness(sim_app(SimBus::scenario("stock-old").unwrap(), vec![]));
    until(&mut h, "NEEDS THE JUMPER");
    h.get_by_label_contains("NXP added it in V3.122");
}

#[test]
fn going_back_to_stock_uses_the_install_on_disk() {
    let dir = std::env::temp_dir().join(format!("verge-gui-stock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let version = StockVersion::parse("3.172").unwrap();
    std::fs::write(
        dir.join("MCU-LINK_CMSIS-DAP_V3_172.s19"),
        firmware_srecord(&SimFirmware::Stock { version }),
    )
    .unwrap();
    let mut h = harness(sim_app(
        SimBus::scenario("ours").unwrap(),
        vec![dir.clone()],
    ));
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "STOCK");
    press(&mut h, "INSTALL STOCK V3.172");
    press(&mut h, "CONFIRM");
    until(&mut h, "DONE");
    h.get_by_label_contains("runs stock V3.172");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_unit_on_the_release_needs_install_anyway() {
    let bus = SimBus::default();
    bus.add_ours("v0.3.0");
    let mut h = harness(sim_app(bus, vec![]));
    until(&mut h, "NOTHING TO UPDATE");
    h.get_by_label_contains("The unit already runs v0.3.0.");
    h.get_by_role(Role::CheckBox).click();
    h.step();
    h.step();
    h.get_by_label("UPDATE TO V0.3.0");
}

/// A window whose backend the test drives by hand, showing the one unit of `scenario`.
fn manual_app(scenario: &str, release: bool) -> (VergeApp, Shared) {
    let bus = SimBus::scenario(scenario).unwrap();
    let units: Vec<Unit> = verge_core::detect(&bus).unwrap();
    let backend: Shared = Arc::new(Mutex::new(Manual::default()));
    {
        let mut manual = backend.lock().unwrap();
        manual.queued.push_back(Event::Units(Ok(units)));
        if release {
            let http = SimHttp::new(repo());
            http.publish_ours("v0.3.0");
            let latest = GitHub::new(http, repo(), None).latest().unwrap();
            manual
                .queued
                .push_back(Event::Release(Ok(Box::new(latest))));
        }
    }
    let app = VergeApp::new(
        Box::new(backend.clone()),
        Log::default(),
        Config {
            repo: repo(),
            simulated: None,
            stock_dirs: vec![],
            record_dir: std::env::temp_dir(),
        },
    );
    (app, backend)
}

fn root_commands(h: &Harness<'_, VergeApp>) -> Vec<ViewportCommand> {
    h.output()
        .viewport_output
        .get(&ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

fn request_close(h: &mut Harness<'_, VergeApp>) {
    h.input_mut()
        .viewports
        .entry(ViewportId::ROOT)
        .or_default()
        .events
        .push(ViewportEvent::Close);
    h.step();
}

#[test]
fn the_window_stays_open_while_an_update_writes() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "CONFIRM");
    assert_eq!(
        backend.lock().unwrap().sent.last().unwrap(),
        "install Release(Some(\"v0.3.0\"))"
    );

    request_close(&mut h);
    assert!(
        root_commands(&h).contains(&ViewportCommand::CancelClose),
        "{:?}",
        root_commands(&h)
    );
    h.step();
    h.get_by_label_contains("The window closes when this update finishes.");
    assert!(!root_commands(&h).contains(&ViewportCommand::Close));

    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Finished(Ok(Outcome::JumperStillFitted)));
    h.step();
    assert!(
        root_commands(&h).contains(&ViewportCommand::Close),
        "{:?}",
        root_commands(&h)
    );
}

#[test]
fn an_idle_window_closes_at_once() {
    let (app, _backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    request_close(&mut h);
    assert!(!root_commands(&h).contains(&ViewportCommand::CancelClose));
}

#[test]
fn cancel_asks_the_job_to_stop_at_a_safe_point() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "CONFIRM");
    let cancel = backend.lock().unwrap().last_cancel.clone().unwrap();
    assert!(!cancel.is_cancelled());
    press(&mut h, "CANCEL");
    assert!(cancel.is_cancelled());
    h.get_by_label("CANCELLING AT THE NEXT SAFE POINT");
}

#[test]
fn a_release_lookup_that_fails_says_so() {
    let (app, backend) = manual_app("bench", false);
    let mut h = harness(app);
    until_contains(&mut h, "Looking up the latest release");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Release(Err("GitHub could not be reached".into())));
    until(&mut h, "NO RELEASE");
    h.get_by_label_contains("GitHub could not be reached");
    // Picking the stock unit shows its own state; the release stays unavailable to it.
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41")
        .click();
    h.step();
    h.step();
    until(&mut h, "STOCK V3.172 · 1-2");
}

#[test]
fn ink_reads_on_every_fill() {
    for (name, fill) in [
        ("OURS", color::OURS),
        ("ATTENTION", color::ATTENTION),
        ("FAULT", color::FAULT),
        ("ACCENT", color::ACCENT),
        ("TEXT_2", color::TEXT_2),
    ] {
        let ratio = style::contrast(color::INK, fill);
        assert!(ratio >= 4.5, "INK on {name}: {ratio:.1}");
    }
    assert!(style::contrast(color::TEXT, color::BACKGROUND) >= 7.0);
    assert!(style::contrast(color::TEXT_2, color::BACKGROUND) >= 4.5);
}

/// Renders the window's states to PNG for a person to look at. Needs a GPU adapter.
///
/// `VERGE_GUI_SHOTS=<dir> cargo test -p verge-gui pictures -- --ignored`
#[test]
#[ignore = "renders with wgpu into $VERGE_GUI_SHOTS"]
fn pictures() {
    let dir = PathBuf::from(std::env::var("VERGE_GUI_SHOTS").expect("VERGE_GUI_SHOTS"));
    std::fs::create_dir_all(&dir).unwrap();
    let shoot = |h: &mut Harness<'_, VergeApp>, name: &str| {
        h.step();
        let image = h.render().expect("wgpu renders the window");
        image.save(dir.join(format!("{name}.png"))).unwrap();
    };
    let rendered = |app: VergeApp| {
        Harness::builder()
            .with_size(vec2(1080.0, 700.0))
            .with_context_setup(style::install)
            .wgpu()
            .build_ui_state(|ui, app: &mut VergeApp| app.show(ui), app)
    };

    let mut h = rendered(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "UPDATE TO V0.3.0");
    shoot(&mut h, "01-ours-selected");
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41")
        .click();
    h.step();
    shoot(&mut h, "02-stock-selected");
    press(&mut h, "INSTALL V0.3.0");
    shoot(&mut h, "03-confirm");

    // Working, held mid-write by a hand-driven backend.
    let (app, backend) = manual_app("ours", true);
    let mut h = rendered(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "CONFIRM");
    {
        let mut manual = backend.lock().unwrap();
        use verge_core::progress::Step;
        manual.queued.push_back(Event::Start {
            step: Step::Download,
            total: 7000,
        });
        manual.queued.push_back(Event::Plan(vec![
            Step::EnterBootloader,
            Step::WaitForBootloader,
            Step::Connect,
            Step::Erase,
            Step::Write,
            Step::Verify,
            Step::Reset,
            Step::WaitForFirmware,
        ]));
        manual.queued.push_back(Event::Loaded(
            "release v0.3.0 (mculink-verge-v0.3.0.s19)".into(),
        ));
        for step in [
            Step::EnterBootloader,
            Step::WaitForBootloader,
            Step::Connect,
            Step::Erase,
        ] {
            manual.queued.push_back(Event::Start { step, total: 0 });
        }
        manual.queued.push_back(Event::Start {
            step: Step::Write,
            total: 235_520,
        });
        manual.queued.push_back(Event::Advance(98_304));
    }
    shoot(&mut h, "04-working");
    request_close(&mut h);
    shoot(&mut h, "05-close-held");

    let mut h = rendered(sim_app(SimBus::scenario("stock").unwrap(), vec![]));
    until(&mut h, "INSTALL V0.3.0");
    press(&mut h, "INSTALL V0.3.0");
    press(&mut h, "CONFIRM");
    until(&mut h, "DONE");
    shoot(&mut h, "06-done");

    let (app, backend) = manual_app("ours", true);
    let mut h = rendered(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "CONFIRM");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Finished(Err(crate::worker::JobError {
            message: "the bootloader: unexpected status code: 10203 (0x27DB) MemoryCumulativeWrite"
                .into(),
            advice: Some(
                "The unit may be in ISP mode with a partial image. Flashing it again recovers it.",
            ),
            left_in_isp: true,
        })));
    shoot(&mut h, "07-failed");

    // The batch view: set up, running, and done.
    let (app, backend) = manual_app("ours", true);
    let mut h = rendered(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BATCH");
    shoot(&mut h, "12-batch-setup");
    press(&mut h, "ARM BATCH");
    shoot(&mut h, "13-batch-confirm");
    press(&mut h, "CONFIRM");
    {
        use verge_core::batch::{Record, Tally};
        use verge_core::progress::Step;
        let mut manual = backend.lock().unwrap();
        manual.queued.push_back(Event::BatchArmed {
            name: "release v0.3.0 (mculink-verge-v0.3.0.s19)".into(),
            filter: "every full MCU-Link running stock or ours older than the image, attached from now on".into(),
        });
        let record = |port: &str,
                      serial: &str,
                      before: &str,
                      after: Option<&str>,
                      result: verge_core::batch::Settled,
                      reason: Option<&str>| {
            Event::BatchRecord(Box::new(Record {
                time: "2026-10-20T14:26:08Z".into(),
                port: port.into(),
                serial_before: Some(serial.into()),
                firmware_before: before.into(),
                serial_after: after.map(|_| "CJ3E7A91C2".to_owned()),
                firmware_after: after.map(str::to_owned),
                result,
                reason: reason.map(str::to_owned),
                seconds: after.map(|_| 4.7),
            }))
        };
        manual.queued.push_back(record(
            "1-1",
            "QK7RM2VTX8D41",
            "stock V3.172",
            Some("ours, build v0.3.0"),
            verge_core::batch::Settled::Updated,
            None,
        ));
        manual.queued.push_back(record(
            "1-3",
            "CJ3E7AC4F5",
            "ours, build v0.3.0",
            None,
            verge_core::batch::Settled::Current,
            None,
        ));
        manual.queued.push_back(record("1-4", "QK7RM2VTX8D45", "stock V3.108", None, verge_core::batch::Settled::Skipped, Some("needs the ISP jumper. Stock V3.108 has no jumper-free update. NXP added it in V3.122.")));
        manual.queued.push_back(Event::BatchStarting("1-2".into()));
        manual.queued.push_back(Event::Start {
            step: Step::Write,
            total: 235_520,
        });
        manual.queued.push_back(Event::Advance(131_072));
        manual
            .queued
            .push_back(Event::BatchState(crate::worker::BatchState {
                tally: Tally {
                    updated: 1,
                    current: 1,
                    skipped: 1,
                    failed: 0,
                    waiting: 1,
                },
                waiting: vec![("1-5".into(), Some("CJ3E7AE717".into()))],
                paused: None,
            }));
    }
    shoot(&mut h, "14-batch-running");
    backend.lock().unwrap().queued.push_back(Event::BatchState(crate::worker::BatchState {
        tally: verge_core::batch::Tally { updated: 1, current: 1, skipped: 1, failed: 1, waiting: 1 },
        waiting: vec![("1-5".into(), Some("CJ3E7AE717".into()))],
        paused: Some("The unit at 1-2 failed and is in ISP mode, maybe with a partial image. The batch waits until it is unplugged; `verge flash` flashes it again.".into()),
    }));
    shoot(&mut h, "15-batch-paused");

    for (scenario, name) in [
        ("stock-old", "08-old-stock-jumper"),
        ("two-isp", "09-two-in-isp"),
        ("busy", "10-busy"),
        ("isp-jumper", "11-isp-mode"),
    ] {
        let mut h = rendered(sim_app(SimBus::scenario(scenario).unwrap(), vec![]));
        for _ in 0..40 {
            h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
        shoot(&mut h, name);
    }
}

/// A directory of its own for a test's batch record.
fn record_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("verge-gui-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_batch_updates_units_as_they_arrive_and_records_each() {
    let dir = record_dir("batch");
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let key = http.public_key();
    let worker = Worker::spawn(
        SimBus::scenario("arrivals").unwrap(),
        GitHub::new(http, repo(), None).with_key(Some(key)),
        || {},
    );
    let app = VergeApp::new(
        Box::new(worker),
        Log::default(),
        Config {
            repo: repo(),
            simulated: Some("arrivals".into()),
            stock_dirs: vec![],
            record_dir: dir.clone(),
        },
    );
    let mut h = harness(app);
    until(&mut h, "BATCH");
    press(&mut h, "BATCH");
    until(&mut h, "ARM BATCH");
    press(&mut h, "ARM BATCH");
    h.get_by_label_contains(
        "Flash release v0.3.0 onto every full MCU-Link running stock or ours older than the image, attached from now on?",
    );
    assert!(
        h.query_by_label_contains("cannot tell an MCU-Link")
            .is_none()
    );
    press(&mut h, "CONFIRM");
    assert!(h.state().busy());
    until(&mut h, "3 updated");
    until(&mut h, "1 current");
    until(&mut h, "1 skipped");
    h.get_by_label("0 failed");
    press(&mut h, "STOP");
    until(&mut h, "ENDED");
    assert!(!h.state().busy());

    let records: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(records.len(), 1);
    let results: Vec<String> = records[0]
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
fn closing_during_a_batch_stops_it_taking_units_and_waits_for_it() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BATCH");
    press(&mut h, "ARM BATCH");
    press(&mut h, "CONFIRM");
    assert!(
        backend
            .lock()
            .unwrap()
            .sent
            .last()
            .unwrap()
            .starts_with("batch Release")
    );
    let stop = backend.lock().unwrap().last_cancel.clone().unwrap();
    request_close(&mut h);
    assert!(root_commands(&h).contains(&ViewportCommand::CancelClose));
    assert!(stop.is_cancelled(), "the batch was not told to stop");
    h.step();
    h.get_by_label_contains("The window closes when the batch's unit under way finishes.");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::BatchEnded(Ok(Default::default())));
    h.step();
    assert!(root_commands(&h).contains(&ViewportCommand::Close));
}

#[test]
fn a_paused_batch_says_why() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BATCH");
    press(&mut h, "ARM BATCH");
    press(&mut h, "CONFIRM");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::BatchState(crate::worker::BatchState {
            paused: Some(
                "An MCU-Link mini is in ISP mode at 1-2. The batch waits until it is unplugged."
                    .into(),
            ),
            ..Default::default()
        }));
    until(&mut h, "PAUSED");
    h.get_by_label_contains("An MCU-Link mini is in ISP mode at 1-2.");
}

#[test]
fn a_batch_that_takes_units_in_isp_mode_says_it_may_not_be_flashing_an_mculink() {
    let (app, _backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BATCH");
    press(
        &mut h,
        "Units that arrive in ISP mode, as after fitting the jumper",
    );
    press(&mut h, "ARM BATCH");
    h.get_by_label_contains(
        "The tool cannot tell an MCU-Link from any other LPC55S69 board in ISP mode. Flash release v0.3.0 onto every full MCU-Link running stock or ours older than the image or in ISP mode",
    );
}

#[test]
fn a_batch_needs_something_to_take() {
    let (app, _backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BATCH");
    h.get_by_label("Stock").click();
    h.step();
    h.get_by_label("Ours").click();
    h.step();
    h.step();
    h.get_by_label("CANNOT ARM");
    h.get_by_label_contains("Tick at least one kind of unit");
}

/// Review: the worker's panic report is `Event::Finished`, which only a single update reads.
#[test]
fn a_worker_that_dies_during_a_batch_lets_the_window_close() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BATCH");
    press(&mut h, "ARM BATCH");
    press(&mut h, "CONFIRM");
    request_close(&mut h);
    assert!(root_commands(&h).contains(&ViewportCommand::CancelClose));
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Finished(Err(crate::worker::JobError {
            message: "the USB worker stopped unexpectedly: boom".into(),
            advice: None,
            left_in_isp: true,
        })));
    for _ in 0..5 {
        h.step();
    }
    assert!(
        !h.state().busy(),
        "the window still holds itself open for a batch whose worker is gone"
    );
}

/// The simulated GitHub, except that a download hangs until the test lets it go, or for a
/// minute: far longer than any test here waits in frames.
#[derive(Clone)]
struct Stalled {
    http: SimHttp,
    release: verge_core::Cancel,
}

/// Lets a stalled download go when the test ends, passed or not, so that the worker it holds can
/// be joined. Declared after the harness, it is dropped before it.
struct Free(verge_core::Cancel);

impl Drop for Free {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl verge_core::release::Http for Stalled {
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<Vec<u8>, verge_core::release::HttpError> {
        if url.contains("/releases/download/") {
            let started = std::time::Instant::now();
            while !self.release.is_cancelled() && started.elapsed() < Duration::from_secs(60) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        self.http.get(url, headers)
    }
}

fn stalled_app() -> (VergeApp, verge_core::Cancel) {
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let release = verge_core::Cancel::new();
    let key = http.public_key();
    let stalled = Stalled {
        http,
        release: release.clone(),
    };
    let app = app_on(
        SimBus::scenario("ours").unwrap(),
        GitHub::new(stalled, repo(), None).with_key(Some(key)),
        vec![],
    );
    (app, release)
}

/// STOP while the batch's image is still downloading ends the batch: nothing has touched USB.
#[test]
fn stop_ends_a_batch_still_loading_its_image() {
    let (app, release) = stalled_app();
    let mut h = harness(app);
    let _free = Free(release);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BATCH");
    press(&mut h, "ARM BATCH");
    press(&mut h, "CONFIRM");
    h.run_steps(5);
    h.get_by_label_contains("loading the image");
    assert!(h.state().busy());
    press(&mut h, "STOP");
    until(&mut h, "ENDED");
    assert!(!h.state().busy());
}

/// Closing the window while the batch's image is still downloading closes it.
#[test]
fn the_window_closes_while_a_batch_loads_its_image() {
    let (app, release) = stalled_app();
    let mut h = harness(app);
    let _free = Free(release);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BATCH");
    press(&mut h, "ARM BATCH");
    press(&mut h, "CONFIRM");
    h.run_steps(5);
    request_close(&mut h);
    assert!(root_commands(&h).contains(&ViewportCommand::CancelClose));
    let mut closed = false;
    for _ in 0..400 {
        h.step();
        if root_commands(&h).contains(&ViewportCommand::Close) {
            closed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        closed,
        "the window did not close while the image was loading"
    );
}

/// The window shows nothing to press for an image of ours on a Pro, only stock.
#[test]
fn ours_is_not_offered_for_a_pro() {
    let mut h = harness(sim_app(SimBus::scenario("pro").unwrap(), vec![]));
    until(&mut h, "NOT FOR THIS BOARD");
    h.get_by_label_contains("The unit is an MCU-Link Pro");
}

/// The worker adds a batch's lines to a record file that has some already.
#[test]
fn a_batch_adds_to_an_existing_record_file() {
    use crate::worker::{Backend, Request};
    use verge_core::batch::{Board, Filter, OursFilter, StockFilter};
    let dir = record_dir("append");
    let record = dir.join("record.jsonl");
    std::fs::write(&record, "{\"from\":\"an earlier batch\"}\n").unwrap();
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let key = http.public_key();
    let mut worker = Worker::spawn(
        SimBus::scenario("arrivals").unwrap(),
        GitHub::new(http, repo(), None).with_key(Some(key)),
        || {},
    );
    worker.send(Request::Batch {
        source: verge_core::Source::Release(None),
        filter: Filter {
            board: Board::McuLink,
            stock: StockFilter::Any,
            ours: OursFilter::Older,
            bootloaders: false,
            attached: false,
        },
        limit: Some(1),
        record: record.clone(),
        stop: verge_core::Cancel::new(),
        cancel: verge_core::Cancel::new(),
    });
    let mut ended = None;
    for _ in 0..2000 {
        match worker.try_recv() {
            Some(Event::BatchEnded(result)) => {
                ended = Some(result);
                break;
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    assert!(matches!(ended, Some(Ok(_))), "{ended:?}");
    let text = std::fs::read_to_string(&record).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "{\"from\":\"an earlier batch\"}");
    assert_eq!(lines.len(), 3, "{text}");
}

/// CANCEL while a single update's image is still downloading ends it at once, with nothing
/// flashed.
#[test]
fn cancel_ends_an_update_still_loading_its_image() {
    let (app, release) = stalled_app();
    let mut h = harness(app);
    let _free = Free(release);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "CONFIRM");
    h.run_steps(5);
    assert!(h.state().busy());
    press(&mut h, "CANCEL");
    until(&mut h, "FAILED");
    h.get_by_label_contains("cancelled before anything was flashed");
    assert!(!h.state().busy());
}
/// Batch records go in the user's data directory: `$XDG_DATA_HOME/verge`, else
/// `~/.local/share/verge`, on Linux. Not the directory the window was started from.
#[cfg(target_os = "linux")]
#[test]
fn batch_records_go_in_the_users_data_directory() {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set")).join(".local/share")
        });
    assert_eq!(crate::record_dir(), data.join("verge"));
    assert_ne!(crate::record_dir(), std::env::current_dir().unwrap());
}

/// The data directory need not exist yet, so a batch makes its record file's directory.
#[test]
fn a_batch_makes_its_record_files_directory() {
    use crate::worker::{Backend, Request};
    use verge_core::batch::{Board, Filter, OursFilter, StockFilter};
    let dir = record_dir("made");
    let record = dir.join("not/there/yet/record.jsonl");
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let key = http.public_key();
    let mut worker = Worker::spawn(
        SimBus::scenario("arrivals").unwrap(),
        GitHub::new(http, repo(), None).with_key(Some(key)),
        || {},
    );
    worker.send(Request::Batch {
        source: verge_core::Source::Release(None),
        filter: Filter {
            board: Board::McuLink,
            stock: StockFilter::Any,
            ours: OursFilter::Older,
            bootloaders: false,
            attached: false,
        },
        limit: Some(1),
        record: record.clone(),
        stop: verge_core::Cancel::new(),
        cancel: verge_core::Cancel::new(),
    });
    let mut ended = None;
    for _ in 0..2000 {
        match worker.try_recv() {
            Some(Event::BatchEnded(result)) => {
                ended = Some(result);
                break;
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let written = std::fs::read_to_string(&record);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(matches!(ended, Some(Ok(_))), "{ended:?}");
    assert_eq!(written.unwrap().lines().count(), 2);
}
