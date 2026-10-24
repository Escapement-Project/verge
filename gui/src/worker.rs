//! USB work on a thread of its own: the window sends requests and drains events each frame.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use std::io::Write as _;
use std::path::PathBuf;

use verge_probe::batch::{
    ArmError, Armed, Batch, Entry, Filter, Observer, Record, Status, Tally, open_record,
};
use verge_probe::progress::{Cancel, Progress, Step};
use verge_probe::release::{Download, GitHub, Http, Release, ReleaseError};
use verge_probe::source::{self, LoadError, Loaded, Source};
use verge_probe::update::{self, Outcome, Timing, UpdateError};
use verge_probe::{Bus, Unit, detect};

pub enum Request {
    Detect,
    Release,
    Install {
        unit: Box<Unit>,
        source: Source,
        force: bool,
        download: Download,
        cancel: Cancel,
    },
    /// Arms a batch and runs it until `stop`. `cancel` stops the unit under way.
    Batch {
        source: Source,
        filter: Filter,
        download: Download,
        limit: Option<usize>,
        record: PathBuf,
        stop: Cancel,
        cancel: Cancel,
    },
}

/// Where a batch stands, after each look at the bus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchState {
    pub tally: Tally,
    /// Waiting units by port, with their serials, first in line first.
    pub waiting: Vec<(String, Option<String>)>,
    pub paused: Option<String>,
}

/// Why a job failed, and what to do about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobError {
    pub message: String,
    pub advice: Option<&'static str>,
    /// Whether the unit may have been left in ISP mode.
    pub left_in_isp: bool,
}

#[derive(Clone, Debug)]
pub enum Event {
    Units(Result<Vec<Unit>, String>),
    Release(Result<Box<Release>, String>),
    Loaded(String),
    Plan(Vec<Step>),
    Start { step: Step, total: u64 },
    Advance(u64),
    Note(String),
    Finished(Result<Outcome, JobError>),
    BatchArmed { name: String, filter: String },
    BatchStarting(String),
    BatchRecord(Box<Record>),
    BatchState(BatchState),
    BatchEnded(Result<Tally, String>),
}

/// What the window talks to: a worker thread, or in tests a hand-driven stand-in.
pub trait Backend {
    fn send(&mut self, request: Request);
    fn try_recv(&mut self) -> Option<Event>;
}

/// Forwards an operation's progress as events.
struct Forward<'a> {
    events: &'a Sender<Event>,
    repaint: &'a (dyn Fn() + Send),
}

impl Forward<'_> {
    fn send(&self, event: Event) {
        // The window may have gone; nothing then reads the event.
        let _ = self.events.send(event);
        (self.repaint)();
    }
}

impl Progress for Forward<'_> {
    fn plan(&mut self, steps: &[Step]) {
        self.send(Event::Plan(steps.to_vec()));
    }
    fn start(&mut self, step: Step, total: u64) {
        self.send(Event::Start { step, total });
    }
    fn advance(&mut self, done: u64) {
        self.send(Event::Advance(done));
    }
    fn note(&mut self, text: &str) {
        self.send(Event::Note(text.to_owned()));
    }
}

/// A load's progress, sent from the thread that loads. Once `stop` is set the load has been left
/// to finish on its own, and what it says would land on whatever the window shows next.
struct Relay {
    events: Sender<Event>,
    stop: Cancel,
}

impl Relay {
    fn send(&self, event: Event) {
        if !self.stop.is_cancelled() {
            let _ = self.events.send(event);
        }
    }
}

impl Progress for Relay {
    fn plan(&mut self, steps: &[Step]) {
        self.send(Event::Plan(steps.to_vec()));
    }
    fn start(&mut self, step: Step, total: u64) {
        self.send(Event::Start { step, total });
    }
    fn advance(&mut self, done: u64) {
        self.send(Event::Advance(done));
    }
    fn note(&mut self, text: &str) {
        self.send(Event::Note(text.to_owned()));
    }
}

