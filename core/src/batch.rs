//! A batch: one image and one filter, armed once, applied to every newly attached unit the filter
//! takes, one unit through the bootloader at a time.
//!
//! Every LPC55S69 bootloader enumerates alike, so the batch starts a unit only while no bootloader
//! is on the bus: anything in ISP mode that is not the unit being flashed pauses it, and so does a
//! mini. Units are tracked by port, as a single update finds its unit again, and each is taken at
//! most once until it leaves the bus.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::bus::{Bus, BusError, UsbDevice};
use crate::dap;
use crate::detect::{self, Firmware, State, Unit};
use crate::ids::Role;
use crate::image::Image;
use crate::paths::{self, Path};
use crate::progress::{Cancel, Progress};
use crate::source::ImageVersion;
use crate::update::{self, Outcome, Timing};
use crate::version::{Build, Relation, StockVersion};

/// The board a batch takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Board {
    /// The full MCU-Link: a probe at `1fc9:0143`, or in ISP mode a bootloader at `1fc9:0021`.
    McuLink,
}

impl Board {
    /// Whether `unit` is this board. The MCU-Link Pro joins here once its support lands; its USB
    /// identity is not known yet, so nothing here guesses it.
    #[must_use]
    pub fn takes(self, unit: &Unit) -> bool {
        match self {
            Board::McuLink => matches!(unit.device.role(), Some(Role::Probe | Role::Bootloader)),
        }
    }
}

impl fmt::Display for Board {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Board::McuLink => "full MCU-Link",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StockFilter {
    None,
    Any,
    /// Stock older than this version.
    Below(StockVersion),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OursFilter {
    None,
    Any,
    /// Ours older than the image, or a build that carries no version to compare.
    Older,
}

/// Which units a batch takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub board: Board,
    pub stock: StockFilter,
    pub ours: OursFilter,
    /// Units that arrive in ISP mode, as after fitting the jumper.
    pub bootloaders: bool,
    /// Units attached already when the batch is armed.
    pub attached: bool,
}

impl Filter {
    /// The filter in words, for the question that arms it.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut firmware = Vec::new();
        match self.stock {
            StockFilter::None => {}
            StockFilter::Any => firmware.push("stock".to_owned()),
            StockFilter::Below(version) => firmware.push(format!("stock below V{version}")),
        }
        match self.ours {
            OursFilter::None => {}
            OursFilter::Any => firmware.push("ours".to_owned()),
            OursFilter::Older => firmware.push("ours older than the image".to_owned()),
        }
        let mut kinds = Vec::new();
        if !firmware.is_empty() {
            kinds.push(format!("running {}", firmware.join(" or ")));
        }
        if self.bootloaders {
            kinds.push("in ISP mode".to_owned());
        }
        if kinds.is_empty() {
            return "no unit".to_owned();
        }
        let when = if self.attached {
            "attached now or later"
        } else {
            "attached from now on"
        };
        format!("every {} {}, {when}", self.board, kinds.join(" or "))
    }
}

/// The image a batch installs, loaded and checked once when it is armed.
#[derive(Clone, Debug)]
pub struct Armed {
    pub image: Image,
    pub name: String,
    pub version: ImageVersion,
}

/// What the batch makes of a unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Take,
    /// It runs the image already.
    Current,
    Skip(String),
}

