//! USB work on a thread of its own: the window sends requests and drains events each frame.

use std::num::NonZeroUsize;

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use std::path::PathBuf;

use escapement_verge_probe::batch::{
    ArmError, Armed, Batch, Entry, Filter, Observer, Record, RecordWriter, Status, Tally,
    open_record,
};
use escapement_verge_probe::board::Board;
use escapement_verge_probe::dap::EntryError;
use escapement_verge_probe::detect::Hardware;
use escapement_verge_probe::progress::{Cancel, Left, Progress, Step};
use escapement_verge_probe::release::{Download, GitHub, Http, Release, ReleaseError};
use escapement_verge_probe::source::{self, LoadError, Loaded, Source};
use escapement_verge_probe::update::{self, Outcome, RestartError, Timing, UpdateError};
use escapement_verge_probe::{Bus, Unit, detect};

pub enum Request {
    Bench(crate::bench_worker::Command),
    Detect,
    /// Looks up the release with this tag, or the latest.
    Release(Option<String>),
    /// Restarts a unit running Verge, as `verge restart` does, then looks at the bus again.
    Restart(Box<Unit>),
    /// Resets the unit in ISP mode at this port out of it, as `verge isp reset` does.
    Reset {
        port: Option<String>,
        cancel: Cancel,
    },
    /// Restarts a unit into its bootloader without the jumper and leaves it there, as `verge isp
    /// enter` does; with `stock`, for NXP's firmware, which goes on any board.
    Enter {
        unit: Box<Unit>,
        stock: bool,
        cancel: Cancel,
    },
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
        limit: Option<NonZeroUsize>,
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
    pub advice: Option<String>,
    /// Whether the unit may have been left in ISP mode.
    pub left_in_isp: bool,
    /// What a cancel left on the unit, where the job was cancelled.
    pub stopped: Option<Stopped>,
}

/// What a cancel left on the unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// Stopped before anything was sent to the unit.
    Unchanged,
    /// Stopped once the unit was asked for its bootloader, leaving it as this says.
    Left(Left),
}

#[derive(Clone, Debug)]
pub enum ProgressEvent {
    Plan(Vec<Step>),
    Start { step: Step, total: u64 },
    Advance(u64),
    Note(String),
}

#[derive(Clone, Debug)]
pub enum InstallEvent {
    Loaded(String),
    Finished(Result<Outcome, JobError>),
    /// The unit waits in ISP mode, or why it does not.
    Entered(Result<(), JobError>),
}

#[derive(Clone, Debug)]
pub enum BatchEvent {
    Armed { name: String, filter: String },
    Starting(String),
    Record(Box<Record>),
    State(BatchState),
    Ended(Result<Tally, String>),
}

#[derive(Clone, Debug)]
pub enum Event {
    Bench(crate::bench_worker::Status),
    Units(Result<Vec<Unit>, String>),
    Release(Result<Box<Release>, String>),
    Progress(ProgressEvent),
    Install(InstallEvent),
    Batch(BatchEvent),
    /// A restart's result: an error says why the unit did not come back.
    Restarted(Result<(), String>),
    WorkerFailed(JobError),
}

/// What the window talks to: a worker thread, or in tests a hand-driven stand-in.
pub trait Backend {
    fn send(&mut self, request: Request);
    fn try_recv(&mut self) -> Option<Event>;
}

/// Forwards an operation's progress as events.
type BenchStatus = std::sync::Arc<std::sync::Mutex<Option<crate::bench_worker::Status>>>;

struct Forward<'a> {
    bench_status: &'a BenchStatus,
    events: &'a Sender<Event>,
    repaint: &'a (dyn Fn() + Send),
}

impl Forward<'_> {
    fn send(&self, event: Event) {
        // The window may have gone; nothing then reads the event.
        if let Event::Bench(status) = event {
            *self.bench_status.lock().expect("bench status lock") = Some(status);
        } else {
            let _ = self.events.send(event);
        }
        (self.repaint)();
    }
}

