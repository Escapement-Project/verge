//! USB work on a thread of its own: the window sends requests and drains events each frame.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;

use updater_core::progress::{Cancel, Progress, Step};
use updater_core::release::{GitHub, Http, Release};
use updater_core::source::{self, LoadError, Source};
use updater_core::update::{self, Outcome, Timing, UpdateError};
use updater_core::{Bus, Unit, detect};

pub enum Request {
    Detect,
    Release,
    Install {
        unit: Box<Unit>,
        source: Source,
        force: bool,
        cancel: Cancel,
    },
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

fn load_error(error: &LoadError) -> JobError {
    let advice = match error {
        LoadError::AlreadyRuns { .. } | LoadError::Newer { .. } => {
            Some("Tick Install anyway to flash it regardless.")
        }
        LoadError::NoStock { .. } => Some("Install NXP's LinkServer, or pick the image as a file."),
        _ => None,
    };
    JobError {
        message: error.to_string(),
        advice,
        left_in_isp: false,
    }
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

fn install<B: Bus, H: Http>(
    bus: &B,
    github: &GitHub<H>,
    unit: &Unit,
    source: &Source,
    force: bool,
    cancel: &Cancel,
    progress: &mut Forward<'_>,
) -> Result<Outcome, JobError> {
    let loaded = source::load(source, github, Some(unit), force, progress)
        .map_err(|error| load_error(&error))?;
    progress.send(Event::Loaded(loaded.name.clone()));
    tracing::info!("installing {}", loaded.name);
    update::install(
        bus,
        unit,
        &loaded.image,
        &Timing::default(),
        progress,
        cancel,
    )
    .map_err(|error| update_error(&error))
}

fn serve<B: Bus, H: Http>(
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
                cancel,
            } => {
                let result = install(bus, github, &unit, &source, force, &cancel, &mut forward);
                match &result {
                    Ok(_) => tracing::info!("the update finished"),
                    Err(error) => tracing::error!("{}", error.message),
                }
                forward.send(Event::Finished(result));
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
        B: Bus + Send + 'static,
        H: Http + Send + 'static,
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
            Request::Install { source, cancel, .. } => {
                self.last_cancel = Some(cancel);
                format!("install {source:?}")
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
