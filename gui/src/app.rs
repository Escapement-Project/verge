//! The window: the attached units on the left, the selected one's state and action on the right,
//! the log along the bottom.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use egui::{
    Color32, CornerRadius, Frame, Margin, ScrollArea, Stroke, Ui, ViewportCommand, pos2, vec2,
};
use escapement_verge_probe::board::{self, Board};
use escapement_verge_probe::cache::Cache;
use escapement_verge_probe::detect::{DENIED, Firmware, Guess, Hardware, Jumpered, State};
use escapement_verge_probe::image::{Image, PAGE};
use escapement_verge_probe::paths::{ISP_IDENTITY_WARNING, USB_PERMISSIONS};
use escapement_verge_probe::progress::{Cancel, Left, Step, Unit as StepUnit};
use escapement_verge_probe::release::{Download, Release, Repo};
use escapement_verge_probe::source::{self, ImageVersion, LoadError, Source};
use escapement_verge_probe::stock::{self, StockImage};
use escapement_verge_probe::update::{self, Outcome};
use escapement_verge_probe::{Path, Unit, offer};

use crate::glyph::{self, Glyph};
use crate::log::Log;
use crate::style::{self, Face, Role, Status, space, text};
use crate::worker::{
    Backend, BatchEvent, Event, InstallEvent, JobError, ProgressEvent, Request, Stopped,
};

mod batch;

use batch::{BatchForm, BatchRun};

/// What the window is started with.
pub struct Config {
    pub repo: Repo,
    /// The scenario's name when the bench is simulated, which the strip says.
    pub simulated: Option<String>,
    /// Where to look for LinkServer's stock images.
    pub stock_dirs: Vec<PathBuf>,
    /// Where a batch's record file goes unless the user names another.
    pub record_dir: PathBuf,
    /// Where the worker keeps the releases it downloads, so a question can say whether one has
    /// to be downloaded first.
    pub cache: Option<Cache>,
}

/// The window's two views: one unit at a time, or a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Units,
    Batch,
    Bench,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Release,
    Stock,
    File,
}

/// A file picked or dropped, read when it arrived.
struct Picked {
    path: PathBuf,
    image: Result<Image, String>,
    /// What the file's name says it is: NXP's firmware for LinkServer's own files.
    version: ImageVersion,
}

struct Current {
    step: Step,
    total: u64,
    done: u64,
    since: f64,
}

/// What a job does to its unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Update,
    /// Resets a unit in ISP mode out of it, flashing nothing.
    Reset,
    /// Restarts a unit into ISP mode and leaves it there.
    Enter,
}

/// How a job that did not fail ended.
#[derive(Clone, Debug)]
enum Done {
    /// The unit came back from an update or a reset, as [`Outcome`] says.
    Back(Outcome),
    /// The unit waits in ISP mode.
    InIsp,
}

struct Job {
    kind: Kind,
    target: String,
    location: Option<String>,
    image: Option<String>,
    rail: Vec<Step>,
    finished: Vec<(Step, f64)>,
    current: Option<Current>,
    notes: Vec<String>,
    /// A file dropped on the window while the job ran, which it ignored.
    dropped: Option<String>,
    /// What to say once the unit waits in ISP mode.
    then: Option<String>,
    started: f64,
    cancel: Cancel,
}

#[derive(Default)]
enum Operation {
    #[default]
    Idle,
    Installing(Job),
    Batch(BatchRun),
}

impl Operation {
    fn job(&self) -> Option<&Job> {
        match self {
            Self::Installing(job) => Some(job),
            _ => None,
        }
    }

    fn take_job(&mut self) -> Option<Job> {
        if !matches!(self, Self::Installing(_)) {
            return None;
        }
        match std::mem::take(self) {
            Self::Installing(job) => Some(job),
            _ => unreachable!(),
        }
    }
}

struct Ended {
    kind: Kind,
    result: Result<Done, JobError>,
    target: String,
    location: Option<String>,
    image: Option<String>,
    seconds: f64,
    steps: Vec<(Step, f64)>,
    /// The step under way when the job failed.
    failed: Option<Step>,
    notes: Vec<String>,
}

enum Scan {
    Scanning,
    Done,
    Failed(String),
}

/// What the slot offers for the selected unit, or why it offers nothing.
#[derive(Clone)]
enum Action {
    Ready {
        label: String,
        fill: Fill,
        source: Source,
        what: String,
    },
    Blocked {
        label: String,
        reason: String,
    },
}

/// The question before an update. What it says is downloaded first is fixed when it opens, so
/// that Confirm agrees to the download the user read about, and one that named none downloads
/// nothing.
#[derive(Clone)]
struct Confirming {
    unit: Box<Unit>,
    source: Source,
    what: String,
    download: Option<String>,
}

/// The worker's leave to download, from what the question said.
fn agreed(download: Option<&String>) -> Download {
    match download {
        Some(_) => Download::Agreed,
        None => Download::CachedOnly,
    }
}

pub struct VergeApp {
    backend: Box<dyn Backend>,
    log: Log,
    config: Config,
    units: Vec<Unit>,
    scan: Scan,
    worker_error: Option<String>,
    selected: Option<String>,
    release: Option<Result<Release, String>>,
    /// The release tag the user asked for; the latest without one.
    tag: Option<String>,
    /// The tag field, as typed.
    tag_field: String,
    stock: Option<StockImage>,
    choice: Choice,
    picked: Option<Picked>,
    force: bool,
    confirming: Option<Confirming>,
    /// The unit the question before entering ISP mode is about, while it is open.
    entering: Option<Box<Unit>>,
    operation: Operation,
    ended: Option<Ended>,
    close_pending: bool,
    log_open: bool,
    about_open: bool,
    picker: Option<Receiver<Option<PathBuf>>>,
    notice: Option<String>,
    view: View,
    batch_form: BatchForm,
    last_batch: Option<BatchRun>,
    bench: crate::bench::Bench,
    mark: style::Mark,
    /// Since when nothing has been attached, which times the waiting mark's loop.
    waiting_since: Option<f64>,
    /// The port of a unit Bench restarted, which stays selected when it comes back.
    restarted: Option<String>,
    /// The log drawer's height on the frame before, which its panel keeps for it.
    log_height: f32,
    /// The width the strip's buttons took on the frame before, which the release line leaves
    /// them.
    strip_end: f32,
}

/// The widest the Units and Batch views' column grows.
const COLUMN: f32 = 960.0;
/// The window height under which the unit's slab drops its glyph and large type.
const COMPACT_BELOW: f32 = 700.0;

fn kib(bytes: u64) -> String {
    format!("{:.1} KiB", bytes as f64 / 1024.0)
}

/// A filled slab or button: its ground, and the colour of what is drawn on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fill {
    pub ground: Color32,
    pub on: Color32,
}

impl Fill {
    /// Verge's identity: installing it, and an update that finished.
    pub fn lume(face: &Face) -> Fill {
        Fill {
            ground: face.lume,
            on: face.on_lume,
        }
    }

    pub fn status(status: Status, face: &Face) -> Fill {
        Fill {
            ground: status.fill(face),
            on: face.on_status,
        }
    }

    /// An action with no identity or status of its own.
    pub fn ink(face: &Face) -> Fill {
        Fill {
            ground: face.ink,
            on: face.page,
        }
    }

    /// Firmware that is not ours.
    pub fn other(face: &Face) -> Fill {
        Fill {
            ground: face.ink_2,
            on: face.page,
        }
    }
}

/// The button that resets a unit out of ISP mode without flashing it.
const RESET: &str = "Reset out of ISP mode";
/// The button that restarts a unit into ISP mode and leaves it there.
const ENTER: &str = "Enter ISP mode";

/// What the window calls NXP's own MCU-Link firmware.
pub(crate) const STOCK: &str = "NXP's firmware";

/// Whether `unit` runs a stock firmware too old to have the CMSIS-DAP v2 interface the tool asks
/// over: V2.x, which speaks HID only.
fn too_old_to_ask(unit: &Unit) -> bool {
    !unit.device.dap_v2
        && matches!(
            unit.state,
            State::Unreadable {
                guess: Guess::Stock(_),
                ..
            }
        )
}

/// The unit's board, for a label: its name, or `None` where nothing says which.
pub(crate) fn board_label(unit: &Unit) -> Option<String> {
    match unit.hardware() {
        Hardware::Board(board) => Some(board.to_string()),
        Hardware::Unknown => None,
        other => Some(other.name()),
    }
}

/// What the unit runs, as its row names it: "Verge 26ea505", "NXP's firmware V3.172".
pub(crate) fn firmware_label(unit: &Unit) -> String {
    let (word, ..) = identity(unit, &crate::tokens::NIGHT);
    match version_of(unit) {
        Some(version) => format!("{word} {version}"),
        None => word.to_owned(),
    }
}

/// The unit's state as the slab's word, its glyph and its fill. `None` is a frame, not a fill.
fn identity(unit: &Unit, face: &Face) -> (&'static str, Glyph, Option<Fill>) {
    match &unit.state {
        State::Probe {
            firmware: Firmware::Ours(_),
            ..
        } => ("Verge", glyph::OURS, Some(Fill::lume(face))),
        State::Probe {
            firmware: Firmware::Stock { .. },
            ..
        } => (STOCK, glyph::STOCK, Some(Fill::other(face))),
        State::Probe {
            firmware: Firmware::DaplinkRs { .. },
            ..
        } => ("daplink-rs", glyph::STOCK, Some(Fill::other(face))),
        State::SwdJumper {
            firmware: Jumpered::Ours,
        }
        | State::Held { .. } => ("Verge", glyph::OURS, Some(Fill::lume(face))),
        State::SwdJumper {
            firmware: Jumpered::Stock(_),
        } => (STOCK, glyph::STOCK, Some(Fill::other(face))),
        State::Unreadable { .. } if too_old_to_ask(unit) => {
            (STOCK, glyph::STOCK, Some(Fill::other(face)))
        }
        State::Unreadable { .. } => ("Unknown", glyph::ATTENTION, None),
        State::Bootloader => ("ISP mode", glyph::WAITING, None),
        State::MiniBootloader => ("Mini", glyph::WAITING, None),
    }
}