impl Progress for Forward<'_> {
    fn plan(&mut self, steps: &[Step]) {
        self.send(Event::Progress(ProgressEvent::Plan(steps.to_vec())));
    }
    fn start(&mut self, step: Step, total: u64) {
        self.send(Event::Progress(ProgressEvent::Start { step, total }));
    }
    fn advance(&mut self, done: u64) {
        self.send(Event::Progress(ProgressEvent::Advance(done)));
    }
    fn note(&mut self, text: &str) {
        self.send(Event::Progress(ProgressEvent::Note(text.to_owned())));
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
        self.send(Event::Progress(ProgressEvent::Plan(steps.to_vec())));
    }
    fn start(&mut self, step: Step, total: u64) {
        self.send(Event::Progress(ProgressEvent::Start { step, total }));
    }
    fn advance(&mut self, done: u64) {
        self.send(Event::Progress(ProgressEvent::Advance(done)));
    }
    fn note(&mut self, text: &str) {
        self.send(Event::Progress(ProgressEvent::Note(text.to_owned())));
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
            Some("Pick NXP's firmware instead: it goes on any board.")
        }
        error if download_again(error) => Some(DOWNLOAD_AGAIN),
        LoadError::Release(ReleaseError::CachedCopyFailed { left: Some(_), .. }) => {
            Some(CLEAR_THE_COPY)
        }
        _ => None,
    };
    JobError {
        message: error.to_string(),
        advice: advice.map(str::to_owned),
        left_in_isp: false,
        stopped: None,
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

/// Looks the release `source` names up again for the window: its tag, or the latest. Its
/// questions judge the cache by its own lookup, and a release the cache could not give may have
/// been published again since, so without this the next question would still name no download.
fn look_up_again<H: Http>(github: &GitHub<H>, source: &Source, forward: &Forward<'_>) {
    let tag = match source {
        Source::Release(tag) => tag.as_deref(),
        _ => None,
    };
    let release = source::release(github, tag)
        .map(Box::new)
        .map_err(|e| e.to_string());
    forward.send(Event::Release(release));
}

/// Why a batch did not arm, with what to change in the batch view.
pub fn arm_error(error: &ArmError) -> String {
    let advice = match error {
        ArmError::OursUndated => {
            " Tick \"of any build\" to take every unit running Verge, or untick Verge."
        }
        ArmError::NotForBoard { .. } => " Pick a Verge image built for it, or NXP's firmware.",
        ArmError::Image(_) => "",
    };
    format!("{}{advice}", crate::app::sentence(&error.to_string()))
}

/// What to do once a cancel left the unit as `left`, in the window's words: a unit in ISP mode is
/// listed under Units, with Reset out of ISP mode and the image choices.
fn recovery(left: Left, board: Option<Board>) -> Option<String> {
    match left {
        Left::InIsp => Some(
            "Reset out of ISP mode starts the image it holds, and flashing it installs the new one."
                .to_owned(),
        ),
        Left::Partial | Left::Unverified => Some(match board {
            Some(board) => format!(
                "Flash it again with the same image, which is built for {}: in ISP mode the window cannot tell its board.",
                board.a()
            ),
            None => "Flash it again with the same image.".to_owned(),
        }),
        Left::Flashed | Left::Reset => None,
    }
}

/// Why entering ISP mode failed. Nothing is flashed, so there is no new image to install.
fn enter_error(error: &UpdateError) -> JobError {
    match error {
        UpdateError::Cancelled(left) => JobError {
            message: error.to_string(),
            advice: (*left == Left::InIsp)
                .then(|| "Reset out of ISP mode starts the image it holds.".to_owned()),
            left_in_isp: left.in_isp(),
            stopped: Some(Stopped::Left(*left)),
        },
        error => update_error(error, None),
    }
}

/// Why a reset out of ISP mode failed. The unit stays in ISP mode unless it left it: a cancel
/// while it restarts, or a restart it did not come back from.
fn reset_error(error: &UpdateError) -> JobError {
    let (left_in_isp, stopped) = match error {
        UpdateError::Cancelled(left) => (left.in_isp(), Some(Stopped::Left(*left))),
        UpdateError::Stopped => (true, Some(Stopped::Unchanged)),
        // The bootloader is still there: it did not answer, or refused the reset.
        UpdateError::RomSilent { .. } | UpdateError::Isp(_) => (true, None),
        // No bootloader to reset, or what came back after the reset was not the unit as expected.
        _ => (false, None),
    };
    JobError {
        message: error.to_string(),
        advice: matches!(error, UpdateError::RomSilent { .. })
            .then(|| update::ROM_SILENT_ADVICE.to_owned()),
        left_in_isp,
        stopped,
    }
}

/// Why an update failed, and what to do. `board` is the unit's board where it is known, for the
/// recovery of a cancel that left it in ISP mode.
fn update_error(error: &UpdateError, board: Option<Board>) -> JobError {
    match error {
        UpdateError::Cancelled(left) => {
            return JobError {
                message: error.to_string(),
                advice: recovery(*left, board),
                left_in_isp: left.in_isp(),
                stopped: Some(Stopped::Left(*left)),
            };
        }
        UpdateError::Stopped => {
            return JobError {
                message: error.to_string(),
                advice: None,
                left_in_isp: false,
                stopped: Some(Stopped::Unchanged),
            };
        }
        // The bootloader never answered, so nothing was written.
        UpdateError::RomSilent { .. } => {
            return JobError {
                message: error.to_string(),
                advice: Some(update::ROM_SILENT_ADVICE.to_owned()),
                left_in_isp: true,
                stopped: None,
            };
        }
        _ => {}
    }
    let (advice, left_in_isp) = match error {
        UpdateError::Isp(_) => (
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
        UpdateError::OtherId { .. } => (
            Some(
                "To bring it back, fit the ISP jumper and flash an image that uses an ID the tool looks for.",
            ),
            false,
        ),
        UpdateError::Entry(EntryError::DaplinkRsBusy) => {
            (Some("Try again once it has finished."), false)
        }
        UpdateError::NoBootloader(_) | UpdateError::OursEntry(_) | UpdateError::Entry(_) => {
            (Some("Use the ISP jumper instead."), false)
        }
        _ => (None, false),
    };
    JobError {
        message: error.to_string(),
        advice: advice.map(str::to_owned),
        left_in_isp,
        stopped: None,
    }
}

/// Restarts a unit running Verge, as `verge restart` does, and waits for it to come back, in the
/// window's words.
fn restart<B: Bus>(bus: &B, unit: &Unit) -> Result<(), String> {
    let name = unit.serial().unwrap_or("the unit");
    update::restart(bus, unit).map_err(|error| match error {
        RestartError::Open(error) => format!("opening {name}: {error}"),
        RestartError::Request(error) => format!("asking {name} to restart: {error}"),
        RestartError::Look(error) => error.to_string(),
        RestartError::NoReturn => format!(
            "{name} did not come back within {} s of its restart",
            update::RESTART_RETURNS.as_secs()
        ),
    })
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
            look_up_again(github, source, progress);
        }
        load_error(&error)
    })?
    .ok_or_else(|| JobError {
        message: UpdateError::Stopped.to_string(),
        advice: None,
        left_in_isp: false,
        stopped: Some(Stopped::Unchanged),
    })?;
    progress.send(Event::Install(InstallEvent::Loaded(loaded.name.clone())));
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
    .map_err(|error| {
        let board = match unit.hardware() {
            Hardware::Board(board) => Some(board),
            _ => None,
        };
        update_error(&error, board)
    })
}

