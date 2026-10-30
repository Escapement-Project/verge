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
use verge_probe::Unit;
use verge_probe::cache::Cache;
use verge_probe::release::{Download, GitHub, Repo};
use verge_probe::sim::http::SimHttp;
use verge_probe::sim::{SimBus, SimFirmware, firmware_srecord};
use verge_probe::update::Outcome;
use verge_probe::version::StockVersion;

use crate::app::{Config, VergeApp};
use crate::log::Log;
use crate::style::{self, color};
use crate::worker::{BatchEvent, Event, InstallEvent, Manual, ProgressEvent, Shared, Worker};

fn repo() -> Repo {
    "someone/mculink-verge".parse().unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn a_record_failure_stops_the_worker_before_the_next_unit() {
    use crate::worker::{Backend, Request};
    use verge_probe::batch::{Board, Filter, OursFilter, StockFilter};
    use verge_probe::{Cancel, Source};

    let bus = SimBus::scenario("arrivals").unwrap();
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let key = http.public_key();
    let mut worker = Worker::spawn(
        bus,
        GitHub::new(http, repo(), None).with_key(Some(key)),
        || {},
    );
    let stop = Cancel::new();
    worker.send(Request::Batch {
        source: Source::Release(None),
        filter: Filter {
            board: Board::McuLink,
            stock: StockFilter::Any,
            ours: OursFilter::Any,
            bootloaders: false,
            attached: false,
        },
        download: Download::Agreed,
        limit: None,
        record: "/dev/full".into(),
        stop: stop.clone(),
        cancel: Cancel::new(),
    });
    let mut started = 0;
    for _ in 0..400 {
        while let Some(event) = worker.try_recv() {
            match event {
                Event::Batch(BatchEvent::Starting(_)) => started += 1,
                Event::Batch(BatchEvent::Ended(result)) => {
                    assert!(
                        result
                            .unwrap_err()
                            .contains("writing the batch record file")
                    );
                    assert!(stop.is_cancelled());
                    assert_eq!(started, 1);
                    return;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.cancel();
    panic!("the batch did not stop after its record failed");
}

/// How a question opens where the simulated release has to be downloaded first. The size
/// follows it.
const DOWNLOAD: &str = "Download release v0.3.0 of someone/mculink-verge from GitHub (";

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
    H: verge_probe::release::Http + Clone + Send + 'static,
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
            cache: None,
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
        if h.query_all_by_label_contains(label).next().is_some() {
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
    h.get_by_label_contains(DOWNLOAD);
    h.get_by_label_contains(" KiB), and flash it onto QK7RM2VTX8D41 at 1-1?");
    assert!(!h.state().busy());
    press(&mut h, "CONFIRM");
    assert!(h.state().busy());
    until(&mut h, "DONE");
    h.get_by_label_contains("runs Verge, build v0.3.0");
    assert!(!h.state().busy());
    press(&mut h, "BACK");
    // The rescan after the update finds the unit by its port and selects it again.
    until(&mut h, "VERGE v0.3.0 · 1-1");
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
    h.get_by_label_contains(&format!(
        "The tool cannot tell an MCU-Link from any other LPC55S69 board in ISP mode. A release says which boards it is built for only once it is downloaded. {DOWNLOAD}"
    ));
    // A jumper-free update restarts a unit it knows, so its confirmation does not say so.
    let mut h = harness(sim_app(SimBus::scenario("ours").unwrap(), vec![]));
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "UPDATE TO V0.3.0");
    h.get_by_label_contains("and flash it onto CJ3E7A91C2");
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
    let units: Vec<Unit> = verge_probe::detect(&bus).unwrap();
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
            cache: None,
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
        "install Release(Some(\"v0.3.0\")) Agreed"
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
        .push_back(Event::Install(InstallEvent::Finished(Ok(
            Outcome::JumperStillFitted,
        ))));
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
        use verge_probe::progress::Step;
        manual
            .queued
            .push_back(Event::Progress(ProgressEvent::Start {
                step: Step::Download,
                total: 7000,
            }));
        manual
            .queued
            .push_back(Event::Progress(ProgressEvent::Plan(vec![
                Step::EnterBootloader,
                Step::WaitForBootloader,
                Step::Connect,
                Step::Erase,
                Step::Write,
                Step::Verify,
                Step::Reset,
                Step::WaitForFirmware,
            ])));
        manual.queued.push_back(Event::Install(InstallEvent::Loaded(
            "release v0.3.0 (mculink-verge-v0.3.0.s19)".into(),
        )));
        for step in [
            Step::EnterBootloader,
            Step::WaitForBootloader,
            Step::Connect,
            Step::Erase,
        ] {
            manual
                .queued
                .push_back(Event::Progress(ProgressEvent::Start { step, total: 0 }));
        }
        manual
            .queued
            .push_back(Event::Progress(ProgressEvent::Start {
                step: Step::Write,
                total: 235_520,
            }));
        manual
            .queued
            .push_back(Event::Progress(ProgressEvent::Advance(98_304)));
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
        .push_back(Event::Install(InstallEvent::Finished(Err(crate::worker::JobError {
            message: "the bootloader: unexpected status code: 10203 (0x27DB) MemoryCumulativeWrite"
                .into(),
            advice: Some(
                "The unit may be in ISP mode with a partial image. Flashing it again recovers it.",
            ),
            left_in_isp: true,
        }))));
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
        use verge_probe::batch::{Record, Tally};
        use verge_probe::progress::Step;
        let mut manual = backend.lock().unwrap();
        manual.queued.push_back(Event::Batch(BatchEvent::Armed{
            name: "release v0.3.0 (mculink-verge-v0.3.0.s19)".into(),
            filter: "every full MCU-Link running stock or Verge older than the image, attached from now on".into(),
        }));
        let record = |port: &str,
                      serial: &str,
                      before: &str,
                      after: Option<&str>,
                      result: verge_probe::batch::Settled,
                      reason: Option<&str>| {
            Event::Batch(BatchEvent::Record(Box::new(Record {
                time: "2026-10-20T14:26:08Z".into(),
                port: port.into(),
                serial_before: Some(serial.into()),
                firmware_before: before.into(),
                serial_after: after.map(|_| "CJ3E7A91C2".to_owned()),
                firmware_after: after.map(str::to_owned),
                result,
                reason: reason.map(str::to_owned),
                seconds: after.map(|_| 4.7),
            })))
        };
        manual.queued.push_back(record(
            "1-1",
            "QK7RM2VTX8D41",
            "stock V3.172",
            Some("Verge, build v0.3.0"),
            verge_probe::batch::Settled::Updated,
            None,
        ));
        manual.queued.push_back(record(
            "1-3",
            "CJ3E7AC4F5",
            "Verge, build v0.3.0",
            None,
            verge_probe::batch::Settled::Current,
            None,
        ));
        manual.queued.push_back(record("1-4", "QK7RM2VTX8D45", "stock V3.108", None, verge_probe::batch::Settled::Skipped, Some("needs the ISP jumper. Stock V3.108 has no jumper-free update. NXP added it in V3.122.")));
        manual
            .queued
            .push_back(Event::Batch(BatchEvent::Starting("1-2".into())));
        manual
            .queued
            .push_back(Event::Progress(ProgressEvent::Start {
                step: Step::Write,
                total: 235_520,
            }));
        manual
            .queued
            .push_back(Event::Progress(ProgressEvent::Advance(131_072)));
        manual
            .queued
            .push_back(Event::Batch(BatchEvent::State(crate::worker::BatchState {
                tally: Tally {
                    updated: 1,
                    current: 1,
                    skipped: 1,
                    failed: 0,
                    waiting: 1,
                },
                waiting: vec![("1-5".into(), Some("CJ3E7AE717".into()))],
                paused: None,
            })));
    }
    shoot(&mut h, "14-batch-running");
    backend.lock().unwrap().queued.push_back(Event::Batch(BatchEvent::State(crate::worker::BatchState {
        tally: verge_probe::batch::Tally { updated: 1, current: 1, skipped: 1, failed: 1, waiting: 1 },
        waiting: vec![("1-5".into(), Some("CJ3E7AE717".into()))],
        paused: Some("The unit at 1-2, a full MCU-Link, failed and is in ISP mode, maybe with a partial image. The batch waits until it is unplugged. In ISP mode the tool cannot tell its board, so flash it again with the batch's image, which is built for it: `verge isp flash --release v0.3.0`.".into()),
    })));
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

    // The Bench view on a simulated Pro whose capture runs, recording into the pictures' directory.
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let key = http.public_key();
    // Paced, so the stream arrives at its rate rather than as fast as the worker reads it.
    let worker = Worker::spawn(
        SimBus::scenario("ours-pro-energy").unwrap().paced(1.0),
        GitHub::new(http, repo(), None).with_key(Some(key)),
        || {},
    );
    let app = VergeApp::new(
        Box::new(worker),
        Log::default(),
        Config {
            repo: repo(),
            simulated: Some("test".into()),
            stock_dirs: vec![],
            record_dir: dir.clone(),
            cache: None,
        },
    );
    let mut h = rendered(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BENCH");
    until(&mut h, "RECORD RUNNING CAPTURE");
    shoot(&mut h, "16-bench-connected");
    press(&mut h, "DIO0–2 OUTPUT");
    until(&mut h, "DRIVE PINS");
    shoot(&mut h, "18-bench-drive-confirmation");
    press(&mut h, "DRIVE PINS");
    for _ in 0..40 {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    shoot(&mut h, "19-bench-outputs");
    press(&mut h, "DIO0–2 INPUT");
    press(&mut h, "RECORD RUNNING CAPTURE");
    until(&mut h, "STOP RECORDING");
    for _ in 0..200 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    shoot(&mut h, "17-bench-recording");
    press(&mut h, "STOP RECORDING");
    until(&mut h, "RECORD RUNNING CAPTURE");
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
            cache: None,
        },
    );
    let mut h = harness(app);
    until(&mut h, "BATCH");
    press(&mut h, "BATCH");
    until(&mut h, "ARM BATCH");
    press(&mut h, "ARM BATCH");
    h.get_by_label_contains(DOWNLOAD);
    h.get_by_label_contains(
        " KiB), and flash it onto every full MCU-Link running stock or Verge older than the image, attached from now on?",
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
        .push_back(Event::Batch(BatchEvent::Ended(Ok(Default::default()))));
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
        .push_back(Event::Batch(BatchEvent::State(crate::worker::BatchState {
            paused: Some(
                "An MCU-Link mini is in ISP mode at 1-2. The batch waits until it is unplugged."
                    .into(),
            ),
            ..Default::default()
        })));
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
    h.get_by_label_contains(&format!(
        "The tool cannot tell an MCU-Link from any other LPC55S69 board in ISP mode. {DOWNLOAD}"
    ));
    h.get_by_label_contains(
        "and flash it onto every full MCU-Link running stock or Verge older than the image or in ISP mode",
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
    h.get_by_label("Verge").click();
    h.step();
    h.step();
    h.get_by_label("CANNOT ARM");
    h.get_by_label_contains("Tick at least one kind of unit");
}

/// A worker failure ends a batch as well as a single-unit update.
#[test]
fn a_worker_failure_during_discovery_is_reported() {
    let (app, backend) = manual_app("ours", false);
    {
        let mut manual = backend.lock().unwrap();
        manual.queued.clear();
        manual
            .queued
            .push_back(Event::WorkerFailed(crate::worker::JobError {
                message: "discovery worker failed".into(),
                advice: None,
                left_in_isp: false,
            }));
    }
    let mut h = harness(app);
    h.step();
    h.get_by_label_contains("Could not list USB devices: discovery worker failed");
    assert!(!h.state().busy());
}

#[test]
fn a_worker_failure_during_installation_finishes_the_job() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "CONFIRM");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::WorkerFailed(crate::worker::JobError {
            message: "install worker failed".into(),
            advice: None,
            left_in_isp: true,
        }));
    until(&mut h, "FAILED");
    assert!(!h.state().busy());
    press(&mut h, "BACK");
    h.get_by_label("WORKER STOPPED");
}

#[test]
fn a_worker_failure_withdraws_an_open_install_question() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "UPDATE TO V0.3.0");
    h.get_by_label("CONFIRM");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::WorkerFailed(crate::worker::JobError {
            message: "the USB worker stopped unexpectedly: boom".into(),
            advice: None,
            left_in_isp: false,
        }));
    until(&mut h, "WORKER STOPPED");
    assert!(h.query_by_label("CONFIRM").is_none());
    assert!(!h.state().busy());
}