/// The version or build a unit runs, for the tally.
fn version_of(unit: &Unit) -> Option<String> {
    match &unit.state {
        State::Probe {
            firmware: Firmware::Ours(ours),
            ..
        }
        | State::Held { ours } => Some(ours.firmware.clone()),
        State::Probe {
            firmware: Firmware::Stock { version },
            ..
        } => version.map(|v| format!("V{v}")),
        State::Unreadable {
            guess: Guess::Stock(Some(v)),
            ..
        }
        | State::SwdJumper {
            firmware: Jumpered::Stock(Some(v)),
        } => Some(format!("V{v}")),
        _ => None,
    }
}

/// Whether the system does not let this user open `unit`: on Linux, what the udev rules allow.
fn denied(unit: &Unit) -> bool {
    matches!(&unit.state, State::Unreadable { reason, .. } if reason == DENIED)
}

/// `words` about the udev rules, and a link to them.
fn udev_rules(ui: &mut Ui, words: &str) {
    let face = style::face(ui.ctx());
    wrapping(ui, text(Role::Body, words).color(face.ink));
    // Underlined at rest: the window's links take the ink colour.
    ui.hyperlink_to(
        text(Role::Body, "Getting started: USB permissions on Linux").underline(),
        USB_PERMISSIONS,
    );
}

fn unit_name(unit: &Unit) -> String {
    let location = unit.device.location.as_deref().unwrap_or("?");
    match unit.serial() {
        Some(serial) => format!("{serial} at {location}"),
        None => format!("the unit at {location}"),
    }
}

fn frame(ui: &mut Ui, fill: Option<Color32>, stroke: Stroke, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(fill.unwrap_or(Color32::TRANSPARENT))
        .stroke(stroke)
        .corner_radius(CornerRadius::same(style::RADIUS))
        .inner_margin(Margin::same(space::L as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// A held notice or a refusal: a line with an edge in its status's ink.
pub(crate) fn edged(ui: &mut Ui, status: Status, words: &str) {
    let face = style::face(ui.ctx());
    let response = Frame::new()
        .fill(face.raised)
        .inner_margin(Margin {
            left: space::M as i8,
            right: space::S as i8,
            top: space::S as i8,
            bottom: space::S as i8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(text(Role::Body, words).color(face.ink));
        })
        .response;
    let rect = response.rect;
    ui.painter().rect_filled(
        egui::Rect::from_min_max(rect.min, pos2(rect.min.x + 3.0, rect.max.y)),
        0.0,
        status.ink(face),
    );
}

pub(crate) fn label(ui: &mut Ui, words: &str) -> egui::Response {
    let face = style::face(ui.ctx());
    ui.label(text(Role::Label, words).color(face.ink_2))
}

/// Text that wraps at the edge of the space it is given.
fn wrapping(ui: &mut Ui, rich: egui::RichText) {
    ui.add(egui::Label::new(rich).wrap());
}

/// A row of the technical strip: the name in a fixed column, the value wrapping beside it.
fn field(ui: &mut Ui, name: &str, add: impl FnOnce(&mut Ui)) {
    const NAME: f32 = 64.0;
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(NAME, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(NAME);
                label(ui, name);
            },
        );
        ui.vertical(add);
    });
}

/// A step's name on the rail, where a cell has room for one word.
fn short(step: Step) -> &'static str {
    match step {
        Step::Download => "Download",
        Step::EnterBootloader => "Enter",
        Step::WaitForBootloader => "Wait",
        Step::Connect => "Connect",
        Step::Erase => "Erase",
        Step::Write => "Write",
        Step::Verify => "Verify",
        Step::Reset => "Reset",
        Step::WaitForFirmware => "Return",
    }
}

/// The wash over a filled button the pointer is on, which egui leaves to the fill it is given.
fn wash_hovered(ui: &Ui, response: &egui::Response) {
    if response.hovered() && ui.is_enabled() {
        let face = style::face(ui.ctx());
        ui.painter().rect_filled(
            response.rect,
            CornerRadius::same(style::RADIUS),
            face.wash(),
        );
    }
}

/// The 48 px action slab: filled when it acts, an outline when it is neutral.
fn slot(ui: &mut Ui, words: &str, fill: Option<Fill>) -> egui::Response {
    let words = text(Role::Action, words);
    let button = match fill {
        Some(fill) => egui::Button::new(words.color(fill.on))
            .fill(fill.ground)
            .stroke(Stroke::NONE),
        None => egui::Button::new(words),
    };
    let response = ui.add_sized(vec2(320.0, style::SLOT_HEIGHT), button);
    if fill.is_some() {
        wash_hovered(ui, &response);
    }
    response
}

/// A filled button the size of its words, for a narrow panel.
pub(crate) fn filled(ui: &mut Ui, words: &str, fill: Fill) -> egui::Response {
    let response = ui.add(
        egui::Button::new(text(Role::Action, words).color(fill.on))
            .fill(fill.ground)
            .stroke(Stroke::NONE),
    );
    wash_hovered(ui, &response);
    response
}

/// On the first frame `response` shows, gives it the keyboard's focus and scrolls it into sight.
fn opened(ui: &Ui, response: &egui::Response) {
    let id = response.id.with("shown");
    let frame = ui.ctx().cumulative_frame_nr();
    let last = ui.data(|d| d.get_temp::<u64>(id));
    if last.is_none_or(|last| last + 1 < frame) {
        response.request_focus();
        response.scroll_to_me(None);
    }
    ui.data_mut(|d| d.insert_temp(id, frame));
}

/// A question's answers: `confirm` filled, and Back. The keyboard's focus goes to `confirm` as the
/// question opens, which scrolls into sight, and Escape answers Back. Slots in a view, buttons the
/// size of their words in a narrow panel. `Some(true)` for `confirm`, `Some(false)` for Back.
pub(crate) fn answers(ui: &mut Ui, confirm: &str, fill: Fill, narrow: bool) -> Option<bool> {
    let mut answer = None;
    ui.horizontal(|ui| {
        let yes = if narrow {
            filled(ui, confirm, fill)
        } else {
            slot(ui, confirm, Some(fill))
        };
        opened(ui, &yes);
        if yes.clicked() {
            answer = Some(true);
        }
        let back = if narrow {
            outline(ui, "Back", true)
        } else {
            slot(ui, "Back", None)
        };
        if back.clicked() {
            answer = Some(false);
        }
    });
    if answer.is_none() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        answer = Some(false);
    }
    answer
}

/// One of a row of choices, the chosen one an ink fill with page text.
pub(crate) fn choice_button(face: &Face, words: &str, chosen: bool) -> egui::Button<'static> {
    let button = egui::Button::new(text(Role::Action, words)).selected(chosen);
    if chosen {
        button.stroke(Stroke::new(1.0, face.ink))
    } else {
        button
    }
}

pub(crate) fn choice(ui: &mut Ui, words: &str, chosen: bool) -> egui::Response {
    let face = style::face(ui.ctx());
    ui.add(choice_button(face, words, chosen))
}

/// A neutral outlined button.
pub(crate) fn outline(ui: &mut Ui, words: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(text(Role::Action, words)))
}

/// `words` with its first letter capitalised.
pub(crate) fn sentence_case(words: &str) -> String {
    let mut chars = words.chars();
    let mut out: String = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default();
    out.extend(chars);
    out
}

impl VergeApp {
    pub fn new(backend: Box<dyn Backend>, log: Log, config: Config) -> VergeApp {
        let stock = stock::newest_from(&config.stock_dirs);
        let batch_form = BatchForm::new(&config.record_dir);
        let bench = crate::bench::Bench::new(&config.record_dir);
        let mut app = VergeApp {
            backend,
            log,
            config,
            units: Vec::new(),
            scan: Scan::Scanning,
            worker_error: None,
            selected: None,
            release: None,
            tag: None,
            tag_field: String::new(),
            stock,
            choice: Choice::Release,
            picked: None,
            force: false,
            confirming: None,
            entering: None,
            operation: Operation::Idle,
            ended: None,
            close_pending: false,
            log_open: false,
            about_open: false,
            picker: None,
            notice: None,
            view: View::Units,
            batch_form,
            last_batch: None,
            bench,
            mark: style::Mark::default(),
            waiting_since: None,
            restarted: None,
            log_height: 32.0,
            strip_end: 200.0,
        };
        app.backend.send(Request::Detect);
        app.backend.send(Request::Release(None));
        app
    }

    #[cfg(test)]
    pub(crate) fn bench_mut(&mut self) -> &mut crate::bench::Bench {
        &mut self.bench
    }

    /// Whether an update or a batch is running, which holds the window open.
    #[must_use]
    pub fn busy(&self) -> bool {
        match &self.operation {
            Operation::Idle => self.bench.busy(),
            Operation::Installing(_) => true,
            Operation::Batch(run) => run.running(),
        }
    }

    fn rescan(&mut self) {
        if self.worker_error.is_some() {
            return;
        }
        self.bench.disconnect(self.backend.as_mut());
        self.scan = Scan::Scanning;
        self.backend.send(Request::Detect);
    }

    fn selected_unit(&self) -> Option<&Unit> {
        let instance = self.selected.as_deref()?;
        self.units.iter().find(|u| u.device.instance == instance)
    }

