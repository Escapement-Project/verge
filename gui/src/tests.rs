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
use escapement_verge_probe::Unit;
use escapement_verge_probe::cache::Cache;
use escapement_verge_probe::release::{Download, GitHub, Repo};
use escapement_verge_probe::sim::http::SimHttp;
use escapement_verge_probe::sim::{SimBus, SimFirmware, firmware_srecord};
use escapement_verge_probe::update::Outcome;
use escapement_verge_probe::version::StockVersion;

use crate::app::{Config, STOCK, VergeApp};
use crate::log::Log;
use crate::style;
use crate::worker::{BatchEvent, Event, InstallEvent, Manual, ProgressEvent, Shared, Worker};
use escapement_verge_probe::board::Board;

fn repo() -> Repo {
    "someone/mculink-verge".parse().unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn a_record_failure_stops_the_worker_before_the_next_unit() {
    use crate::worker::{Backend, Request};
    use escapement_verge_probe::batch::{Board, Filter, OursFilter, StockFilter};
    use escapement_verge_probe::{Cancel, Source};

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
    H: escapement_verge_probe::release::Http + Clone + Send + 'static,
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

/// The style, with Verge's mark held still: what a test sees must not depend on the frame it looks.
fn still(ctx: &egui::Context) {
    style::install(ctx);
    crate::mark::set_motion(ctx, false);
}

fn harness(app: VergeApp) -> Harness<'static, VergeApp> {
    Harness::builder()
        .with_size(vec2(1080.0, 700.0))
        .with_context_setup(still)
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

/// As [`press`], for the one widget of `role` labelled `label`: the batch view's image choice and
/// the units it takes are both labelled Stock.
fn press_as(h: &mut Harness<'_, VergeApp>, role: Role, label: &str) {
    h.get_by_role_and_label(role, label).scroll_to_me();
    h.run_steps(20);
    h.get_by_role_and_label(role, label).click();
    h.step();
    h.step();
}

#[test]
fn about_gives_the_version_and_the_licences() {
    let mut h = harness(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "About");
    h.get_by_label(&format!("Verge {} (verge-gui)", env!("CARGO_PKG_VERSION")));
    h.get_by_label_contains("THIRD-PARTY-LICENSES.html");
    h.get_by_label_contains("SIL Open Font License 1.1");
    h.get_by_label("Verge's icon");
    h.get_by_label_contains("Creative Commons Attribution 4.0 International Public License");
}

#[test]
fn the_bench_is_listed_with_the_release_to_install() {
    let mut h = harness(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    h.get_by_role_and_label(Role::ListBoxOption, "CJ3E7A91C2");
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41");
    h.get_by_label_contains("runs 14d05d4, which carries no version");
    h.get_by_label("Simulated bench: test");
}

#[test]
fn an_update_is_confirmed_runs_and_reports_the_unit_back() {
    let mut h = harness(sim_app(SimBus::scenario("stock").unwrap(), vec![]));
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    // Nothing runs until the user confirms.
    h.get_by_label_contains(DOWNLOAD);
    h.get_by_label_contains(" KiB), and flash it onto QK7RM2VTX8D41 at 1-1?");
    assert!(!h.state().busy());
    press(&mut h, "Confirm");
    assert!(h.state().busy());
    until(&mut h, "Done");
    h.get_by_label_contains("runs Verge, build v0.3.0");
    assert!(!h.state().busy());
    press(&mut h, "Back");
    // The rescan after the update finds the unit by its port and selects it again.
    until(&mut h, &format!("Verge v0.3.0 · {} · 1-1", Board::McuLink));
}

/// The second of two units, updated, comes back as a new enumeration with a new serial; the
/// window keeps it selected by its port rather than falling back to the first unit.
#[test]
fn the_updated_unit_stays_selected_when_it_returns() {
    let mut h = harness(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41")
        .click();
    h.step();
    h.step();
    press(&mut h, "Install v0.3.0");
    press(&mut h, "Confirm");
    until(&mut h, "Done");
    press(&mut h, "Back");
    until_contains(&mut h, "CJ3E7B02F9 · 1-2 · 1fc9:0143");
}

#[test]
fn confirming_a_unit_already_in_isp_mode_says_it_may_not_be_an_mculink() {
    let mut h = harness(sim_app(SimBus::scenario("isp").unwrap(), vec![]));
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    h.get_by_label_contains(&format!(
        "In ISP mode an MCU-Link looks like any other LPC55S69 board. A release says which boards it is built for only once it is downloaded. {DOWNLOAD}"
    ));
    // A jumper-free update restarts a unit it knows, so its confirmation does not say so.
    let mut h = harness(sim_app(SimBus::scenario("ours").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    h.get_by_label_contains("and flash it onto CJ3E7A91C2");
    assert!(
        h.query_by_label_contains("looks like any other LPC55S69 board")
            .is_none()
    );
}

#[test]
fn daplink_rs_is_named_and_updates_without_the_jumper() {
    let mut h = harness(sim_app(SimBus::scenario("daplink-rs").unwrap(), vec![]));
    until(&mut h, "Install v0.3.0");
    h.get_by_label("daplink-rs");
    h.get_by_label("Updates without the jumper");
    press(&mut h, "Install v0.3.0");
    press(&mut h, "Confirm");
    until(&mut h, "Done");
    h.get_by_label_contains("runs Verge, build v0.3.0");
}

#[test]
fn a_mini_in_isp_mode_leaves_the_other_bootloader_nothing_to_press() {
    let mut h = harness(sim_app(SimBus::scenario("isp-mini").unwrap(), vec![]));
    until(&mut h, "Cannot update");
    h.get_by_label_contains("unplug the mini first");
}

#[test]
fn two_units_in_isp_mode_leave_nothing_to_press() {
    let mut h = harness(sim_app(SimBus::scenario("two-isp").unwrap(), vec![]));
    until(&mut h, "Cannot update");
    assert!(
        h.get_by_label("Cannot update")
            .accesskit_node()
            .is_disabled()
    );
    h.get_by_label_contains("2 probes are in ISP mode");
}

#[test]
fn old_stock_is_sent_to_the_jumper() {
    let mut h = harness(sim_app(SimBus::scenario("stock-old").unwrap(), vec![]));
    until(&mut h, "Needs the jumper");
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
    until(&mut h, "Update to v0.3.0");
    press(&mut h, STOCK);
    press(&mut h, &format!("Install {STOCK} V3.172"));
    press(&mut h, "Confirm");
    until(&mut h, "Done");
    h.get_by_label_contains("runs NXP's firmware V3.172");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_unit_on_the_release_needs_install_anyway() {
    let bus = SimBus::default();
    bus.add_ours("v0.3.0");
    let mut h = harness(sim_app(bus, vec![]));
    until(&mut h, "Nothing to update");
    h.get_by_label_contains("The probe already runs v0.3.0.");
    h.get_by_role(Role::CheckBox).click();
    h.step();
    h.step();
    h.get_by_label("Update to v0.3.0");
}

/// A window whose backend the test drives by hand, showing the one unit of `scenario`.
fn manual_app(scenario: &str, release: bool) -> (VergeApp, Shared) {
    let bus = SimBus::scenario(scenario).unwrap();
    let units: Vec<Unit> = escapement_verge_probe::detect(&bus).unwrap();
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
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    press(&mut h, "Confirm");
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
    until(&mut h, "Update to v0.3.0");
    request_close(&mut h);
    assert!(!root_commands(&h).contains(&ViewportCommand::CancelClose));
}

#[test]
fn cancel_asks_the_job_to_stop_at_a_safe_point() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    press(&mut h, "Confirm");
    let cancel = backend.lock().unwrap().last_cancel.clone().unwrap();
    assert!(!cancel.is_cancelled());
    press(&mut h, "Cancel");
    assert!(cancel.is_cancelled());
    h.get_by_label("Cancelling at the next safe point");
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
    until(&mut h, "No release");
    h.get_by_label_contains("GitHub could not be reached");
    // Picking the stock unit shows its own state; the release stays unavailable to it.
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41")
        .click();
    h.step();
    h.step();
    until(
        &mut h,
        &format!("{STOCK} V3.172 · {} · 1-2", Board::McuLink),
    );
}

#[test]
fn every_colour_reads_on_what_it_is_drawn_on() {
    use crate::app::Fill;
    use crate::style::Status;
    for (face_name, face) in [
        ("day", &crate::tokens::DAY),
        ("night", &crate::tokens::NIGHT),
    ] {
        for (name, fill) in [
            ("Verge's", Fill::lume(face)),
            ("attention's", Fill::status(Status::Attention, face)),
            ("fault's", Fill::status(Status::Fault, face)),
            ("an action's", Fill::ink(face)),
            ("other firmware's", Fill::other(face)),
        ] {
            let ratio = style::contrast(fill.on, fill.ground);
            assert!(
                ratio >= 4.5,
                "{face_name}: words on {name} fill are {ratio:.2}:1"
            );
        }
        let pairs = [
            // Page text on an ink fill: a selected row or choice.
            ("page on ink", face.page, face.ink, 4.5),
            // Text on the grounds: the page, the panels, notices and fields.
            ("ink on page", face.ink, face.page, 7.0),
            ("ink on surface", face.ink, face.surface, 7.0),
            ("ink on raised", face.ink, face.raised, 7.0),
            ("ink on sunken", face.ink, face.sunken, 7.0),
            ("ink_2 on page", face.ink_2, face.page, 4.5),
            ("ink_2 on surface", face.ink_2, face.surface, 4.5),
            ("ink_2 on raised", face.ink_2, face.raised, 4.5),
            // Status words in the panels and the log.
            (
                "attention_ink on surface",
                face.attention_ink,
                face.surface,
                4.5,
            ),
            ("fault_ink on surface", face.fault_ink, face.surface, 4.5),
            // Control outlines and the trace, against the panels and the plot.
            ("rule on surface", face.rule, face.surface, 3.0),
            ("rule on raised", face.rule, face.raised, 3.0),
            (
                "series_verge on surface",
                face.series_verge,
                face.surface,
                3.0,
            ),
        ];
        for (name, fore, back, least) in pairs {
            let ratio = style::contrast(fore, back);
            assert!(
                ratio >= least,
                "{face_name}: {name} is {ratio:.2}:1, under {least}:1"
            );
        }
    }
}

/// Renders the window's states to PNG for a person to look at. Needs a GPU adapter.
///
/// `VERGE_GUI_SHOTS=<dir> cargo test -p escapement-verge-gui pictures -- --ignored`
#[test]
#[ignore = "renders with wgpu into $VERGE_GUI_SHOTS"]
fn pictures() {
    let dir = PathBuf::from(std::env::var("VERGE_GUI_SHOTS").expect("VERGE_GUI_SHOTS"));
    std::fs::create_dir_all(&dir).unwrap();
    // Each state in both faces, night first, as the harness starts.
    let shoot = |h: &mut Harness<'_, VergeApp>, name: &str| {
        for (theme, face) in [(egui::Theme::Dark, "night"), (egui::Theme::Light, "day")] {
            h.ctx.set_theme(theme);
            h.step();
            let image = h.render().expect("wgpu renders the window");
            image.save(dir.join(format!("{name}-{face}.png"))).unwrap();
        }
        h.ctx.set_theme(egui::Theme::Dark);
        h.step();
    };
    let rendered = |app: VergeApp| {
        Harness::builder()
            .with_size(vec2(1080.0, 700.0))
            .with_context_setup(still)
            .wgpu()
            .build_ui_state(|ui, app: &mut VergeApp| app.show(ui), app)
    };

    let mut h = rendered(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    shoot(&mut h, "01-ours-selected");
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41")
        .click();
    h.step();
    shoot(&mut h, "02-stock-selected");
    let mut d = rendered(sim_app(SimBus::scenario("daplink-rs").unwrap(), vec![]));
    until(&mut d, "Install v0.3.0");
    shoot(&mut d, "02b-daplink-rs-selected");
    // The interaction layer: the pointer on one choice, the focus on the chosen one.
    d.get_by_label(STOCK).hover();
    d.get_by_label("Release").focus();
    shoot(&mut d, "02c-hover-and-focus");
    press(&mut d, "About");
    shoot(&mut d, "02d-licences");
    d.get_by_label("Verge's icon").scroll_to_me();
    d.run_steps(20);
    shoot(&mut d, "02e-licences-icon");
    press(&mut h, "Install v0.3.0");
    shoot(&mut h, "03-confirm");

    // Working, held mid-write by a hand-driven backend.
    let (app, backend) = manual_app("ours", true);
    let mut h = rendered(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    press(&mut h, "Confirm");
    {
        let mut manual = backend.lock().unwrap();
        use escapement_verge_probe::progress::Step;
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
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    press(&mut h, "Confirm");
    until(&mut h, "Done");
    shoot(&mut h, "06-done");

    let (app, backend) = manual_app("ours", true);
    let mut h = rendered(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    press(&mut h, "Confirm");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Install(InstallEvent::Finished(Err(
        crate::worker::JobError {
            message: "the bootloader: unexpected status code: 10203 (0x27DB) MemoryCumulativeWrite"
                .into(),
            advice: Some(
                "The probe may be in ISP mode with a partial image. Flashing it again recovers it."
                    .into(),
            ),
            left_in_isp: true,
            stopped: None,
        },
    ))));
    shoot(&mut h, "07-failed");

    // The batch view: set up, running, and done.
    let (app, backend) = manual_app("ours", true);
    let mut h = rendered(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    shoot(&mut h, "12-batch-setup");
    press(&mut h, "Arm batch");
    shoot(&mut h, "13-batch-confirm");
    press(&mut h, "Confirm");
    {
        use escapement_verge_probe::batch::{Record, Tally};
        use escapement_verge_probe::progress::Step;
        let mut manual = backend.lock().unwrap();
        manual.queued.push_back(Event::Batch(BatchEvent::Armed{
            name: "release v0.3.0 (mculink-verge-v0.3.0.s19)".into(),
            filter: "every standalone MCU-Link running NXP's firmware or Verge older than the image, attached from now on".into(),
        }));
        let record = |port: &str,
                      serial: &str,
                      before: &str,
                      after: Option<&str>,
                      result: escapement_verge_probe::batch::Settled,
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
            "NXP's firmware V3.172",
            Some("Verge, build v0.3.0"),
            escapement_verge_probe::batch::Settled::Updated,
            None,
        ));
        manual.queued.push_back(record(
            "1-3",
            "CJ3E7AC4F5",
            "Verge, build v0.3.0",
            None,
            escapement_verge_probe::batch::Settled::Current,
            None,
        ));
        manual.queued.push_back(record("1-4", "QK7RM2VTX8D45", "NXP's firmware V3.108", None, escapement_verge_probe::batch::Settled::Skipped, Some("needs the ISP jumper. NXP's firmware V3.108 has no jumper-free update. NXP added it in V3.122.")));
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
        tally: escapement_verge_probe::batch::Tally { updated: 1, current: 1, skipped: 1, failed: 1, waiting: 1 },
        waiting: vec![("1-5".into(), Some("CJ3E7AE717".into()))],
        paused: Some(format!("The probe at 1-2, {}, failed and is in ISP mode, maybe with a partial image. The batch waits until it is unplugged. To flash it again with the batch's image, stop the batch, then pick the probe in ISP mode under Probes and flash it with release v0.3.0.", Board::McuLink.a())),
    })));
    shoot(&mut h, "15-batch-paused");

    // Nothing attached: the mark above the instruction, at rest as the harness holds it.
    let mut n = rendered(sim_app(SimBus::default(), vec![]));
    until(&mut n, "Plug in an MCU-Link");
    shoot(&mut n, "22-nothing-attached");
    // The mark's sequence at three instants: crossing to the step, on it, crossing back. At 160 and
    // 64 px on the master, at 24 px on the fitted master the strip uses at launch.
    let mut m = Harness::builder()
        .with_size(vec2(600.0, 300.0))
        .with_context_setup(still)
        .wgpu()
        .build_ui_state(
            |ui, icon: &mut style::Mark| {
                let f = style::face(ui.ctx());
                let colours = crate::mark::Colours::of(f, true);
                ui.horizontal(|ui| {
                    use crate::mark::{BACK, GROW, MOVE, SWEEP};
                    for t in [
                        GROW + SWEEP / 2.0,
                        (SWEEP + MOVE + BACK) / 2.0,
                        BACK + SWEEP / 2.0,
                    ] {
                        ui.vertical(|ui| {
                            for (size, geometry) in [
                                (160.0, &crate::mark::MASTER),
                                (64.0, &crate::mark::MASTER),
                                (24.0, &crate::mark::FITTED_24),
                            ] {
                                let (rect, _) =
                                    ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
                                crate::mark::paint(
                                    ui.painter(),
                                    rect,
                                    geometry,
                                    &crate::mark::at(t),
                                    &colours,
                                );
                            }
                        });
                    }
                    // The strip's launch play as it draws it, on the icon's pixels: crossing to
                    // the step, crossing back, its last frame, and then the icon, which must look
                    // the same as the last frame.
                    ui.vertical(|ui| {
                        let bare = crate::mark::Colours::of(f, false);
                        let decoded = eframe::icon_data::from_png_bytes(include_bytes!(
                            "../icons/verge-24.png"
                        ))
                        .unwrap();
                        let pixels = crate::mark::Icon::from_rgba(
                            24,
                            &decoded.rgba,
                            crate::tokens::DAY.plate,
                            crate::tokens::DAY.lume,
                        );
                        for frame in [
                            crate::mark::at(crate::mark::GROW + crate::mark::SWEEP / 2.0),
                            crate::mark::at(crate::mark::BACK + crate::mark::SWEEP / 2.0),
                            crate::mark::Frame::REST,
                        ] {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(24.0, 24.0), egui::Sense::hover());
                            crate::mark::paint_exact(
                                ui.painter(),
                                rect,
                                &crate::mark::FITTED_24,
                                &frame,
                                &bare,
                                1.0,
                                &pixels,
                            );
                        }
                        icon.show(ui);
                    });
                });
            },
            style::Mark::default(),
        );
    for (theme, face) in [(egui::Theme::Dark, "night"), (egui::Theme::Light, "day")] {
        m.ctx.set_theme(theme);
        m.step();
        m.render()
            .expect("wgpu renders the mark")
            .save(dir.join(format!("23-mark-sequence-{face}.png")))
            .unwrap();
    }

    for (scenario, name) in [
        ("stock-old", "08-old-stock-jumper"),
        ("two-isp", "09-two-in-isp"),
        ("busy", "10-busy"),
        ("ours-pro-debugger", "10b-held-by-a-debugger"),
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
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "Record running capture");
    shoot(&mut h, "16-bench-connected");
    press(&mut h, "DIO0–2 output");
    until(&mut h, "Drive DIO0–2");
    shoot(&mut h, "18-bench-drive-confirmation");
    press(&mut h, "Drive DIO0–2");
    for _ in 0..40 {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    shoot(&mut h, "19-bench-outputs");
    press(&mut h, "DIO0–2 input");
    press(&mut h, "Record running capture");
    until(&mut h, "Stop recording");
    for _ in 0..200 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    shoot(&mut h, "17-bench-recording");
    press(&mut h, "Stop recording");
    until(&mut h, "Record running capture");

    // A Pro with no capture running offers the current ranges for the one it would start.
    let http = SimHttp::new(repo());
    let worker = Worker::spawn(
        SimBus::scenario("ours-pro").unwrap().paced(1.0),
        GitHub::new(http, repo(), None),
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
    until(&mut h, "Bench");
    press(&mut h, "Bench");
    until(&mut h, "Start recording");
    press(&mut h, "High range");
    for _ in 0..5 {
        h.step();
    }
    shoot(&mut h, "20-bench-high-range");
    press(&mut h, "Turn on");
    until(&mut h, "Turn off");
    press(&mut h, "Start recording");
    until_contains(&mut h, "Starting clears the probe's energy total.");
    shoot(&mut h, "20b-bench-start-question");
    press(&mut h, "Start recording");
    until(&mut h, "Stop recording");
    for _ in 0..200 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    shoot(&mut h, "21-bench-high-range-held");
    until_contains(&mut h, "This capture held the high range");
    press(&mut h, "Stop recording");
    until(&mut h, "Start recording");

    // A standalone MCU-Link gives the bench its DIO pins alone.
    let worker = Worker::spawn(
        SimBus::scenario("ours-plain").unwrap(),
        GitHub::new(SimHttp::new(repo()), repo(), None),
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
    until(&mut h, "Bench");
    press(&mut h, "Bench");
    until(&mut h, "DIO3 output");
    shoot(&mut h, "24-bench-standalone");
    press(&mut h, "DIO1 output");
    until(&mut h, "Drive DIO1");
    shoot(&mut h, "25-bench-standalone-drive-confirmation");
    press(&mut h, "Drive DIO1");
    until(&mut h, "Set high");
    shoot(&mut h, "26-bench-standalone-output");

    // A Pro without its energy circuit gives the bench its target supply and DIO pins.
    let worker = Worker::spawn(
        SimBus::scenario("ours-pro-unfitted").unwrap(),
        GitHub::new(SimHttp::new(repo()), repo(), None),
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
    until(&mut h, "Bench");
    press(&mut h, "Bench");
    until(&mut h, "DIO0–2 output");
    until(&mut h, "Turn on");
    shoot(&mut h, "27-bench-pro-without-energy");
    // Its pins recorded alone, as `verge dio record` does.
    press(&mut h, "Start recording");
    until(&mut h, "Stop recording");
    for _ in 0..60 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    shoot(&mut h, "27b-bench-pro-without-energy-recording");
    press(&mut h, "Stop recording");
    until_contains(&mut h, "DIO3 edges");
    h.run_steps(20);
    shoot(&mut h, "27c-bench-dio-recording-opened");

    pictures_of_the_fix_round(&dir, &shoot);
}

/// A window at `size` on the bench `app` drives, rendered.
fn rendered_at(app: VergeApp, size: egui::Vec2) -> Harness<'static, VergeApp> {
    Harness::builder()
        .with_size(size)
        .with_context_setup(still)
        .wgpu()
        .build_ui_state(|ui, app: &mut VergeApp| app.show(ui), app)
}

/// A window on `bus` recording into `record_dir`, as a release build draws it where `release`:
/// no simulated badge, and the project's own repository.
fn window_on(bus: SimBus, record_dir: PathBuf, release: bool) -> VergeApp {
    window_logging(bus, record_dir, release, Log::default())
}

/// [`window_on`], logging to `log`.
fn window_logging(bus: SimBus, record_dir: PathBuf, release: bool, log: Log) -> VergeApp {
    let repo = if release {
        escapement_verge_probe::release::DEFAULT_REPO
            .parse()
            .unwrap()
    } else {
        repo()
    };
    let http = SimHttp::new(repo.clone());
    http.publish_ours("v0.3.0");
    let key = http.public_key();
    let worker = Worker::spawn(
        bus,
        GitHub::new(http, repo.clone(), None).with_key(Some(key)),
        || {},
    );
    VergeApp::new(
        Box::new(worker),
        log,
        Config {
            repo,
            simulated: (!release).then(|| "test".into()),
            stock_dirs: vec![],
            record_dir,
            cache: None,
        },
    )
}

/// The states the interface fix round of 2026-11-18 changed, each in both faces.
fn pictures_of_the_fix_round(
    dir: &std::path::Path,
    shoot: &dyn Fn(&mut Harness<'_, VergeApp>, &str),
) {
    let steps = |h: &mut Harness<'_, VergeApp>, n: usize| {
        for _ in 0..n {
            h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let work = |name: &str| {
        let at = dir.join("work").join(name);
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(&at).unwrap();
        at
    };
    let size = vec2(1080.0, 700.0);
    let small = vec2(860.0, 640.0);

    // The strip as a release build draws it, at the smallest size, the default and the largest.
    for (at, name) in [
        (small, "40a-units-860x640"),
        (size, "40b-units-1080x700"),
        (vec2(2560.0, 1440.0), "40c-units-2560x1440"),
    ] {
        let mut h = rendered_at(
            window_on(
                SimBus::scenario("ours-pro-and-plain").unwrap(),
                work("units"),
                true,
            ),
            at,
        );
        until(&mut h, "Update to v0.3.0");
        shoot(&mut h, name);
    }
    // The hover on a unit row, and the log open with its times.
    let log = Log::default();
    log.push(
        tracing::Level::INFO,
        "installing release v0.3.0 (mculink-verge-v0.3.0.s19)".into(),
    );
    log.push(
        tracing::Level::WARN,
        "Paused. An MCU-Link mini is in ISP mode at 1-2. The batch waits until it is unplugged."
            .into(),
    );
    log.push(
        tracing::Level::ERROR,
        "the bootloader: unexpected status code: 10203 (0x27DB) MemoryCumulativeWrite".into(),
    );
    let mut h = rendered_at(
        window_logging(SimBus::scenario("bench").unwrap(), work("hover"), true, log),
        size,
    );
    until(&mut h, "Update to v0.3.0");
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41")
        .hover();
    steps(&mut h, 60);
    h.render()
        .unwrap()
        .save(dir.join("40d-unit-row-hover-night.png"))
        .unwrap();
    press(&mut h, "Show");
    shoot(&mut h, "40e-log-open");

    // A unit the system will not let the user open.
    let mut h = rendered_at(
        window_on(SimBus::scenario("denied").unwrap(), work("denied"), false),
        size,
    );
    until_contains(&mut h, "Your user may not open it");
    shoot(&mut h, "41-unit-the-user-may-not-open");
    // NXP's firmware too old to ask.
    let mut h = rendered_at(
        window_on(SimBus::scenario("stock-hid").unwrap(), work("hid"), false),
        size,
    );
    until_contains(&mut h, "Too old to ask");
    shoot(&mut h, "42-nxps-firmware-too-old-to-ask");
    // A unit in ISP mode, with the reset that leaves it; and the release picked by its tag.
    let mut h = rendered_at(
        window_on(SimBus::scenario("isp-ours").unwrap(), work("isp"), false),
        size,
    );
    until(&mut h, "Reset out of ISP mode");
    h.get_by_label(RESET_LABEL).scroll_to_me();
    steps(&mut h, 5);
    shoot(&mut h, "43a-isp-mode-reset-offered");
    h.get_by_label("Tag").click();
    h.step();
    h.get_by_role(Role::TextInput).type_text("v0.2.0");
    h.step();
    press(&mut h, "Look up");
    steps(&mut h, 40);
    shoot(&mut h, "43b-release-tag-not-found");

    // The question takes the focus, and the view scrolls to it.
    let (app, backend) = manual_app("ours", true);
    let mut h = rendered_at(app, size);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    steps(&mut h, 3);
    shoot(&mut h, "44a-batch-question-in-sight");
    press(&mut h, "Back");
    press_as(&mut h, Role::CheckBox, STOCK);
    press_as(&mut h, Role::CheckBox, "Verge");
    shoot(&mut h, "44b-batch-cannot-arm-under-its-field");
    press_as(&mut h, Role::CheckBox, STOCK);
    press_as(&mut h, Role::CheckBox, "Verge");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
    {
        let mut m = backend.lock().unwrap();
        m.queued.push_back(Event::Batch(BatchEvent::Armed {
            name: "release v0.3.0 (mculink-verge-v0.3.0.s19)".into(),
            filter: format!(
                "every {} running NXP's firmware or Verge older than the image, attached from now on",
                Board::McuLink
            ),
        }));
        for k in 0..12 {
            m.queued.push_back(Event::Batch(BatchEvent::Record(Box::new(
                escapement_verge_probe::batch::Record {
                    time: "2026-10-20T14:26:08Z".into(),
                    port: format!("1-{}", k + 1),
                    serial_before: Some(format!("QK7RM2VTX8D{k:02}")),
                    firmware_before: "NXP's firmware V3.172".into(),
                    serial_after: Some(format!("CJ3E7A91{k:02}")),
                    firmware_after: Some("Verge, build v0.3.0".into()),
                    result: escapement_verge_probe::batch::Settled::Updated,
                    reason: None,
                    seconds: Some(4.7),
                },
            ))));
        }
    }
    steps(&mut h, 5);
    shoot(&mut h, "44c-batch-running-stop-in-the-slab");
    press(&mut h, "Stop");
    shoot(&mut h, "44d-batch-stopping-no-unit-under-way");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Batch(BatchEvent::Ended(Ok(
            escapement_verge_probe::batch::Tally {
                updated: 12,
                ..Default::default()
            },
        ))));
    steps(&mut h, 3);
    shoot(&mut h, "44e-batch-ended");
    let (app, backend) = manual_app("ours", true);
    let mut h = rendered_at(app, size);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Batch(BatchEvent::Ended(Err(
        "making the record file's directory /nonexistent/verge: Permission denied (os error 13)"
            .into(),
    ))));
    steps(&mut h, 3);
    shoot(&mut h, "44f-batch-not-armed");

    // The USB worker stopped, on each view; a failed look at USB.
    let (app, backend) = manual_app("ours-pro", true);
    let mut h = rendered_at(app, size);
    until(&mut h, "Update to v0.3.0");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::WorkerFailed(crate::worker::JobError {
            message: "the USB worker stopped: no message".into(),
            advice: Some(
                "Quit and start Verge again. If a flash was under way, flash the probe again."
                    .into(),
            ),
            left_in_isp: true,
            stopped: None,
        }));
    steps(&mut h, 3);
    shoot(&mut h, "45a-worker-stopped-units");
    press(&mut h, "Batch");
    shoot(&mut h, "45b-worker-stopped-batch");
    press(&mut h, "Bench");
    steps(&mut h, 3);
    shoot(&mut h, "45c-worker-stopped-bench");
    let (app, backend) = manual_app("ours", true);
    let mut h = rendered_at(app, size);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Rescan");
    backend.lock().unwrap().queued.push_back(Event::Units(Err(
        "could not list USB devices: libusb: access denied (insufficient permissions)".into(),
    )));
    steps(&mut h, 3);
    shoot(&mut h, "45d-scan-failed");

    // Results: a real refused write, a refusal before USB, the jumper still fitted, a dropped
    // file during the update.
    let mut h = rendered_at(
        window_on(
            SimBus::scenario("fails-writes").unwrap(),
            work("fails"),
            false,
        ),
        size,
    );
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    press(&mut h, "Confirm");
    until(&mut h, "Failed");
    shoot(&mut h, "46a-failed-write");
    let mut h = rendered_at(
        window_on(SimBus::scenario("pro").unwrap(), work("pro"), false),
        size,
    );
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    press(&mut h, "Confirm");
    until(&mut h, "Failed");
    shoot(&mut h, "46b-failed-nothing-flashed");
    let (app, backend) = manual_app("ours", true);
    let mut h = rendered_at(app, size);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    press(&mut h, "Confirm");
    {
        use escapement_verge_probe::progress::Step;
        let mut m = backend.lock().unwrap();
        m.queued.push_back(Event::Progress(ProgressEvent::Start {
            step: Step::Write,
            total: 235_520,
        }));
        m.queued
            .push_back(Event::Progress(ProgressEvent::Advance(98_304)));
    }
    steps(&mut h, 3);
    drop_file(&mut h, &dir.join("dropped.s19"));
    shoot(&mut h, "46c-dropped-during-update");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Install(InstallEvent::Finished(Ok(
            Outcome::JumperStillFitted,
        ))));
    steps(&mut h, 3);
    shoot(&mut h, "46d-flashed-jumper-still-fitted");

    // Bench: refusals, the old build, the worker's questions inline, the restart, marked pins.
    let bench = |scenario: &str, wait: &str| {
        let bus = SimBus::scenario(scenario).unwrap().paced(1.0);
        let mut h = rendered_at(window_on(bus, work(scenario), false), size);
        steps(&mut h, 40);
        press(&mut h, "Bench");
        until_contains(&mut h, wait);
        steps(&mut h, 20);
        h
    };
    let mut h = bench("stock", "not Verge");
    shoot(&mut h, "47a-bench-not-running-verge");
    let mut h = bench("isp", "ISP mode");
    shoot(&mut h, "47b-bench-isp-mode");
    let mut h = bench("ours", "Verge Aux");
    shoot(&mut h, "47c-bench-build-without-aux");
    let mut h = bench("ours-pro-j15", "J15");
    shoot(&mut h, "47d-bench-dio-unavailable");
    let mut h = bench("ours-pro-energy-failed", "The last capture failed");
    shoot(&mut h, "47e-bench-capture-failed");
    let mut h = bench("ours-pro-energy", "Started elsewhere");
    if h.query_by_label("Turn on").is_some() {
        press(&mut h, "Turn on");
        until(&mut h, "Turn off");
    }
    press(&mut h, "Turn off");
    until_contains(&mut h, "Turn off the target supply?");
    shoot(&mut h, "47f-bench-supply-off-question");
    let mut h = bench("ours-plain", "DIO3 output");
    press(&mut h, "Restart");
    shoot(&mut h, "47g-bench-restart-question");
    let bus = SimBus::scenario("ours-plain").unwrap();
    bus.mark_dio(0, 0x0C);
    let mut h = rendered_at(window_on(bus, work("marked"), false), size);
    steps(&mut h, 40);
    press(&mut h, "Bench");
    until_contains(&mut h, "held by a bench tool's command marker");
    shoot(&mut h, "47h-bench-two-pins-marked");
    let mut h = bench("ours-pro-and-plain", "Turn");
    h.get_by_role(Role::ComboBox).click();
    steps(&mut h, 5);
    h.render()
        .unwrap()
        .save(dir.join("47i-bench-unit-list-open-night.png"))
        .unwrap();
    let mut h = rendered_at(window_on(SimBus::default(), work("nothing"), false), size);
    steps(&mut h, 20);
    press(&mut h, "Bench");
    steps(&mut h, 20);
    shoot(&mut h, "47j-bench-nothing-attached");

    // Bench recording at the smallest size, then closing while it is saved.
    let bus = SimBus::scenario("ours-pro-energy").unwrap().paced(1.0);
    let mut h = rendered_at(window_on(bus, work("small"), false), small);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "Record running capture");
    press(&mut h, "Record running capture");
    until(&mut h, "Stop recording");
    for _ in 0..150 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    shoot(&mut h, "48a-bench-recording-860x640");
    request_close(&mut h);
    for k in 0..40 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
        if k == 5 {
            h.render()
                .unwrap()
                .save(dir.join("48b-bench-closing-night.png"))
                .unwrap();
        }
    }

    // Recordings opened from Kept on disk: one with markers and a window past its end, one of DIO
    // edges alone; and the inline Discard.
    let files = work("files");
    let captures = files.join("captures");
    std::fs::create_dir_all(&captures).unwrap();
    let markers: Vec<escapement_verge_probe::stream::Marker> = (0..14)
        .map(|n| escapement_verge_probe::stream::Marker {
            time_ns: 1_000_000_000 * (u64::from(n) + 1) + 400_000_000,
            number: 40 + n,
            label: format!("step {n}").into_bytes(),
        })
        .collect();
    crate::bench::write_for_pictures(
        &captures.join("capture-2026-11-18T09-00-00.000Z.tickwave"),
        &captures.join("dio-only.tickwave"),
        &markers,
    );
    let mut h = rendered_at(window_on(SimBus::default(), files, false), size);
    steps(&mut h, 20);
    press(&mut h, "Bench");
    steps(&mut h, 20);
    open_kept(&mut h, "dio-only.tickwave");
    steps(&mut h, 80);
    shoot(&mut h, "49d-bench-dio-only-file");
    open_kept(&mut h, "capture-2026-11-18T09-00-00.000Z.tickwave");
    until(&mut h, "Measurements");
    steps(&mut h, 60);
    shoot(&mut h, "49a-bench-file-opened");
    h.get_by_label_contains("step 3").scroll_to_me();
    steps(&mut h, 20);
    h.get_by_label_contains("step 3").click();
    steps(&mut h, 30);
    press(&mut h, "Select to next marker");
    steps(&mut h, 60);
    h.get_by_label("Unmeasured").scroll_to_me();
    steps(&mut h, 20);
    shoot(&mut h, "49b-bench-file-measurements");
    press(&mut h, "Discard…");
    steps(&mut h, 3);
    shoot(&mut h, "49c-bench-discard-question");

    // Entering ISP mode: the question, the unit waiting there, and a Pro that may not enter it
    // with a release chosen.
    let mut h = rendered_at(
        window_on(SimBus::scenario("ours").unwrap(), work("enter"), false),
        size,
    );
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Enter ISP mode");
    shoot(&mut h, "50a-enter-isp-question");
    press(&mut h, "Enter ISP mode");
    until(&mut h, "In ISP mode");
    shoot(&mut h, "50b-in-isp-mode");
    let mut h = rendered_at(
        window_on(
            SimBus::scenario("ours-pro").unwrap(),
            work("enter-pro"),
            false,
        ),
        size,
    );
    until(&mut h, "Update to v0.3.0");
    h.get_by_label("Enter ISP mode").scroll_to_me();
    steps(&mut h, 5);
    h.get_by_label("Enter ISP mode").hover();
    steps(&mut h, 60);
    h.render()
        .unwrap()
        .save(dir.join("50c-enter-isp-refused-pro-night.png"))
        .unwrap();

    // The Pro's calibration points.
    let bus = SimBus::scenario("ours-pro-energy").unwrap().paced(1.0);
    let mut h = rendered_at(window_on(bus, work("calibration"), false), size);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "Calibration");
    press(&mut h, "Calibration");
    h.get_by_label("High range").scroll_to_me();
    steps(&mut h, 5);
    shoot(&mut h, "51-bench-calibration");
}

/// The label of the button that resets a unit out of ISP mode.
const RESET_LABEL: &str = "Reset out of ISP mode";

/// Opens `name` from the Kept on disk list.
fn open_kept(h: &mut Harness<'_, VergeApp>, name: &str) {
    until_contains(h, "Kept on disk");
    h.get_by_role_and_label(Role::ComboBox, "Kept on disk")
        .click();
    h.run_steps(5);
    h.get_by_label(name).click();
    h.run_steps(5);
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
    until(&mut h, "Batch");
    press(&mut h, "Batch");
    until(&mut h, "Arm batch");
    press(&mut h, "Arm batch");
    h.get_by_label_contains(DOWNLOAD);
    h.get_by_label_contains(
        " KiB), and flash it onto every standalone MCU-Link running NXP's firmware or Verge older than the image, attached from now on?",
    );
    assert!(
        h.query_by_label_contains("looks like any other LPC55S69 board")
            .is_none()
    );
    press(&mut h, "Confirm");
    assert!(h.state().busy());
    until(&mut h, "3 updated");
    until(&mut h, "1 current");
    until(&mut h, "1 skipped");
    h.get_by_label("0 failed");
    press(&mut h, "Stop");
    until(&mut h, "Ended");
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
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
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
    h.get_by_label_contains("The window closes when the batch's probe under way finishes.");
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
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
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
    until(&mut h, "Paused");
    h.get_by_label_contains("An MCU-Link mini is in ISP mode at 1-2.");
}

#[test]
fn a_batch_that_takes_units_in_isp_mode_says_it_may_not_be_flashing_an_mculink() {
    let (app, _backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(
        &mut h,
        "Probes that arrive in ISP mode, as after fitting the jumper",
    );
    press(&mut h, "Arm batch");
    h.get_by_label_contains(&format!(
        "In ISP mode an MCU-Link looks like any other LPC55S69 board. {DOWNLOAD}"
    ));
    h.get_by_label_contains(
        "and flash it onto every standalone MCU-Link running NXP's firmware or Verge older than the image or in ISP mode",
    );
}

#[test]
fn a_batch_needs_something_to_take() {
    let (app, _backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    h.get_by_role_and_label(Role::CheckBox, STOCK).click();
    h.step();
    h.get_by_role_and_label(Role::CheckBox, "Verge").click();
    h.step();
    h.step();
    h.get_by_label("Cannot arm");
    // Under the field it is about, and at the foot.
    assert_eq!(
        h.query_all_by_label_contains("Tick at least one kind of probe")
            .count(),
        2
    );
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
                stopped: None,
            }));
    }
    let mut h = harness(app);
    h.step();
    // Once, in the strip: the listing did not fail, the worker did.
    assert_eq!(
        h.query_all_by_label_contains("Discovery worker failed.")
            .count(),
        1
    );
    assert!(
        h.query_by_label_contains("Could not list USB devices")
            .is_none()
    );
    assert!(!h.state().busy());
}