#[test]
fn a_question_about_a_unit_that_left_is_withdrawn() {
    let bus = SimBus::default();
    bus.add_ours("v0.2.0");
    bus.add_ours("v0.2.0");
    let units: Vec<Unit> = verge_probe::detect(&bus).unwrap();
    assert_eq!(units.len(), 2);
    let backend: Shared = Arc::new(Mutex::new(Manual::default()));
    {
        let mut manual = backend.lock().unwrap();
        manual.queued.push_back(Event::Units(Ok(units.clone())));
        let http = SimHttp::new(repo());
        http.publish_ours("v0.3.0");
        let latest = GitHub::new(http, repo(), None).latest().unwrap();
        manual
            .queued
            .push_back(Event::Release(Ok(Box::new(latest))));
    }
    let app = VergeApp::new(
        Box::new(backend.clone()),
        Log::default(),
        Config {
            repo: repo(),
            simulated: None,
            stock_dirs: vec![],
            record_dir: std::env::temp_dir(),
            cache: None,
        },
    );
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "UPDATE TO V0.3.0");
    h.get_by_label("CONFIRM");
    // The asked-about unit is unplugged; the rescan finds the other, which becomes selected.
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Units(Ok(vec![units[1].clone()])));
    h.run_steps(3);
    assert!(h.query_by_label("CONFIRM").is_none());
    h.get_by_label("UPDATE TO V0.3.0");
}