    fn handle(&mut self, event: Event, now: f64) {
        match event {
            Event::Bench(status) => self.bench.receive(status),
            Event::Units(Ok(units)) => {
                let kept = self
                    .selected
                    .clone()
                    .filter(|i| units.iter().any(|u| &u.device.instance == i));
                // After an update or a restart the unit comes back as a new enumeration on the
                // same port.
                let returned = self
                    .restarted
                    .take()
                    .or_else(|| self.ended.as_ref().and_then(|e| e.location.clone()))
                    .and_then(|location| {
                        units
                            .iter()
                            .find(|u| u.device.location.as_deref() == Some(location.as_str()))
                            .map(|u| u.device.instance.clone())
                    });
                self.selected = returned
                    .or(kept)
                    .or_else(|| units.first().map(|u| u.device.instance.clone()));
                self.units = units;
                self.scan = Scan::Done;
                self.bench.units_changed();
            }
            Event::Units(Err(error)) => {
                self.scan = Scan::Failed(error);
                self.confirming = None;
                self.entering = None;
            }
            Event::Restarted(result) => self.bench.restarted(result),
            Event::Release(release) => self.release = Some(release.map(|r| *r)),
            Event::Install(InstallEvent::Loaded(name)) => {
                if let Operation::Installing(job) = &mut self.operation {
                    job.image = Some(name);
                }
            }
            Event::Progress(progress) => self.progress(progress, now),
            Event::Batch(event) => {
                self.batch_event(event, now);
            }
            Event::Install(InstallEvent::Finished(result)) => {
                self.finish_job(result.map(Done::Back), now);
                self.rescan();
            }
            Event::Install(InstallEvent::Entered(result)) => {
                self.finish_job(result.map(|()| Done::InIsp), now);
                self.rescan();
            }
            Event::WorkerFailed(error) => {
                self.bench.worker_failed();
                let mut words = sentence(&error.message);
                if let Some(advice) = &error.advice {
                    words = format!("{words} {advice}");
                }
                self.worker_error = Some(words);
                self.finish_job(Err(error.clone()), now);
                self.batch_event(BatchEvent::Ended(Err(error.message.clone())), now);
                if self.release.is_none() {
                    self.release = Some(Err(error.message));
                }
            }
        }
    }

    fn batch(&self) -> Option<&BatchRun> {
        match &self.operation {
            Operation::Batch(run) => Some(run),
            _ => self.last_batch.as_ref(),
        }
    }

    fn progress(&mut self, event: ProgressEvent, now: f64) {
        match &mut self.operation {
            Operation::Idle => {}
            Operation::Batch(run) => run.progress(event, now),
            Operation::Installing(job) => match event {
                ProgressEvent::Plan(steps) => {
                    let downloaded = job.finished.iter().any(|(step, _)| *step == Step::Download)
                        || job
                            .current
                            .as_ref()
                            .is_some_and(|c| c.step == Step::Download);
                    job.rail = if downloaded {
                        std::iter::once(Step::Download).chain(steps).collect()
                    } else {
                        steps
                    };
                }
                ProgressEvent::Start { step, total } => {
                    if let Some(current) = job.current.take() {
                        job.finished.push((current.step, now - current.since));
                    }
                    job.current = Some(Current {
                        step,
                        total,
                        done: 0,
                        since: now,
                    });
                }
                ProgressEvent::Advance(done) => {
                    if let Some(current) = &mut job.current {
                        current.done = done;
                    }
                }
                ProgressEvent::Note(note) => job.notes.push(note),
            },
        }
    }

    fn finish_job(&mut self, result: Result<Done, JobError>, now: f64) {
        if let Some(mut job) = self.operation.take_job() {
            if matches!(result, Ok(Done::InIsp)) {
                job.notes.extend(job.then.take());
            }
            let mut failed = None;
            if let Some(current) = job.current.take() {
                match &result {
                    Ok(_) => job.finished.push((current.step, now - current.since)),
                    // A cancel stops a step; it does not fail it.
                    Err(JobError {
                        stopped: Some(_), ..
                    }) => {}
                    Err(_) => failed = Some(current.step),
                }
            }
            self.ended = Some(Ended {
                kind: job.kind,
                failed,
                result,
                target: job.target,
                location: job.location,
                image: job.image,
                seconds: now - job.started,
                steps: job.finished,
                notes: job.notes,
            });
        }
    }

    fn batch_event(&mut self, event: BatchEvent, now: f64) {
        let Operation::Batch(run) = &mut self.operation else {
            return;
        };
        let ended = matches!(event, BatchEvent::Ended(_));
        run.handle(event, now);
        if ended {
            if let Operation::Batch(run) = std::mem::take(&mut self.operation) {
                self.last_batch = Some(run);
            }
            self.rescan();
        }
    }

    fn take_file(&mut self, path: PathBuf) {
        if let Operation::Installing(job) = &mut self.operation {
            let running = match job.kind {
                Kind::Update => "an update",
                Kind::Reset => "a reset",
                Kind::Enter => "a restart into ISP mode",
            };
            job.dropped = Some(format!(
                "{} arrived while {running} was running, and was ignored.",
                path.display()
            ));
            return;
        }
        // Read as `--file` reads it: LinkServer's files are known by their names.
        let (image, version) = match source::load_file(&path) {
            Ok(loaded) => (Ok(loaded.image), loaded.version),
            Err(error) => (Err(sentence(&error.to_string())), ImageVersion::Unknown),
        };
        self.picked = Some(Picked {
            path,
            image,
            version,
        });
        self.choice = Choice::File;
        self.notice = None;
        // A question asked about another image no longer stands.
        self.confirming = None;
    }

    fn open_picker(&mut self, ctx: &egui::Context) {
        if self.picker.is_some() {
            return;
        }
        // The dialog is created on the window's thread, which macOS requires, and waited for on
        // another, so the window keeps drawing while it is open.
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("Pick a firmware image")
            .add_filter("Firmware image", &["s19", "srec", "elf"])
            .add_filter("Any file", &["*"])
            .pick_file();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let picked = pollster::block_on(dialog).map(|handle| handle.path().to_path_buf());
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
        self.picker = Some(rx);
    }