/// Whether the batch takes `unit`.
#[must_use]
pub fn judge(unit: &Unit, filter: &Filter, version: &ImageVersion) -> Verdict {
    if !filter.board.takes(unit) {
        return Verdict::Skip(match unit.state {
            State::MiniBootloader => "an MCU-Link mini, a different board".into(),
            _ => format!("not a {}", filter.board),
        });
    }
    let taken = match &unit.state {
        State::Bootloader if filter.bootloaders => return Verdict::Take,
        State::Bootloader => {
            return Verdict::Skip("in ISP mode, which this batch does not take".into());
        }
        State::MiniBootloader => unreachable!("no board takes a mini"),
        State::Unreadable { reason, .. } => {
            return Verdict::Skip(format!("the tool could not ask what it runs: {reason}"));
        }
        State::Probe {
            firmware: Firmware::Ours(ours),
            ..
        } => {
            let build = Build::parse(&ours.build);
            if let ImageVersion::Ours(tag) = version
                && build.compare(tag) == Relation::UpToDate
            {
                return Verdict::Current;
            }
            match (filter.ours, version) {
                (OursFilter::None, _) => {
                    Verdict::Skip("runs ours, which this batch does not take".into())
                }
                (OursFilter::Any, _) => Verdict::Take,
                (OursFilter::Older, ImageVersion::Ours(tag)) => match build.compare(tag) {
                    Relation::Behind | Relation::Unknown => Verdict::Take,
                    Relation::Ahead => Verdict::Skip(format!("runs {build}, newer than {tag}")),
                    Relation::OtherVariant => {
                        Verdict::Skip(format!("runs {tag}, built as {build}"))
                    }
                    Relation::UpToDate => Verdict::Current,
                },
                (OursFilter::Older, _) => {
                    Verdict::Skip("the image carries no version of ours to compare with".into())
                }
            }
        }
        State::Probe {
            firmware: Firmware::Stock { version: running },
            ..
        } => {
            if let ImageVersion::Stock(image) = version
                && *running == Some(*image)
            {
                return Verdict::Current;
            }
            match (filter.stock, running) {
                (StockFilter::None, _) => {
                    Verdict::Skip("runs stock, which this batch does not take".into())
                }
                (StockFilter::Any, _) => Verdict::Take,
                (StockFilter::Below(limit), Some(running)) if *running < limit => Verdict::Take,
                (StockFilter::Below(limit), Some(running)) => {
                    Verdict::Skip(format!("runs stock V{running}, not below V{limit}"))
                }
                (StockFilter::Below(_), None) => {
                    Verdict::Skip("stock did not report its version".into())
                }
            }
        }
    };
    if taken != Verdict::Take {
        return taken;
    }
    // Whether it can enter its bootloader at all, judged alone: other units on the bus are the
    // batch's to wait for, not a reason to skip this one.
    let offer = paths::offer(unit, std::slice::from_ref(unit));
    match offer.best() {
        Some(Path::OursJumperFree | Path::StockJumperFree) => Verdict::Take,
        Some(Path::Jumper) => {
            Verdict::Skip(format!("needs the ISP jumper. {}", offer.notes.join(" ")))
        }
        _ => Verdict::Skip(offer.notes.join(" ")),
    }
}

/// Where a unit stands in the batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Attached when the batch was armed, and left alone.
    LeftAlone,
    Waiting,
    Updated,
    /// Flashed, and back in ISP mode because its jumper is fitted.
    JumperFitted,
    Current,
    Skipped(String),
    Failed(String),
}

/// One unit the batch has seen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub port: String,
    pub device: UsbDevice,
    pub status: Status,
    pub serial_before: Option<String>,
    pub firmware_before: String,
    /// When the bus first showed it, in the bus's time.
    pub seen: Duration,
}

/// One line of the batch's record.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Record {
    /// RFC 3339, UTC.
    pub time: String,
    pub port: String,
    pub serial_before: Option<String>,
    pub firmware_before: String,
    pub serial_after: Option<String>,
    pub firmware_after: Option<String>,
    /// `updated`, `jumper-fitted`, `current`, `skipped` or `failed`.
    pub result: &'static str,
    pub reason: Option<String>,
    pub seconds: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Tally {
    pub updated: usize,
    pub current: usize,
    pub skipped: usize,
    pub failed: usize,
    pub waiting: usize,
}

impl fmt::Display for Tally {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} updated, {} current, {} skipped, {} failed, {} waiting",
            self.updated, self.current, self.skipped, self.failed, self.waiting
        )
    }
}

/// What a batch tells its front end, besides each update's progress.
pub trait Observer: Progress {
    /// A unit's update starts.
    fn starting(&mut self, _entry: &Entry) {}
    /// A unit is settled, as its record says.
    fn record(&mut self, _record: &Record) {}
    /// The batch pauses, saying why, or resumes with `None`.
    fn paused(&mut self, _reason: Option<&str>) {}
}

/// Ignores everything.
pub struct Unobserved;

impl Progress for Unobserved {}
impl Observer for Unobserved {}