/// A batch's events, and its record file.
struct BatchForward<'a, 'b> {
    forward: &'a mut Forward<'b>,
    record: RecordWriter<std::io::BufWriter<std::fs::File>>,
    record_error: Option<std::io::Error>,
    stop: Cancel,
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
        self.forward
            .send(Event::Batch(BatchEvent::Starting(entry.port.clone())));
    }

    fn record(&mut self, record: &Record) {
        if self.record_error.is_none()
            && let Err(error) = self.record.write(record)
        {
            tracing::error!("the record file: {error}");
            self.record_error = Some(error);
            self.stop.cancel();
        }
        tracing::info!("{}: {}", record.port, record.result);
        self.forward
            .send(Event::Batch(BatchEvent::Record(Box::new(record.clone()))));
    }

    fn paused(&mut self, reason: Option<&str>) {
        match reason {
            Some(reason) => tracing::warn!("paused. {reason}"),
            None => tracing::info!("resumed"),
        }
    }
}

fn state<B: Bus + Sync>(batch: &Batch<'_, B>, source: &Source) -> BatchState {
    BatchState {
        tally: batch.tally(),
        waiting: batch
            .entries()
            .iter()
            .filter(|e| e.status == Status::Waiting)
            .map(|e| (e.port.clone(), e.serial_before.clone()))
            .collect(),
        paused: batch.paused().map(|reason| match batch.failed_in_isp() {
            Some(entry) => failed_in_isp(entry, source),
            None => reason.to_owned(),
        }),
    }
}