    fn poll_picker(&mut self) {
        let Some(picker) = &self.picker else {
            return;
        };
        match picker.try_recv() {
            Ok(picked) => {
                self.picker = None;
                if let Some(path) = picked {
                    self.take_file(path);
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.picker = None,
        }
    }

    /// The slot's offer for the selected unit.
    fn action(&self, face: &Face) -> Action {
        let blocked = |label: &str, reason: String| Action::Blocked {
            label: label.to_owned(),
            reason,
        };
        // The strip says why.
        if self.worker_error.is_some() {
            return blocked("Worker stopped", String::new());
        }
        // The unit may have gone, or come back as another enumeration, since the look that found
        // it.
        if let Scan::Failed(_) = self.scan {
            return blocked(
                "Cannot update",
                "The last look at USB failed. Rescan to try again.".into(),
            );
        }
        // `main` asks only once a unit is selected.
        let Some(unit) = self.selected_unit() else {
            return blocked("Pick a unit", "Pick a unit on the left.".into());
        };
        let offer = offer(unit, &self.units);
        let Some(path) = offer.best() else {
            return blocked("Cannot update", String::new());
        };
        let action = match self.choice {
            Choice::Release => match &self.release {
                None => blocked("Looking up releases", String::new()),
                Some(Err(error)) => blocked("No release", error.clone()),
                Some(Ok(release)) => match source::check_release(Some(unit), release, self.force) {
                    Ok(()) => Action::Ready {
                        label: if unit.build().is_some() {
                            format!("Update to {}", release.tag)
                        } else {
                            format!("Install {}", release.tag)
                        },
                        fill: Fill::lume(face),
                        source: Source::Release(Some(release.tag.clone())),
                        what: format!("release {}", release.tag),
                    },
                    Err(error @ (LoadError::AlreadyRuns { .. } | LoadError::Newer { .. })) => {
                        blocked(
                            "Nothing to update",
                            format!("{} Tick Install anyway to flash it regardless.", sentence(&error.to_string())),
                        )
                    }
                    Err(error) => blocked("No release", error.to_string()),
                },
            },
            Choice::Stock => match &self.stock {
                None => blocked(
                    &format!("No image of {STOCK}"),
                    "No MCU-LINK_CMSIS-DAP_V*.s19 in LinkServer's usual places. Install NXP's LinkServer, or pick the image as a file.".into(),
                ),
                Some(found) => Action::Ready {
                    label: format!("Install {STOCK} V{}", found.version),
                    fill: Fill::status(Status::Attention, face),
                    source: Source::Stock(found.path.parent().map(std::path::Path::to_path_buf)),
                    what: format!("{STOCK} V{}", found.version),
                },
            },
            Choice::File => match &self.picked {
                None => blocked("Pick a file", "Pick an ELF or S-record, or drop one on the window.".into()),
                Some(Picked { image: Err(error), .. }) => blocked("Not an image", error.clone()),
                Some(Picked {
                    path, image: Ok(_), ..
                }) => {
                    let name = path
                        .file_name()
                        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                    Action::Ready {
                        label: format!("Flash {name}"),
                        fill: Fill::ink(face),
                        source: Source::File(path.clone()),
                        what: name,
                    }
                }
            },
        };
        // A release's image says which boards it supports only once it is downloaded.
        if let Action::Ready { source, .. } = &action {
            let checked = match (source, &self.picked) {
                (
                    Source::File(_),
                    Some(Picked {
                        image: Ok(image),
                        version,
                        ..
                    }),
                ) => source::check_board(Some(unit), image, version),
                _ => source::check_unit(Some(unit), source),
            };
            if let Err(error) = checked {
                return blocked(
                    "Not for this board",
                    format!("{} Pick {STOCK} for that.", sentence(&error.to_string())),
                );
            }
        }
        if path == Path::Jumper {
            return blocked("Needs the jumper", jumper_advice(unit, &action));
        }
        action
    }

    /// The picked file's image, where it read as one.
    fn picked_image(&self) -> Option<&Image> {
        match &self.picked {
            Some(Picked {
                image: Ok(image), ..
            }) => Some(image),
            _ => None,
        }
    }

    /// The question's sentence on the boards the image is built for, where the unit cannot say
    /// its own: stock's goes on any, a file says, and a release says only once it is downloaded.
    fn built_for(&self, source: &Source) -> Option<String> {
        match source {
            Source::Stock(_) => None,
            // NXP's own file goes on any board.
            Source::File(_)
                if self
                    .picked
                    .as_ref()
                    .is_some_and(|picked| picked.version.is_stock()) =>
            {
                None
            }
            Source::File(_) => self
                .picked_image()
                .and_then(|image| board::image_is_for(image).ok())
                .map(|clause| format!("The image {clause}.")),
            Source::Release(_) => Some(
                "A release says which boards it is built for only once it is downloaded.".into(),
            ),
        }
    }

    /// What confirming `source` downloads first, for its question: a release the cache does not
    /// hold. Where this says nothing, the worker is told to use only the cache, so a release
    /// that left the cache since is refused rather than downloaded unasked.
    fn download(&self, source: &Source) -> Option<String> {
        let (Source::Release(Some(tag)), Some(Ok(release))) = (source, &self.release) else {
            return None;
        };
        let cached = self
            .config
            .cache
            .as_ref()
            .is_some_and(|cache| cache.holds(&self.config.repo, release));
        (release.tag == *tag && !cached).then(|| source::download_what(&self.config.repo, release))
    }

    /// A job on the unit `target` names at `location`, which the worker is about to run.
    fn job(&mut self, kind: Kind, target: String, location: Option<String>, now: f64) -> Cancel {
        let cancel = Cancel::new();
        self.operation = Operation::Installing(Job {
            kind,
            target,
            location,
            image: None,
            rail: Vec::new(),
            finished: Vec::new(),
            current: None,
            notes: Vec::new(),
            dropped: None,
            then: None,
            started: now,
            cancel: cancel.clone(),
        });
        self.ended = None;
        self.confirming = None;
        self.entering = None;
        cancel
    }

    fn start(&mut self, unit: Box<Unit>, source: Source, download: Download, now: f64) {
        let cancel = self.job(
            Kind::Update,
            unit_name(&unit),
            unit.device.location.clone(),
            now,
        );
        self.backend.send(Request::Install {
            unit,
            source,
            force: self.force,
            download,
            cancel,
        });
        self.force = false;
    }

    /// Restarts `unit` into its bootloader and leaves it there, as `verge isp enter` does: for
    /// NXP's firmware where that is the image chosen, which goes on any board.
    fn enter(&mut self, unit: Box<Unit>, now: f64) {
        let stock = self.choice == Choice::Stock;
        let then = match unit.hardware() {
            _ if stock => format!(
                "In ISP mode the tool cannot tell the unit's board: flash it with {STOCK}, which goes on any board."
            ),
            Hardware::Board(board) => format!(
                "In ISP mode the tool cannot tell the unit's board: flash it with an image built for {}.",
                board.a()
            ),
            _ => "In ISP mode the tool cannot tell the unit's board, and its firmware did not say: check that an image is built for it before you flash it.".to_owned(),
        };
        let cancel = self.job(
            Kind::Enter,
            unit_name(&unit),
            unit.device.location.clone(),
            now,
        );
        if let Operation::Installing(job) = &mut self.operation {
            job.then = Some(then);
        }
        self.backend.send(Request::Enter {
            unit,
            stock,
            cancel,
        });
    }

    /// Why `unit` cannot enter ISP mode from the window, by the rules `verge isp enter` refuses by:
    /// a board other than the standalone MCU-Link only for NXP's firmware, and only by a
    /// jumper-free path. `None` where it can.
    fn enter_refusal(&self, unit: &Unit) -> Option<String> {
        if let Err(refused) = update::check_entry(unit, self.choice == Choice::Stock) {
            return Some(format!(
                "{} To put {STOCK} on it, pick {STOCK} as the image first. To put a Verge image built for it on it, update it with that file instead: the update checks that the image declares this board before anything is flashed.",
                sentence(&refused.to_string())
            ));
        }
        // The bus's own answer is the worker's; real USB tells bootloaders apart on Linux only.
        update::entry_path(unit, &self.units, true, cfg!(target_os = "linux"))
            .err()
            .map(|refused| sentence(&refused.to_string()))
    }

    /// Resets the unit in ISP mode at `location` out of it, as `verge isp reset` does.
    fn reset(&mut self, target: String, location: Option<String>, now: f64) {
        let cancel = self.job(Kind::Reset, target, location.clone(), now);
        self.backend.send(Request::Reset {
            port: location,
            cancel,
        });
    }

    /// One frame of the window.
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        while let Some(event) = self.backend.try_recv() {
            self.handle(event, now);
        }
        self.bench.tick();
        if self.bench.take_failure() {
            self.close_pending = false;
        }
        self.poll_picker();

        if ctx.input(|i| i.viewport().close_requested()) && self.busy() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.close_pending = true;
            if self.bench.recording() {
                self.backend
                    .send(Request::Bench(crate::bench_worker::Command::Stop));
            }
            if let Operation::Batch(run) = &self.operation {
                run.stop.cancel();
            }
        }
        if self.close_pending && !self.busy() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        for dropped in ctx.input(|i| i.raw.dropped_files.clone()) {
            let path = dropped.path().to_path_buf();
            if !path.as_os_str().is_empty() {
                self.take_file(path);
            }
        }
        if self.busy() {
            // The elapsed time moves even when no event arrives.
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        let face = style::face(&ctx);
        // Escape closes About before it answers a question beneath it.
        if self.about_open
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.about_open = false;
        }
        egui::Panel::top("strip")
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(face.surface)
                    .inner_margin(Margin::symmetric(space::M as i8, space::S as i8)),
            )
            .show(ui, |ui| self.strip(ui));
        // The log's place along the bottom, at the height its drawer took last frame. The drawer
        // itself is drawn last, so the keyboard reaches it after the view's controls.
        let log = egui::Panel::bottom("log")
            .resizable(false)
            .exact_size(self.log_height)
            .frame(Frame::new().fill(face.surface))
            .show(ui, |_| {})
            .response
            .rect;
        if self.view == View::Bench {
            self.bench
                .show(ui, &self.units, &mut self.selected, self.backend.as_mut());
            if self.bench.take_units_request() {
                self.bench.disconnect(self.backend.as_mut());
                self.view = View::Units;
            }
            if let Some(unit) = self.bench.take_restart() {
                self.restarted = unit.device.location.clone();
                self.backend.send(Request::Restart(Box::new(unit)));
            }
        } else {
            self.views(ui, now);
        }
        // Laid from the bottom up, in as much height as it can take, so its height does not
        // depend on the panel's.
        let margin = vec2(space::M, space::XS);
        let tallest = egui::Rect::from_min_max(
            pos2(log.left(), log.bottom() - LOG_HEIGHT),
            log.right_bottom(),
        );
        let mut drawer = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("log-drawer")
                .max_rect(tallest.shrink2(margin))
                .layout(egui::Layout::bottom_up(egui::Align::Min)),
        );
        self.log_drawer(&mut drawer);
        let height = drawer.min_rect().height() + 2.0 * margin.y;
        if (height - self.log_height).abs() > 0.5 {
            self.log_height = height;
            ctx.request_repaint();
        }

        if self.about_open {
            let mut open = true;
            egui::Window::new("About")
                .open(&mut open)
                // Wide enough for the licence texts' 80 columns, which break mid-sentence narrower.
                .default_size(vec2(720.0, 420.0))
                .show(&ctx, |ui| {
                    ScrollArea::vertical().show(ui, |ui| {
                        ui.label(
                            text(Role::Title, format!("Verge {} (verge-gui)", env!("CARGO_PKG_VERSION")))
                                .color(face.ink),
                        );
                        ui.add_space(space::M);
                        ui.label(text(Role::Body, "The licence of every crate inside is in THIRD-PARTY-LICENSES.html, in the archive Verge came in."));
                        ui.add_space(space::M);
                        ui.label(text(Role::Body, "verge is licensed under Apache-2.0. Its fonts are under the SIL Open Font License 1.1, unmodified:"));
                        for (faces, licence) in style::FONT_LICENCES {
                            ui.add_space(space::S);
                            label(ui, faces);
                            ui.label(text(Role::Code, *licence));
                        }
                        ui.add_space(space::L);
                        ui.label(text(Role::Body, "Its icon is one of the Escapement Project's marks, under the Creative Commons Attribution 4.0 International licence (CC BY 4.0):"));
                        ui.add_space(space::S);
                        label(ui, "Verge's icon");
                        ui.label(text(Role::Code, style::ICON_LICENCE));
                    });
                });
            self.about_open = open;
        }
        style::focus_ring(&ctx);
    }

    /// The Units and Batch views, which scroll in the central panel.
    fn views(&mut self, ui: &mut Ui, now: f64) {
        let face = style::face(ui.ctx());
        if self.view == View::Units {
            egui::Panel::left("units")
                .resizable(false)
                .exact_size(300.0)
                .frame(
                    Frame::new()
                        .fill(face.surface)
                        .inner_margin(Margin::same(space::M as i8)),
                )
                .show(ui, |ui| self.unit_list(ui));
        }
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(face.page)
                    .inner_margin(Margin::same(space::L as i8)),
            )
            .show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| {
                    // Lines stay short enough to read on a wide window.
                    ui.set_max_width(ui.available_width().min(COLUMN));
                    match self.view {
                        View::Units => self.main(ui, now),
                        View::Batch => self.batch_view(ui, now),
                        // Drawn by `show`, in panels of its own.
                        View::Bench => {}
                    }
                });
            });
    }

    fn strip(&mut self, ui: &mut Ui) {
        let face = style::face(ui.ctx());
        ui.horizontal(|ui| {
            self.mark.show(ui);
            ui.label(text(Role::Title, "Verge").color(face.ink));
            ui.add_space(space::M);
            let idle = !self.busy();
            for (view, words) in [
                (View::Units, "Units"),
                (View::Batch, "Batch"),
                (View::Bench, "Bench"),
            ] {
                let selected = self.view == view;
                let response =
                    ui.add_enabled(idle || selected, choice_button(face, words, selected));
                if response.clicked() && !selected {
                    if self.view == View::Bench {
                        self.bench.disconnect(self.backend.as_mut());
                    }
                    self.view = view;
                    self.confirming = None;
                    self.entering = None;
                    self.batch_form.confirming = None;
                }
                response.on_disabled_hover_text("Wait for what is running to finish.");
            }
            ui.add_space(space::M);
            // The release line takes what the buttons after it leave, as they measured last frame,
            // and is cut short where that is too little. Its tag comes first, so the repository is
            // what is cut.
            let line = (ui.available_width() - self.strip_end).max(0.0);
            ui.allocate_ui_with_layout(
                vec2(line, ui.available_height()),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    let mut releases = egui::text::LayoutJob::default();
                    let mut add = |rich: egui::RichText| {
                        rich.append_to(
                            &mut releases,
                            ui.style(),
                            egui::FontSelection::Default,
                            egui::Align::Center,
                        );
                    };
                    let tag =
                        |words: &str| text(Role::Label, format!("{words} ")).color(face.ink_2);
                    match &self.release {
                        None => add(tag("looking up a release")),
                        Some(Ok(release)) => {
                            add(tag(if self.tag.is_some() {
                                "release"
                            } else {
                                "latest"
                            }));
                            add(text(Role::Code, &release.tag).color(face.ink));
                        }
                        Some(Err(_)) => add(tag("no release found")),
                    }
                    add(tag(" ·"));
                    add(text(Role::Code, self.config.repo.to_string()).color(face.ink));
                    ui.add(egui::Label::new(releases).truncate());
                },
            );
            let end = ui.cursor().left();
            if let Some(scenario) = &self.config.simulated {
                ui.add_space(space::M);
                Frame::new()
                    .stroke(Stroke::new(1.0, face.attention_ink))
                    .inner_margin(Margin::symmetric(space::S as i8, 2))
                    .show(ui, |ui| {
                        ui.label(
                            text(Role::Label, format!("Simulated bench: {scenario}"))
                                .color(face.attention_ink),
                        );
                    });
            }
            ui.add_space(space::M);
            let rescan = outline(ui, "Rescan", idle && self.worker_error.is_none());
            if !idle {
                rescan.on_disabled_hover_text("Wait for what is running to finish.");
            } else if rescan.clicked() {
                self.ended = None;
                self.rescan();
                self.backend.send(Request::Release(self.tag.clone()));
            }
            if outline(ui, "About", true).clicked() {
                self.about_open = true;
            }
            let taken = ui.cursor().left() - end;
            if (taken - self.strip_end).abs() > 0.5 {
                self.strip_end = taken;
                ui.ctx().request_repaint();
            }
        });
        if let Some(error) = &self.worker_error {
            edged(ui, Status::Fault, error);
        }
        if self.close_pending {
            edged(
                ui,
                Status::Attention,
                if matches!(self.operation, Operation::Batch(_)) {
                    "The window closes when the batch's unit under way finishes. The batch takes no new units."
                } else if self.bench.recording() {
                    "Closing after the recording stops."
                } else if self.bench.dialog_open() {
                    "Closing once the file dialog is closed. Close it to continue."
                } else if self.bench.compacting() {
                    "Closing once the recording is saved."
                } else if self.bench.busy() {
                    "Closing once the file is written."
                } else {
                    "The window closes when this update finishes. Cancel stops it at the next safe point; a unit already in its bootloader stays in ISP mode for the next flash."
                },
            );
        }
    }

    fn unit_list(&mut self, ui: &mut Ui) {
        let face = style::face(ui.ctx());
        ui.horizontal(|ui| {
            label(ui, "Attached");
            ui.label(text(Role::Figures, self.units.len().to_string()).color(face.ink));
            if let Scan::Scanning = self.scan {
                label(ui, "· scanning");
            }
        });
        if let Scan::Failed(error) = &self.scan {
            let words = if error.starts_with("could not list USB devices") {
                sentence(error)
            } else {
                format!("Could not list USB devices: {error}")
            };
            edged(ui, Status::Fault, &words);
        }
        ui.add_space(space::S);
        // What the last look at USB found, which may have changed since.
        let stale = matches!(self.scan, Scan::Failed(_)) || self.worker_error.is_some();
        if stale && !self.units.is_empty() {
            label(ui, "As of the last look at USB");
            ui.disable();
        }
        let idle = self.operation.job().is_none();
        let mut clicked = None;
        for unit in &self.units {
            let selected = self.selected.as_deref() == Some(unit.device.instance.as_str());
            let (word, glyph, fill) = identity(unit, face);
            // Selected, the row is an ink fill with page text; under the pointer, the wash.
            let (ink, ink_2) = if selected {
                (face.page, face.page)
            } else {
                (face.ink, face.ink_2)
            };
            let ground = ui.painter().add(egui::Shape::Noop);
            let response = Frame::new()
                .inner_margin(Margin::same(space::S as i8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        match fill {
                            Some(fill) => glyph.chip(ui, 20.0, fill.ground, fill.on),
                            None => glyph.chip(ui, 20.0, Color32::TRANSPARENT, ink_2),
                        };
                        ui.vertical(|ui| {
                            let name = match &unit.state {
                                State::Bootloader => "LPC55S69 bootloader",
                                State::MiniBootloader => "MCU-Link mini bootloader",
                                _ => unit.serial().unwrap_or("no serial"),
                            };
                            ui.label(text(Role::Code, name).color(ink));
                            let mut facts = vec![firmware_label(unit)];
                            facts.extend(board_label(unit));
                            facts.push(unit.device.location.clone().unwrap_or_else(|| "?".into()));
                            ui.label(text(Role::Label, facts.join(" · ")).color(ink_2));
                        });
                    });
                })
                .response;
            let response = ui
                .interact(
                    response.rect,
                    response.id.with("pick"),
                    egui::Sense::click(),
                )
                .on_hover_text(self.path_line(unit));
            let under = if selected {
                Some(face.ink)
            } else if response.hovered() && idle {
                Some(face.wash())
            } else {
                None
            };
            if let Some(under) = under {
                ui.painter().set(
                    ground,
                    egui::Shape::rect_filled(
                        response.rect,
                        CornerRadius::same(style::RADIUS),
                        under,
                    ),
                );
            }
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::Role::ListBoxOption,
                    idle,
                    selected,
                    unit.serial().unwrap_or(word),
                )
            });
            if idle && response.clicked() {
                clicked = Some(unit.device.instance.clone());
            }
        }
        if let Some(instance) = clicked {
            self.selected = Some(instance);
            self.confirming = None;
            self.entering = None;
            self.ended = None;
        }
    }

    fn main(&mut self, ui: &mut Ui, now: f64) {
        let face = style::face(ui.ctx());
        if let Some(job) = self.operation.job() {
            Self::working(ui, job, now);
            if let Some(dropped) = &job.dropped {
                edged(ui, Status::Attention, dropped);
            }
            ui.add_space(space::L);
            if job.cancel.is_cancelled() {
                ui.label(text(Role::Action, "Cancelling at the next safe point").color(face.ink_2));
            } else if slot(ui, "Cancel", None).clicked() {
                job.cancel.cancel();
            }
            return;
        }
        if let Some(ended) = &self.ended {
            Self::result(ui, ended);
            ui.add_space(space::L);
            // Flashed, and back in ISP mode because its jumper is still fitted: once it is off, a
            // reset starts the new image, as `verge isp reset` does.
            let reset = ended.kind == Kind::Update
                && matches!(ended.result, Ok(Done::Back(Outcome::JumperStillFitted)))
                && self.worker_error.is_none();
            let (target, location) = (ended.target.clone(), ended.location.clone());
            ui.horizontal(|ui| {
                if slot(ui, "Back", None).clicked() {
                    self.ended = None;
                }
                if reset && slot(ui, RESET, None).clicked() {
                    self.reset(target, location, now);
                }
            });
            return;
        }
        // While nothing is attached the mark loops above the instruction, from the V each time the
        // list empties.
        let waiting = if self.units.is_empty() {
            let since = *self.waiting_since.get_or_insert(now);
            Some(((now - since) as f32) % crate::mark::LOOP)
        } else {
            self.waiting_since = None;
            None
        };
        let Some(unit) = self.selected_unit().cloned() else {
            frame(ui, None, Stroke::new(1.0, face.rule), |ui| {
                if let Some(t) = waiting {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(64.0, 64.0), egui::Sense::hover());
                    let moving = crate::mark::motion(ui.ctx());
                    let at = if moving {
                        crate::mark::at(t)
                    } else {
                        crate::mark::Frame::REST
                    };
                    let colours = crate::mark::Colours::of(face, true);
                    crate::mark::paint(ui.painter(), rect, &crate::mark::MASTER, &at, &colours);
                    if moving && t < crate::mark::END {
                        ui.ctx().request_repaint();
                    } else if moving {
                        ui.ctx()
                            .request_repaint_after(Duration::from_secs_f32(crate::mark::LOOP - t));
                    }
                    ui.add_space(space::M);
                }
                ui.label(text(Role::Display, "Nothing").color(face.ink_2));
                ui.label(
                    text(
                        Role::Instruction,
                        if self.units.is_empty() {
                            "Plug in an MCU-Link"
                        } else {
                            "Pick a unit"
                        },
                    )
                    .color(face.ink),
                );
                if self.units.is_empty() && cfg!(target_os = "linux") {
                    ui.add_space(space::M);
                    udev_rules(
                        ui,
                        "Plugged in already? On Linux your user needs the udev rules on Verge's Getting started page.",
                    );
                }
            });
            return;
        };
        self.unit_slab(ui, &unit);
        ui.add_space(space::M);
        if denied(&unit) {
            let refused = "The system does not let your user open this unit. The tool cannot ask it what it runs or update it, with or without the jumper.";
            if cfg!(target_os = "linux") {
                udev_rules(
                    ui,
                    &format!(
                        "{refused} Your user needs the udev rules on Verge's Getting started page. Then unplug the unit, plug it back in and press Rescan."
                    ),
                );
            } else {
                wrapping(ui, text(Role::Body, refused).color(face.ink));
            }
            ui.add_space(space::L);
            ui.add_enabled(
                false,
                egui::Button::new(text(Role::Action, "Cannot update").color(face.ink_2))
                    .min_size(vec2(320.0, style::SLOT_HEIGHT)),
            );
            return;
        }
        let offer = offer(&unit, &self.units);
        field(ui, "Path", |ui| match offer.best() {
            Some(path) => wrapping(
                ui,
                text(Role::Body, format!("{path}. {}", path.describe())).color(face.ink),
            ),
            None => wrapping(ui, text(Role::Body, "None.").color(face.ink_2)),
        });
        for note in window_notes(&unit, &offer) {
            edged(ui, Status::Attention, &note);
        }
        ui.add_space(space::M);
        self.image_choice(ui, Some(&unit));
        ui.add_space(space::L);

        let action = self.action(face);
        // A question asked before the worker stopped, or about a unit no longer selected, no longer
        // stands: confirming it would start an update nothing can run, or one on a unit that left.
        let asked_about_another = self.confirming.as_ref().is_some_and(|question| {
            self.selected.as_deref() != Some(question.unit.device.instance.as_str())
        });
        if asked_about_another || matches!(action, Action::Blocked { .. }) {
            self.confirming = None;
        }
        // The question before entering ISP mode stands for the unit it was asked about, while it
        // can still enter.
        if self.entering.as_ref().is_some_and(|asked| {
            asked.device.instance != unit.device.instance
                || self.worker_error.is_some()
                || matches!(self.scan, Scan::Failed(_))
                || self.enter_refusal(&unit).is_some()
        }) {
            self.entering = None;
        }
        if let Some(asked) = self.entering.clone() {
            edged(
                ui,
                Status::Attention,
                &format!(
                    "Restart {} into ISP mode? It stays there until it is flashed, or reset out of ISP mode. In ISP mode the tool cannot tell its board.",
                    unit_name(&asked)
                ),
            );
            ui.add_space(space::S);
            match answers(ui, ENTER, Fill::ink(face), false) {
                Some(true) => self.enter(asked, now),
                Some(false) => self.entering = None,
                None => {}
            }
            return;
        }
        if let Some(Confirming {
            unit,
            source,
            what,
            download,
        }) = self.confirming.clone()
        {
            let lead = match &download {
                Some(download) => format!("Download {download}, and flash it"),
                None => format!("Flash {what}"),
            };
            let mut question = format!(
                "{lead} onto {}? Keep it plugged in until the tool says it is done.",
                unit_name(&unit)
            );
            let unknown =
                source::board_unknown(&unit).filter(|_| !matches!(source, Source::Stock(_)));
            if (unit.is_bootloader() || unknown.is_some())
                && let Some(built_for) = self.built_for(&source)
            {
                question = format!("{built_for} {question}");
            }
            if unit.is_bootloader() {
                question = format!("{ISP_IDENTITY_WARNING} {question}");
            }
            if let Some(unknown) = unknown {
                question = format!("{unknown} {question}");
            }
            edged(ui, Status::Attention, &question);
            ui.add_space(space::S);
            let fill = match &action {
                Action::Ready { fill, .. } => *fill,
                Action::Blocked { .. } => Fill::ink(face),
            };
            match answers(ui, "Confirm", fill, false) {
                Some(true) => self.start(unit, source, agreed(download.as_ref()), now),
                Some(false) => self.confirming = None,
                None => {}
            }
            return;
        }
        match action {
            Action::Ready {
                label: words,
                fill,
                source,
                what,
            } => {
                if slot(ui, &words, Some(fill)).clicked() {
                    self.confirming = Some(Confirming {
                        unit: Box::new(unit.clone()),
                        download: self.download(&source),
                        source,
                        what,
                    });
                }
            }
            Action::Blocked {
                label: words,
                reason,
            } => {
                ui.add_enabled(
                    false,
                    egui::Button::new(text(Role::Action, words).color(face.ink_2))
                        .min_size(vec2(320.0, style::SLOT_HEIGHT)),
                );
                if !reason.is_empty() {
                    wrapping(ui, text(Role::Body, reason).color(face.ink_2));
                }
            }
        }
        // After a failed look at USB the unit may have gone: nothing is offered for it.
        let current = self.worker_error.is_none() && !matches!(self.scan, Scan::Failed(_));
        if unit.is_bootloader() && current {
            ui.add_space(space::M);
            wrapping(
                ui,
                text(
                    Role::Body,
                    "Or leave ISP mode without flashing: the unit starts the image it holds. Take its ISP jumper off first.",
                )
                .color(face.ink_2),
            );
            if outline(ui, RESET, true).clicked() {
                self.reset(unit_name(&unit), unit.device.location.clone(), now);
            }
        }
        // A unit running a firmware the tool asked can be put in ISP mode for a flash later, as
        // `verge isp enter` does.
        if matches!(unit.state, State::Probe { .. }) && current && self.confirming.is_none() {
            ui.add_space(space::M);
            wrapping(
                ui,
                text(
                    Role::Body,
                    "Or put it in ISP mode without the jumper, and leave it there for a flash.",
                )
                .color(face.ink_2),
            );
            let refusal = self.enter_refusal(&unit);
            let response = outline(ui, ENTER, refusal.is_none());
            if response.clicked() {
                self.entering = Some(Box::new(unit.clone()));
            }
            if let Some(refusal) = refusal {
                response.on_disabled_hover_text(refusal);
            }
        }
    }

    /// What the unit's state means for an update, in a few words: the slab's subline.
    fn path_line(&self, unit: &Unit) -> String {
        match &unit.state {
            State::Bootloader => "Waiting for an image".to_owned(),
            State::MiniBootloader => "A different board: not supported".to_owned(),
            State::Unreadable { .. } if denied(unit) => "Your user may not open it".to_owned(),
            State::Unreadable { .. } if too_old_to_ask(unit) => {
                format!("Too old to ask: {STOCK} V2.x speaks HID only")
            }
            State::Unreadable { reason, .. } => format!("Could not ask: {reason}"),
            State::Held { .. } => "Another program holds its CMSIS-DAP".to_owned(),
            State::Probe { .. } => match offer(unit, &self.units).best() {
                Some(Path::OursJumperFree | Path::StockJumperFree | Path::DaplinkRsJumperFree) => {
                    "Updates without the jumper"
                }
                Some(Path::Jumper) => "Needs the ISP jumper to update",
                _ => "Cannot be updated as it is",
            }
            .to_owned(),
            State::SwdJumper { .. } => match offer(unit, &self.units).best() {
                Some(Path::OursJumperFree) => "SWD jumper fitted. Updates without the ISP jumper",
                Some(Path::Jumper) => "SWD jumper fitted. Needs the ISP jumper to update",
                _ => "SWD jumper fitted. Cannot be updated as it is",
            }
            .to_owned(),
        }
    }

    fn unit_slab(&self, ui: &mut Ui, unit: &Unit) {
        let face = style::face(ui.ctx());
        let (word, glyph, fill) = identity(unit, face);
        let ink = fill.map_or(face.ink, |fill| fill.on);
        let stroke = if fill.is_some() {
            Stroke::NONE
        } else {
            Stroke::new(1.0, face.rule)
        };
        // A short window keeps the slab to its words, so the action stays in sight.
        let compact = ui.ctx().content_rect().height() < COMPACT_BELOW;
        frame(ui, fill.map(|fill| fill.ground), stroke, |ui| {
            let subline = match board_label(unit) {
                Some(board) => format!("{board} · {}", self.path_line(unit)),
                None => self.path_line(unit),
            };
            if compact {
                let line = format!("{} · {subline}", firmware_label(unit));
                wrapping(ui, text(Role::Instruction, line).color(ink));
            } else {
                ui.horizontal(|ui| {
                    glyph.show(ui, 48.0, ink);
                    ui.add_space(space::M);
                    ui.label(text(Role::Display, word).color(ink));
                    if let Some(version) = version_of(unit) {
                        ui.add_space(space::L);
                        ui.label(text(Role::Tally, version).color(ink));
                    }
                });
                wrapping(ui, text(Role::Instruction, subline).color(ink));
            }
            let mut facts: Vec<String> = unit.serial().map(str::to_owned).into_iter().collect();
            facts.extend([
                unit.device.location.clone().unwrap_or_else(|| "?".into()),
                format!("{:04x}:{:04x}", unit.device.vid, unit.device.pid),
            ]);
            if let State::Probe { identity, .. } = &unit.state {
                facts.extend(identity.product.clone());
                facts.extend(identity.protocol.as_ref().map(|p| format!("CMSIS-DAP {p}")));
            }
            wrapping(ui, text(Role::Code, facts.join(" · ")).color(ink));
        });
    }

    fn image_choice(&mut self, ui: &mut Ui, unit: Option<&Unit>) {
        let ctx = ui.ctx().clone();
        let face = style::face(&ctx);
        field(ui, "Image", |ui| {
            ui.horizontal(|ui| {
                for (choice, words) in [
                    (Choice::Release, "Release"),
                    (Choice::Stock, STOCK),
                    (Choice::File, "File"),
                ] {
                    let selected = self.choice == choice;
                    if ui.add(choice_button(face, words, selected)).clicked() {
                        self.choice = choice;
                        self.confirming = None;
                        self.entering = None;
                        if choice == Choice::File && self.picked.is_none() {
                            self.open_picker(&ctx);
                        }
                    }
                }
                if self.choice == Choice::File
                    && outline(ui, "Browse", self.picker.is_none()).clicked()
                {
                    self.open_picker(&ctx);
                }
            });
            if self.choice == Choice::Release {
                self.release_tag(ui);
            }
            let detail = match self.choice {
                Choice::Release => match &self.release {
                    None => match &self.tag {
                        Some(tag) => format!("Looking up release {tag}."),
                        None => "Looking up the latest release.".to_owned(),
                    },
                    Some(Err(_)) => "No release to install.".to_owned(),
                    Some(Ok(release)) => {
                        let standing = unit.and_then(Unit::build).map(|build| {
                            match build.compare(&release.tag) {
                                escapement_verge_probe::version::Relation::UpToDate => {
                                    "runs this release".to_owned()
                                }
                                escapement_verge_probe::version::Relation::OtherVariant => {
                                    format!("runs this release built as {build}")
                                }
                                escapement_verge_probe::version::Relation::Behind => {
                                    format!("runs {build}, older")
                                }
                                escapement_verge_probe::version::Relation::Ahead => {
                                    format!("runs {build}, newer than this release")
                                }
                                escapement_verge_probe::version::Relation::Unknown => {
                                    format!("runs {build}, which carries no version to compare")
                                }
                            }
                        });
                        let mut words = format!(
                            "{} · {} · {} bytes · signed in {}, checksum in {}",
                            release.tag,
                            release.image.name,
                            release.image.size,
                            release.signature.name,
                            release.checksum.name
                        );
                        if let Some(standing) = standing {
                            words.push_str(&format!(" · the unit {standing}"));
                        }
                        words
                    }
                },
                Choice::Stock => match &self.stock {
                    Some(found) => {
                        format!("{STOCK} V{} from {}", found.version, found.path.display())
                    }
                    None => format!("No image of {STOCK} in LinkServer's usual places."),
                },
                Choice::File => match &self.picked {
                    None if self.picker.is_some() => "The file dialog is open.".to_owned(),
                    None => "Pick an ELF or S-record, or drop one on the window.".to_owned(),
                    Some(Picked {
                        path,
                        image: Ok(image),
                        ..
                    }) => {
                        let pages: usize = image
                            .page_runs()
                            .iter()
                            .map(|r| r.data.len() / PAGE as usize)
                            .sum();
                        format!(
                            "{} · {} · {} bytes in {pages} pages · sha256 {}",
                            path.display(),
                            image.format,
                            image.data_len(),
                            &image.sha256_hex()[..16]
                        )
                    }
                    // The error names the file.
                    Some(Picked {
                        image: Err(error), ..
                    }) => error.clone(),
                },
            };
            wrapping(ui, text(Role::Code, detail).color(face.ink_2));
        });
        if let Some(notice) = &self.notice {
            edged(ui, Status::Attention, notice);
        }
        let release_refused = self.choice == Choice::Release
            && matches!((&self.release, unit.and_then(Unit::build)), (Some(Ok(release)), Some(build))
                if matches!(build.compare(&release.tag), escapement_verge_probe::version::Relation::UpToDate | escapement_verge_probe::version::Relation::Ahead));
        if unit.is_some() && (release_refused || (self.force && self.choice == Choice::Release)) {
            Frame::new()
                .stroke(Stroke::new(1.0, face.attention_ink))
                .inner_margin(Margin::same(space::S as i8))
                .show(ui, |ui| {
                    ui.checkbox(
                        &mut self.force,
                        text(Role::Body, "Install anyway: flash this release even though the unit runs the same or a newer build").color(face.ink),
                    );
                });
        }
    }

    /// Which release to install: the latest, or one the user names by its tag, as `verge update
    /// --release` takes it.
    fn release_tag(&mut self, ui: &mut Ui) {
        let face = style::face(ui.ctx());
        ui.horizontal(|ui| {
            let name = label(ui, "Tag");
            let field = ui
                .add(
                    egui::TextEdit::singleline(&mut self.tag_field)
                        .hint_text("latest")
                        .font(Role::Code.style())
                        .desired_width(128.0),
                )
                .labelled_by(name.id);
            let typed = self.tag_field.trim();
            let wanted = (!typed.is_empty() && typed != "latest").then(|| typed.to_owned());
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (outline(ui, "Look up", wanted != self.tag).clicked() || entered)
                && wanted != self.tag
            {
                self.tag = wanted;
                self.release = None;
                self.confirming = None;
                self.batch_form.confirming = None;
                self.backend.send(Request::Release(self.tag.clone()));
            }
            if self.tag.is_some() {
                ui.label(text(Role::Label, "Empty for the latest").color(face.ink_2));
            }
        });
    }

    fn working(ui: &mut Ui, job: &Job, now: f64) {
        let face = style::face(ui.ctx());
        frame(ui, None, Stroke::new(2.0, face.ink), |ui| {
            ui.horizontal(|ui| {
                glyph::WORKING.show(ui, 48.0, face.ink);
                ui.add_space(space::M);
                ui.label(text(Role::Display, "Working").color(face.ink));
            });
            ui.label(text(Role::Instruction, "Do not unplug the unit").color(face.ink));
            let mut facts = vec![job.target.clone()];
            facts.extend(job.image.clone());
            facts.push(format!("{:.1} s", now - job.started));
            ui.label(text(Role::Code, facts.join(" · ")).color(face.ink_2));
            ui.add_space(space::M);
            if let Some(current) = &job.current {
                ui.horizontal(|ui| {
                    label(ui, &sentence_case(&current.step.to_string()));
                    if current.step.unit() == StepUnit::Bytes && current.total > 0 {
                        ui.label(
                            text(
                                Role::Figures,
                                format!("{} / {}", kib(current.done), kib(current.total)),
                            )
                            .color(face.ink),
                        );
                    }
                    ui.label(
                        text(Role::Figures, format!("{:.1} s", now - current.since))
                            .color(face.ink_2),
                    );
                });
                if current.step.unit() == StepUnit::Bytes && current.total > 0 {
                    progress(
                        ui,
                        current.done as f32 / current.total as f32,
                        f32::INFINITY,
                    );
                }
            }
        });
        ui.add_space(space::M);
        Self::rail(
            ui,
            &job.rail,
            &job.finished,
            job.current.as_ref().map(|c| c.step),
            None,
        );
        for note in &job.notes {
            edged(ui, Status::Attention, note);
        }
    }

    /// One cell per step: filled when done, with its seconds; a heavy outline for the current one;
    /// a cross in the fault ink for the one that failed.
    fn rail(
        ui: &mut Ui,
        rail: &[Step],
        finished: &[(Step, f64)],
        current: Option<Step>,
        failed: Option<Step>,
    ) {
        if rail.is_empty() {
            return;
        }
        let face = style::face(ui.ctx());
        let width = (ui.available_width() / rail.len() as f32).min(112.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for step in rail {
                let seconds = finished.iter().find(|(s, _)| s == step).map(|(_, t)| *t);
                ui.allocate_ui_with_layout(
                    vec2(width, 56.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        let (rect, response) =
                            ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
                        let failed = failed == Some(*step);
                        if failed {
                            let stroke = Stroke::new(2.0, face.fault_ink);
                            ui.painter()
                                .rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
                            let inner = rect.shrink(3.5);
                            ui.painter()
                                .line_segment([inner.left_top(), inner.right_bottom()], stroke);
                            ui.painter()
                                .line_segment([inner.right_top(), inner.left_bottom()], stroke);
                        } else if seconds.is_some() {
                            ui.painter().rect_filled(rect, 0.0, face.ink);
                        } else if current == Some(*step) {
                            ui.painter().rect_stroke(
                                rect,
                                0.0,
                                Stroke::new(2.0, face.ink),
                                egui::StrokeKind::Inside,
                            );
                        } else {
                            ui.painter().rect_stroke(
                                rect,
                                0.0,
                                Stroke::new(1.0, face.rule),
                                egui::StrokeKind::Inside,
                            );
                        }
                        let name = if failed {
                            format!("{}: failed", sentence_case(&step.to_string()))
                        } else {
                            step.to_string()
                        };
                        response.on_hover_text(&name).widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Label, true, &name)
                        });
                        label(ui, short(*step));
                        if failed {
                            ui.label(text(Role::Label, "failed").color(face.fault_ink));
                        } else if let Some(t) = seconds {
                            ui.label(text(Role::Figures, format!("{t:.1} s")).color(face.ink_2));
                        }
                    },
                );
            }
        });
    }

    fn result(ui: &mut Ui, ended: &Ended) {
        let face = style::face(ui.ctx());
        let reset = ended.kind == Kind::Reset;
        let (word, instruction, glyph, fill) = match &ended.result {
            Ok(Done::Back(Outcome::Running(_))) => {
                ("Done", "The unit is back", glyph::DONE, Fill::lume(face))
            }
            Ok(Done::Back(Outcome::JumperStillFitted)) if reset => (
                "Still in ISP mode",
                "Its ISP jumper is still fitted, or its flash holds no image it starts: take the jumper off and reset it again, or flash it",
                glyph::ATTENTION,
                Fill::status(Status::Attention, face),
            ),
            Ok(Done::Back(Outcome::NotRestarted { .. })) if reset => (
                "Still in ISP mode",
                "It did not restart: unplug it and plug it back in",
                glyph::ATTENTION,
                Fill::status(Status::Attention, face),
            ),
            Ok(Done::Back(Outcome::JumperStillFitted)) => (
                "Flashed",
                "Its ISP jumper is still fitted: take it off, then reset it out of ISP mode",
                glyph::ATTENTION,
                Fill::status(Status::Attention, face),
            ),
            Ok(Done::Back(Outcome::NotRestarted {
                maybe_jumper: false,
            })) => (
                "Flashed",
                "It did not restart: unplug it and plug it back in",
                glyph::ATTENTION,
                Fill::status(Status::Attention, face),
            ),
            Ok(Done::Back(Outcome::NotRestarted { maybe_jumper: true })) => (
                "Flashed",
                "It did not restart, or its jumper is fitted: unplug it, take any jumper off, plug it back in",
                glyph::ATTENTION,
                Fill::status(Status::Attention, face),
            ),
            Ok(Done::InIsp) => (
                "In ISP mode",
                "Flash it, or reset it out of ISP mode",
                glyph::WAITING,
                Fill::status(Status::Attention, face),
            ),
            // Cancelled: the headline says what that left, as the message under it does.
            Err(JobError {
                stopped: Some(stopped),
                ..
            }) => (
                "Stopped",
                match stopped {
                    Stopped::Unchanged => "Nothing was changed",
                    Stopped::Left(Left::InIsp) => "In ISP mode, with the image it held",
                    Stopped::Left(Left::Partial) => "In ISP mode, with a partial image",
                    Stopped::Left(Left::Unverified) => "In ISP mode, with an image not checked yet",
                    Stopped::Left(Left::Flashed) => "Restarting into the new image",
                    Stopped::Left(Left::Reset) => "Restarting into the image it holds",
                },
                glyph::ATTENTION,
                Fill::status(Status::Attention, face),
            ),
            Err(error) if ended.kind == Kind::Enter && !error.left_in_isp => (
                "Failed",
                "The unit did not reach ISP mode",
                glyph::FAILED,
                Fill::status(Status::Fault, face),
            ),
            Err(error) if reset && error.left_in_isp => (
                "Failed",
                "The unit is still in ISP mode",
                glyph::FAILED,
                Fill::status(Status::Fault, face),
            ),
            Err(_) if reset => (
                "Failed",
                "The reset did not finish",
                glyph::FAILED,
                Fill::status(Status::Fault, face),
            ),
            Err(error) if error.left_in_isp => (
                "Failed",
                "The unit may be in ISP mode",
                glyph::FAILED,
                Fill::status(Status::Fault, face),
            ),
            // Nothing is erased before the bootloader is connected.
            Err(_)
                if matches!(
                    ended.failed,
                    None | Some(
                        Step::Download
                            | Step::EnterBootloader
                            | Step::WaitForBootloader
                            | Step::Connect
                    )
                ) =>
            {
                (
                    "Failed",
                    "Nothing was flashed",
                    glyph::FAILED,
                    Fill::status(Status::Fault, face),
                )
            }
            Err(_) if ended.failed == Some(Step::WaitForFirmware) => (
                "Failed",
                "The unit did not come back",
                glyph::FAILED,
                Fill::status(Status::Fault, face),
            ),
            Err(_) => (
                "Failed",
                "The update did not finish",
                glyph::FAILED,
                Fill::status(Status::Fault, face),
            ),
        };
        frame(ui, Some(fill.ground), Stroke::NONE, |ui| {
            ui.horizontal(|ui| {
                glyph.show(ui, 48.0, fill.on);
                ui.add_space(space::M);
                ui.label(text(Role::Display, word).color(fill.on));
            });
            wrapping(ui, text(Role::Instruction, instruction).color(fill.on));
            let mut facts = vec![ended.target.clone()];
            facts.extend(ended.image.clone());
            facts.push(format!("{:.1} s", ended.seconds));
            ui.label(text(Role::Code, facts.join(" · ")).color(fill.on));
            if let Ok(Done::Back(Outcome::Running(unit))) = &ended.result {
                wrapping(
                    ui,
                    text(
                        Role::Code,
                        format!("now {}, which runs {}", unit_name(unit), unit.state),
                    )
                    .color(fill.on),
                );
            }
        });
        ui.add_space(space::M);
        if let Err(error) = &ended.result {
            ui.add(
                egui::Label::new(text(Role::Code, &error.message).color(face.ink)).selectable(true),
            );
            if let Some(advice) = &error.advice {
                edged(ui, Status::Attention, advice);
            }
        }
        if !ended.steps.is_empty() || ended.failed.is_some() {
            let rail: Vec<Step> = ended
                .steps
                .iter()
                .map(|(s, _)| *s)
                .chain(ended.failed)
                .collect();
            Self::rail(ui, &rail, &ended.steps, None, ended.failed);
        }
        for note in &ended.notes {
            edged(ui, Status::Attention, note);
        }
    }

    fn log_drawer(&mut self, ui: &mut Ui) {
        let (lines, warnings, errors) = self.log.tail(if self.log_open { 400 } else { 1 });
        let face = style::face(ui.ctx());
        let level_color = |level: tracing::Level| match level {
            tracing::Level::ERROR => face.fault_ink,
            tracing::Level::WARN => face.attention_ink,
            tracing::Level::INFO => face.ink,
            _ => face.ink_2,
        };
        // The bar first: the drawer is laid from the bottom up.
        let row = ui.spacing().interact_size.y + space::S;
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), row),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                label(ui, "Log");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if outline(ui, if self.log_open { "Hide" } else { "Show" }, true).clicked() {
                        self.log_open = !self.log_open;
                    }
                    if self.log_open
                        && outline(ui, "Copy", true)
                            .on_hover_text("Copies every line the log keeps, with its time.")
                            .clicked()
                    {
                        ui.ctx().copy_text(self.log.text());
                    }
                    ui.label(
                        text(Role::Label, format!("{warnings} warn · {errors} error"))
                            .color(face.ink_2),
                    );
                    if !self.log_open
                        && let Some(line) = lines.last()
                    {
                        ui.add(
                            egui::Label::new(
                                text(Role::Code, line.to_string()).color(level_color(line.level)),
                            )
                            .truncate(),
                        );
                    }
                });
            },
        );
        if self.log_open {
            // As tall as its lines, up to `LOG_LINES`, in which it scrolls.
            let row = ui.text_style_height(&Role::Code.style()) + ui.spacing().item_spacing.y;
            let height = (lines.len().max(1) as f32 * row).min(LOG_LINES);
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ScrollArea::vertical()
                        .id_salt("log-lines")
                        .max_height(height)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            for line in &lines {
                                ui.add(
                                    egui::Label::new(
                                        text(Role::Code, line.to_string())
                                            .color(level_color(line.level)),
                                    )
                                    .selectable(true),
                                );
                            }
                        });
                },
            );
        }
    }
}

