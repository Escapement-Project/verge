//! Batches on the simulated bench, with units plugged in and out over simulated time. Every run
//! is bounded in the bus's time; none takes real time to time out.

use std::time::Duration;

use verge_core::Cancel;
use verge_core::batch::{
    Armed, Batch, Board, Entry, Filter, Observer, OursFilter, Record, Status, StockFilter, Verdict,
    judge,
};
use verge_core::progress::Progress;
use verge_core::sim::{SimBus, SimFirmware, firmware_image};
use verge_core::source::ImageVersion;
use verge_core::update::Timing;
use verge_core::version::StockVersion;

#[derive(Default)]
struct Seen {
    records: Vec<Record>,
    started: Vec<String>,
    pauses: Vec<Option<String>>,
}

impl Progress for Seen {}

impl Observer for Seen {
    fn starting(&mut self, entry: &Entry) {
        self.started.push(entry.port.clone());
    }
    fn record(&mut self, record: &Record) {
        self.records.push(record.clone());
    }
    fn paused(&mut self, reason: Option<&str>) {
        self.pauses.push(reason.map(str::to_owned));
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

fn arm(bus: &SimBus, filter: Filter, limit: Option<usize>) -> Batch<'_, SimBus> {
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
fn drive(batch: &mut Batch<'_, SimBus>, bus: &SimBus, seen: &mut Seen, until: Duration) {
    use verge_core::Bus;
    while bus.now() < until && !batch.finished() {
        batch.step(seen).unwrap();
    }
}

fn status(batch: &Batch<'_, SimBus>, port: &str) -> Status {
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
    assert_eq!(seen.records[0].result, "jumper-fitted");
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
    assert_eq!(seen.records[0].result, "failed");
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
    assert_eq!(seen.records[0].result, "current");
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