/// The wait for a unit that failed and is in ISP mode, in the window's words: the window flashes
/// it under Units once the batch is stopped.
fn failed_in_isp(entry: &Entry, source: &Source) -> String {
    let image = match source {
        Source::Release(Some(tag)) => format!("release {tag}"),
        Source::Release(None) => "the latest release".to_owned(),
        Source::Stock(_) => crate::app::STOCK.to_owned(),
        Source::File(path) => format!("the file {}", path.display()),
    };
    let board = match &entry.hardware {
        Hardware::Board(board) => format!(", {},", board.a()),
        _ => String::new(),
    };
    format!(
        "The unit at {}{board} failed and is in ISP mode, maybe with a partial image. The batch \
         waits until it is unplugged. To flash it again with the batch's image, stop the batch, \
         then pick the unit in ISP mode under Units and flash it with {image}.",
        entry.port
    )
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
    limit: Option<NonZeroUsize>,
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
            look_up_again(github, source, forward);
        }
        match load_error(&error).advice.as_deref() {
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
    let armed = Armed::new(loaded, source.clone(), github.repo().clone());
    armed.check(&filter).map_err(|error| arm_error(&error))?;
    // The default record directory, the user's data directory, need not exist yet.
    if let Some(dir) = record.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("making the record file's directory {}: {e}", dir.display()))?;
    }
    let file = open_record(record)
        .map_err(|e| format!("opening the record file {}: {e}", record.display()))?;
    forward.send(Event::Batch(BatchEvent::Armed {
        name: armed.name.clone(),
        filter: filter.describe(),
    }));
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
        record: RecordWriter::new(std::io::BufWriter::new(file)),
        record_error: None,
        stop: stop.clone(),
    };
    let mut shown = None;
    let mut stepped = Ok(());
    while !batch.finished() && stepped.is_ok() {
        stepped = batch.step(&mut observer);
        let now = state(&batch, source);
        if shown.as_ref() != Some(&now) {
            observer
                .forward
                .send(Event::Batch(BatchEvent::State(now.clone())));
            shown = Some(now);
        }
    }
    batch.close(&mut observer);
    if let Some(error) = observer.record_error {
        return Err(format!("writing the batch record file: {error}"));
    }
    stepped.map_err(|e| e.to_string())?;
    Ok(batch.tally())
}

