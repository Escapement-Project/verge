//! Batches on the simulated bench, with units plugged in and out over simulated time. Every run
//! is bounded in the bus's time; none takes real time to time out.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use verge_core::batch::{
    Armed, Batch, Board, Entry, Filter, Observer, OursFilter, Record, Settled, Status, StockFilter,
    Verdict, judge,
};
use verge_core::bus::{BusError, UsbDevice};
use verge_core::dap::Ours;
use verge_core::progress::Progress;
use verge_core::sim::{SimBus, SimDap, SimFirmware, SimRom, firmware_image};
use verge_core::source::ImageVersion;
use verge_core::update::Timing;
use verge_core::version::StockVersion;
use verge_core::{Bus, Cancel};

#[derive(Default)]
struct Seen {
    records: Vec<Record>,
    started: Vec<String>,
    pauses: Vec<Option<String>>,
    unplugs: Vec<(String, bool)>,
}

impl Progress for Seen {}

impl Observer for Seen {
    fn starting(&mut self, entry: &Entry) {
        self.started.push(entry.port.clone());
    }
    fn record(&mut self, record: &Record) {
        self.records.push(record.clone());
    }
    fn unplug(&mut self, port: &str, blinking: bool) {
        self.unplugs.push((port.to_owned(), blinking));
    }
    fn paused(&mut self, reason: Option<&str>) {
        self.pauses.push(reason.map(str::to_owned));
    }
}

/// The simulated bench with hooks: on every `open_dap`, and a say in whether `devices` fails.
struct Hooked<'a> {
    bus: &'a SimBus,
    /// When each `open_dap` came, in the bus's time, and on which instance.
    opened: RefCell<Vec<(Duration, String)>>,
    on_open: Box<dyn Fn(&UsbDevice) + 'a>,
    fail: Box<dyn Fn() -> bool + 'a>,
}

impl<'a> Hooked<'a> {
    fn new(bus: &'a SimBus) -> Hooked<'a> {
        Hooked {
            bus,
            opened: RefCell::new(Vec::new()),
            on_open: Box::new(|_| {}),
            fail: Box::new(|| false),
        }
    }
}

impl Bus for Hooked<'_> {
    type Dap = SimDap;
    type Isp = SimRom;

    fn devices(&self) -> Result<Vec<UsbDevice>, BusError> {
        if (self.fail)() {
            return Err(BusError::Enumerate("the test's bus failed".into()));
        }
        self.bus.devices()
    }

    fn open_dap(&self, device: &UsbDevice) -> Result<SimDap, BusError> {
        self.opened
            .borrow_mut()
            .push((self.bus.now(), device.instance.clone()));
        (self.on_open)(device);
        self.bus.open_dap(device)
    }

    fn open_isp(&self, device: &UsbDevice) -> Result<SimRom, BusError> {
        self.bus.open_isp(device)
    }

    fn now(&self) -> Duration {
        self.bus.now()
    }

    fn sleep(&self, duration: Duration) {
        self.bus.sleep(duration);
    }
}

fn ours(build: &str) -> SimFirmware {
    SimFirmware::Ours {
        build: build.to_owned(),
    }
}

fn stock(version: &str) -> SimFirmware {
    SimFirmware::Stock {
        version: StockVersion::parse(version).unwrap(),
    }
}

fn release() -> Armed {
    Armed {
        image: firmware_image(&ours("v0.3.0")),
        name: "release v0.3.0".into(),
        version: ImageVersion::Ours("v0.3.0".into()),
    }
}

fn filter() -> Filter {
    Filter {
        board: Board::McuLink,
        stock: StockFilter::Any,
        ours: OursFilter::Older,
        bootloaders: false,
        attached: false,
    }
}

fn arm<B: Bus>(bus: &B, filter: Filter, limit: Option<usize>) -> Batch<'_, B> {
    Batch::arm(
        bus,
        release(),
        filter,
        Timing::default(),
        Cancel::new(),
        Cancel::new(),
        limit,
    )
}

/// Steps until the bus's clock reaches `until`, or the batch finishes.
fn drive<B: Bus>(batch: &mut Batch<'_, B>, bus: &SimBus, seen: &mut Seen, until: Duration) {
    while bus.now() < until && !batch.finished() {
        batch.step(seen).unwrap();
    }
}