/// The open log's lines, at most this tall, and the whole drawer.
const LOG_LINES: f32 = 220.0;
const LOG_HEIGHT: f32 = LOG_LINES + 80.0;

/// A bar of `fraction` done, as wide as the space or `max_width`.
pub(crate) fn progress(ui: &mut Ui, fraction: f32, max_width: f32) {
    let face = style::face(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width().min(max_width), 8.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, face.hair);
    let mut done = rect;
    done.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    ui.painter().rect_filled(done, 0.0, face.ink);
}

/// What to do for a unit that needs the jumper, naming its board and the image to flash once it
/// is in ISP mode, where nothing tells its board.
fn jumper_advice(unit: &Unit, action: &Action) -> String {
    let fit = Path::Jumper.describe();
    let Hardware::Board(board) = unit.hardware() else {
        return format!(
            "{fit} This unit's firmware does not say which board it is, and in ISP mode nothing does: flash it with an image built for it."
        );
    };
    match action {
        Action::Ready {
            source: Source::Release(_),
            ..
        } if board != Board::McuLink => format!(
            "{fit} This unit is {}. In ISP mode the tool cannot tell its board, and a release says which boards it is for only once it is downloaded: flash it with {STOCK}, or with a file built for {}.",
            board.a(),
            board.a()
        ),
        Action::Ready {
            source: Source::Release(_),
            what,
            ..
        } => format!(
            "{fit} This unit is {}. In ISP mode the tool cannot tell its board, and a release says which boards it is for only once it is downloaded: flash it then with {what} only if it is built for {}.",
            board.a(),
            board.a()
        ),
        Action::Ready { what, .. } => format!(
            "{fit} This unit is {}. In ISP mode the tool cannot tell its board, so flash it then with {what}, which is built for it.",
            board.a()
        ),
        Action::Blocked { .. } => format!(
            "{fit} This unit is {}. In ISP mode the tool cannot tell its board, so flash it then with an image built for {}.",
            board.a(),
            board.a()
        ),
    }
}

