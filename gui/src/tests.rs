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
use updater_core::Unit;
use updater_core::release::{GitHub, Repo};
use updater_core::sim::http::SimHttp;
use updater_core::sim::{SimBus, SimFirmware, firmware_srecord};
use updater_core::update::Outcome;
use updater_core::version::StockVersion;

use crate::app::{Config, UpdaterApp};
use crate::log::Log;
use crate::style::{self, color};
use crate::worker::{Event, Manual, Shared, Worker};

fn repo() -> Repo {
    "someone/mculink-cjtag-fw".parse().unwrap()
}

fn sim_app(bus: SimBus, stock_dirs: Vec<PathBuf>) -> UpdaterApp {
    let http = SimHttp::new(repo());
    http.publish_ours("v0.3.0");
    let worker = Worker::spawn(bus, GitHub::new(http, repo(), None), || {});
    UpdaterApp::new(
        Box::new(worker),
        Log::default(),
        Config {
            repo: repo(),
            simulated: Some("test".into()),
            stock_dirs,
        },
    )
}

fn harness(app: UpdaterApp) -> Harness<'static, UpdaterApp> {
    Harness::builder()
        .with_size(vec2(1080.0, 700.0))
        .with_context_setup(style::install)
        .build_ui_state(|ui, app: &mut UpdaterApp| app.show(ui), app)
}

/// Steps until a widget labelled `label` shows, or fails after 400 frames.
fn until(h: &mut Harness<'_, UpdaterApp>, label: &str) {
    for _ in 0..400 {
        h.step();
        if h.query_by_label(label).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no {label:?} after 400 frames");
}

fn until_contains(h: &mut Harness<'_, UpdaterApp>, label: &str) {
    for _ in 0..400 {
        h.step();
        if h.query_by_label_contains(label).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("nothing containing {label:?} after 400 frames");
}

fn press(h: &mut Harness<'_, UpdaterApp>, label: &str) {
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
    let dir = std::env::temp_dir().join(format!("updater-gui-stock-{}", std::process::id()));
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
fn manual_app(scenario: &str, release: bool) -> (UpdaterApp, Shared) {
    let bus = SimBus::scenario(scenario).unwrap();
    let units: Vec<Unit> = updater_core::detect(&bus).unwrap();
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
    let app = UpdaterApp::new(
        Box::new(backend.clone()),
        Log::default(),
        Config {
            repo: repo(),
            simulated: None,
            stock_dirs: vec![],
        },
    );
    (app, backend)
}

fn root_commands(h: &Harness<'_, UpdaterApp>) -> Vec<ViewportCommand> {
    h.output()
        .viewport_output
        .get(&ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

fn request_close(h: &mut Harness<'_, UpdaterApp>) {
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
/// `UPDATER_GUI_SHOTS=<dir> cargo test -p updater-gui pictures -- --ignored`
#[test]
#[ignore = "renders with wgpu into $UPDATER_GUI_SHOTS"]
fn pictures() {
    let dir = PathBuf::from(std::env::var("UPDATER_GUI_SHOTS").expect("UPDATER_GUI_SHOTS"));
    std::fs::create_dir_all(&dir).unwrap();
    let shoot = |h: &mut Harness<'_, UpdaterApp>, name: &str| {
        h.step();
        let image = h.render().expect("wgpu renders the window");
        image.save(dir.join(format!("{name}.png"))).unwrap();
    };
    let rendered = |app: UpdaterApp| {
        Harness::builder()
            .with_size(vec2(1080.0, 700.0))
            .with_context_setup(style::install)
            .wgpu()
            .build_ui_state(|ui, app: &mut UpdaterApp| app.show(ui), app)
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
        use updater_core::progress::Step;
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
            "release v0.3.0 (mculink-cjtag-fw-v0.3.0.s19)".into(),
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