#[test]
fn a_worker_failure_during_installation_finishes_the_job() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    press(&mut h, "Confirm");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::WorkerFailed(crate::worker::JobError {
            message: "install worker failed".into(),
            advice: None,
            left_in_isp: true,
            stopped: None,
        }));
    until(&mut h, "Failed");
    assert!(!h.state().busy());
    press(&mut h, "Back");
    h.get_by_label("Worker stopped");
}

#[test]
fn a_worker_failure_withdraws_an_open_install_question() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    h.get_by_label("Confirm");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::WorkerFailed(crate::worker::JobError {
            message: "the USB worker stopped unexpectedly: boom".into(),
            advice: None,
            left_in_isp: false,
            stopped: None,
        }));
    until(&mut h, "Worker stopped");
    assert!(h.query_by_label("Confirm").is_none());
    assert!(!h.state().busy());
}

#[test]
fn a_question_about_a_unit_that_left_is_withdrawn() {
    let bus = SimBus::default();
    bus.add_ours("v0.2.0");
    bus.add_ours("v0.2.0");
    let units: Vec<Unit> = escapement_verge_probe::detect(&bus).unwrap();
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
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    h.get_by_label("Confirm");
    // The asked-about unit is unplugged; the rescan finds the other, which becomes selected.
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Units(Ok(vec![units[1].clone()])));
    h.run_steps(3);
    assert!(h.query_by_label("Confirm").is_none());
    h.get_by_label("Update to v0.3.0");
}