#[test]
fn an_install_preserves_the_previous_batch_result() {
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
        .push_back(Event::Batch(BatchEvent::Ended(Ok(Default::default()))));
    until(&mut h, "ENDED");
    press(&mut h, "UNITS");
    press(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "CONFIRM");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Install(InstallEvent::Finished(Ok(
            Outcome::JumperStillFitted,
        ))));
    h.run_steps(3);
    assert!(!h.state().busy());
    press(&mut h, "BATCH");
    h.get_by_label("ENDED");
}

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
        .push_back(Event::WorkerFailed(crate::worker::JobError {
            message: "the USB worker stopped unexpectedly: boom".into(),
            advice: None,
            left_in_isp: true,
        }));
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
    release: verge_probe::Cancel,
}

/// Lets a stalled download go when the test ends, passed or not, so that the worker it holds can
/// be joined. Declared after the harness, it is dropped before it.
struct Free(verge_probe::Cancel);

impl Drop for Free {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl verge_probe::release::Http for Stalled {
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<Vec<u8>, verge_probe::release::HttpError> {
        if url.contains("/releases/download/") {
            let started = std::time::Instant::now();
            while !self.release.is_cancelled() && started.elapsed() < Duration::from_secs(60) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        self.http.get(url, headers)
    }
}

fn stalled_app() -> (VergeApp, verge_probe::Cancel) {
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let release = verge_probe::Cancel::new();
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

/// A file dropped on the window, as a file manager would drop it.
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|error| error.to_string())
    }
}

fn drop_file(h: &mut Harness<'_, VergeApp>, path: &std::path::Path) {
    h.input_mut()
        .dropped_files
        .push(Arc::new(Dropped(path.to_path_buf())));
    h.step();
    h.step();
}