fn port(device: &UsbDevice) -> String {
    device
        .location
        .clone()
        .unwrap_or_else(|| format!("device {}", device.instance))
}

/// The bus, noting when each port first shows a device. An update polls the bus while it waits,
/// so units that arrive during one keep their order.
struct Watch<'b, B> {
    bus: &'b B,
    seen: RefCell<HashMap<String, Duration>>,
}

impl<B: Bus> Bus for Watch<'_, B> {
    type Dap = B::Dap;
    type Isp = B::Isp;

    fn devices(&self) -> Result<Vec<UsbDevice>, BusError> {
        let devices = self.bus.devices()?;
        let now = self.bus.now();
        let mut seen = self.seen.borrow_mut();
        for device in &devices {
            seen.entry(port(device)).or_insert(now);
        }
        Ok(devices)
    }

    fn open_dap(&self, device: &UsbDevice) -> Result<B::Dap, BusError> {
        self.bus.open_dap(device)
    }

    fn open_isp(&self, device: &UsbDevice) -> Result<B::Isp, BusError> {
        self.bus.open_isp(device)
    }

    fn now(&self) -> Duration {
        self.bus.now()
    }

    fn sleep(&self, duration: Duration) {
        self.bus.sleep(duration);
    }
}

fn now_rfc3339() -> String {
    humantime::format_rfc3339_seconds(SystemTime::now()).to_string()
}

/// An armed batch. Drive it with [`Batch::step`] until [`Batch::finished`].
pub struct Batch<'b, B: Bus> {
    bus: Watch<'b, B>,
    armed: Armed,
    filter: Filter,
    timing: Timing,
    entries: Vec<Entry>,
    queue: VecDeque<String>,
    paused: Option<String>,
    records: Vec<Record>,
    stop: Cancel,
    cancel: Cancel,
    limit: Option<usize>,
    updated: usize,
    armed_on_bus: bool,
    lit: bool,
}