fn status<B: Bus>(batch: &Batch<'_, B>, port: &str) -> Status {
    batch
        .entries()
        .iter()
        .find(|e| e.port == port)
        .unwrap_or_else(|| panic!("no entry for {port}: {:?}", batch.entries()))
        .status
        .clone()
}

fn index_of(log: &[String], what: &str) -> usize {
    log.iter()
        .position(|l| l == what)
        .unwrap_or_else(|| panic!("no {what:?} in {log:#?}"))
}

#[test]
fn two_units_arriving_together_go_through_the_bootloader_one_at_a_time() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let a = bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    let b = bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(60));

    assert_eq!(status(&batch, "1-1"), Status::Updated);
    assert_eq!(status(&batch, "1-2"), Status::Updated);
    assert_eq!(seen.started, ["1-1", "1-2"]);
    // Never two bootloaders on the bus: the second unit was asked only once the first was back.
    assert_eq!(bus.max_bootloaders(), 1);
    let log = bus.log();
    let first_back = index_of(
        &log,
        "1-1: enumerates as Firmware(Ours { build: \"v0.3.0\" })",
    );
    let second_asked = index_of(&log, "1-2: DAP 8101");
    assert!(first_back < second_asked, "{log:#?}");
    assert_eq!(batch.tally().updated, 2);
    let _ = (a, b);
}

#[test]
fn a_unit_back_with_its_jumper_fitted_is_not_taken_again() {
    let bus = SimBus::default();
    let mut batch = arm(
        &bus,
        Filter {
            bootloaders: true,
            ..filter()
        },
        None,
    );
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let unit = bus.plug_in_bootloader_later(Duration::from_secs(1));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));

    assert_eq!(status(&batch, "1-1"), Status::JumperFitted);
    let flashes = bus
        .log()
        .iter()
        .filter(|l| l.contains("ROM command 0x04"))
        .count();
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(60));
    assert_eq!(
        bus.log()
            .iter()
            .filter(|l| l.contains("ROM command 0x04"))
            .count(),
        flashes,
        "flashed again"
    );
    assert_eq!(seen.records.len(), 1);
    assert_eq!(seen.records[0].result, Settled::JumperFitted);
    assert!(
        batch.paused().unwrap().contains("jumper fitted"),
        "{:?}",
        batch.paused()
    );
    // Unplugged, the batch resumes.
    bus.unplug(unit);
    batch.step(&mut seen).unwrap();
    assert_eq!(batch.paused(), None);
}

#[test]
fn a_failure_does_not_stop_the_batch_and_its_bootloader_is_named() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let a = bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    bus.set_fail_writes(a, true);
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));

    assert!(
        matches!(status(&batch, "1-1"), Status::Failed(_)),
        "{:?}",
        status(&batch, "1-1")
    );
    assert_eq!(status(&batch, "1-2"), Status::Waiting);
    let paused = batch.paused().unwrap();
    assert!(paused.contains("1-1 failed and is in ISP mode"), "{paused}");
    assert!(paused.contains("verge flash"), "{paused}");
    assert_eq!(seen.records[0].result, Settled::Failed);
    assert!(
        seen.records[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("bootloader")
    );

    bus.unplug(a);
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(60));
    assert_eq!(status(&batch, "1-2"), Status::Updated);
    assert_eq!(bus.max_bootloaders(), 1);
    assert_eq!(batch.tally().updated, 1);
}

#[test]
fn a_unit_already_running_the_image_is_skipped_as_current() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let unit = bus.plug_in_later(Duration::from_secs(1), ours("v0.3.0"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(10));
    assert_eq!(status(&batch, "1-1"), Status::Current);
    assert!(!bus.log().iter().any(|l| l.contains("control")));
    assert_eq!(seen.records[0].result, Settled::Current);
    // It is as finished as an updated unit, so it blinks to be unplugged.
    assert!(bus.host_status_count(unit) > 0);
}

#[test]
fn a_mini_pauses_the_batch_until_it_is_unplugged() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let mini = bus.add_mini_bootloader();
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(10));
    assert!(
        batch.paused().unwrap().contains("mini"),
        "{:?}",
        batch.paused()
    );
    assert_eq!(status(&batch, "1-2"), Status::Waiting);
    assert!(!bus.log().iter().any(|l| l.contains("DAP 8101")));

    bus.unplug(mini);
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(40));
    assert_eq!(status(&batch, "1-2"), Status::Updated);
    assert_eq!(seen.pauses.last(), Some(&None));
}