/// Loads an image on a thread of its own, so that `stop` ends the wait at once. A download that
/// has stalled is left to its timeouts; nothing has touched USB yet. `None` when stopped.
fn load_stoppable<H: Http + Clone + Send + 'static>(
    github: &GitHub<H>,
    source: &Source,
    unit: Option<&Unit>,
    force: bool,
    download: Download,
    forward: &Forward<'_>,
    stop: &Cancel,
) -> Result<Option<Loaded>, LoadError> {
    let (done, result) = channel();
    let (github, source, unit) = (github.clone(), source.clone(), unit.cloned());
    let mut relay = Relay {
        events: forward.events.clone(),
        stop: stop.clone(),
    };
    let thread = std::thread::Builder::new()
        .name("load".into())
        .spawn(move || {
            let loaded = source::load(&source, &github, unit.as_ref(), force, download, &mut relay);
            let _ = done.send(loaded);
        })
        .expect("the load thread starts");
    loop {
        match result.recv_timeout(Duration::from_millis(50)) {
            Ok(loaded) => return loaded.map(Some),
            Err(RecvTimeoutError::Timeout) if stop.is_cancelled() => return Ok(None),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => match thread.join() {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(()) => unreachable!("the load thread sends its result before it ends"),
            },
        }
    }
}

fn load_error(error: &LoadError) -> JobError {
    let advice = match error {
        LoadError::AlreadyRuns { .. } | LoadError::Newer { .. } => {
            Some("Tick Install anyway to flash it regardless.")
        }
        LoadError::NoStock { .. } => Some("Install NXP's LinkServer, or pick the image as a file."),
        LoadError::OtherBoard { .. } | LoadError::NotForBoard { .. } => {
            Some("Pick STOCK to put NXP's image on it instead.")
        }
        error if download_again(error) => Some(DOWNLOAD_AGAIN),
        LoadError::Release(ReleaseError::CachedCopyFailed { left: Some(_), .. }) => {
            Some(CLEAR_THE_COPY)
        }
        _ => None,
    };
    JobError {
        message: error.to_string(),
        advice,
        left_in_isp: false,
    }
}

/// Whether `error` is a release that could not come from the cache and was not to be downloaded.
/// The next question asks to download it. A failed copy the tool could not delete is not one: the
/// cache would still hold it.
fn download_again(error: &LoadError) -> bool {
    matches!(
        error,
        LoadError::Release(
            ReleaseError::CachedCopyFailed { left: None, .. } | ReleaseError::NotAgreed { .. }
        )
    )
}

const DOWNLOAD_AGAIN: &str = "Try again: the question then asks to download the release.";
const CLEAR_THE_COPY: &str = "Delete that copy, then try again.";

/// Looks the latest release up again for the window. Its questions judge the cache by its own
/// lookup, and a release the cache could not give may have been published again since, so
/// without this the next question would still name no download.
fn look_up_again<H: Http>(github: &GitHub<H>, forward: &Forward<'_>) {
    let release = source::release(github, None)
        .map(Box::new)
        .map_err(|e| e.to_string());
    forward.send(Event::Release(release));
}

/// Why a batch did not arm, with what to change in the batch view.
pub fn arm_error(error: &ArmError) -> String {
    let advice = match error {
        ArmError::OursUndated => {
            " Tick \"of any build\" to take every unit running ours, or untick Ours."
        }
        ArmError::NotForBoard { .. } => " Pick an image of ours built for it, or STOCK.",
        ArmError::Image(_) => "",
    };
    format!("{}{advice}", crate::app::sentence(&error.to_string()))
}

fn update_error(error: &UpdateError) -> JobError {
    let (advice, left_in_isp) = match error {
        UpdateError::Isp(_) | UpdateError::Cancelled | UpdateError::RomSilent { .. } => (
            Some(
                "The unit may be in ISP mode with a partial image. Flashing it again recovers it.",
            ),
            true,
        ),
        UpdateError::NoReturn(_) => (
            Some(
                "Look at what is attached now. If the unit stays away, fit the ISP jumper and flash it again.",
            ),
            false,
        ),
        UpdateError::Ambiguous { .. } => (Some("Look at what is attached now."), false),
        UpdateError::NoBootloader(_) | UpdateError::OursEntry(_) | UpdateError::StockEntry(_) => {
            (Some("Use the ISP jumper instead."), false)
        }
        _ => (None, false),
    };
    JobError {
        message: error.to_string(),
        advice,
        left_in_isp,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the request's fields, passed through"
)]
fn install<B: Bus, H: Http + Clone + Send + 'static>(
    bus: &B,
    github: &GitHub<H>,
    unit: &Unit,
    source: &Source,
    force: bool,
    download: Download,
    cancel: &Cancel,
    progress: &mut Forward<'_>,
) -> Result<Outcome, JobError> {
    let loaded = load_stoppable(
        github,
        source,
        Some(unit),
        force,
        download,
        progress,
        cancel,
    )
    .map_err(|error| {
        if download_again(&error) {
            look_up_again(github, progress);
        }
        load_error(&error)
    })?
    .ok_or_else(|| JobError {
        message: "cancelled before anything was flashed".into(),
        advice: None,
        left_in_isp: false,
    })?;
    progress.send(Event::Loaded(loaded.name.clone()));
    tracing::info!("installing {}", loaded.name);
    update::install(
        bus,
        unit,
        loaded.payload(),
        &Timing::default(),
        progress,
        cancel,
        cancel,
    )
    .map_err(|error| update_error(&error))
}