impl<'b, B: Bus> Batch<'b, B> {
    /// Arms a batch. `stop` stops it taking units; `cancel` stops the update under way, as a
    /// single update's cancel does. `limit` stops it after that many units are updated.
    pub fn arm(
        bus: &'b B,
        armed: Armed,
        filter: Filter,
        timing: Timing,
        stop: Cancel,
        cancel: Cancel,
        limit: Option<usize>,
    ) -> Batch<'b, B> {
        Batch {
            bus: Watch {
                bus,
                seen: RefCell::new(HashMap::new()),
            },
            armed,
            filter,
            timing,
            entries: Vec::new(),
            queue: VecDeque::new(),
            paused: None,
            records: Vec::new(),
            stop,
            cancel,
            limit,
            updated: 0,
            armed_on_bus: false,
            lit: false,
        }
    }

    #[must_use]
    pub fn armed(&self) -> &Armed {
        &self.armed
    }

    #[must_use]
    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    #[must_use]
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Why the batch is waiting, when it is.
    #[must_use]
    pub fn paused(&self) -> Option<&str> {
        self.paused.as_deref()
    }

    #[must_use]
    pub fn tally(&self) -> Tally {
        let mut tally = Tally::default();
        for entry in &self.entries {
            match entry.status {
                Status::LeftAlone => {}
                Status::Waiting => tally.waiting += 1,
                Status::Updated | Status::JumperFitted => tally.updated += 1,
                Status::Current => tally.current += 1,
                Status::Skipped(_) => tally.skipped += 1,
                Status::Failed(_) => tally.failed += 1,
            }
        }
        tally
    }

    /// Whether it takes no more units: stopped, or at its limit. No update is ever under way
    /// between steps.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.stop.is_cancelled() || self.limit.is_some_and(|limit| self.updated >= limit)
    }

    fn entry(&mut self, port: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.port == port)
    }

    fn settle(
        &mut self,
        observer: &mut dyn Observer,
        port: &str,
        after: Option<&Unit>,
        seconds: Option<f64>,
    ) {
        let Some(entry) = self.entries.iter().find(|e| e.port == port) else {
            return;
        };
        let (result, reason) = match &entry.status {
            Status::Updated => ("updated", None),
            Status::JumperFitted => ("jumper-fitted", None),
            Status::Current => ("current", None),
            Status::Skipped(reason) => ("skipped", Some(reason.clone())),
            Status::Failed(reason) => ("failed", Some(reason.clone())),
            Status::Waiting | Status::LeftAlone => return,
        };
        let record = Record {
            time: now_rfc3339(),
            port: entry.port.clone(),
            serial_before: entry.serial_before.clone(),
            firmware_before: entry.firmware_before.clone(),
            serial_after: after.and_then(|u| u.serial().map(str::to_owned)),
            firmware_after: after.map(|u| u.state.to_string()),
            result,
            reason,
            seconds,
        };
        observer.record(&record);
        self.records.push(record);
    }

    /// The batch's rule for starting a unit: nothing in ISP mode on the bus, unless it is a
    /// waiting unit the batch takes, and then that one goes first.
    fn guard(&self, devices: &[UsbDevice]) -> Option<String> {
        if let Some(mini) = devices
            .iter()
            .find(|d| d.role() == Some(Role::MiniBootloader))
        {
            return Some(format!(
                "An MCU-Link mini is in ISP mode at {}. The batch waits until it is unplugged.",
                port(mini)
            ));
        }
        let roms: Vec<&UsbDevice> = devices
            .iter()
            .filter(|d| d.role() == Some(Role::Bootloader))
            .collect();
        match roms.as_slice() {
            [] => None,
            [rom] => {
                let at = port(rom);
                let status = self
                    .entries
                    .iter()
                    .find(|e| e.port == at)
                    .map(|e| &e.status);
                match status {
                    Some(Status::Waiting) => None,
                    Some(Status::Failed(_)) => Some(format!(
                        "The unit at {at} failed and is in ISP mode, maybe with a partial image. The batch waits until it is unplugged; `verge flash` flashes it again."
                    )),
                    Some(Status::JumperFitted) => Some(format!(
                        "The unit at {at} is in ISP mode with its jumper fitted. The batch waits until it is unplugged; take the jumper off before plugging it in again."
                    )),
                    _ => Some(format!(
                        "A unit in ISP mode that this batch does not take is attached at {at}. The batch waits until it is unplugged or flashed with `verge flash`."
                    )),
                }
            }
            several => Some(format!(
                "{} units are in ISP mode, and every LPC55S69 bootloader looks the same. The batch waits until at most one is.",
                several.len()
            )),
        }
    }

    /// The next unit to start: a waiting bootloader first, since it blocks every other entry.
    fn next(&mut self) -> Option<String> {
        let rom = self.queue.iter().position(|p| {
            self.entries
                .iter()
                .any(|e| &e.port == p && e.device.role() == Some(Role::Bootloader))
        });
        match rom {
            Some(index) => self.queue.remove(index),
            None => self.queue.pop_front(),
        }
    }

    /// Looks at the bus once, and starts at most one unit's update. Without an update to run it
    /// waits one poll.
    pub fn step(&mut self, observer: &mut dyn Observer) -> Result<(), BusError> {
        let devices: Vec<UsbDevice> = self
            .bus
            .devices()?
            .into_iter()
            .filter(|d| d.role().is_some())
            .collect();
        let present: HashMap<String, &UsbDevice> = devices.iter().map(|d| (port(d), d)).collect();

        // A unit that left the bus, or came back as a new enumeration the batch did not cause, is
        // forgotten, and is new when it is next seen.
        self.entries.retain(|e| {
            present
                .get(&e.port)
                .is_some_and(|d| d.instance == e.device.instance)
        });
        let tracked: Vec<String> = self.entries.iter().map(|e| e.port.clone()).collect();
        self.queue.retain(|p| tracked.contains(p));
        self.bus
            .seen
            .borrow_mut()
            .retain(|p, _| present.contains_key(p));

        let seen = self.bus.seen.borrow().clone();
        let mut arrivals: Vec<&UsbDevice> = devices
            .iter()
            .filter(|d| !tracked.contains(&port(d)))
            .collect();
        arrivals.sort_by_key(|d| (seen.get(&port(d)).copied(), port(d)));
        for device in arrivals {
            let Some(unit) = detect::query_device(&self.bus, device.clone()) else {
                continue;
            };
            let at = port(device);
            let status = if !self.armed_on_bus && !self.filter.attached {
                Status::LeftAlone
            } else {
                match judge(&unit, &self.filter, &self.armed.version) {
                    Verdict::Take => {
                        self.queue.push_back(at.clone());
                        Status::Waiting
                    }
                    Verdict::Current => Status::Current,
                    Verdict::Skip(reason) => Status::Skipped(reason),
                }
            };
            self.entries.push(Entry {
                port: at.clone(),
                device: unit.device.clone(),
                status,
                serial_before: unit.serial().map(str::to_owned),
                firmware_before: unit.state.to_string(),
                seen: seen.get(&at).copied().unwrap_or_default(),
            });
            self.settle(observer, &at, None, None);
        }
        self.armed_on_bus = true;

        let paused = self.guard(&devices);
        if paused != self.paused {
            observer.paused(paused.as_deref());
            self.paused = paused;
        }

        if self.paused.is_none()
            && !self.finished()
            && let Some(at) = self.next()
        {
            self.run(&at, observer)?;
            return Ok(());
        }
        self.blink();
        self.bus.sleep(self.timing.poll);
        Ok(())
    }

    fn run(&mut self, at: &str, observer: &mut dyn Observer) -> Result<(), BusError> {
        let Some(device) = self.entry(at).map(|e| e.device.clone()) else {
            return Ok(());
        };
        // Asked again: it may have changed while it waited.
        let Some(unit) = detect::query_device(&self.bus, device) else {
            return Ok(());
        };
        let verdict = judge(&unit, &self.filter, &self.armed.version);
        if verdict != Verdict::Take {
            if let Some(entry) = self.entry(at) {
                entry.status = match verdict {
                    Verdict::Current => Status::Current,
                    Verdict::Skip(reason) => Status::Skipped(reason),
                    Verdict::Take => unreachable!(),
                };
            }
            self.settle(observer, at, None, None);
            return Ok(());
        }
        if let Some(entry) = self.entries.iter().find(|e| e.port == at) {
            observer.starting(entry);
        }
        let started = self.bus.now();
        let result = update::install(
            &self.bus,
            &unit,
            &self.armed.image,
            &self.timing,
            observer,
            &self.cancel,
        );
        let seconds = Some((self.bus.now() - started).as_secs_f64());
        // Whatever is on the port now is this unit, so the next step does not take it for a new one.
        let now_at = self.bus.devices()?.into_iter().find(|d| port(d) == at);
        let (status, after) = match result {
            Ok(Outcome::Running(back)) => {
                self.updated += 1;
                (Status::Updated, Some(*back))
            }
            Ok(Outcome::JumperStillFitted) => {
                self.updated += 1;
                (Status::JumperFitted, None)
            }
            Err(error) => (Status::Failed(error.to_string()), None),
        };
        if let Some(entry) = self.entry(at) {
            entry.status = status;
            if let Some(device) = after.as_ref().map(|u| u.device.clone()).or(now_at) {
                entry.device = device;
            }
        }
        self.settle(observer, at, after.as_ref(), seconds);
        Ok(())
    }

    /// Toggles the LEDs of every finished unit still attached, so a person can see which to
    /// unplug.
    fn blink(&mut self) {
        self.lit = !self.lit;
        for entry in &self.entries {
            if !matches!(entry.status, Status::Updated | Status::Current) || !entry.device.dap_v2 {
                continue;
            }
            match self.bus.open_dap(&entry.device) {
                Ok(mut link) => {
                    if let Err(error) = dap::set_leds(&mut link, self.lit) {
                        tracing::debug!("{}: no blink: {error}", entry.port);
                    }
                }
                Err(error) => tracing::debug!("{}: no blink: {error}", entry.port),
            }
        }
    }

    /// Turns the finished units' LEDs off. Call once the batch has finished.
    pub fn close(&mut self) {
        self.lit = true;
        self.blink();
    }

    /// Steps until the batch finishes, then closes it.
    pub fn run_to_end(&mut self, observer: &mut dyn Observer) -> Result<(), BusError> {
        while !self.finished() {
            self.step(observer)?;
        }
        self.close();
        Ok(())
    }
}