#[test]
fn units_attached_when_armed_are_left_alone_unless_asked_for() {
    let bus = SimBus::default();
    bus.add_stock("3.172");
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(10));
    assert_eq!(status(&batch, "1-1"), Status::LeftAlone);
    assert!(seen.records.is_empty());

    let bus = SimBus::default();
    bus.add_stock("3.172");
    let mut batch = arm(
        &bus,
        Filter {
            attached: true,
            ..filter()
        },
        None,
    );
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));
    assert_eq!(status(&batch, "1-1"), Status::Updated);
}

#[test]
fn a_count_stops_the_batch_after_that_many_units() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), Some(1));
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(60));
    assert!(batch.finished());
    assert_eq!(batch.tally().updated, 1);
    assert_eq!(status(&batch, "1-2"), Status::Waiting);
}

#[test]
fn stopping_takes_no_new_unit() {
    let bus = SimBus::default();
    let stop = Cancel::new();
    let mut batch = Batch::arm(
        &bus,
        release(),
        filter(),
        Timing::default(),
        stop.clone(),
        Cancel::new(),
        None,
    );
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    stop.cancel();
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    for _ in 0..40 {
        batch.step(&mut seen).unwrap();
    }
    assert!(batch.finished());
    assert!(!bus.log().iter().any(|l| l.contains("DAP 8101")));
}

#[test]
fn the_filter_takes_only_what_it_names() {
    let bus = SimBus::default();
    let mut batch = arm(
        &bus,
        Filter {
            stock: StockFilter::Below(StockVersion::parse("3.172").unwrap()),
            ours: OursFilter::None,
            ..filter()
        },
        None,
    );
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    bus.plug_in_later(Duration::from_secs(1), stock("3.150"));
    bus.plug_in_later(Duration::from_secs(1), ours("14d05d4"));
    bus.plug_in_later(Duration::from_secs(1), stock("3.108"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(60));
    let skipped = |port: &str| match status(&batch, port) {
        Status::Skipped(reason) => reason,
        other => panic!("{port}: {other:?}"),
    };
    assert!(skipped("1-1").contains("not below V3.172"));
    assert_eq!(status(&batch, "1-2"), Status::Updated);
    assert!(skipped("1-3").contains("does not take"));
    // Below the limit, but with no jumper-free update of its own.
    assert!(skipped("1-4").contains("needs the ISP jumper"));
}

#[test]
fn a_finished_unit_blinks_until_it_is_unplugged() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let unit = bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));
    assert_eq!(status(&batch, "1-1"), Status::Updated);
    let blinks = bus.host_status_count(unit);
    assert!(blinks > 0);
    batch.step(&mut seen).unwrap();
    assert!(bus.host_status_count(unit) > blinks);
    bus.unplug(unit);
    batch.step(&mut seen).unwrap();
    assert!(batch.entries().is_empty());
}

#[test]
fn each_unit_gets_a_record_line() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));
    let record = &seen.records[0];
    let json: serde_json::Value = serde_json::to_value(record).unwrap();
    assert_eq!(json["port"], "1-1");
    assert_eq!(json["result"], "updated");
    assert_eq!(json["serial_before"], "QK7RM2VTX8D41");
    assert_eq!(json["firmware_before"], "stock V3.172");
    assert_eq!(json["firmware_after"], "ours, build v0.3.0");
    assert!(json["serial_after"].as_str().unwrap().starts_with("CJ"));
    assert!(json["time"].as_str().unwrap().ends_with('Z'));
    assert!(json["seconds"].as_f64().unwrap() > 2.4);
}

/// A unit that arrives in ISP mode blocks every other entry, so it is flashed first.
#[test]
fn a_waiting_bootloader_goes_before_a_waiting_probe() {
    let bus = SimBus::default();
    let mut batch = arm(
        &bus,
        Filter {
            bootloaders: true,
            ..filter()
        },
        None,
    );
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    let jumpered = bus.plug_in_bootloader_later(Duration::from_secs(1));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));
    assert_eq!(seen.started, ["1-2"]);
    assert_eq!(status(&batch, "1-2"), Status::JumperFitted);
    assert_eq!(status(&batch, "1-1"), Status::Waiting);
    bus.unplug(jumpered);
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(60));
    assert_eq!(seen.started, ["1-2", "1-1"]);
    assert_eq!(status(&batch, "1-1"), Status::Updated);
    assert_eq!(bus.max_bootloaders(), 1);
}