/// A batch's events, and its record file.
struct BatchForward<'a, 'b> {
    forward: &'a mut Forward<'b>,
    record: std::io::BufWriter<std::fs::File>,
}

impl Progress for BatchForward<'_, '_> {
    fn plan(&mut self, steps: &[Step]) {
        self.forward.plan(steps);
    }
    fn start(&mut self, step: Step, total: u64) {
        self.forward.start(step, total);
    }
    fn advance(&mut self, done: u64) {
        self.forward.advance(done);
    }
    fn note(&mut self, text: &str) {
        self.forward.note(text);
    }
}

impl Observer for BatchForward<'_, '_> {
    fn starting(&mut self, entry: &Entry) {
        tracing::info!("{}: updating", entry.port);
        self.forward.send(Event::BatchStarting(entry.port.clone()));
    }

    fn record(&mut self, record: &Record) {
        let written = serde_json::to_writer(&mut self.record, record)
            .map_err(std::io::Error::from)
            .and_then(|()| writeln!(self.record))
            .and_then(|()| self.record.flush());
        if let Err(error) = written {
            tracing::error!("the record file: {error}");
        }
        tracing::info!("{}: {}", record.port, record.result);
        self.forward
            .send(Event::BatchRecord(Box::new(record.clone())));
    }

    fn paused(&mut self, reason: Option<&str>) {
        match reason {
            Some(reason) => tracing::warn!("paused. {reason}"),
            None => tracing::info!("resumed"),
        }
    }
}

fn state<B: Bus + Sync>(batch: &Batch<'_, B>) -> BatchState {
    BatchState {
        tally: batch.tally(),
        waiting: batch
            .entries()
            .iter()
            .filter(|e| e.status == Status::Waiting)
            .map(|e| (e.port.clone(), e.serial_before.clone()))
            .collect(),
        paused: batch.paused().map(str::to_owned),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the request's fields, passed through"
)]
fn run_batch<B: Bus + Sync, H: Http + Clone + Send + 'static>(
    bus: &B,
    github: &GitHub<H>,
    forward: &mut Forward<'_>,
    source: &Source,
    filter: Filter,
    download: Download,
    limit: Option<usize>,
    record: &std::path::Path,
    stop: &Cancel,
    cancel: &Cancel,
) -> Result<Tally, String> {
    filter
        .check_source(source)
        .map_err(|error| arm_error(&error))?;
    let loaded = load_stoppable(github, source, None, true, download, forward, stop);
    let Some(loaded) = loaded.map_err(|error| {
        if download_again(&error) {
            look_up_again(github, forward);
        }
        match load_error(&error).advice {
            Some(advice @ (DOWNLOAD_AGAIN | CLEAR_THE_COPY)) => {
                format!("{} {advice}", crate::app::sentence(&error.to_string()))
            }
            _ => error.to_string(),
        }
    })?
    else {
        tracing::info!("stopped before the batch was armed");
        return Ok(Tally::default());
    };
    let armed = Armed {
        image: loaded.image,
        name: loaded.name,
        version: loaded.version,
        source: source.clone(),
    };
    armed.check(&filter).map_err(|error| arm_error(&error))?;
    // The default record directory, the user's data directory, need not exist yet.
    if let Some(dir) = record.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("making the record file's directory {}: {e}", dir.display()))?;
    }
    let file = open_record(record)
        .map_err(|e| format!("opening the record file {}: {e}", record.display()))?;
    forward.send(Event::BatchArmed {
        name: armed.name.clone(),
        filter: filter.describe(),
    });
    tracing::info!("armed: {}, for {}", armed.name, filter.describe());
    let mut batch = Batch::arm(
        bus,
        armed,
        filter,
        Timing::default(),
        stop.clone(),
        cancel.clone(),
        limit,
    )
    .map_err(|error| arm_error(&error))?;
    let mut observer = BatchForward {
        forward,
        record: std::io::BufWriter::new(file),
    };
    let mut shown = None;
    let mut stepped = Ok(());
    while !batch.finished() && stepped.is_ok() {
        stepped = batch.step(&mut observer);
        let now = state(&batch);
        if shown.as_ref() != Some(&now) {
            observer.forward.send(Event::BatchState(now.clone()));
            shown = Some(now);
        }
    }
    batch.close(&mut observer);
    stepped.map_err(|e| e.to_string())?;
    Ok(batch.tally())
}