/// The notes on `unit`'s update paths, as the window gives them. For a unit the tool could not
/// ask, the window builds its own from the unit's state, since it looks again when Rescan is
/// pressed: what the unit's USB strings suggest decides, as in `offer`, whether asking again can
/// find a jumper-free path.
fn window_notes(unit: &Unit, offer: &escapement_verge_probe::paths::Offer) -> Vec<String> {
    let State::Unreadable { reason, guess } = &unit.state else {
        return offer.notes.clone();
    };
    if reason == DENIED {
        return offer.notes.clone();
    }
    let mut notes = vec![format!(
        "The tool could not ask this unit what it runs: {reason}."
    )];
    if escapement_verge_probe::paths::asking_again_may_free(guess) {
        notes.push(
            "Close whatever has it open, then press Rescan to look for the jumper-free path again."
                .into(),
        );
    }
    notes
}

/// An error message as a sentence: capitalised, with a full stop.
pub fn sentence(message: &str) -> String {
    let mut out = sentence_case(message);
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

/// The worker's thread is joined when the window's state goes. A batch still running is told to
/// stop first, so that the join ends once the unit under way is done.
impl Drop for VergeApp {
    fn drop(&mut self) {
        if let Operation::Batch(run) = &self.operation {
            run.stop.cancel();
        }
    }
}

impl eframe::App for VergeApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}