fn serve<B: Bus + Sync, H: Http + Clone + Send + 'static>(
    bus: &B,
    github: &GitHub<H>,
    requests: &Receiver<Request>,
    events: &Sender<Event>,
    repaint: &(dyn Fn() + Send),
    bench_status: &BenchStatus,
) {
    let mut forward = Forward {
        events,
        repaint,
        bench_status,
    };
    let mut bench: Option<crate::bench_worker::Session<B::Aux>> = None;
    loop {
        if let Some(session) = bench.as_mut() {
            match session.tick() {
                Ok(true) => forward.send(Event::Bench(session.status())),
                Ok(false) => {}
                Err(error) => {
                    session.fail(error);
                    forward.send(Event::Bench(session.status()));
                    bench = None;
                }
            }
        }
        let request =
            match requests.recv_timeout(if bench.as_ref().is_some_and(|b| b.recording()) {
                Duration::ZERO
            } else {
                Duration::from_millis(50)
            }) {
                Ok(request) => request,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            };
        match request {
            Request::Bench(command) => {
                use crate::bench_worker::{Command, Session, Status};
                match command {
                    Command::Connect(unit) => {
                        bench.take();
                        match Session::open(bus, &unit) {
                            Ok(session) => {
                                forward.send(Event::Bench(session.status()));
                                bench = Some(session);
                            }
                            Err(error) => forward.send(Event::Bench(Status {
                                instance: unit.device.instance.clone(),
                                dio_reason: Some(crate::bench_worker::DioUnavailable::connection(
                                    &error,
                                )),
                                refused: crate::bench_worker::refused(&error),
                                error: Some(format!("{error:#}")),
                                ..Status::default()
                            })),
                        }
                    }
                    Command::Disconnect => {
                        bench.take();
                    }
                    command => {
                        if let Some(session) = bench.as_mut() {
                            let open = session.command(command);
                            forward.send(Event::Bench(session.status()));
                            if !open {
                                bench = None;
                            }
                        }
                    }
                }
            }
            Request::Detect => {
                bench.take();
                forward.send(Event::Units(detect(bus).map_err(|e| e.to_string())));
            }
            Request::Release(tag) => {
                let release = source::release(github, tag.as_deref())
                    .map(Box::new)
                    .map_err(|e| e.to_string());
                forward.send(Event::Release(release));
            }
            Request::Restart(unit) => {
                bench.take();
                let result = restart(bus, &unit);
                if let Err(error) = &result {
                    tracing::error!("{error}");
                }
                forward.send(Event::Restarted(result));
                forward.send(Event::Units(detect(bus).map_err(|e| e.to_string())));
            }
            Request::Reset { port, cancel } => {
                bench.take();
                let result = update::reset_now(
                    bus,
                    port.as_deref(),
                    &Timing::default(),
                    &mut forward,
                    &cancel,
                )
                .map_err(|error| reset_error(&error));
                match &result {
                    Ok(_) => tracing::info!("the reset finished"),
                    Err(error) => tracing::error!("{}", error.message),
                }
                forward.send(Event::Install(InstallEvent::Finished(result)));
            }
            Request::Enter {
                unit,
                stock,
                cancel,
            } => {
                bench.take();
                let result = update::enter_bootloader(
                    bus,
                    &unit,
                    stock,
                    &Timing::default(),
                    &mut forward,
                    &cancel,
                )
                .map(drop)
                .map_err(|error| enter_error(&error));
                match &result {
                    Ok(()) => {
                        tracing::info!("{} is in ISP mode", unit.serial().unwrap_or("the unit"))
                    }
                    Err(error) => tracing::error!("{}", error.message),
                }
                forward.send(Event::Install(InstallEvent::Entered(result)));
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
                forward.send(Event::Install(InstallEvent::Finished(result)));
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
                forward.send(Event::Batch(BatchEvent::Ended(result)));
            }
        }
    }
}