fn serve<B: Bus + Sync, H: Http + Clone + Send + 'static>(
    bus: &B,
    github: &GitHub<H>,
    requests: &Receiver<Request>,
    events: &Sender<Event>,
    repaint: &(dyn Fn() + Send),
) {
    let mut forward = Forward { events, repaint };
    while let Ok(request) = requests.recv() {
        match request {
            Request::Detect => {
                forward.send(Event::Units(detect(bus).map_err(|e| e.to_string())));
            }
            Request::Release => {
                let release = source::release(github, None)
                    .map(Box::new)
                    .map_err(|e| e.to_string());
                forward.send(Event::Release(release));
            }
            Request::Install {
                unit,
                source,
                force,
                download,
                cancel,
            } => {
                let result = install(
                    bus,
                    github,
                    &unit,
                    &source,
                    force,
                    download,
                    &cancel,
                    &mut forward,
                );
                match &result {
                    Ok(_) => tracing::info!("the update finished"),
                    Err(error) => tracing::error!("{}", error.message),
                }
                forward.send(Event::Finished(result));
            }
            Request::Batch {
                source,
                filter,
                download,
                limit,
                record,
                stop,
                cancel,
            } => {
                let result = run_batch(
                    bus,
                    github,
                    &mut forward,
                    &source,
                    filter,
                    download,
                    limit,
                    &record,
                    &stop,
                    &cancel,
                );
                if let Err(error) = &result {
                    tracing::error!("the batch stopped: {error}");
                }
                forward.send(Event::BatchEnded(result));
            }
        }
    }
}

/// The worker thread. Dropping it lets the thread finish its request and end.
pub struct Worker {
    requests: Option<Sender<Request>>,
    events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn<B, H>(bus: B, github: GitHub<H>, repaint: impl Fn() + Send + 'static) -> Worker
    where
        B: Bus + Send + Sync + 'static,
        H: Http + Clone + Send + 'static,
    {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let thread = std::thread::Builder::new()
            .name("usb".into())
            .spawn(move || serve(&bus, &github, &request_rx, &event_tx, &repaint))
            .expect("the worker thread starts");
        Worker {
            requests: Some(request_tx),
            events: event_rx,
            thread: Some(thread),
        }
    }
}

impl Backend for Worker {
    fn send(&mut self, request: Request) {
        if let Some(requests) = &self.requests {
            // A closed channel means the worker panicked; its join reports that.
            let _ = requests.send(request);
        }
    }

    fn try_recv(&mut self) -> Option<Event> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if let Some(thread) = self.thread.take()
                    && let Err(panic) = thread.join()
                {
                    let text = panic
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_owned())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "no message".into());
                    tracing::error!("the USB worker stopped: {text}");
                    return Some(Event::Finished(Err(JobError {
                        message: format!("the USB worker stopped unexpectedly: {text}"),
                        advice: Some(
                            "Restart the tool. If a flash was under way, flash the unit again.",
                        ),
                        left_in_isp: true,
                    })));
                }
                None
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Records requests and hands out events a test queues.
#[derive(Default)]
pub struct Manual {
    pub sent: Vec<String>,
    pub queued: std::collections::VecDeque<Event>,
    pub last_cancel: Option<Cancel>,
}

impl Backend for Manual {
    fn send(&mut self, request: Request) {
        self.sent.push(match request {
            Request::Detect => "detect".into(),
            Request::Release => "release".into(),
            Request::Install {
                source,
                download,
                cancel,
                ..
            } => {
                self.last_cancel = Some(cancel);
                format!("install {source:?} {download:?}")
            }
            Request::Batch {
                source,
                download,
                stop,
                ..
            } => {
                self.last_cancel = Some(stop);
                format!("batch {source:?} {download:?}")
            }
        });
    }

    fn try_recv(&mut self) -> Option<Event> {
        self.queued.pop_front()
    }
}

/// A [`Manual`] the test keeps a handle to while the window owns the other.
pub type Shared = std::sync::Arc<std::sync::Mutex<Manual>>;

impl Backend for Shared {
    fn send(&mut self, request: Request) {
        self.lock().expect("the test's backend lock").send(request);
    }

    fn try_recv(&mut self) -> Option<Event> {
        self.lock().expect("the test's backend lock").try_recv()
    }
}