/// Older than the image means nothing for stock or a file, so ours is taken only when the filter
/// takes any build. A release of ours is the case where the two filters disagree.
#[test]
fn ours_older_than_the_image_needs_an_image_of_ours() {
    let bus = SimBus::default();
    bus.add_running(ours("14d05d4"));
    bus.add_running(ours("v0.3.0"));
    let units = verge_core::detect::detect(&bus).unwrap();
    let (old, current) = (&units[0], &units[1]);
    let older = filter();
    let any = Filter {
        ours: OursFilter::Any,
        ..filter()
    };
    let stock = ImageVersion::Stock(StockVersion::parse("3.172").unwrap());
    for version in [&stock, &ImageVersion::Unknown] {
        assert_eq!(
            judge(old, &older, version),
            Verdict::Skip("the image carries no version of ours to compare with".into())
        );
        assert_eq!(judge(old, &any, version), Verdict::Take);
        assert_eq!(judge(current, &any, version), Verdict::Take);
    }
    let release = ImageVersion::Ours("v0.3.0".into());
    assert_eq!(judge(old, &older, &release), Verdict::Take);
    assert_eq!(judge(current, &older, &release), Verdict::Current);
    assert_eq!(judge(current, &any, &release), Verdict::Current);
}

fn stock_probe(product: &str) -> verge_core::Unit {
    use verge_core::bus::UsbDevice;
    use verge_core::dap::Identity;
    use verge_core::{Firmware, State};
    verge_core::Unit {
        device: UsbDevice {
            vid: 0x1fc9,
            pid: 0x0143,
            location: Some("1-4".into()),
            instance: "1".into(),
            serial: Some("PROSERIAL".into()),
            manufacturer: Some("NXP Semiconductors".into()),
            product: Some(product.into()),
            dap_v2: true,
            hid: true,
        },
        state: State::Probe {
            identity: Identity::default(),
            firmware: Firmware::Stock {
                version: Some(StockVersion {
                    major: 3,
                    minor: 172,
                }),
            },
        },
    }
}

/// LinkServer's Readme: one stock image for every LPC55S69 MCU-Link, product string
/// `MCU-LINK [Pro|MR|on-board|<board>] (r<RRR>) CMSIS-DAP V<V>.<BBB>`. The firmware's PRO.md: do not flash
/// today's image on a Pro. The batch's board is "the full MCU-Link".
#[test]
fn a_batch_for_the_full_mculink_does_not_take_a_pro() {
    for product in [
        "MCU-LINK Pro (r1E2) CMSIS-DAP V3.172",
        "MCU-LINK FRDM-MCXN947 (r0E7) CMSIS-DAP V3.172",
    ] {
        let verdict = judge(
            &stock_probe(product),
            &filter(),
            &ImageVersion::Ours("v0.3.0".into()),
        );
        assert!(
            matches!(verdict, Verdict::Skip(_)),
            "{product}: {verdict:?}"
        );
    }
}

/// The update path waits `Timing::settle` after an enumeration before asking a unit what it runs.
/// A batch asks an arrival at once, and a unit it could not ask is skipped until it leaves the bus.
#[test]
fn a_unit_not_ready_for_a_moment_after_arriving_is_still_updated() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let a = bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    // Not openable for its first moments on the bus.
    bus.set_busy(a, true);
    while bus.now() < Duration::from_millis(1300) {
        batch.step(&mut seen).unwrap();
    }
    bus.set_busy(a, false);
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(60));
    assert_eq!(status(&batch, "1-1"), Status::Updated, "{:?}", seen.records);
}

/// Ours reports its board from identify format 3, so a Pro running ours is not taken either.
#[test]
fn a_batch_does_not_take_ours_on_a_pro() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let pro = bus.plug_in_later(Duration::from_secs(1), ours("14d05d4"));
    bus.set_board(pro, Ours::BOARD_MCU_LINK_PRO);
    let plain = bus.plug_in_later(Duration::from_secs(1), ours("14d05d4"));
    bus.set_board(plain, Ours::BOARD_MCU_LINK);
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));
    assert_eq!(
        status(&batch, "1-1"),
        Status::Skipped("an MCU-Link Pro, not a full MCU-Link".into())
    );
    assert_eq!(status(&batch, "1-2"), Status::Updated);
    assert!(!bus.log().iter().any(|l| l.starts_with("1-1: control")));
}