#[test]
fn an_install_preserves_the_previous_batch_result() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Batch(BatchEvent::Ended(Ok(Default::default()))));
    until(&mut h, "Ended");
    press(&mut h, "Probes");
    press(&mut h, "Update to v0.3.0");
    press(&mut h, "Confirm");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Install(InstallEvent::Finished(Ok(
            Outcome::JumperStillFitted,
        ))));
    h.run_steps(3);
    assert!(!h.state().busy());
    press(&mut h, "Batch");
    h.get_by_label("Ended");
}

#[test]
fn a_worker_that_dies_during_a_batch_lets_the_window_close() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
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
            stopped: None,
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
    release: escapement_verge_probe::Cancel,
}

/// Lets a stalled download go when the test ends, passed or not, so that the worker it holds can
/// be joined. Declared after the harness, it is dropped before it.
struct Free(escapement_verge_probe::Cancel);

impl Drop for Free {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl escapement_verge_probe::release::Http for Stalled {
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<Vec<u8>, escapement_verge_probe::release::HttpError> {
        if url.contains("/releases/download/") {
            let started = std::time::Instant::now();
            while !self.release.is_cancelled() && started.elapsed() < Duration::from_secs(60) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        self.http.get(url, headers)
    }
}

fn stalled_app() -> (VergeApp, escapement_verge_probe::Cancel) {
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let release = escapement_verge_probe::Cancel::new();
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
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
    h.run_steps(5);
    h.get_by_label_contains("loading the image");
    assert!(h.state().busy());
    press(&mut h, "Stop");
    until(&mut h, "Ended");
    assert!(!h.state().busy());
}

/// Closing the window while the batch's image is still downloading closes it.
#[test]
fn the_window_closes_while_a_batch_loads_its_image() {
    let (app, release) = stalled_app();
    let mut h = harness(app);
    let _free = Free(release);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
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
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    press(&mut h, "Confirm");
    until(&mut h, "Failed");
    h.get_by_label_contains(
        "the probe is an MCU-Link Pro, and the image declares no boards, so verge takes it as a Verge image for the standalone MCU-Link only",
    );
    h.get_by_label_contains(
        "Pick NXP's firmware under Image, or a Verge file built for this board.",
    );
    assert!(!bus.log().iter().any(|l| l.contains("DAP 8101")));
    press(&mut h, "Back");

    let dir = record_dir("pro-files");
    let ours = SimFirmware::Ours {
        build: "v0.3.0".into(),
    };
    let plain = dir.join("plain.s19");
    std::fs::write(&plain, firmware_srecord(&ours)).unwrap();
    let pro = dir.join("pro.s19");
    std::fs::write(
        &pro,
        escapement_verge_probe::sim::firmware_srecord_declaring(&ours, 1 << 1),
    )
    .unwrap();
    drop_file(&mut h, &plain);
    until(&mut h, "Not for this board");
    h.get_by_label_contains("The probe is an MCU-Link Pro");
    drop_file(&mut h, &pro);
    until(&mut h, "Flash pro.s19");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A running unit that does not say which board it is gets an image of ours only after a
/// question that says so.
#[test]
fn a_unit_that_names_no_board_is_asked_about() {
    let mut h = harness(sim_app(SimBus::scenario("unnamed-board").unwrap(), vec![]));
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    h.get_by_label_contains(&format!(
        "This probe's firmware does not say which board it is, so nothing can check that the image is built for it. A release says which boards it is built for only once it is downloaded. {DOWNLOAD}"
    ));
}

/// The worker adds a batch's lines to a record file that has some already.
#[test]
fn a_batch_adds_to_an_existing_record_file() {
    use crate::worker::{Backend, Request};
    use escapement_verge_probe::batch::{Board, Filter, OursFilter, StockFilter};
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
        source: escapement_verge_probe::Source::Release(None),
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
        stop: escapement_verge_probe::Cancel::new(),
        cancel: escapement_verge_probe::Cancel::new(),
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

/// A unit a debugger holds is offered no update until the debugger closes, not the ISP jumper.
#[test]
fn a_unit_a_debugger_holds_waits_for_it() {
    let mut h = harness(sim_app(
        SimBus::scenario("ours-pro-debugger").unwrap(),
        vec![],
    ));
    until_contains(&mut h, "Close the debugger first.");
    assert!(h.query_by_label("Needs the jumper").is_none());
    assert!(h.query_all_by_label_contains("ISP jumper").next().is_none());
}

/// Cancel while a single update's image is still downloading ends it at once, with nothing
/// flashed.
#[test]
fn cancel_ends_an_update_still_loading_its_image() {
    let (app, release) = stalled_app();
    let mut h = harness(app);
    let _free = Free(release);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    press(&mut h, "Confirm");
    h.run_steps(5);
    assert!(h.state().busy());
    press(&mut h, "Cancel");
    // A cancel, not a failure: the headline says nothing was changed.
    until(&mut h, "Stopped");
    h.get_by_label("Nothing was changed");
    h.get_by_label_contains("stopped before anything was sent to the probe, so nothing changed");
    assert!(h.query_by_label("Failed").is_none());
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
    use escapement_verge_probe::batch::{Board, Filter, OursFilter, StockFilter};
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
        source: escapement_verge_probe::Source::Release(None),
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
        stop: escapement_verge_probe::Cancel::new(),
        cancel: escapement_verge_probe::Cancel::new(),
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
    until(&mut h, "Batch");
    press(&mut h, "Batch");
    press_as(&mut h, Role::Button, STOCK);
    until(&mut h, "Cannot arm");
    assert_eq!(
        h.query_all_by_label_contains("The image carries no Verge version")
            .count(),
        2
    );
    press(&mut h, "of any build, not only those older than the image");
    until(&mut h, "Arm batch");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A release's image says which boards it is for only once it is downloaded, so a batch for the
/// Pro with a release that does not declare it ends there, unarmed.
#[test]
fn a_batch_for_the_pro_does_not_arm_with_an_image_not_built_for_it() {
    let mut h = harness(sim_app(SimBus::scenario("arrivals").unwrap(), vec![]));
    until(&mut h, "Batch");
    press(&mut h, "Batch");
    press(&mut h, "MCU-Link Pro");
    press(&mut h, "Arm batch");
    h.get_by_label_contains("onto every MCU-Link Pro running");
    press(&mut h, "Confirm");
    until(&mut h, "Not armed");
    h.get_by_label_contains(
        "The batch takes the MCU-Link Pro, and the image declares no boards, so verge takes it as a Verge image for the standalone MCU-Link only.",
    );
    assert!(!h.state().busy());
}

/// A Pro that needs the jumper: in ISP mode nothing tells its board, so the view names the board
/// and the image to flash then, and refuses a file not built for it before the jumper.
#[test]
fn the_jumper_advice_names_the_board_and_the_image() {
    let mut h = harness(sim_app(SimBus::scenario("pro-old").unwrap(), vec![]));
    until(&mut h, "Needs the jumper");
    h.get_by_label_contains(
        "This probe is an MCU-Link Pro. In ISP mode the app cannot tell its board, and a release says which boards it is for only once it is downloaded: flash it with NXP's firmware, or with a file built for an MCU-Link Pro.",
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
        escapement_verge_probe::sim::firmware_srecord_declaring(&ours, 1 << 1),
    )
    .unwrap();
    drop_file(&mut h, &plain);
    until(&mut h, "Not for this board");
    drop_file(&mut h, &pro);
    until(&mut h, "Needs the jumper");
    h.get_by_label_contains(
        "This probe is an MCU-Link Pro. In ISP mode the app cannot tell its board, so flash it with pro.s19, which is built for it.",
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The question before flashing a unit in ISP mode names the boards a file is built for, as the
/// command line's does.
#[test]
fn the_question_names_the_boards_a_file_is_built_for() {
    let mut h = harness(sim_app(SimBus::scenario("isp").unwrap(), vec![]));
    until(&mut h, "Install v0.3.0");
    let dir = record_dir("question-file");
    let pro = dir.join("pro.s19");
    std::fs::write(
        &pro,
        escapement_verge_probe::sim::firmware_srecord_declaring(
            &SimFirmware::Ours {
                build: "v0.3.0".into(),
            },
            1 << 1,
        ),
    )
    .unwrap();
    drop_file(&mut h, &pro);
    until(&mut h, "Flash pro.s19");
    press(&mut h, "Flash pro.s19");
    h.get_by_label_contains(
        "LPC55S69 board. The image is built for the MCU-Link Pro only. Flash pro.s19",
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A batch that cannot date ours is refused before its image is looked for.
#[test]
fn the_worker_refuses_an_undatable_batch_before_loading() {
    use crate::worker::{Backend, Request};
    use escapement_verge_probe::batch::{Board, Filter, OursFilter, StockFilter};
    let dir = record_dir("undated");
    let record = dir.join("record.jsonl");
    let http = SimHttp::new(repo());
    let mut worker = Worker::spawn(
        SimBus::scenario("arrivals").unwrap(),
        GitHub::new(http, repo(), None),
        || {},
    );
    worker.send(Request::Batch {
        source: escapement_verge_probe::Source::Stock(Some(PathBuf::from(
            "/nonexistent/probe_firmware",
        ))),
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
        stop: escapement_verge_probe::Cancel::new(),
        cancel: escapement_verge_probe::Cancel::new(),
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

/// A file dropped while the question is up must not leave a question that names one file's
/// boards and flashes another. The question goes, and the view offers the new file instead.
#[test]
fn the_question_names_the_boards_of_the_file_it_flashes() {
    let mut h = harness(sim_app(SimBus::scenario("isp").unwrap(), vec![]));
    until(&mut h, "Install v0.3.0");
    let dir = record_dir("question-two-files");
    let pro = dir.join("pro.s19");
    let plain = dir.join("plain.s19");
    let ours = SimFirmware::Ours {
        build: "v0.3.0".into(),
    };
    std::fs::write(
        &pro,
        escapement_verge_probe::sim::firmware_srecord_declaring(&ours, 1 << 1),
    )
    .unwrap();
    std::fs::write(&plain, firmware_srecord(&ours)).unwrap();
    drop_file(&mut h, &pro);
    until(&mut h, "Flash pro.s19");
    press(&mut h, "Flash pro.s19");
    h.get_by_label_contains("The image is built for the MCU-Link Pro only. Flash pro.s19");
    drop_file(&mut h, &plain);
    h.run();
    let wrong = h
        .query_by_label_contains(
            "The image is built for the standalone MCU-Link only. Flash pro.s19",
        )
        .is_some();
    let confirm = h.query_by_label("Confirm").is_some();
    let offered = h.query_by_label("Flash plain.s19").is_some();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        !wrong,
        "the question says the standalone MCU-Link only, and Confirm flashes pro.s19, built for the Pro only"
    );
    assert!(!confirm && offered, "confirm {confirm}, offered {offered}");
}

/// A unit that needs the jumper, with a release chosen: nothing has checked the release's boards,
/// so the advice does not vouch for it.
#[test]
fn the_jumper_advice_does_not_vouch_for_a_release() {
    let mut h = harness(sim_app(SimBus::scenario("stock-old").unwrap(), vec![]));
    until(&mut h, "Needs the jumper");
    h.get_by_label_contains(
        "This probe is a standalone MCU-Link. In ISP mode the app cannot tell its board, and a release says which boards it is for only once it is downloaded: flash it with release v0.3.0 only if it is built for a standalone MCU-Link.",
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
fn keep(http: &SimHttp, cache: &Cache) -> escapement_verge_probe::release::Release {
    let github = GitHub::new(http.clone(), repo(), None)
        .with_key(Some(http.public_key()))
        .with_cache(Some(cache.clone()));
    let release = github.latest().unwrap();
    github
        .download(
            &release,
            Download::Agreed,
            &mut escapement_verge_probe::progress::Quiet,
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
    until(&mut h, "Update to v0.3.0");
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41")
        .click();
    h.step();
    h.step();
    press(&mut h, "Install v0.3.0");
    h.get_by_label_contains(DOWNLOAD);
    press(&mut h, "Confirm");
    until(&mut h, "Done");
    press(&mut h, "Back");
    assert_eq!(downloads(&http), 3);

    h.get_by_role_and_label(Role::ListBoxOption, "CJ3E7A91C2")
        .click();
    h.step();
    h.step();
    press(&mut h, "Update to v0.3.0");
    h.get_by_label_contains("Flash release v0.3.0 onto CJ3E7A91C2 at");
    assert!(h.query_by_label_contains("Download release").is_none());
    press(&mut h, "Confirm");
    until(&mut h, "Done");
    press(&mut h, "Back");
    assert_eq!(downloads(&http), 3);

    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    h.get_by_label_contains("Flash release v0.3.0 onto every standalone MCU-Link");
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
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    h.get_by_label_contains("Flash release v0.3.0 onto QK7RM2VTX8D41 at 1-1?");
    press(&mut h, "Confirm");
    until(&mut h, "Failed");
    h.get_by_label_contains("the copy of release v0.3.0 in the cache failed its check");
    h.get_by_label_contains("Try again: the question then asks to download the release.");
    assert_eq!(downloads(&http), 3);
    press(&mut h, "Back");

    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    h.get_by_label_contains(DOWNLOAD);
    press(&mut h, "Confirm");
    until(&mut h, "Done");
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
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    h.get_by_label_contains("Flash release v0.3.0 onto QK7RM2VTX8D41 at 1-1?");
    std::fs::remove_dir_all(cache.root()).unwrap();
    h.run_steps(2);
    h.get_by_label_contains("Flash release v0.3.0 onto QK7RM2VTX8D41 at 1-1?");
    press(&mut h, "Confirm");
    until(&mut h, "Failed");
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
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    // Stock carries no version of ours to compare, so the batch takes ours of any build.
    press(&mut h, "of any build, not only those older than the image");
    until(&mut h, "Arm batch");
    press(&mut h, "Arm batch");
    h.get_by_label_contains(DOWNLOAD);
    press_as(&mut h, Role::Button, STOCK);
    assert!(h.query_by_label_contains("Download release").is_none());
    assert!(h.query_by_label("Confirm").is_none());
    press(&mut h, "Arm batch");
    h.get_by_label_contains(&format!(
        "Flash {STOCK} V3.172 onto every {}",
        Board::McuLink
    ));
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

impl escapement_verge_probe::release::Http for Republished {
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<Vec<u8>, escapement_verge_probe::release::HttpError> {
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
    use escapement_verge_probe::sim::firmware_image;
    use escapement_verge_probe::sim::http::{SimAsset, SimRelease};

    let cache = Cache::at(record_dir(name));
    let _ = std::fs::remove_dir_all(cache.root());
    let before = SimHttp::new(repo());
    before.publish_ours("v0.3.0");
    keep(&before, &cache);
    let image = firmware_image(&SimFirmware::Ours {
        build: "v0.3.0".into(),
    })
    .to_srecord("simulated, built again");
    let sha = escapement_verge_probe::image::Image::parse(image.as_bytes())
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
/// the download.
#[test]
fn a_release_published_again_is_asked_about_after_try_again() {
    let (app, switched, cache) = republished_app("republished");
    let mut h = harness(app);
    until(&mut h, "Install v0.3.0");
    switched.store(true, std::sync::atomic::Ordering::SeqCst);
    press(&mut h, "Install v0.3.0");
    h.get_by_label_contains("Flash release v0.3.0 onto QK7RM2VTX8D41 at 1-1?");
    press(&mut h, "Confirm");
    until(&mut h, "Failed");
    h.get_by_label_contains("would have to be downloaded, and that was not asked");
    h.get_by_label_contains("Try again: the question then asks to download the release.");
    press(&mut h, "Back");
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    h.get_by_label_contains(DOWNLOAD);
    press(&mut h, "Confirm");
    until(&mut h, "Done");
    std::fs::remove_dir_all(cache.root()).unwrap();
}

/// The same for a batch: the arming question that named no download arms nothing new, and the
/// next one names the download.
#[test]
fn a_batch_of_a_release_published_again_is_asked_about_after_try_again() {
    let (app, switched, cache) = republished_app("republished-batch");
    let mut h = harness(app);
    until(&mut h, "Install v0.3.0");
    switched.store(true, std::sync::atomic::Ordering::SeqCst);
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    h.get_by_label_contains("Flash release v0.3.0 onto every standalone MCU-Link");
    press(&mut h, "Confirm");
    until(&mut h, "Not armed");
    h.get_by_label_contains("would have to be downloaded, and that was not asked");
    h.get_by_label_contains("Try again: the question then asks to download the release.");
    press(&mut h, "Back");
    until(&mut h, "Arm batch");
    press(&mut h, "Arm batch");
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
    until(&mut h, "Batch");
    press(&mut h, "Batch");
    until(&mut h, "Arm batch");
    press(&mut h, "Arm batch");
    h.get_by_label_contains("Flash release v0.3.0 onto every standalone MCU-Link");
    std::fs::remove_dir_all(cache.root()).unwrap();
    h.run_steps(2);
    h.get_by_label_contains("Flash release v0.3.0 onto every standalone MCU-Link");
    assert!(h.query_by_label_contains("Download release").is_none());
    press(&mut h, "Confirm");
    until(&mut h, "Not armed");
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
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
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
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Install v0.3.0");
    press(&mut h, "Confirm");
    until(&mut h, "Failed");
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
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "Record running capture");
    until(&mut h, "DIO0–2 output");
    // A capture started before the GUI connected still requires confirmation.
    if h.query_by_label("Turn on").is_some() {
        press(&mut h, "Turn on");
        until(&mut h, "Turn off");
    }
    press(&mut h, "Turn off");
    until_contains(&mut h, "Turn off the target supply?");
    press(&mut h, "Back");
    for _ in 0..10 {
        h.step();
    }
    assert!(
        h.query_by_label_contains("Turn off the target supply?")
            .is_none()
    );
    assert!(h.query_by_label("Turn off").is_some());
    press(&mut h, "Turn off");
    until_contains(&mut h, "Turn off the target supply?");
    press(&mut h, "Turn off");
    until(&mut h, "Turn on");
    press(&mut h, "Turn on");
    until(&mut h, "Turn off");
}

#[test]
fn bench_dio_explains_each_unavailable_state() {
    for (scenario, reason) in [
        ("ours-pro-old-aux", "predates DIO on Verge Aux"),
        ("ours-pro-j15", "take J15 off"),
        (
            "ours-pro-aux-taken",
            "Close the program holding it and reconnect.",
        ),
    ] {
        let mut h = harness(sim_app(SimBus::scenario(scenario).unwrap(), vec![]));
        until(&mut h, "Update to v0.3.0");
        press(&mut h, "Bench");
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
        assert!(h.query_by_label("DIO0–2 output").is_none());
    }
}

/// A standalone MCU-Link connects for its DIO pins alone. Each pin turns on its own, after the same
/// question. The bench offers no supply, and records the pins alone.
#[test]
fn bench_drives_a_standalone_units_dio_pins() {
    let bus = SimBus::scenario("ours-plain").unwrap();
    let mut h = harness(sim_app(bus.clone(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "DIO3 output");
    for pin in 0..4 {
        assert!(h.query_by_label(&format!("DIO{pin} input")).is_some());
    }
    for absent in [
        "Turn on",
        "Turn off",
        "Record running capture",
        "Auto range",
    ] {
        assert!(h.query_by_label(absent).is_none(), "{absent}");
    }
    h.get_by_label("Start recording");
    press(&mut h, "DIO1 output");
    until_contains(&mut h, "Drive DIO1? ");
    until_contains(&mut h, "driving this pin. ");
    until_contains(&mut h, "J8 drives 3.3 V");
    press(&mut h, "Drive DIO1");
    for _ in 0..400 {
        h.step();
        if h.query_all_by_label("Set high").count() == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(h.query_all_by_label("Set high").count(), 1);
    let log = bus.log();
    assert!(log.iter().any(|s| s.contains("AUX 8e020200")));
    // Opening Aux ends any stream the last program left, `0x90` mode 7, on every board.
    assert!(
        !log.iter()
            .any(|s| s.contains("AUX 8f") || (s.contains("AUX 90") && !s.ends_with("AUX 9007"))),
        "{log:#?}"
    );
}

/// With no recording open, the graph offers a trace to choose, and Fit Y, only where the probe
/// captures energy: a standalone MCU-Link has none to draw.
#[test]
fn bench_offers_a_trace_only_where_the_probe_captures_energy() {
    let mut h = harness(sim_app(SimBus::scenario("ours-plain").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "DIO3 output");
    for absent in ["Current", "Voltage", "Power", "Fit Y"] {
        assert!(h.query_by_label(absent).is_none(), "{absent}");
    }
    let mut h = harness(sim_app(SimBus::scenario("ours-pro").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "Fit Y");
    for present in ["Current", "Voltage", "Power"] {
        assert!(h.query_all_by_label(present).count() > 0, "{present}");
    }
}

/// A Pro without its energy circuit and with J15 fitted still switches its target supply, and its
/// DIO panel says why it has no pins.
#[test]
fn bench_switches_the_supply_of_a_pro_without_its_energy_circuit_or_dio() {
    let bus = SimBus::scenario("ours-pro-unfitted-j15").unwrap();
    let mut h = harness(sim_app(bus.clone(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "Turn on");
    until_contains(&mut h, "take J15 off");
    // The reason reads as a sentence, as the panel's other text does.
    h.get_by_label(
        "CJ3E7A91C2 has DIO disabled: take J15 off, then unplug the probe and plug it back in.",
    );
    assert!(h.query_by_label_contains("output").is_none());
    press(&mut h, "Turn on");
    until(&mut h, "Turn off");
}

/// A standalone MCU-Link whose J8 pins a bench tool's command marker holds says so, and how to
/// give them back, in place of the pins.
#[test]
fn bench_says_when_the_bench_marker_holds_every_dio_pin() {
    let bus = SimBus::scenario("ours-plain").unwrap();
    bus.mark_dio(0, 0x0F);
    let mut h = harness(sim_app(bus.clone(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until_contains(
        &mut h,
        "A bench tool has given the DIO pins to its command marker.",
    );
    assert!(h.query_by_label_contains("output").is_none());
}

/// A Pro without its energy circuit switches its target supply and drives its DIO pins as `verge
/// power` and `verge dio` do, says why it measures nothing, and its question before driving a pin
/// has no word of J8.
#[test]
fn bench_drives_a_pro_without_its_energy_circuits_supply_and_dio_pins() {
    use escapement_verge_probe::{Bus, DapTransport, VergeControl, detect};
    let bus = SimBus::scenario("ours-pro-unfitted").unwrap();
    // Another program turned the supply on: Bench shows it as it is.
    let unit = detect(&bus).unwrap().remove(0);
    let mut link = bus.open_dap(&unit.device).unwrap();
    assert!(
        VergeControl::new(&mut DapTransport(&mut link))
            .set_target_power(true)
            .unwrap()
    );
    drop(link);
    let mut h = harness(sim_app(bus.clone(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "DIO0–2 output");
    until_contains(&mut h, "no energy measurement circuit fitted");
    for absent in [
        "Record running capture",
        "No reading without a capture",
        "Auto range",
    ] {
        assert!(h.query_by_label(absent).is_none(), "{absent}");
    }
    // Its pins alone, as `verge dio record` records them.
    h.get_by_label("Start recording");
    until(&mut h, "Turn off");
    press(&mut h, "Turn off");
    until(&mut h, "Turn on");
    press(&mut h, "DIO0–2 output");
    until_contains(&mut h, "Drive DIO0–2? ");
    until_contains(&mut h, "driving these pins. ");
    assert!(h.query_all_by_label_contains("J8").next().is_none());
}

#[test]
fn bench_dio_asks_before_driving_and_keeps_inputs_on_cancel() {
    let bus = SimBus::scenario("ours-pro-held").unwrap();
    let mut h = harness(sim_app(bus.clone(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "DIO0–2 output");
    h.get_by_label("high");
    press(&mut h, "DIO0–2 output");
    until_contains(&mut h, "Drive DIO0–2? ");
    until_contains(&mut h, "driving these pins. ");
    assert!(h.query_all_by_label_contains("J8").next().is_none());
    assert!(!bus.log().iter().any(|s| s.contains("AUX 8e0707")));
    press(&mut h, "Back");
    for _ in 0..10 {
        h.step();
    }
    assert!(h.query_by_label_contains("Drive DIO0–2? ").is_none());
    assert!(!bus.log().iter().any(|s| s.contains("AUX 8e0707")));
    press(&mut h, "DIO0–2 output");
    until(&mut h, "Drive DIO0–2");
    press(&mut h, "Drive DIO0–2");
    for _ in 0..400 {
        h.step();
        if h.query_all_by_label("Set high").count() == 3 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(h.query_all_by_label("Set high").count(), 3);
    assert!(bus.log().iter().any(|s| s.contains("AUX 8e0707")));
    press(&mut h, "DIO0–2 input");
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(h.query_by_label_contains("Drive DIO0–2? ").is_none());
    assert_eq!(h.query_all_by_label("Set high").count(), 0);
}

#[test]
fn bench_refuses_older_firmware_and_reaches_a_debugger_held_pro() {
    let mut h = harness(sim_app(SimBus::scenario("ours").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until_contains(&mut h, "has no Verge Aux");
    assert!(h.query_by_label("Start recording").is_none());
    let mut h = harness(sim_app(
        SimBus::scenario("ours-pro-debugger").unwrap(),
        vec![],
    ));
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    press(&mut h, "Bench");
    until(&mut h, "Start recording");
}

#[test]
fn a_worker_failure_during_recording_unlocks_the_window() {
    use crate::bench_worker::Status;
    use crate::worker::JobError;
    let (app, backend) = manual_app("ours-pro", true);
    let instance = escapement_verge_probe::detect(&SimBus::scenario("ours-pro").unwrap()).unwrap()
        [0]
    .device
    .instance
    .clone();
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Bench(Status {
            instance,
            connected: true,
            target_power: true,
            energy: true,
            board: Some(escapement_verge_probe::BoardKind::Pro),
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
            stopped: None,
        }));
    h.step();
    assert!(!h.state().busy());
    press(&mut h, "Probes");
    h.step();
}

#[test]
fn a_session_the_worker_dropped_can_be_reconnected() {
    use crate::bench_worker::Status;
    let (app, backend) = manual_app("ours-pro", true);
    let instance = escapement_verge_probe::detect(&SimBus::scenario("ours-pro").unwrap()).unwrap()
        [0]
    .device
    .instance
    .clone();
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
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
    until(&mut h, "Reconnect");
    press(&mut h, "Reconnect");
    h.step();
    assert_eq!(connects(&backend), 2);
}

#[test]
fn a_recording_failure_keeps_a_pending_close_open() {
    use crate::bench_worker::Status;
    let (app, backend) = manual_app("ours-pro", true);
    let instance = escapement_verge_probe::detect(&SimBus::scenario("ours-pro").unwrap()).unwrap()
        [0]
    .device
    .instance
    .clone();
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Bench(Status {
            instance: instance.clone(),
            connected: true,
            target_power: true,
            energy: true,
            board: Some(escapement_verge_probe::BoardKind::Pro),
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
            target_power: true,
            energy: true,
            board: Some(escapement_verge_probe::BoardKind::Pro),
            recording: false,
            error: Some("disk full".into()),
            ..Status::default()
        }));
    h.step();
    assert!(!root_commands(&h).contains(&ViewportCommand::Close));
    assert!(!h.state().busy());
    h.get_by_label("Disk full.");
}

#[test]
fn the_icon_decodes() {
    let icon = crate::icon();
    assert_eq!((icon.width, icon.height), (512, 512));
    assert_eq!(icon.rgba.len(), 512 * 512 * 4);
}

/// The desktop entry and the macOS bundle carry the window's app id. A desktop entry named or
/// pointing otherwise leaves the window without its entry's name and icon.
#[test]
fn the_desktop_entry_and_the_bundle_carry_the_app_id() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("package");
    let entry = std::fs::read_to_string(package.join(format!("linux/{}.desktop", crate::APP_ID)))
        .expect("a desktop entry named for the app id");
    for key in ["Icon", "StartupWMClass"] {
        assert!(
            entry
                .lines()
                .any(|line| line == format!("{key}={}", crate::APP_ID)),
            "{key} in {entry}"
        );
    }
    let plist = std::fs::read_to_string(package.join("macos/Info.plist")).unwrap();
    let bundle = format!(
        "<key>CFBundleIdentifier</key>\n\t<string>{}</string>",
        crate::APP_ID.replace('_', "-")
    );
    assert!(plist.contains(&bundle), "{plist}");
}

/// Focus is a ring of ink just outside the widget, drawn over everything on its layer, so it shows
/// on a chosen button's ink fill as well as on an outline.
#[test]
fn the_focused_widget_has_an_ink_ring_outside_it() {
    let mut h = harness(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    let ink = crate::tokens::NIGHT.ink;
    let rings = |h: &Harness<'_, VergeApp>, around: egui::Rect| {
        h.output()
            .shapes
            .iter()
            .filter(|clipped| {
                matches!(&clipped.shape, egui::Shape::Rect(rect)
                    if rect.stroke.width == crate::tokens::FOCUS_WIDTH
                        && rect.stroke.color == ink
                        && rect.rect.contains_rect(around)
                        && rect.rect != around)
            })
            .count()
    };
    for label in ["Release", "Rescan"] {
        let around = h.get_by_label(label).rect();
        assert_eq!(
            rings(&h, around),
            0,
            "{label} has a ring before it has focus"
        );
        h.get_by_label(label).focus();
        h.step();
        h.step();
        assert_eq!(rings(&h, around), 1, "{label} has no ring with focus");
    }
}

/// The desktop's reduced-motion setting as the window reads it at start. Needs a session bus.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "reads the desktop's portal"]
fn reads_the_desktops_motion_setting() {
    let connection = zbus::blocking::Connection::session().expect("a session bus");
    let setting = crate::portal_reduced_motion(&connection).expect("the portal answers");
    assert!(setting <= 1, "reduced-motion is 0 or 1, not {setting}");
    println!("reduced-motion: {setting}");
}

/// On Linux, an empty list says where the udev rules are, since a unit the user may not open can
/// be missing from it.
#[cfg(target_os = "linux")]
#[test]
fn nothing_attached_points_at_the_udev_rules() {
    let mut h = harness(sim_app(SimBus::default(), vec![]));
    until(&mut h, "Plug in an MCU-Link");
    h.get_by_label_contains("Plugged in already? On Linux your user needs the udev rules");
    h.get_by_label("Getting started: USB permissions on Linux");
}

/// A unit Linux will not let the user open is sent to the udev rules, not to the ISP jumper:
/// the bootloader needs the same rules.
#[cfg(target_os = "linux")]
#[test]
fn a_unit_the_user_may_not_open_points_at_the_udev_rules() {
    let mut h = harness(sim_app(SimBus::scenario("denied").unwrap(), vec![]));
    until_contains(&mut h, "Your user may not open it");
    h.get_by_label_contains("The system does not let your user open this probe.");
    h.get_by_label_contains("Your user needs the udev rules");
    h.get_by_label("Getting started: USB permissions on Linux");
    assert!(
        h.get_by_label("Cannot update")
            .accesskit_node()
            .is_disabled()
    );
    for absent in ["Needs the jumper", "Release"] {
        assert!(h.query_by_label(absent).is_none(), "{absent}");
    }
}

/// The list and the slab name each unit's board, and Bench's unit list too, where it greys out a
/// unit it can do nothing with.
#[test]
fn every_unit_is_named_with_its_board() {
    let mut h = harness(sim_app(
        SimBus::scenario("ours-pro-and-plain").unwrap(),
        vec![],
    ));
    until_contains(&mut h, "Verge 26ea505 · MCU-Link Pro · 1-1");
    h.get_by_label_contains(&format!("Verge 26ea505 · {} · 1-2", Board::McuLink));
    h.get_by_label_contains("MCU-Link Pro · Updates without the jumper");
    press(&mut h, "Bench");
    until(&mut h, "Turn on");
    h.get_by_role(Role::ComboBox).click();
    h.run_steps(3);
    h.get_by_label("CJ3E7A91C2 · MCU-Link Pro, Verge 26ea505");
    h.get_by_label_contains(&format!("· {}, Verge 26ea505", Board::McuLink));

    let mut h = harness(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    h.get_by_role(Role::ComboBox).click();
    h.run_steps(3);
    let entry = format!("QK7RM2VTX8D41 · {}, {STOCK} V3.172", Board::McuLink);
    assert!(
        h.get_by_label_contains(&entry)
            .accesskit_node()
            .is_disabled()
    );
}

/// While a question is open, Confirm has the keyboard's focus, so it is in sight and Enter
/// answers it; Escape answers Back.
#[test]
fn a_question_takes_the_focus_and_escape_answers_back() {
    let (app, _backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    h.step();
    assert!(h.get_by_label("Confirm").is_focused());
    h.key_press(egui::Key::Escape);
    h.step();
    assert!(h.query_by_label("Confirm").is_none());
    h.get_by_label("Arm batch");

    press(&mut h, "Probes");
    press(&mut h, "Update to v0.3.0");
    h.step();
    assert!(h.get_by_label("Confirm").is_focused());
    h.key_press(egui::Key::Escape);
    h.step();
    assert!(h.query_by_label("Confirm").is_none());
}

/// A running batch keeps its Stop in its slab, above the records that pile up under it.
#[test]
fn a_running_batch_keeps_stop_above_its_records() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
    {
        let mut m = backend.lock().unwrap();
        m.queued.push_back(Event::Batch(BatchEvent::Armed {
            name: "release v0.3.0 (mculink-verge-v0.3.0.s19)".into(),
            filter: "every unit".into(),
        }));
        for k in 0..12 {
            m.queued.push_back(Event::Batch(BatchEvent::Record(Box::new(
                escapement_verge_probe::batch::Record {
                    time: "2026-10-20T14:26:08Z".into(),
                    port: format!("1-{}", k + 1),
                    serial_before: Some(format!("QK7RM2VTX8D{k:02}")),
                    firmware_before: "NXP's firmware V3.172".into(),
                    serial_after: None,
                    firmware_after: None,
                    result: escapement_verge_probe::batch::Settled::Current,
                    reason: None,
                    seconds: None,
                },
            ))));
        }
    }
    h.run_steps(3);
    let stop = h.get_by_label("Stop").rect();
    let first = h.get_by_label("1-12").rect();
    assert!(stop.bottom() < first.top(), "{stop:?} {first:?}");
    assert!(stop.bottom() < 700.0, "in sight: {stop:?}");
}

/// A batch record's reason reads as a sentence on its own line, and its serial and firmware changes
/// read with arrows.
#[test]
fn a_batch_record_says_its_reason_as_a_sentence() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
    {
        let mut m = backend.lock().unwrap();
        m.queued.push_back(Event::Batch(BatchEvent::Armed {
            name: "release v0.3.0 (mculink-verge-v0.3.0.s19)".into(),
            filter: "every probe".into(),
        }));
        for (port, before, after, result, reason) in [
            (
                "1-1",
                "QK7RM2VTX8D41",
                Some("CJ3E7A91C2"),
                escapement_verge_probe::batch::Settled::Updated,
                None,
            ),
            (
                "1-4",
                "QK7RM2VTX8D45",
                None,
                escapement_verge_probe::batch::Settled::Skipped,
                Some("needs the ISP jumper. NXP's firmware V3.108 has no jumper-free update"),
            ),
        ] {
            m.queued.push_back(Event::Batch(BatchEvent::Record(Box::new(
                escapement_verge_probe::batch::Record {
                    time: "2026-10-20T14:26:08Z".into(),
                    port: port.into(),
                    serial_before: Some(before.into()),
                    firmware_before: "NXP's firmware V3.172".into(),
                    serial_after: after.map(Into::into),
                    firmware_after: after.map(|_| "Verge, build v0.3.0".into()),
                    result,
                    reason: reason.map(Into::into),
                    seconds: None,
                },
            ))));
        }
    }
    h.run_steps(3);
    h.get_by_label("Needs the ISP jumper. NXP's firmware V3.108 has no jumper-free update.");
    h.get_by_label("QK7RM2VTX8D41 → CJ3E7A91C2");
    h.get_by_label("NXP's firmware V3.172 → Verge, build v0.3.0");
}

/// The log's bar counts its warnings and errors in words, each in the plural it takes.
#[test]
fn the_log_bar_counts_warnings_and_errors_in_words() {
    let mut h = harness(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    h.get_by_label("0 warnings · 0 errors");
}

/// When the USB worker stops, the strip says so once on every view, and Bench does not wait for a
/// connection that cannot come.
#[test]
fn a_stopped_worker_is_reported_once_on_every_view() {
    let (app, backend) = manual_app("ours-pro", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::WorkerFailed(crate::worker::JobError {
            message: "the USB worker stopped: boom".into(),
            advice: Some(
                "Quit and start Verge again. If a flash was under way, flash the probe again."
                    .into(),
            ),
            left_in_isp: true,
            stopped: None,
        }));
    h.step();
    let notice = "The USB worker stopped: boom. Quit and start Verge again. If a flash was under way, flash the probe again.";
    for view in ["Probes", "Batch", "Bench"] {
        press(&mut h, view);
        h.run_steps(3);
        assert_eq!(h.query_all_by_label(notice).count(), 1, "{view}");
        assert!(
            h.query_by_label_contains("Could not list USB devices")
                .is_none(),
            "{view}"
        );
    }
    h.get_by_label("Not connected: the USB worker stopped.");
    assert!(h.query_by_label("Connecting…").is_none());
}

/// After a failed look at USB, the units listed may have changed, so nothing is offered for them
/// until a look succeeds.
#[test]
fn a_failed_scan_offers_no_update() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Rescan");
    backend.lock().unwrap().queued.push_back(Event::Units(Err(
        "could not list USB devices: access denied".into(),
    )));
    h.run_steps(3);
    h.get_by_label("Could not list USB devices: access denied.");
    h.get_by_label("As of the last look at USB");
    assert!(h.query_by_label("Update to v0.3.0").is_none());
    assert!(
        h.get_by_label("Cannot update")
            .accesskit_node()
            .is_disabled()
    );
    h.get_by_label("The last look at USB failed. Rescan to try again.");
    assert!(h.query_by_label("Enter ISP mode").is_none());
}

/// A batch paused on a unit that failed in ISP mode says how to flash it from the window, not
/// with a command.
#[test]
fn a_batch_paused_on_a_failed_unit_says_how_the_window_flashes_it() {
    use crate::worker::{Backend, Request};
    use escapement_verge_probe::batch::{Board as BatchBoard, Filter, OursFilter, StockFilter};
    use escapement_verge_probe::{Cancel, Source};

    let bus = SimBus::default();
    let unit = bus.plug_in_later(
        Duration::from_secs(1),
        SimFirmware::Stock {
            version: StockVersion::parse("3.172").unwrap(),
        },
    );
    bus.set_fail_writes(unit, true);
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
        source: Source::Release(Some("v0.3.0".into())),
        filter: Filter {
            board: BatchBoard::McuLink,
            stock: StockFilter::Any,
            ours: OursFilter::Any,
            bootloaders: false,
            attached: false,
        },
        download: Download::Agreed,
        limit: None,
        record: record_dir("paused-failed").join("record.jsonl"),
        stop: stop.clone(),
        cancel: Cancel::new(),
    });
    for _ in 0..800 {
        while let Some(event) = worker.try_recv() {
            if let Event::Batch(BatchEvent::State(state)) = event
                && let Some(paused) = state.paused
            {
                stop.cancel();
                assert!(
                    paused.contains(
                        "stop the batch, then pick the probe in ISP mode under Probes and flash it with release v0.3.0."
                    ),
                    "{paused}"
                );
                assert!(!paused.contains("verge isp flash"), "{paused}");
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.cancel();
    panic!("the batch did not pause on the failed unit");
}

/// Bench restarts a unit running Verge after asking, as `verge restart` does, and gives a
/// bench marker's pins back to DIO.
#[test]
fn bench_restarts_a_unit_after_asking() {
    let bus = SimBus::scenario("ours-plain").unwrap();
    bus.mark_dio(0, 0x0F);
    let mut h = harness(sim_app(bus.clone(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until_contains(&mut h, "Restart the probe to give them back.");
    press(&mut h, "Restart");
    h.get_by_label_contains("Restart CJ3E7A91C2? Its DIO pins go back to inputs");
    assert!(!bus.log().iter().any(|l| l.contains("control 0x40 0x03")));
    // The confirm, named after the button.
    let confirm = h
        .get_all_by_label("Restart")
        .find(|n| n.is_focused())
        .expect("the confirm has the focus");
    confirm.click();
    h.step();
    until(&mut h, "DIO3 output");
    assert!(
        bus.log()
            .iter()
            .any(|l| l.contains("control 0x40 0x03 wValue 0"))
    );
}

/// The release picked by its tag, as `verge update --release` takes one.
#[test]
fn a_release_can_be_picked_by_its_tag() {
    let http = SimHttp::new(repo());
    http.publish_ours("v0.2.0");
    http.publish_ours("v0.3.0");
    let key = http.public_key();
    let mut h = harness(app_on(
        SimBus::scenario("ours").unwrap(),
        GitHub::new(http, repo(), None).with_key(Some(key)),
        vec![],
    ));
    until(&mut h, "Update to v0.3.0");
    h.get_by_label_contains("latest v0.3.0 ·");
    h.get_by_label("Tag").click();
    h.step();
    h.get_by_role(Role::TextInput).type_text("v0.2.0");
    h.step();
    press(&mut h, "Look up");
    until(&mut h, "Update to v0.2.0");
    h.get_by_label_contains("release v0.2.0 ·");
}

/// A unit in ISP mode can leave it without being flashed, as `verge isp reset` does; and an update
/// that left the unit there with its jumper fitted offers the same.
#[test]
fn a_unit_in_isp_mode_is_reset_out_of_it() {
    let mut h = harness(sim_app(SimBus::scenario("isp-ours").unwrap(), vec![]));
    until(&mut h, "Reset out of ISP mode");
    press(&mut h, "Reset out of ISP mode");
    until(&mut h, "Done");
    h.get_by_label_contains("runs Verge, build v0.3.0");

    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    press(&mut h, "Confirm");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Install(InstallEvent::Finished(Ok(
            Outcome::JumperStillFitted,
        ))));
    until(&mut h, "Reset out of ISP mode");
    h.get_by_label_contains("take it off, then reset it out of ISP mode");
    press(&mut h, "Reset out of ISP mode");
    assert_eq!(
        backend.lock().unwrap().sent.last().unwrap(),
        "reset Some(\"1-1\")"
    );
}

/// Bench shows the probe's energy total, and a capture that failed before the window connected,
/// with what it measured.
#[test]
fn bench_shows_the_energy_total_and_a_failed_capture() {
    let mut h = harness(sim_app(
        SimBus::scenario("ours-pro-energy").unwrap(),
        vec![],
    ));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until_contains(&mut h, "total ");
    h.get_by_label_contains(" samples since");

    let mut h = harness(sim_app(
        SimBus::scenario("ours-pro-energy-failed").unwrap(),
        vec![],
    ));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until_contains(&mut h, "The last capture failed after ");
    h.get_by_label_contains(
        "the ADC overran, its DMA failed or the probe fell behind reading it. Starting a capture clears it.",
    );
}

/// Closing while Bench finishes a recording says what it waits for: the recording's save, or a
/// file dialog only while one is open.
#[test]
fn closing_says_what_bench_waits_for() {
    let (app, _backend) = manual_app("ours-pro", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    h.state_mut()
        .bench_mut()
        .hold(false, Some(std::env::temp_dir().join("held.tickwave")));
    request_close(&mut h);
    h.step();
    h.get_by_label("Closing once the recording is saved.");
    assert!(h.query_by_label_contains("file dialog").is_none());

    let (app, _backend) = manual_app("ours-pro", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    h.state_mut().bench_mut().hold(true, None);
    request_close(&mut h);
    h.step();
    h.get_by_label("Closing once the file dialog is closed. Close it to continue.");
}

/// The step under way when an update failed is marked failed, not done, and the headline says
/// what the failure left: nothing flashed before the bootloader was reached, a unit that did not
/// come back after.
#[test]
fn a_failure_marks_its_step_and_says_what_it_left() {
    use escapement_verge_probe::progress::Step;
    let failed = |steps: &[Step], message: &str| {
        let (app, backend) = manual_app("ours", true);
        let mut h = harness(app);
        until(&mut h, "Update to v0.3.0");
        press(&mut h, "Update to v0.3.0");
        press(&mut h, "Confirm");
        {
            let mut m = backend.lock().unwrap();
            for &step in steps {
                m.queued
                    .push_back(Event::Progress(ProgressEvent::Start { step, total: 0 }));
            }
            m.queued
                .push_back(Event::Install(InstallEvent::Finished(Err(
                    crate::worker::JobError {
                        message: message.into(),
                        advice: None,
                        left_in_isp: false,
                        stopped: None,
                    },
                ))));
        }
        until(&mut h, "Failed");
        h
    };
    let h = failed(&[Step::Download], "the release is built for another board");
    h.get_by_label("Nothing was flashed");
    h.get_by_label("Download the release: failed");

    let h = failed(
        &[
            Step::EnterBootloader,
            Step::WaitForBootloader,
            Step::Connect,
            Step::Erase,
            Step::Write,
            Step::Verify,
            Step::Reset,
            Step::WaitForFirmware,
        ],
        "the probe did not come back within 10 s",
    );
    h.get_by_label("The probe did not come back");
    h.get_by_label("Wait for the firmware: failed");
    // The steps before it are done.
    assert!(h.query_by_label("Write: failed").is_none());
}

/// Bench asks before starting a capture in its own panel, with the rest of the panel held until
/// the answer; Escape answers Back.
#[test]
fn bench_asks_inline_and_holds_the_panel() {
    let mut h = harness(sim_app(SimBus::scenario("ours-pro").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "Start recording");
    press(&mut h, "Start recording");
    h.get_by_label_contains("Starting clears the probe's energy total.");
    let confirm = h.get_by_label("Start recording");
    assert!(confirm.is_focused());
    for held in ["High range", "Turn on", "DIO3 output", "Restart"] {
        assert!(
            h.get_by_label(held).accesskit_node().is_disabled(),
            "{held}"
        );
    }
    h.key_press(egui::Key::Escape);
    h.step();
    assert!(h.query_by_label_contains("Starting clears").is_none());
    assert!(!h.get_by_label("High range").accesskit_node().is_disabled());
}

/// A capture another program runs is recorded at its own rate, which Bench shows in place of a
/// rate to choose.
#[test]
fn bench_shows_the_rate_of_a_capture_started_elsewhere() {
    let mut h = harness(sim_app(
        SimBus::scenario("ours-pro-energy").unwrap(),
        vec![],
    ));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "Record running capture");
    until_contains(&mut h, "Started elsewhere, at ");
    h.get_by_label("Started elsewhere, at 1000 samples/s");
    for absent in ["25k", "Auto range"] {
        assert!(h.query_by_label(absent).is_none(), "{absent}");
    }
}

/// A unit not running Verge is refused once, in the window's words, with the way to update it.
#[test]
fn bench_refuses_a_unit_not_running_verge_once() {
    let mut h = harness(sim_app(SimBus::scenario("stock").unwrap(), vec![]));
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Bench");
    h.step();
    let refusal = format!("QK7RM2VTX8D41 runs {STOCK} V3.172, not Verge. Bench needs Verge.");
    assert_eq!(h.query_all_by_label(&refusal).count(), 1);
    assert!(h.query_by_label_contains("energy measurement").is_none());
    assert!(h.query_by_label_contains("DIO pins").is_none());
    press(&mut h, "Update under Probes");
    until(&mut h, "Install v0.3.0");

    let mut h = harness(sim_app(SimBus::scenario("isp").unwrap(), vec![]));
    until(&mut h, "Install v0.3.0");
    press(&mut h, "Bench");
    h.step();
    h.get_by_label(
        "LPC55S69 bootloader at 1-1 is a probe in ISP mode. Bench needs it running Verge.",
    );
}

/// A Verge build without Verge Aux is refused for what it runs: reconnecting cannot help, so Bench
/// offers the update and says why once.
#[test]
fn bench_offers_an_update_not_a_reconnect_for_a_build_without_aux() {
    let mut h = harness(sim_app(SimBus::scenario("ours").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "Update under Probes");
    assert!(h.query_by_label("Reconnect").is_none());
    assert_eq!(h.query_all_by_label_contains("has no Verge Aux").count(), 1);
}

/// The batch's slab says what it did: no image or record for a batch that never armed, and a
/// clock that stops when it ends.
#[test]
fn an_ended_batch_says_what_it_did() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Batch(BatchEvent::Ended(Err(
            "making the record file's directory: denied".into(),
        ))));
    until(&mut h, "Not armed");
    h.get_by_label("record file: nothing was recorded");
    assert!(h.query_by_label_contains("loading the image").is_none());
    // The facts line: no image, the units it would have taken and its seconds.
    let facts = |h: &Harness<'_, VergeApp>| {
        let node = h.get_by_label_contains("from now on · ");
        let node = node.accesskit_node();
        node.label().or_else(|| node.value()).map(|l| l.to_string())
    };
    let before = facts(&h);
    assert!(
        before.as_deref().is_some_and(|f| f.starts_with("every ")),
        "{before:?}"
    );
    for _ in 0..90 {
        h.step();
    }
    assert_eq!(facts(&h), before, "the clock stopped");
}

/// After Stop, with no unit under way, there is nothing to cancel.
#[test]
fn stopping_between_units_offers_no_cancel() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Batch");
    press(&mut h, "Arm batch");
    press(&mut h, "Confirm");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Batch(BatchEvent::Armed {
            name: "release v0.3.0 (mculink-verge-v0.3.0.s19)".into(),
            filter: "every unit".into(),
        }));
    h.step();
    press(&mut h, "Stop");
    h.get_by_label("Stopping: no probe under way.");
    assert!(h.query_by_label("Cancel the probe under way").is_none());
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Batch(BatchEvent::Starting("1-1".into())));
    h.step();
    h.get_by_label("Cancel the probe under way");
}

/// Bench offers what the selected unit allows: no recording to start where it cannot record,
/// and its DIO pins are connecting rather than missing while it connects.
#[test]
fn bench_placeholders_offer_what_the_unit_allows() {
    let mut h = harness(sim_app(SimBus::default(), vec![]));
    until(&mut h, "Plug in an MCU-Link");
    press(&mut h, "Bench");
    h.get_by_label("Open a recording.");
    assert!(h.query_by_label_contains("Start a recording").is_none());

    let (app, _backend) = manual_app("ours-pro", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    // The hand-driven worker never answers the connection.
    h.run_steps(3);
    assert_eq!(h.query_all_by_label("Connecting…").count(), 2);
    assert!(
        h.query_by_label_contains("Select a Pro or standalone")
            .is_none()
    );
}

/// NXP's firmware too old to ask is named as what its USB strings say it is.
#[test]
fn nxps_firmware_too_old_to_ask_is_named() {
    let mut h = harness(sim_app(SimBus::scenario("stock-hid").unwrap(), vec![]));
    until_contains(&mut h, "Too old to ask what it runs");
    h.get_by_role_and_label(Role::Label, STOCK);
    h.get_by_label("V2.245");
    assert!(h.query_by_label("Unknown").is_none());
}

/// A unit another program holds is looked at again with Rescan, which the window names.
#[test]
fn a_busy_unit_is_looked_at_again_with_rescan() {
    let mut h = harness(sim_app(SimBus::scenario("busy").unwrap(), vec![]));
    until_contains(&mut h, "then press Rescan");
    assert!(h.query_by_label_contains("look again").is_none());
}

/// A file dropped while an update runs is answered in the Working view, and not after.
#[test]
fn a_file_dropped_during_an_update_is_answered_at_once() {
    let (app, backend) = manual_app("ours", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Update to v0.3.0");
    press(&mut h, "Confirm");
    drop_file(&mut h, &std::env::temp_dir().join("dropped.s19"));
    h.get_by_label_contains("dropped.s19 arrived while an update was running, and was ignored.");
    backend
        .lock()
        .unwrap()
        .queued
        .push_back(Event::Install(InstallEvent::Finished(Ok(
            Outcome::JumperStillFitted,
        ))));
    until(&mut h, "Flashed");
    press(&mut h, "Back");
    assert!(h.query_by_label_contains("arrived while").is_none());
}

/// On a wide window the Units view keeps its column short enough to read.
#[test]
fn the_units_column_stays_readable_on_a_wide_window() {
    let mut h = Harness::builder()
        .with_size(vec2(2560.0, 1440.0))
        .with_context_setup(still)
        .build_ui_state(
            |ui, app: &mut VergeApp| app.show(ui),
            sim_app(SimBus::scenario("bench").unwrap(), vec![]),
        );
    until(&mut h, "Update to v0.3.0");
    // The release's line is long enough to fill any column it is given.
    let release = h.get_by_label_contains("signed in").rect();
    assert!(release.right() < 300.0 + 24.0 + 960.0 + 1.0, "{release:?}");
}

/// Held still, the window's own animations finish at once as well as the mark's.
#[test]
fn reduced_motion_stops_egui_animating_too() {
    let ctx = egui::Context::default();
    crate::mark::set_motion(&ctx, false);
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        assert_eq!(ctx.style_of(theme).animation_time, 0.0);
    }
    crate::mark::set_motion(&ctx, true);
    assert!(ctx.global_style().animation_time > 0.0);
}

/// Each log line has its time, and Copy takes every line the log keeps.
#[test]
fn the_log_has_times_and_copies_whole() {
    let log = Log::default();
    log.push(tracing::Level::INFO, "first".into());
    log.push(tracing::Level::ERROR, "second".into());
    let text = log.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    for (line, words) in lines.iter().zip(["INFO  first", "ERROR second"]) {
        let (time, rest) = line.split_once(' ').unwrap();
        assert!(
            time.len() == 8 && time.as_bytes()[2] == b':' && time.as_bytes()[5] == b':',
            "{line}"
        );
        assert_eq!(rest, words);
    }
    let worker = Worker::spawn(
        SimBus::default(),
        GitHub::new(SimHttp::new(repo()), repo(), None),
        || {},
    );
    let app = VergeApp::new(
        Box::new(worker),
        log,
        Config {
            repo: repo(),
            simulated: None,
            stock_dirs: vec![],
            record_dir: std::env::temp_dir(),
            cache: None,
        },
    );
    // What each frame copies, kept: a click's frame is not the last of the step that makes it.
    let mut h = Harness::builder()
        .with_size(vec2(1080.0, 700.0))
        .with_context_setup(still)
        .build_ui_state(
            |ui, (app, copied): &mut (VergeApp, Vec<String>)| {
                app.show(ui);
                copied.extend(ui.ctx().output(|o| {
                    o.commands
                        .iter()
                        .filter_map(|command| match command {
                            egui::OutputCommand::CopyText(text) => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                }));
            },
            (app, Vec::new()),
        );
    h.step();
    h.get_by_label("Show").click();
    h.run_steps(2);
    h.get_by_label("Copy").click();
    h.run_steps(2);
    assert_eq!(h.state().1, [text]);
}

/// A unit row's hover text says what the row leaves out: how the unit updates.
#[test]
fn a_unit_rows_hover_says_how_it_updates() {
    let mut h = harness(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    h.get_by_role_and_label(Role::ListBoxOption, "QK7RM2VTX8D41")
        .hover();
    h.run_steps(60);
    // Not the state the row already gives.
    let unit = escapement_verge_probe::detect(&SimBus::scenario("bench").unwrap())
        .unwrap()
        .remove(1);
    assert_eq!(unit.serial(), Some("QK7RM2VTX8D41"));
    assert!(h.query_by_label(&unit.state.to_string()).is_none());
    assert_eq!(
        h.query_all_by_label("Updates without the jumper").count(),
        1,
        "the hover alone: the selected unit's slab names its board too"
    );
}

/// Tab goes through the strip in the order it shows, then the view, then the log.
#[test]
fn tab_goes_through_the_window_in_order() {
    let mut h = harness(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    let mut order = Vec::new();
    for _ in 0..40 {
        h.key_press(egui::Key::Tab);
        h.run_steps(2);
        let focused = h
            .query_all(egui_kittest::kittest::by())
            .find(|n| n.is_focused())
            .and_then(|n| n.accesskit_node().label().map(|l| l.to_string()));
        if let Some(label) = focused {
            if order.first() == Some(&label) {
                break;
            }
            order.push(label);
        }
    }
    let at = |label: &str| {
        order
            .iter()
            .position(|l| l == label)
            .unwrap_or_else(|| panic!("{label} in {order:?}"))
    };
    assert_eq!(&order[..5], ["Probes", "Batch", "Bench", "Rescan", "About"]);
    assert!(at("Update to v0.3.0") < at("Show"), "{order:?}");
    assert_eq!(order.last().map(String::as_str), Some("Show"), "{order:?}");
    assert!(!order.iter().any(|l| l == "Resize panel"), "{order:?}");
}

/// Escape closes About.
#[test]
fn escape_closes_about() {
    let mut h = harness(sim_app(SimBus::scenario("bench").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "About");
    h.get_by_label("Verge's icon");
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.query_by_label("Verge's icon").is_none());
}

/// Where DIO is unavailable, Bench says a recording holds no DIO pins.
#[test]
fn a_recording_without_dio_says_so() {
    let mut h = harness(sim_app(SimBus::scenario("ours-pro-j15").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until_contains(&mut h, "take J15 off");
    h.get_by_label("DIO is unavailable, so a recording holds no DIO pins.");
}

/// A standalone MCU-Link records its DIO pins alone, as `verge dio record` does.
#[test]
fn bench_records_a_standalone_units_dio_pins() {
    let dir = record_dir("dio-record");
    let worker = Worker::spawn(
        SimBus::scenario("ours-plain").unwrap(),
        GitHub::new(SimHttp::new(repo()), repo(), None),
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
    let mut h = harness(app);
    until(&mut h, "Bench");
    press(&mut h, "Bench");
    until(&mut h, "Start recording");
    press(&mut h, "Start recording");
    until(&mut h, "Stop recording");
    for _ in 0..40 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    press(&mut h, "Stop recording");
    until(&mut h, "Start recording");
    until_contains(&mut h, "DIO3 edges");
    // A stopped recording is compacted into a copy beside it, which then replaces it.
    for _ in 0..400 {
        if !h.state_mut().bench_mut().compacting() {
            break;
        }
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!h.state_mut().bench_mut().compacting());
    let kept: Vec<_> = std::fs::read_dir(dir.join("captures"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A unit running a firmware the tool asked enters ISP mode after asking, as `verge isp enter`
/// puts it there, and waits there for a flash or a reset.
#[test]
fn a_unit_enters_isp_mode_after_asking() {
    let bus = SimBus::scenario("ours").unwrap();
    let mut h = harness(sim_app(bus.clone(), vec![]));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Enter ISP mode");
    h.get_by_label_contains("Restart CJ3E7A91C2 at 1-1 into ISP mode?");
    assert!(h.query_by_label("Update to v0.3.0").is_none());
    let confirm = h.get_by_label("Enter ISP mode");
    assert!(confirm.is_focused());
    confirm.click();
    h.step();
    until(&mut h, "In ISP mode");
    h.get_by_label_contains(&format!(
        "In ISP mode the app cannot tell the probe's board: flash it with an image built for {}.",
        Board::McuLink.a()
    ));
    press(&mut h, "Back");
    until(&mut h, "Reset out of ISP mode");
    h.get_by_label_contains("LPC55S69 bootloader");
}

/// The window enters ISP mode by the rules `verge isp enter` refuses by: a board other than the
/// standalone MCU-Link only for NXP's firmware, only by a jumper-free path, and only for a unit
/// running a firmware it could ask.
#[test]
fn entering_isp_mode_follows_the_rules_isp_enter_refuses_by() {
    let dir = record_dir("enter-stock");
    std::fs::write(
        dir.join("MCU-LINK_CMSIS-DAP_V3_172.s19"),
        firmware_srecord(&SimFirmware::Stock {
            version: StockVersion::parse("3.172").unwrap(),
        }),
    )
    .unwrap();
    let (app, _backend) = manual_app("ours-pro", true);
    let mut h = harness(app);
    until(&mut h, "Update to v0.3.0");
    assert!(
        h.get_by_label("Enter ISP mode")
            .accesskit_node()
            .is_disabled(),
        "a Pro, with a release chosen"
    );
    // With NXP's firmware chosen, which goes on any board.
    let units: Vec<Unit> =
        escapement_verge_probe::detect(&SimBus::scenario("ours-pro").unwrap()).unwrap();
    let instance = units[0].device.instance.clone();
    let mut manual = Manual::default();
    manual.queued.push_back(Event::Units(Ok(units)));
    let backend: Shared = Arc::new(Mutex::new(manual));
    let app = VergeApp::new(
        Box::new(backend.clone()),
        Log::default(),
        Config {
            repo: repo(),
            simulated: None,
            stock_dirs: vec![dir.clone()],
            record_dir: std::env::temp_dir(),
            cache: None,
        },
    );
    let mut h = harness(app);
    until_contains(&mut h, "MCU-Link Pro");
    press(&mut h, STOCK);
    press(&mut h, "Enter ISP mode");
    let entered = |backend: &Shared| {
        backend
            .lock()
            .unwrap()
            .sent
            .iter()
            .filter(|s| s.starts_with("enter"))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(entered(&backend).is_empty(), "nothing before the answer");
    press(&mut h, "Enter ISP mode");
    assert_eq!(entered(&backend), [format!("enter {instance} stock true")]);

    // No jumper-free path into its bootloader.
    let mut h = harness(sim_app(SimBus::scenario("stock-old").unwrap(), vec![]));
    until(&mut h, "Needs the jumper");
    assert!(
        h.get_by_label("Enter ISP mode")
            .accesskit_node()
            .is_disabled()
    );
    // Held by a debugger: not a firmware the tool could ask over CMSIS-DAP.
    let mut h = harness(sim_app(
        SimBus::scenario("ours-pro-debugger").unwrap(),
        vec![],
    ));
    until_contains(&mut h, "Close the debugger first.");
    assert!(h.query_by_label("Enter ISP mode").is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Bench shows a Pro's calibration points, as `verge energy calibration` reads them.
#[test]
fn bench_shows_the_calibration_points() {
    let mut h = harness(sim_app(
        SimBus::scenario("ours-pro-energy").unwrap(),
        vec![],
    ));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "Calibration");
    press(&mut h, "Calibration");
    for label in ["Low range", "High range", "code 31200", "code 49600"] {
        h.get_by_label(label);
    }
    // Without the energy circuit there is nothing calibrated.
    let mut h = harness(sim_app(
        SimBus::scenario("ours-pro-unfitted").unwrap(),
        vec![],
    ));
    until(&mut h, "Update to v0.3.0");
    press(&mut h, "Bench");
    until(&mut h, "Turn on");
    assert!(h.query_by_label("Calibration").is_none());
}

/// For a unit the tool could not ask, the window gives its own notes from the unit's state: the
/// way to look again is Rescan, offered only where asking again can find a jumper-free path.
#[test]
fn the_window_builds_the_notes_of_a_unit_it_could_not_ask() {
    let mut h = harness(sim_app(SimBus::scenario("busy").unwrap(), vec![]));
    until_contains(&mut h, "then press Rescan");
    h.get_by_label("The app could not ask this probe what it runs: another program has it open.");
    h.get_by_label(
        "Close whatever has it open, then press Rescan to look for the jumper-free path again.",
    );
    let mut h = harness(sim_app(SimBus::scenario("stock-hid").unwrap(), vec![]));
    until_contains(&mut h, "The app could not ask this probe what it runs");
    assert!(h.query_by_label_contains("press Rescan").is_none());
}

/// The question before flashing a unit in ISP mode says what a file is for as `verge isp flash`
/// does: nothing for NXP's own file, which goes on any board, and what verge takes a file that
/// declares no boards to be.
#[test]
fn the_question_says_what_a_file_is_for_as_the_command_line_does() {
    let dir = record_dir("question-files");
    let nxp = dir.join("MCU-LINK_CMSIS-DAP_V3_172.s19");
    std::fs::write(
        &nxp,
        firmware_srecord(&SimFirmware::Stock {
            version: StockVersion::parse("3.172").unwrap(),
        }),
    )
    .unwrap();
    let plain = dir.join("plain.s19");
    std::fs::write(
        &plain,
        firmware_srecord(&SimFirmware::Ours {
            build: "v0.3.0".into(),
        }),
    )
    .unwrap();
    let mut h = harness(sim_app(SimBus::scenario("isp").unwrap(), vec![]));
    until(&mut h, "Install v0.3.0");
    drop_file(&mut h, &nxp);
    until(&mut h, "Flash MCU-LINK_CMSIS-DAP_V3_172.s19");
    press(&mut h, "Flash MCU-LINK_CMSIS-DAP_V3_172.s19");
    h.get_by_label_contains("LPC55S69 board. Flash MCU-LINK_CMSIS-DAP_V3_172.s19 onto");
    assert!(h.query_by_label_contains("The image").is_none());
    press(&mut h, "Back");
    drop_file(&mut h, &plain);
    until(&mut h, "Flash plain.s19");
    press(&mut h, "Flash plain.s19");
    h.get_by_label_contains(&format!(
        "The image declares no boards, so verge takes it as a Verge image for the {} only. Flash plain.s19",
        Board::McuLink
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The release's line names its signature and checksum files without claiming a check made
/// before the download.
#[test]
fn the_release_line_claims_no_check() {
    let mut h = harness(sim_app(SimBus::scenario("ours").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    h.get_by_label_contains(
        "signed in mculink-verge-v0.3.0.s19.minisig, checksum in mculink-verge-v0.3.0.s19.sha256",
    );
    assert!(h.query_by_label_contains("checked against").is_none());
}

/// NXP's own LinkServer file, picked as a file, goes on any board as NXP's firmware does: on a Pro
/// the window offers to flash it, and a batch for the Pro arms with it, as the command line does.
#[test]
fn nxps_own_file_goes_on_a_pro() {
    let dir = record_dir("nxp-file-on-a-pro");
    let file = dir.join("MCU-LINK_CMSIS-DAP_V3_172.s19");
    std::fs::write(
        &file,
        firmware_srecord(&SimFirmware::Stock {
            version: StockVersion::parse("3.172").unwrap(),
        }),
    )
    .unwrap();
    let mut h = harness(sim_app(SimBus::scenario("ours-pro").unwrap(), vec![]));
    until(&mut h, "Update to v0.3.0");
    drop_file(&mut h, &file);
    until(&mut h, "Flash MCU-LINK_CMSIS-DAP_V3_172.s19");
    assert!(h.query_by_label("Not for this board").is_none());

    press(&mut h, "Batch");
    press(&mut h, "MCU-Link Pro");
    // Only units running NXP's firmware, which a file of NXP's firmware dates.
    press_as(&mut h, Role::CheckBox, "Verge");
    until(&mut h, "Arm batch");
    assert!(h.query_by_label("Cannot arm").is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A cancel's headline says what it left on the unit, as the message under it does: never that
/// the unit did not come back, or that it is still in ISP mode, when it is restarting. The step it
/// stopped is not marked failed.
#[test]
fn a_cancel_is_headlined_by_what_it_left() {
    use crate::worker::Stopped;
    use escapement_verge_probe::progress::{Left, Step};
    for (stopped, words) in [
        (Stopped::Unchanged, "Nothing was changed"),
        (
            Stopped::Left(Left::Flashed),
            "Restarting into the new image",
        ),
        (
            Stopped::Left(Left::Reset),
            "Restarting into the image it holds",
        ),
        (
            Stopped::Left(Left::InIsp),
            "In ISP mode, with the image it held",
        ),
        (
            Stopped::Left(Left::Partial),
            "In ISP mode, with a partial image",
        ),
    ] {
        let (app, backend) = manual_app("ours", true);
        let mut h = harness(app);
        until(&mut h, "Update to v0.3.0");
        press(&mut h, "Update to v0.3.0");
        press(&mut h, "Confirm");
        {
            let mut m = backend.lock().unwrap();
            m.queued.push_back(Event::Progress(ProgressEvent::Start {
                step: Step::Write,
                total: 0,
            }));
            m.queued
                .push_back(Event::Install(InstallEvent::Finished(Err(
                    crate::worker::JobError {
                        message: "stopped".into(),
                        advice: None,
                        left_in_isp: false,
                        stopped: Some(stopped),
                    },
                ))));
        }
        until(&mut h, "Stopped");
        h.get_by_label(words);
        for wrong in [
            "Failed",
            "The probe did not come back",
            "The update did not finish",
        ] {
            assert!(h.query_by_label(wrong).is_none(), "{stopped:?}: {wrong}");
        }
        assert!(
            h.query_by_label_contains(": failed").is_none(),
            "{stopped:?}: the stopped step is marked failed"
        );
    }
}

/// A reset with no unit in ISP mode is not reported as leaving one there.
#[test]
fn a_reset_with_nothing_in_isp_mode_leaves_nothing_in_it() {
    use crate::worker::{Backend, Request};
    use escapement_verge_probe::Cancel;
    let mut worker = Worker::spawn(
        SimBus::default(),
        GitHub::new(SimHttp::new(repo()), repo(), None),
        || {},
    );
    worker.send(Request::Reset {
        port: None,
        cancel: Cancel::new(),
    });
    for _ in 0..800 {
        while let Some(event) = worker.try_recv() {
            if let Event::Install(InstallEvent::Finished(result)) = event {
                let error = result.expect_err("nothing to reset");
                assert!(!error.left_in_isp, "{error:?}");
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the reset did not finish");
}

/// Bench offers a restart to a unit running Verge that another program holds, as `verge restart`
/// sends one: the request goes to the unit's USB core, past the program holding CMSIS-DAP.
#[test]
fn bench_offers_a_restart_to_a_unit_another_program_holds() {
    let mut h = harness(sim_app(SimBus::scenario("busy").unwrap(), vec![]));
    until_contains(&mut h, "then press Rescan");
    press(&mut h, "Bench");
    until(&mut h, "Restart");
}