/// The worker thread. Dropping it lets the thread finish its request and end.
pub struct Worker {
    requests: Option<Sender<Request>>,
    events: Receiver<Event>,
    bench_status: BenchStatus,
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
        let bench_status = BenchStatus::default();
        let work_status = bench_status.clone();
        let thread = std::thread::Builder::new()
            .name("usb".into())
            .spawn(move || {
                serve(
                    &bus,
                    &github,
                    &request_rx,
                    &event_tx,
                    &repaint,
                    &work_status,
                )
            })
            .expect("the worker thread starts");
        Worker {
            requests: Some(request_tx),
            events: event_rx,
            bench_status,
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
        if let Some(status) = self.bench_status.lock().expect("bench status lock").take() {
            return Some(Event::Bench(status));
        }
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
                    return Some(Event::WorkerFailed(JobError {
                        message: format!("the USB worker stopped: {text}"),
                        advice: Some(
                            "Quit and start Verge again. If a flash was under way, flash the unit again."
                                .into(),
                        ),
                        left_in_isp: true,
                        stopped: None,
                    }));
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
            Request::Bench(command) => format!("bench {command:?}"),
            Request::Detect => "detect".into(),
            Request::Release(None) => "release".into(),
            Request::Release(Some(tag)) => format!("release {tag}"),
            Request::Restart(unit) => format!("restart {}", unit.device.instance),
            Request::Reset { port, cancel } => {
                self.last_cancel = Some(cancel);
                format!("reset {port:?}")
            }
            Request::Enter {
                unit,
                stock,
                cancel,
            } => {
                self.last_cancel = Some(cancel);
                format!("enter {} stock {stock}", unit.device.instance)
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A cancel says what it left, and a unit left in ISP mode gets the way back in the window's
    /// words, not a command. One after the verified flash leaves the unit restarting, and needs
    /// none.
    #[test]
    fn a_cancel_says_what_it_left_and_how_to_recover_it() {
        for left in [
            Left::InIsp,
            Left::Partial,
            Left::Unverified,
            Left::Flashed,
            Left::Reset,
        ] {
            let error = update_error(&UpdateError::Cancelled(left), None);
            assert_eq!(error.message, left.to_string());
            assert_eq!(error.left_in_isp, left.in_isp(), "{left:?}");
            assert_eq!(error.stopped, Some(Stopped::Left(left)));
            assert_eq!(error.advice.is_some(), left.in_isp(), "{left:?}");
            assert!(
                !error
                    .advice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("verge "),
                "{left:?}: {:?}",
                error.advice
            );
        }
        let error = update_error(&UpdateError::Cancelled(Left::Partial), Some(Board::McuLink));
        let advice = error.advice.unwrap();
        assert!(advice.contains("a standalone MCU-Link"), "{advice}");
    }

    /// A reset leaves the unit in ISP mode unless it left it: cancelled while it restarts, or gone
    /// and not back. An entry cancelled once the unit is in ISP mode says so, with the way out.
    #[test]
    fn a_reset_or_an_entry_says_where_it_left_the_unit() {
        let restarting = reset_error(&UpdateError::Cancelled(Left::Reset));
        assert!(!restarting.left_in_isp);
        assert_eq!(restarting.stopped, Some(Stopped::Left(Left::Reset)));
        let gone = reset_error(&UpdateError::NoReturn(std::time::Duration::from_secs(10)));
        assert!(!gone.left_in_isp);
        assert!(!reset_error(&UpdateError::NoneInIsp).left_in_isp);
        let unchanged = reset_error(&UpdateError::Stopped);
        assert!(unchanged.left_in_isp);
        assert_eq!(unchanged.stopped, Some(Stopped::Unchanged));
        let entered = enter_error(&UpdateError::Cancelled(Left::InIsp));
        assert!(entered.left_in_isp);
        assert_eq!(entered.stopped, Some(Stopped::Left(Left::InIsp)));
        assert_eq!(
            enter_error(&UpdateError::Stopped).stopped,
            Some(Stopped::Unchanged)
        );
        assert_eq!(
            entered.advice.as_deref(),
            Some("Reset out of ISP mode starts the image it holds.")
        );
        let silent = update_error(
            &UpdateError::RomSilent {
                waited: std::time::Duration::from_secs(5),
                last: "timed out".into(),
            },
            None,
        );
        assert_eq!(silent.advice.as_deref(), Some(update::ROM_SILENT_ADVICE));
    }
}