/// The update path waits `Timing::settle` after an enumeration before it asks the unit anything,
/// and a batch's arrival gets the same.
#[test]
fn an_arrival_is_not_opened_before_it_has_settled() {
    let bus = SimBus::default();
    let hooked = Hooked::new(&bus);
    let mut batch = arm(&hooked, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));
    assert_eq!(status(&batch, "1-1"), Status::Updated);
    let first_seen = batch.entries()[0].seen;
    let (first_open, _) = hooked.opened.borrow()[0].clone();
    assert!(
        first_open >= first_seen + Timing::default().settle,
        "opened at {first_open:?}, seen at {first_seen:?}"
    );
}

/// An open that fails soon after the unit arrives is not final until it has failed for the
/// settle time and a few polls more.
#[test]
fn an_arrival_that_cannot_be_opened_past_its_settle_time_is_still_updated() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let a = bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    bus.set_busy(a, true);
    // Busy two polls past the settle time.
    let timing = Timing::default();
    let until = Duration::from_secs(1) + timing.settle + timing.poll * 2;
    while bus.now() < until {
        batch.step(&mut seen).unwrap();
    }
    assert!(batch.entries().is_empty(), "{:?}", batch.entries());
    bus.set_busy(a, false);
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(60));
    assert_eq!(status(&batch, "1-1"), Status::Updated, "{:?}", seen.records);
}

#[test]
fn an_arrival_that_never_opens_is_skipped_once_its_grace_is_over() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let a = bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    bus.set_busy(a, true);
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));
    assert_eq!(
        status(&batch, "1-1"),
        Status::Skipped("the tool could not ask what it runs: another program has it open".into())
    );
    assert_eq!(seen.records.len(), 1);
    assert!(!bus.log().iter().any(|l| l.contains("DAP 8101")));
}

/// A stop that lands while the batch asks a unit, between taking it off the queue and starting
/// its update, still keeps it out of its bootloader.
#[test]
fn a_stop_while_a_unit_is_asked_again_starts_no_update() {
    let bus = SimBus::default();
    let stop = Cancel::new();
    let opens = Cell::new(0);
    let mut hooked = Hooked::new(&bus);
    hooked.on_open = Box::new(|_| {
        opens.set(opens.get() + 1);
        // The first open asks the arrival what it runs; the second asks again before its update.
        if opens.get() == 2 {
            stop.cancel();
        }
    });
    let mut batch = Batch::arm(
        &hooked,
        release(),
        filter(),
        Timing::default(),
        stop.clone(),
        Cancel::new(),
        None,
    );
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));
    assert!(batch.finished());
    assert_eq!(opens.get(), 2);
    assert!(!bus.log().iter().any(|l| l.contains("DAP 8101")));
    assert_eq!(status(&batch, "1-1"), Status::Waiting);
}

/// Units still waiting when the batch ends get a line in the record saying so.
#[test]
fn units_still_waiting_when_the_batch_ends_are_recorded() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), Some(1));
    let mut seen = Seen::default();
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    batch.run_to_end(&mut seen).unwrap();
    let results: Vec<(&str, Settled)> = seen
        .records
        .iter()
        .map(|r| (r.port.as_str(), r.result))
        .collect();
    assert_eq!(
        results,
        [("1-1", Settled::Updated), ("1-2", Settled::Waiting)]
    );
    assert_eq!(
        seen.records[1].reason.as_deref(),
        Some("the batch stopped before its turn came")
    );
    // Closing again writes nothing more.
    batch.close(&mut seen);
    assert_eq!(seen.records.len(), 2);
}