/// On a Pro an image of ours goes on only where its declaration names the Pro. A release says so
/// once it is downloaded, and one that does not is refused before the unit is touched. A file
/// says so as soon as it is picked.
#[test]
fn ours_goes_on_a_pro_only_where_its_image_declares_it() {
    let bus = SimBus::scenario("pro").unwrap();
    let mut h = harness(sim_app(bus.clone(), vec![]));
    until(&mut h, "INSTALL V0.3.0");
    press(&mut h, "INSTALL V0.3.0");
    press(&mut h, "CONFIRM");
    until(&mut h, "FAILED");
    h.get_by_label_contains(
        "the unit is an MCU-Link Pro, and this image is built for the full MCU-Link only",
    );
    h.get_by_label_contains("Pick STOCK");
    assert!(!bus.log().iter().any(|l| l.contains("DAP 8101")));
    press(&mut h, "BACK");

    let dir = record_dir("pro-files");
    let ours = SimFirmware::Ours {
        build: "v0.3.0".into(),
    };
    let plain = dir.join("plain.s19");
    std::fs::write(&plain, firmware_srecord(&ours)).unwrap();
    let pro = dir.join("pro.s19");
    std::fs::write(
        &pro,
        verge_probe::sim::firmware_srecord_declaring(&ours, 1 << 1),
    )
    .unwrap();
    drop_file(&mut h, &plain);
    until(&mut h, "NOT FOR THIS BOARD");
    h.get_by_label_contains("The unit is an MCU-Link Pro");
    drop_file(&mut h, &pro);
    until(&mut h, "FLASH PRO.S19");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A running unit that does not say which board it is gets an image of ours only after a
/// question that says so.
#[test]
fn a_unit_that_names_no_board_is_asked_about() {
    let mut h = harness(sim_app(SimBus::scenario("unnamed-board").unwrap(), vec![]));
    until(&mut h, "INSTALL V0.3.0");
    press(&mut h, "INSTALL V0.3.0");
    h.get_by_label_contains(&format!(
        "This unit's firmware does not say which board it is, so the tool cannot check that the image is built for it. A release says which boards it is built for only once it is downloaded. {DOWNLOAD}"
    ));
}

/// The worker adds a batch's lines to a record file that has some already.
#[test]
fn a_batch_adds_to_an_existing_record_file() {
    use crate::worker::{Backend, Request};
    use verge_probe::batch::{Board, Filter, OursFilter, StockFilter};
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
        source: verge_probe::Source::Release(None),
        filter: Filter {
            board: Board::McuLink,
            stock: StockFilter::Any,
            ours: OursFilter::Older,
            bootloaders: false,
            attached: false,
        },
        download: Download::Agreed,
        limit: std::num::NonZeroUsize::new(1),
        record: record.clone(),
        stop: verge_probe::Cancel::new(),
        cancel: verge_probe::Cancel::new(),
    });
    let mut ended = None;
    for _ in 0..2000 {
        match worker.try_recv() {
            Some(Event::Batch(BatchEvent::Ended(result))) => {
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
    use verge_probe::batch::{Board, Filter, OursFilter, StockFilter};
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
        source: verge_probe::Source::Release(None),
        filter: Filter {
            board: Board::McuLink,
            stock: StockFilter::Any,
            ours: OursFilter::Older,
            bootloaders: false,
            attached: false,
        },
        download: Download::Agreed,
        limit: std::num::NonZeroUsize::new(1),
        record: record.clone(),
        stop: verge_probe::Cancel::new(),
        cancel: verge_probe::Cancel::new(),
    });
    let mut ended = None;
    for _ in 0..2000 {
        match worker.try_recv() {
            Some(Event::Batch(BatchEvent::Ended(result))) => {
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

/// Stock's image carries no version of ours, so a batch that takes ours older than the image
/// cannot arm with it, and the view says what to change.
#[test]
fn a_batch_that_cannot_date_ours_cannot_arm() {
    let dir = record_dir("stock-batch");
    std::fs::write(
        dir.join("MCU-LINK_CMSIS-DAP_V3_172.s19"),
        firmware_srecord(&SimFirmware::Stock {
            version: StockVersion::parse("3.172").unwrap(),
        }),
    )
    .unwrap();
    let mut h = harness(sim_app(
        SimBus::scenario("arrivals").unwrap(),
        vec![dir.clone()],
    ));
    until(&mut h, "BATCH");
    press(&mut h, "BATCH");
    press(&mut h, "STOCK");
    until(&mut h, "CANNOT ARM");
    h.get_by_label_contains("The image carries no Verge version");
    h.get_by_label_contains("Tick \"of any build\"");
    press(&mut h, "of any build, not only those older than the image");
    until(&mut h, "ARM BATCH");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A release's image says which boards it is for only once it is downloaded, so a batch for the
/// Pro with a release that does not declare it ends there, unarmed.
#[test]
fn a_batch_for_the_pro_does_not_arm_with_an_image_not_built_for_it() {
    let mut h = harness(sim_app(SimBus::scenario("arrivals").unwrap(), vec![]));
    until(&mut h, "BATCH");
    press(&mut h, "BATCH");
    press(&mut h, "MCU-Link Pro");
    press(&mut h, "ARM BATCH");
    h.get_by_label_contains("onto every MCU-Link Pro running");
    press(&mut h, "CONFIRM");
    until(&mut h, "NOT ARMED");
    h.get_by_label_contains(
        "The batch takes the MCU-Link Pro, and the image is built for the full MCU-Link only.",
    );
    assert!(!h.state().busy());
}

/// A Pro that needs the jumper: in ISP mode nothing tells its board, so the view names the board
/// and the image to flash then, and refuses a file not built for it before the jumper.
#[test]
fn the_jumper_advice_names_the_board_and_the_image() {
    let mut h = harness(sim_app(SimBus::scenario("pro-old").unwrap(), vec![]));
    until(&mut h, "NEEDS THE JUMPER");
    h.get_by_label_contains(
        "This unit is an MCU-Link Pro. In ISP mode the tool cannot tell its board, and a release says which boards it is for only once it is downloaded: flash it with STOCK, or with a file built for an MCU-Link Pro.",
    );
    let dir = record_dir("jumper-files");
    let ours = SimFirmware::Ours {
        build: "v0.3.0".into(),
    };
    let plain = dir.join("plain.s19");
    std::fs::write(&plain, firmware_srecord(&ours)).unwrap();
    let pro = dir.join("pro.s19");
    std::fs::write(
        &pro,
        verge_probe::sim::firmware_srecord_declaring(&ours, 1 << 1),
    )
    .unwrap();
    drop_file(&mut h, &plain);
    until(&mut h, "NOT FOR THIS BOARD");
    drop_file(&mut h, &pro);
    until(&mut h, "NEEDS THE JUMPER");
    h.get_by_label_contains(
        "This unit is an MCU-Link Pro. In ISP mode the tool cannot tell its board, so flash it then with pro.s19, which is built for it.",
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The question before flashing a unit in ISP mode names the boards a file is built for, as the
/// command line's does.
#[test]
fn the_question_names_the_boards_a_file_is_built_for() {
    let mut h = harness(sim_app(SimBus::scenario("isp").unwrap(), vec![]));
    until(&mut h, "INSTALL V0.3.0");
    let dir = record_dir("question-file");
    let pro = dir.join("pro.s19");
    std::fs::write(
        &pro,
        verge_probe::sim::firmware_srecord_declaring(
            &SimFirmware::Ours {
                build: "v0.3.0".into(),
            },
            1 << 1,
        ),
    )
    .unwrap();
    drop_file(&mut h, &pro);
    until(&mut h, "FLASH PRO.S19");
    press(&mut h, "FLASH PRO.S19");
    h.get_by_label_contains(
        "board in ISP mode. The image is built for the MCU-Link Pro only. Flash pro.s19",
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A batch that cannot date ours is refused before its image is looked for.
#[test]
fn the_worker_refuses_an_undatable_batch_before_loading() {
    use crate::worker::{Backend, Request};
    use verge_probe::batch::{Board, Filter, OursFilter, StockFilter};
    let dir = record_dir("undated");
    let record = dir.join("record.jsonl");
    let http = SimHttp::new(repo());
    let mut worker = Worker::spawn(
        SimBus::scenario("arrivals").unwrap(),
        GitHub::new(http, repo(), None),
        || {},
    );
    worker.send(Request::Batch {
        source: verge_probe::Source::Stock(Some(PathBuf::from("/nonexistent/probe_firmware"))),
        filter: Filter {
            board: Board::McuLink,
            stock: StockFilter::Any,
            ours: OursFilter::Older,
            bootloaders: false,
            attached: false,
        },
        download: Download::Agreed,
        limit: None,
        record: record.clone(),
        stop: verge_probe::Cancel::new(),
        cancel: verge_probe::Cancel::new(),
    });
    let mut ended = None;
    for _ in 0..2000 {
        match worker.try_recv() {
            Some(Event::Batch(BatchEvent::Ended(result))) => {
                ended = Some(result);
                break;
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let made = record.exists();
    std::fs::remove_dir_all(&dir).unwrap();
    let Some(Err(error)) = ended else {
        panic!("{ended:?}");
    };
    assert!(error.contains("carries no Verge version"), "{error}");
    assert!(!made);
}

/// The second review of the fixes: a file dropped while the question is up must not leave a
/// question that names one file's boards and flashes another. The question goes, and the view
/// offers the new file instead.
#[test]
fn the_question_names_the_boards_of_the_file_it_flashes() {
    let mut h = harness(sim_app(SimBus::scenario("isp").unwrap(), vec![]));
    until(&mut h, "INSTALL V0.3.0");
    let dir = record_dir("question-two-files");
    let pro = dir.join("pro.s19");
    let plain = dir.join("plain.s19");
    let ours = SimFirmware::Ours {
        build: "v0.3.0".into(),
    };
    std::fs::write(
        &pro,
        verge_probe::sim::firmware_srecord_declaring(&ours, 1 << 1),
    )
    .unwrap();
    std::fs::write(&plain, firmware_srecord(&ours)).unwrap();
    drop_file(&mut h, &pro);
    until(&mut h, "FLASH PRO.S19");
    press(&mut h, "FLASH PRO.S19");
    h.get_by_label_contains("The image is built for the MCU-Link Pro only. Flash pro.s19");
    drop_file(&mut h, &plain);
    h.run();
    let wrong = h
        .query_by_label_contains("The image is built for the full MCU-Link only. Flash pro.s19")
        .is_some();
    let confirm = h.query_by_label("CONFIRM").is_some();
    let offered = h.query_by_label("FLASH PLAIN.S19").is_some();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        !wrong,
        "the question says the full MCU-Link only, and Confirm flashes pro.s19, built for the Pro only"
    );
    assert!(!confirm && offered, "confirm {confirm}, offered {offered}");
}

/// A unit that needs the jumper, with a release chosen: nothing has checked the release's boards,
/// so the advice does not vouch for it.
#[test]
fn the_jumper_advice_does_not_vouch_for_a_release() {
    let mut h = harness(sim_app(SimBus::scenario("stock-old").unwrap(), vec![]));
    until(&mut h, "NEEDS THE JUMPER");
    h.get_by_label_contains(
        "This unit is a full MCU-Link. In ISP mode the tool cannot tell its board, and a release says which boards it is for only once it is downloaded: flash it then with release v0.3.0 only if it is built for a full MCU-Link.",
    );
    assert!(h.query_by_label_contains("which is built for it").is_none());
}

/// A window whose worker keeps releases in `cache`, as the real one does, on the bench
/// `scenario`, with the simulated GitHub it downloads from.
fn cached_app(scenario: &str, cache: &Cache) -> (VergeApp, SimHttp) {
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let github = GitHub::new(http.clone(), repo(), None)
        .with_key(Some(http.public_key()))
        .with_cache(Some(cache.clone()));
    let worker = Worker::spawn(SimBus::scenario(scenario).unwrap(), github, || {});
    let app = VergeApp::new(
        Box::new(worker),
        Log::default(),
        Config {
            repo: repo(),
            simulated: Some(scenario.into()),
            stock_dirs: vec![],
            record_dir: std::env::temp_dir(),
            cache: Some(cache.clone()),
        },
    );
    (app, http)
}

/// Puts `http`'s latest release in `cache`, as an earlier run would have.
fn keep(http: &SimHttp, cache: &Cache) -> verge_probe::release::Release {
    let github = GitHub::new(http.clone(), repo(), None)
        .with_key(Some(http.public_key()))
        .with_cache(Some(cache.clone()));
    let release = github.latest().unwrap();
    github
        .download(
            &release,
            Download::Agreed,
            &mut verge_probe::progress::Quiet,
        )
        .unwrap();
    release
}

fn downloads(http: &SimHttp) -> usize {
    http.requests()
        .iter()
        .filter(|url| url.contains("/download/"))
        .count()
}

/// The first install downloads the release after a question that says so; the next takes it
/// from the cache, and its question, and the batch's, say nothing of a download.
#[test]
fn a_release_in_the_cache_is_not_asked_about_again() {
    let cache = Cache::at(record_dir("cache-kept"));
    let (app, http) = cached_app("bench", &cache);
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41")
        .click();
    h.step();
    h.step();
    press(&mut h, "INSTALL V0.3.0");
    h.get_by_label_contains(DOWNLOAD);
    press(&mut h, "CONFIRM");
    until(&mut h, "DONE");
    press(&mut h, "BACK");
    assert_eq!(downloads(&http), 3);

    h.get_by_role_and_label(Role::ListBoxOption, "CJ3E7A91C2")
        .click();
    h.step();
    h.step();
    press(&mut h, "UPDATE TO V0.3.0");
    h.get_by_label_contains("Flash release v0.3.0 onto CJ3E7A91C2 at");
    assert!(h.query_by_label_contains("Download release").is_none());
    press(&mut h, "CONFIRM");
    until(&mut h, "DONE");
    press(&mut h, "BACK");
    assert_eq!(downloads(&http), 3);

    press(&mut h, "BATCH");
    press(&mut h, "ARM BATCH");
    h.get_by_label_contains("Flash release v0.3.0 onto every full MCU-Link");
    assert!(h.query_by_label_contains("Download release").is_none());
    std::fs::remove_dir_all(cache.root()).unwrap();
}

/// A cached copy that fails its check ends the job, and is deleted, so the next install asks to
/// download the release afresh.
#[test]
fn a_cached_copy_that_fails_is_downloaded_again_only_when_asked() {
    let cache = Cache::at(record_dir("cache-spoilt"));
    let (app, http) = cached_app("stock", &cache);
    let release = keep(&http, &cache);
    let image = cache.path(&repo(), "v0.3.0", &release.image.name);
    let mut bytes = std::fs::read(&image).unwrap();
    let at = bytes.len() / 2
        + bytes[bytes.len() / 2..]
            .iter()
            .position(u8::is_ascii_hexdigit)
            .unwrap();
    bytes[at] = if bytes[at] == b'0' { b'1' } else { b'0' };
    std::fs::write(&image, bytes).unwrap();

    let mut h = harness(app);
    until(&mut h, "INSTALL V0.3.0");
    press(&mut h, "INSTALL V0.3.0");
    h.get_by_label_contains("Flash release v0.3.0 onto QK7RM2VTX8D41 at 1-1?");
    press(&mut h, "CONFIRM");
    until(&mut h, "FAILED");
    h.get_by_label_contains("the copy of release v0.3.0 in the cache failed its check");
    h.get_by_label_contains("Try again: the question then asks to download the release.");
    assert_eq!(downloads(&http), 3);
    press(&mut h, "BACK");

    until(&mut h, "INSTALL V0.3.0");
    press(&mut h, "INSTALL V0.3.0");
    h.get_by_label_contains(DOWNLOAD);
    press(&mut h, "CONFIRM");
    until(&mut h, "DONE");
    assert_eq!(downloads(&http), 6);
    std::fs::remove_dir_all(cache.root()).unwrap();
}

/// The question said nothing of a download, so the job downloads nothing, even where the release
/// has left the cache since.
#[test]
fn a_release_gone_from_the_cache_since_the_question_is_not_downloaded() {
    let cache = Cache::at(record_dir("cache-gone"));
    let (app, http) = cached_app("stock", &cache);
    keep(&http, &cache);
    let mut h = harness(app);
    until(&mut h, "INSTALL V0.3.0");
    press(&mut h, "INSTALL V0.3.0");
    h.get_by_label_contains("Flash release v0.3.0 onto QK7RM2VTX8D41 at 1-1?");
    std::fs::remove_dir_all(cache.root()).unwrap();
    h.run_steps(2);
    h.get_by_label_contains("Flash release v0.3.0 onto QK7RM2VTX8D41 at 1-1?");
    press(&mut h, "CONFIRM");
    until(&mut h, "FAILED");
    h.get_by_label_contains("release v0.3.0 would have to be downloaded, and that was not asked");
    assert_eq!(downloads(&http), 3);
}

/// The arming question's download is fixed when it opens, so picking another image closes it
/// rather than leave it naming a download for an image no longer chosen.
#[test]
fn another_image_closes_the_arming_question() {
    let dir = record_dir("arming-stock");
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
    press(&mut h, "BATCH");
    // Stock carries no version of ours to compare, so the batch takes ours of any build.
    press(&mut h, "of any build, not only those older than the image");
    until(&mut h, "ARM BATCH");
    press(&mut h, "ARM BATCH");
    h.get_by_label_contains(DOWNLOAD);
    press(&mut h, "STOCK");
    assert!(h.query_by_label_contains("Download release").is_none());
    assert!(h.query_by_label("CONFIRM").is_none());
    press(&mut h, "ARM BATCH");
    h.get_by_label_contains("Flash stock V3.172 onto every full MCU-Link");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Serves `before` until `switched` is set, then `after`: a release published again under its
/// tag while the window is open.
#[derive(Clone)]
struct Republished {
    before: SimHttp,
    after: SimHttp,
    switched: Arc<std::sync::atomic::AtomicBool>,
}

impl verge_probe::release::Http for Republished {
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<Vec<u8>, verge_probe::release::HttpError> {
        if self.switched.load(std::sync::atomic::Ordering::SeqCst) {
            self.after.get(url, headers)
        } else {
            self.before.get(url, headers)
        }
    }
}

/// A window on the `stock` bench whose cache keeps v0.3.0, and whose GitHub serves v0.3.0 again,
/// signed by the same key for the same name and tag but of another size, once the flag is set.
fn republished_app(name: &str) -> (VergeApp, Arc<std::sync::atomic::AtomicBool>, Cache) {
    use verge_probe::sim::firmware_image;
    use verge_probe::sim::http::{SimAsset, SimRelease};

    let cache = Cache::at(record_dir(name));
    let _ = std::fs::remove_dir_all(cache.root());
    let before = SimHttp::new(repo());
    before.publish_ours("v0.3.0");
    keep(&before, &cache);
    let image = firmware_image(&SimFirmware::Ours {
        build: "v0.3.0".into(),
    })
    .to_srecord("simulated, built again");
    let sha = verge_probe::image::Image::parse(image.as_bytes())
        .unwrap()
        .sha256_hex();
    let file = "mculink-verge-v0.3.0.s19";
    let after = SimHttp::new(repo());
    after.publish(SimRelease {
        tag: "v0.3.0".into(),
        prerelease: false,
        assets: vec![
            SimAsset::new(file, image.clone().into_bytes()),
            SimAsset::new(
                &format!("{file}.sha256"),
                format!("{sha}  {file}\n").into_bytes(),
            ),
            before.key().sign(
                &format!("{file}.minisig"),
                image.as_bytes(),
                &format!("file:{file} tag:v0.3.0"),
            ),
        ],
    });
    let switched = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let http = Republished {
        before: before.clone(),
        after,
        switched: switched.clone(),
    };
    let github = GitHub::new(http, repo(), None)
        .with_key(Some(before.public_key()))
        .with_cache(Some(cache.clone()));
    let worker = Worker::spawn(SimBus::scenario("stock").unwrap(), github, || {});
    let app = VergeApp::new(
        Box::new(worker),
        Log::default(),
        Config {
            repo: repo(),
            simulated: Some("stock".into()),
            stock_dirs: vec![],
            record_dir: cache.root().to_path_buf(),
            cache: Some(cache.clone()),
        },
    );
    (app, switched, cache)
}

/// The window's question judges the cache by the release the window looked up, and the worker by
/// a lookup of its own. Where the release was published again since, the job ends asking to try
/// again, and the worker looks the release up again for the window, so the next question names
/// the download. (The F10/F11 review's proof.)
#[test]
fn a_release_published_again_is_asked_about_after_try_again() {
    let (app, switched, cache) = republished_app("republished");
    let mut h = harness(app);
    until(&mut h, "INSTALL V0.3.0");
    switched.store(true, std::sync::atomic::Ordering::SeqCst);
    press(&mut h, "INSTALL V0.3.0");
    h.get_by_label_contains("Flash release v0.3.0 onto QK7RM2VTX8D41 at 1-1?");
    press(&mut h, "CONFIRM");
    until(&mut h, "FAILED");
    h.get_by_label_contains("would have to be downloaded, and that was not asked");
    h.get_by_label_contains("Try again: the question then asks to download the release.");
    press(&mut h, "BACK");
    until(&mut h, "INSTALL V0.3.0");
    press(&mut h, "INSTALL V0.3.0");
    h.get_by_label_contains(DOWNLOAD);
    press(&mut h, "CONFIRM");
    until(&mut h, "DONE");
    std::fs::remove_dir_all(cache.root()).unwrap();
}

/// The same for a batch: the arming question that named no download arms nothing new, and the
/// next one names the download.
#[test]
fn a_batch_of_a_release_published_again_is_asked_about_after_try_again() {
    let (app, switched, cache) = republished_app("republished-batch");
    let mut h = harness(app);
    until(&mut h, "INSTALL V0.3.0");
    switched.store(true, std::sync::atomic::Ordering::SeqCst);
    press(&mut h, "BATCH");
    press(&mut h, "ARM BATCH");
    h.get_by_label_contains("Flash release v0.3.0 onto every full MCU-Link");
    press(&mut h, "CONFIRM");
    until(&mut h, "NOT ARMED");
    h.get_by_label_contains("would have to be downloaded, and that was not asked");
    h.get_by_label_contains("Try again: the question then asks to download the release.");
    press(&mut h, "BACK");
    until(&mut h, "ARM BATCH");
    press(&mut h, "ARM BATCH");
    h.get_by_label_contains(DOWNLOAD);
    std::fs::remove_dir_all(cache.root()).unwrap();
}

/// The batch view's question, like the unit view's, fixes its download when it opens: a release
/// that left the cache since is not downloaded, and the advice says what to do.
#[test]
fn a_batch_release_gone_from_the_cache_since_its_question_is_not_downloaded() {
    let cache = Cache::at(record_dir("cache-gone-batch"));
    let (app, http) = cached_app("arrivals", &cache);
    keep(&http, &cache);
    let mut h = harness(app);
    until(&mut h, "BATCH");
    press(&mut h, "BATCH");
    until(&mut h, "ARM BATCH");
    press(&mut h, "ARM BATCH");
    h.get_by_label_contains("Flash release v0.3.0 onto every full MCU-Link");
    std::fs::remove_dir_all(cache.root()).unwrap();
    h.run_steps(2);
    h.get_by_label_contains("Flash release v0.3.0 onto every full MCU-Link");
    assert!(h.query_by_label_contains("Download release").is_none());
    press(&mut h, "CONFIRM");
    until(&mut h, "NOT ARMED");
    h.get_by_label_contains("would have to be downloaded, and that was not asked");
    h.get_by_label_contains("Try again: the question then asks to download the release.");
    assert_eq!(downloads(&http), 3);
}

/// A kept copy the window cannot read, though it is there at its size, is not taken as kept: the
/// question names the download. Where the permissions do not bind, as for root, a directory in
/// its place stands in, which has another size too.
#[test]
fn a_kept_copy_that_cannot_be_read_is_asked_about() {
    let cache = Cache::at(record_dir("cache-unreadable"));
    let (app, http) = cached_app("stock", &cache);
    let release = keep(&http, &cache);
    let image = cache.path(&repo(), "v0.3.0", &release.image.name);
    unreadable(&image);
    let mut h = harness(app);
    until(&mut h, "INSTALL V0.3.0");
    press(&mut h, "INSTALL V0.3.0");
    h.get_by_label_contains(DOWNLOAD);
    std::fs::remove_dir_all(cache.root()).unwrap();
}

/// Makes the file at `path` unreadable to this user, keeping its size where the permissions bind.
fn unreadable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o0)).unwrap();
        if std::fs::read(path).is_err() {
            return;
        }
    }
    std::fs::remove_file(path).unwrap();
    std::fs::create_dir(path).unwrap();
}

/// A failed copy the tool could not delete, as in a cache another user wrote, would be read
/// again by every job, so the advice is to delete it rather than to try again. Needs a user the
/// permissions bind, so it passes without checking anything as root.
#[cfg(unix)]
#[test]
fn a_failed_copy_that_cannot_be_deleted_is_left_to_the_user() {
    use std::os::unix::fs::PermissionsExt;
    let chmod = |path: &std::path::Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let cache = Cache::at(record_dir("cache-undeletable"));
    let (app, http) = cached_app("stock", &cache);
    let release = keep(&http, &cache);
    let image = cache.path(&repo(), "v0.3.0", &release.image.name);
    let mut bytes = std::fs::read(&image).unwrap();
    let at = bytes.len() / 2
        + bytes[bytes.len() / 2..]
            .iter()
            .position(u8::is_ascii_hexdigit)
            .unwrap();
    bytes[at] = if bytes[at] == b'0' { b'1' } else { b'0' };
    std::fs::write(&image, bytes).unwrap();
    let dir = cache.release_dir(&repo(), "v0.3.0");
    chmod(&dir, 0o555);
    if std::fs::write(dir.join("probe"), b"").is_ok() {
        eprintln!("the permissions do not bind this user; nothing checked");
        chmod(&dir, 0o755);
        std::fs::remove_dir_all(cache.root()).unwrap();
        return;
    }
    let mut h = harness(app);
    until(&mut h, "INSTALL V0.3.0");
    press(&mut h, "INSTALL V0.3.0");
    press(&mut h, "CONFIRM");
    until(&mut h, "FAILED");
    chmod(&dir, 0o755);
    h.get_by_label_contains("failed its check, and could not be deleted");
    h.get_by_label_contains("Delete that copy, then try again.");
    assert!(
        h.query_by_label_contains("Try again: the question")
            .is_none()
    );
    std::fs::remove_dir_all(cache.root()).unwrap();
}

#[test]
fn bench_supply_controls_use_aux_and_confirm_during_capture() {
    let mut h = harness(sim_app(
        SimBus::scenario("ours-pro-energy").unwrap(),
        vec![],
    ));
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BENCH");
    until(&mut h, "RECORD RUNNING CAPTURE");
    until(&mut h, "DIO0–2 OUTPUT");
    // A capture started before the GUI connected still requires confirmation.
    if h.query_by_label("TURN ON").is_some() {
        press(&mut h, "TURN ON");
        until(&mut h, "TURN OFF");
    }
    press(&mut h, "TURN OFF");
    until(&mut h, "Turn off target supply?");
    press(&mut h, "KEEP SUPPLY ON");
    for _ in 0..10 {
        h.step();
    }
    assert!(h.query_by_label("Turn off target supply?").is_none());
    assert!(h.query_by_label("TURN OFF").is_some());
    press(&mut h, "TURN OFF");
    until(&mut h, "TURN OFF SUPPLY");
    press(&mut h, "TURN OFF SUPPLY");
    until(&mut h, "TURN ON");
    press(&mut h, "TURN ON");
    until(&mut h, "TURN OFF");
}

#[test]
fn bench_dio_explains_each_unavailable_state() {
    for (scenario, reason) in [
        ("ours-pro-old-aux", "predates DIO on Verge Aux"),
        ("ours-pro-j15", "remove J15"),
        (
            "ours-pro-aux-taken",
            "Close the program holding it and reconnect.",
        ),
        ("ours-plain", "not an MCU-Link Pro"),
    ] {
        let mut h = harness(sim_app(SimBus::scenario(scenario).unwrap(), vec![]));
        until(&mut h, "UPDATE TO V0.3.0");
        press(&mut h, "BENCH");
        for _ in 0..400 {
            h.step();
            if h.query_all_by_label_contains(reason).next().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            h.query_all_by_label_contains(reason).next().is_some(),
            "{scenario}"
        );
        assert!(h.query_by_label("DIO0–2 OUTPUT").is_none());
    }
}

#[test]
fn bench_dio_asks_before_driving_and_keeps_inputs_on_cancel() {
    let bus = SimBus::scenario("ours-pro-held").unwrap();
    let mut h = harness(sim_app(bus.clone(), vec![]));
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BENCH");
    until(&mut h, "DIO0–2 OUTPUT");
    h.get_by_label("HIGH");
    press(&mut h, "DIO0–2 OUTPUT");
    until(&mut h, "Drive DIO0–2?");
    assert!(!bus.log().iter().any(|s| s.contains("AUX 8e0707")));
    press(&mut h, "KEEP AS INPUTS");
    for _ in 0..10 {
        h.step();
    }
    assert!(h.query_by_label("Drive DIO0–2?").is_none());
    assert!(!bus.log().iter().any(|s| s.contains("AUX 8e0707")));
    press(&mut h, "DIO0–2 OUTPUT");
    until(&mut h, "DRIVE PINS");
    press(&mut h, "DRIVE PINS");
    for _ in 0..400 {
        h.step();
        if h.query_all_by_label("SET HIGH").count() == 3 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(h.query_all_by_label("SET HIGH").count(), 3);
    assert!(bus.log().iter().any(|s| s.contains("AUX 8e0707")));
    press(&mut h, "DIO0–2 INPUT");
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(h.query_by_label("Drive DIO0–2?").is_none());
    assert_eq!(h.query_all_by_label("SET HIGH").count(), 0);
}

#[test]
fn bench_refuses_older_firmware_and_reaches_a_debugger_held_pro() {
    let mut h = harness(sim_app(SimBus::scenario("ours").unwrap(), vec![]));
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BENCH");
    until_contains(&mut h, "which recording needs");
    assert!(h.query_by_label("START RECORDING").is_none());
    let mut h = harness(sim_app(
        SimBus::scenario("ours-pro-debugger").unwrap(),
        vec![],
    ));
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    press(&mut h, "BENCH");
    until(&mut h, "START RECORDING");
}

#[test]
fn a_worker_failure_during_recording_unlocks_the_window() {
    use crate::bench_worker::Status;
    use crate::worker::JobError;
    let (app, backend) = manual_app("ours-pro", true);
    let instance = verge_probe::detect(&SimBus::scenario("ours-pro").unwrap()).unwrap()[0]
        .device
        .instance
        .clone();
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BENCH");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Bench(Status {
            instance,
            connected: true,
            recording: true,
            ..Status::default()
        }));
    h.step();
    assert!(h.state().busy());
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::WorkerFailed(JobError {
            message: "USB worker failed".into(),
            advice: None,
            left_in_isp: false,
        }));
    h.step();
    assert!(!h.state().busy());
    press(&mut h, "UNITS");
    h.step();
}

#[test]
fn a_session_the_worker_dropped_can_be_reconnected() {
    use crate::bench_worker::Status;
    let (app, backend) = manual_app("ours-pro", true);
    let instance = verge_probe::detect(&SimBus::scenario("ours-pro").unwrap()).unwrap()[0]
        .device
        .instance
        .clone();
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BENCH");
    h.step();
    let connects = |backend: &std::sync::Arc<std::sync::Mutex<Manual>>| {
        backend
            .lock()
            .unwrap()
            .sent
            .iter()
            .filter(|sent| sent.starts_with("bench Connect"))
            .count()
    };
    assert_eq!(connects(&backend), 1);
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Bench(Status {
            instance,
            connected: false,
            error: Some("disk full".into()),
            ..Status::default()
        }));
    until(&mut h, "RECONNECT");
    press(&mut h, "RECONNECT");
    h.step();
    assert_eq!(connects(&backend), 2);
}

#[test]
fn a_recording_failure_keeps_a_pending_close_open() {
    use crate::bench_worker::Status;
    let (app, backend) = manual_app("ours-pro", true);
    let instance = verge_probe::detect(&SimBus::scenario("ours-pro").unwrap()).unwrap()[0]
        .device
        .instance
        .clone();
    let mut h = harness(app);
    until(&mut h, "UPDATE TO V0.3.0");
    press(&mut h, "BENCH");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Bench(Status {
            instance: instance.clone(),
            connected: true,
            recording: true,
            ..Status::default()
        }));
    h.step();
    request_close(&mut h);
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Bench(Status {
            instance,
            connected: true,
            recording: false,
            error: Some("disk full".into()),
            ..Status::default()
        }));
    h.step();
    assert!(!root_commands(&h).contains(&ViewportCommand::Close));
    assert!(!h.state().busy());
    h.get_by_label("disk full");
}