/// The bus fails the look that follows an install. The install already ran, so it is recorded.
#[test]
fn a_bus_error_after_an_install_still_records_it() {
    let bus = SimBus::default();
    let back = Cell::new(false);
    let mut hooked = Hooked::new(&bus);
    // The unit comes back running ours; the look after the update has asked it fails, once.
    hooked.on_open = Box::new(|device| {
        if device
            .product
            .as_deref()
            .is_some_and(|p| p.contains("Verge"))
        {
            back.set(true);
        }
    });
    hooked.fail = Box::new(|| back.replace(false));
    let mut batch = arm(&hooked, filter(), None);
    let mut seen = Seen::default();
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    let error = batch.run_to_end(&mut seen).unwrap_err();
    assert_eq!(error, BusError::Enumerate("the test's bus failed".into()));
    assert_eq!(seen.records.len(), 1, "{:?}", seen.records);
    assert_eq!(seen.records[0].result, Settled::Updated);
    assert_eq!(
        seen.records[0].firmware_after.as_deref(),
        Some("ours, build v0.3.0")
    );
}

/// The bus fails while a finished unit's LEDs are lit. The batch ends, and turns them off.
#[test]
fn a_bus_error_still_turns_the_leds_off() {
    let bus = SimBus::default();
    let unit = bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    let mut hooked = Hooked::new(&bus);
    hooked.fail = Box::new(|| bus.leds_on(unit));
    let mut batch = arm(&hooked, filter(), None);
    let mut seen = Seen::default();
    assert!(batch.run_to_end(&mut seen).is_err());
    assert_eq!(seen.records[0].result, Settled::Updated);
    assert!(!bus.leds_on(unit), "the LEDs were left on");
}

/// A unit blinks only while the batch runs, so the last one under `--count` is not blinking.
#[test]
fn a_finished_unit_is_named_for_unplugging_and_blinks_only_while_the_batch_runs() {
    let bus = SimBus::default();
    let mut batch = arm(&bus, filter(), Some(2));
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    bus.plug_in_later(Duration::from_secs(1), ours("v0.3.0"));
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    batch.run_to_end(&mut seen).unwrap();
    assert_eq!(
        seen.unplugs,
        [
            ("1-1".to_owned(), true),
            ("1-2".to_owned(), true),
            ("1-3".to_owned(), false)
        ]
    );
}

/// The batch knows units by port, so a unit that reports none pauses it and is never taken.
#[test]
fn a_unit_with_no_port_pauses_the_batch() {
    let bus = SimBus::default();
    bus.set_locations(false);
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let unit = bus.plug_in_later(Duration::from_secs(1), stock("3.172"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));
    let paused = batch.paused().unwrap();
    assert!(paused.contains("reports no USB port"), "{paused}");
    assert!(batch.entries().is_empty());
    assert!(!bus.log().iter().any(|l| l.contains("DAP 8101")));
    bus.unplug(unit);
    batch.step(&mut seen).unwrap();
    assert_eq!(batch.paused(), None);
}

/// On Windows a unit that comes back keeps its instance, so the batch sees it leave only by
/// looking while it is gone. One that is gone for a look is taken as new when it returns.
#[test]
fn where_a_unit_keeps_its_instance_it_is_new_once_it_was_seen_gone() {
    let bus = SimBus::default();
    bus.set_stable_instances(true);
    let mut batch = arm(&bus, filter(), None);
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let unit = bus.plug_in_later(Duration::from_secs(1), ours("v0.3.0"));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(5));
    assert_eq!(status(&batch, "1-1"), Status::Current);
    bus.unplug(unit);
    batch.step(&mut seen).unwrap();
    assert!(batch.entries().is_empty());
    bus.plug_back_later(Duration::from_millis(100), unit);
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(10));
    assert_eq!(seen.records.len(), 2);
    assert_eq!(status(&batch, "1-1"), Status::Current);
}

/// The jumper case where a bootloader that comes back keeps its instance, as on Windows.
#[test]
fn where_a_bootloader_keeps_its_instance_a_jumper_fitted_unit_is_still_seen() {
    let bus = SimBus::default();
    bus.set_stable_instances(true);
    let mut batch = arm(
        &bus,
        Filter {
            bootloaders: true,
            ..filter()
        },
        None,
    );
    let mut seen = Seen::default();
    batch.step(&mut seen).unwrap();
    let unit = bus.plug_in_bootloader_later(Duration::from_secs(1));
    drive(&mut batch, &bus, &mut seen, Duration::from_secs(30));
    assert_eq!(status(&batch, "1-1"), Status::JumperFitted);
    assert!(batch.paused().unwrap().contains("jumper fitted"));
    bus.unplug(unit);
    batch.step(&mut seen).unwrap();
    assert_eq!(batch.paused(), None);
}
