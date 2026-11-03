//! The window: the attached units on the left, the selected one's state and action on the right,
//! the log along the bottom.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use egui::{
    Color32, CornerRadius, Frame, Margin, ScrollArea, Stroke, Ui, ViewportCommand, pos2, vec2,
};
use verge_probe::board::{self, Board};
use verge_probe::cache::Cache;
use verge_probe::detect::{Firmware, Guess, Hardware, Jumpered, State};
use verge_probe::image::{Image, PAGE};
use verge_probe::paths::ISP_IDENTITY_WARNING;
use verge_probe::progress::{Cancel, Step, Unit as StepUnit};
use verge_probe::release::{Download, Release, Repo};
use verge_probe::source::{self, ImageVersion, LoadError, Source};
use verge_probe::stock::{self, StockImage};
use verge_probe::update::Outcome;
use verge_probe::{Path, Unit, offer};

use crate::glyph::{self, Glyph};
use crate::log::Log;
use crate::style::{self, Role, color, space, text};
use crate::worker::{Backend, BatchEvent, Event, InstallEvent, JobError, ProgressEvent, Request};

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
}

struct Current {
    step: Step,
    total: u64,
    done: u64,
    since: f64,
}

struct Job {
    target: String,
    location: Option<String>,
    image: Option<String>,
    rail: Vec<Step>,
    finished: Vec<(Step, f64)>,
    current: Option<Current>,
    notes: Vec<String>,
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
    result: Result<Outcome, JobError>,
    target: String,
    location: Option<String>,
    image: Option<String>,
    seconds: f64,
    steps: Vec<(Step, f64)>,
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
        fill: Color32,
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
    stock: Option<StockImage>,
    choice: Choice,
    picked: Option<Picked>,
    force: bool,
    confirming: Option<Confirming>,
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
}

fn kib(bytes: u64) -> String {
    format!("{:.1} KiB", bytes as f64 / 1024.0)
}

/// The unit's state as the slab's word, its glyph and its fill. `None` is a frame, not a fill.
fn identity(unit: &Unit) -> (&'static str, Glyph, Option<Color32>) {
    match &unit.state {
        State::Probe {
            firmware: Firmware::Ours(_),
            ..
        } => ("VERGE", glyph::OURS, Some(color::OURS)),
        State::Probe {
            firmware: Firmware::Stock { .. },
            ..
        } => ("STOCK", glyph::STOCK, Some(color::TEXT_2)),
        State::Probe {
            firmware: Firmware::DaplinkRs { .. },
            ..
        } => ("DAPLINK-RS", glyph::STOCK, Some(color::TEXT_2)),
        State::SwdJumper {
            firmware: Jumpered::Ours,
        }
        | State::Held { .. } => ("VERGE", glyph::OURS, Some(color::OURS)),
        State::SwdJumper {
            firmware: Jumpered::Stock(_),
        } => ("STOCK", glyph::STOCK, Some(color::TEXT_2)),
        State::Unreadable { .. } => ("UNKNOWN", glyph::ATTENTION, None),
        State::Bootloader => ("ISP MODE", glyph::WAITING, None),
        State::MiniBootloader => ("MINI", glyph::WAITING, None),
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

/// A held notice or a refusal: a line with a coloured left edge.
pub(crate) fn edged(ui: &mut Ui, edge: Color32, words: &str) {
    let response = Frame::new()
        .fill(color::SURFACE)
        .inner_margin(Margin {
            left: space::M as i8,
            right: space::S as i8,
            top: space::S as i8,
            bottom: space::S as i8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(text(Role::Body, words).color(color::TEXT));
        })
        .response;
    let rect = response.rect;
    ui.painter().rect_filled(
        egui::Rect::from_min_max(rect.min, pos2(rect.min.x + 3.0, rect.max.y)),
        0.0,
        edge,
    );
}

pub(crate) fn label(ui: &mut Ui, words: &str) -> egui::Response {
    ui.label(text(Role::Label, words.to_uppercase()).color(color::TEXT_2))
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
        Step::Download => "DOWNLOAD",
        Step::EnterBootloader => "ENTER",
        Step::WaitForBootloader => "WAIT",
        Step::Connect => "CONNECT",
        Step::Erase => "ERASE",
        Step::Write => "WRITE",
        Step::Verify => "VERIFY",
        Step::Reset => "RESET",
        Step::WaitForFirmware => "RETURN",
    }
}

/// The 48 px action slab: filled and inked when it acts, an outline when it is neutral.
fn slot(ui: &mut Ui, words: &str, fill: Option<Color32>) -> egui::Response {
    let words = text(Role::Action, words.to_uppercase());
    let button = match fill {
        Some(fill) => egui::Button::new(words.color(color::INK))
            .fill(fill)
            .stroke(Stroke::NONE),
        None => egui::Button::new(words.color(color::TEXT))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, color::BORDER)),
    };
    ui.add_sized(vec2(320.0, style::SLOT_HEIGHT), button)
}

/// One of a row of choices: filled with the accent when it is the one chosen.
pub(crate) fn choice(ui: &mut Ui, words: &str, chosen: bool) -> egui::Response {
    let rich = text(Role::Action, words.to_uppercase()).color(if chosen {
        color::INK
    } else {
        color::TEXT
    });
    let edge = if chosen { color::ACCENT } else { color::BORDER };
    ui.add(
        egui::Button::new(rich)
            .fill(if chosen {
                color::ACCENT
            } else {
                Color32::TRANSPARENT
            })
            .stroke(Stroke::new(1.0, edge))
            .selected(chosen),
    )
}

/// A neutral outlined button.
pub(crate) fn outline(ui: &mut Ui, words: &str, enabled: bool) -> egui::Response {
    let button = egui::Button::new(text(Role::Action, words.to_uppercase()).color(color::TEXT))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, color::BORDER));
    ui.add_enabled(enabled, button)
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
            stock,
            choice: Choice::Release,
            picked: None,
            force: false,
            confirming: None,
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
        };
        app.backend.send(Request::Detect);
        app.backend.send(Request::Release);
        app
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
                // After an update the unit comes back as a new enumeration on the same port.
                let returned = self
                    .ended
                    .as_ref()
                    .and_then(|e| e.location.clone())
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
            }
            Event::Units(Err(error)) => self.scan = Scan::Failed(error),
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
                self.finish_job(result, now);
                self.rescan();
            }
            Event::WorkerFailed(error) => {
                self.bench.worker_failed(&error.message);
                self.worker_error = Some(error.message.clone());
                self.finish_job(Err(error.clone()), now);
                self.batch_event(BatchEvent::Ended(Err(error.message.clone())), now);
                self.scan = Scan::Failed(error.message.clone());
                if self.release.is_none() {
                    self.release = Some(Err(error.message.clone()));
                }
                self.notice = Some(error.message);
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

    fn finish_job(&mut self, result: Result<Outcome, JobError>, now: f64) {
        if let Some(mut job) = self.operation.take_job() {
            if let Some(current) = job.current.take() {
                job.finished.push((current.step, now - current.since));
            }
            self.ended = Some(Ended {
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
        if self.operation.job().is_some() {
            self.notice = Some(format!(
                "{} arrived while an update was running, and was ignored.",
                path.display()
            ));
            return;
        }
        let image = Image::from_path(&path)
            .and_then(|image| image.check_for_mculink().map(|()| image))
            .map_err(|error| error.to_string());
        self.picked = Some(Picked { path, image });
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
    fn action(&self) -> Action {
        let blocked = |label: &str, reason: String| Action::Blocked {
            label: label.to_owned(),
            reason,
        };
        if let Some(error) = &self.worker_error {
            return blocked("Worker stopped", format!("{error}. Restart the tool."));
        }
        let Some(unit) = self.selected_unit() else {
            return if self.units.is_empty() {
                blocked(
                    "Nothing attached",
                    "Plug in an MCU-Link. On Linux your user also needs the udev rules in the README.".into(),
                )
            } else {
                blocked("Pick a unit", "Pick a unit on the left.".into())
            };
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
                        fill: color::OURS,
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
                    "No stock image",
                    "No MCU-LINK_CMSIS-DAP_V*.s19 in LinkServer's usual places. Install NXP's LinkServer, or pick the image as a file.".into(),
                ),
                Some(found) => Action::Ready {
                    label: format!("Install stock V{}", found.version),
                    fill: color::ATTENTION,
                    source: Source::Stock(found.path.parent().map(std::path::Path::to_path_buf)),
                    what: format!("stock V{}", found.version),
                },
            },
            Choice::File => match &self.picked {
                None => blocked("Pick a file", "Pick an ELF or S-record, or drop one on the window.".into()),
                Some(Picked { image: Err(error), .. }) => blocked("Not an image", error.clone()),
                Some(Picked { path, image: Ok(_) }) => {
                    let name = path
                        .file_name()
                        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                    Action::Ready {
                        label: format!("Flash {name}"),
                        fill: color::ACCENT,
                        source: Source::File(path.clone()),
                        what: name,
                    }
                }
            },
        };
        // A release's image says which boards it supports only once it is downloaded.
        if let Action::Ready { source, .. } = &action {
            let checked = match (source, self.picked_image()) {
                (Source::File(_), Some(image)) => {
                    source::check_board(Some(unit), image, &ImageVersion::Unknown)
                }
                _ => source::check_unit(Some(unit), source),
            };
            if let Err(error) = checked {
                return blocked(
                    "Not for this board",
                    format!("{} Pick STOCK for that.", sentence(&error.to_string())),
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
            Source::File(_) => self
                .picked_image()
                .and_then(|image| image.boards().ok())
                .map(|boards| format!("The image is built for {}.", board::built_for(&boards))),
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

    fn start(&mut self, unit: Box<Unit>, source: Source, download: Download, now: f64) {
        let cancel = Cancel::new();
        self.operation = Operation::Installing(Job {
            target: unit_name(&unit),
            location: unit.device.location.clone(),
            image: None,
            rail: Vec::new(),
            finished: Vec::new(),
            current: None,
            notes: Vec::new(),
            started: now,
            cancel: cancel.clone(),
        });
        self.ended = None;
        self.confirming = None;
        self.backend.send(Request::Install {
            unit,
            source,
            force: self.force,
            download,
            cancel,
        });
        self.force = false;
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

        egui::Panel::top("strip")
            .frame(
                Frame::new()
                    .fill(color::SURFACE)
                    .inner_margin(Margin::symmetric(space::M as i8, space::S as i8)),
            )
            .show(ui, |ui| self.strip(ui));
        egui::Panel::bottom("log")
            .frame(
                Frame::new()
                    .fill(color::SUNKEN)
                    .inner_margin(Margin::symmetric(space::M as i8, space::XS as i8)),
            )
            .show(ui, |ui| self.log_drawer(ui));
        if self.view == View::Bench {
            self.bench
                .show(ui, &self.units, &mut self.selected, self.backend.as_mut());
        } else {
            self.views(ui, now);
        }

        if self.about_open {
            let mut open = true;
            egui::Window::new("Licences")
                .open(&mut open)
                .default_size(vec2(560.0, 420.0))
                .show(&ctx, |ui| {
                    ScrollArea::vertical().show(ui, |ui| {
                        ui.label(text(Role::Body, "verge is licensed under Apache-2.0. Its fonts are under the SIL Open Font License 1.1, unmodified:"));
                        for (faces, licence) in style::FONT_LICENCES {
                            ui.add_space(space::S);
                            label(ui, faces);
                            ui.label(text(Role::Data, *licence));
                        }
                    });
                });
            self.about_open = open;
        }
    }

    /// The Units and Batch views, which scroll in the central panel.
    fn views(&mut self, ui: &mut Ui, now: f64) {
        if self.view == View::Units {
            egui::Panel::left("units")
                .exact_size(300.0)
                .frame(
                    Frame::new()
                        .fill(color::SUNKEN)
                        .inner_margin(Margin::same(space::M as i8)),
                )
                .show(ui, |ui| self.unit_list(ui));
        }
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(color::BACKGROUND)
                    .inner_margin(Margin::same(space::L as i8)),
            )
            .show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| match self.view {
                    View::Units => self.main(ui, now),
                    View::Batch => self.batch_view(ui, now),
                    // Drawn by `show`, in panels of its own.
                    View::Bench => {}
                });
            });
    }

    fn strip(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(text(Role::Title, "VERGE").color(color::TEXT));
            ui.add_space(space::M);
            let idle = !self.busy();
            for (view, words) in [
                (View::Units, "Units"),
                (View::Batch, "Batch"),
                (View::Bench, "Bench"),
            ] {
                let selected = self.view == view;
                let button = egui::Button::new(
                    text(Role::Action, words.to_uppercase()).color(if selected {
                        color::INK
                    } else {
                        color::TEXT
                    }),
                )
                .fill(if selected {
                    color::ACCENT
                } else {
                    Color32::TRANSPARENT
                })
                .stroke(Stroke::new(
                    1.0,
                    if selected {
                        color::ACCENT
                    } else {
                        color::BORDER
                    },
                ))
                .selected(selected);
                let response = ui.add_enabled(idle || selected, button);
                if response.clicked() && !selected {
                    if self.view == View::Bench {
                        self.bench.disconnect(self.backend.as_mut());
                    }
                    self.view = view;
                    self.confirming = None;
                    self.batch_form.confirming = None;
                }
                response.on_disabled_hover_text("Wait for what is running to finish.");
            }
            ui.add_space(space::M);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if outline(ui, "Licences", true).clicked() {
                    self.about_open = true;
                }
                let rescan = outline(ui, "Rescan", idle && self.worker_error.is_none());
                if !idle {
                    rescan.on_disabled_hover_text("Wait for what is running to finish.");
                } else if rescan.clicked() {
                    self.ended = None;
                    self.rescan();
                    self.backend.send(Request::Release);
                }
                if let Some(scenario) = &self.config.simulated {
                    ui.add_space(space::M);
                    Frame::new()
                        .stroke(Stroke::new(1.0, color::ATTENTION))
                        .inner_margin(Margin::symmetric(space::S as i8, 2))
                        .show(ui, |ui| {
                            ui.label(
                                text(
                                    Role::Label,
                                    format!("SIMULATED BENCH: {}", scenario.to_uppercase()),
                                )
                                .color(color::ATTENTION),
                            );
                        });
                }
                ui.add_space(space::M);
                // One line, cut short where the window leaves too little room for it.
                let mut releases = egui::text::LayoutJob::default();
                let mut add = |rich: egui::RichText| {
                    rich.append_to(
                        &mut releases,
                        ui.style(),
                        egui::FontSelection::Default,
                        egui::Align::Center,
                    );
                };
                let tag = |words: &str| {
                    text(Role::Label, format!("{} ", words.to_uppercase())).color(color::TEXT_2)
                };
                add(tag("releases"));
                add(text(Role::Data, format!("{}  ", self.config.repo)).color(color::TEXT));
                match &self.release {
                    None => add(tag("looking up")),
                    Some(Ok(release)) => {
                        add(tag("latest"));
                        add(text(Role::Data, &release.tag).color(color::TEXT));
                    }
                    Some(Err(_)) => add(tag("none found")),
                }
                ui.add(egui::Label::new(releases).truncate());
            });
        });
        if self.close_pending {
            edged(
                ui,
                color::ATTENTION,
                if matches!(self.operation, Operation::Batch(_)) {
                    "The window closes when the batch's unit under way finishes. The batch takes no new units."
                } else if self.bench.recording() {
                    "Closing after the recording stops."
                } else if self.bench.busy() {
                    "Closing when the file operation finishes. Close any file dialog to continue."
                } else {
                    "The window closes when this update finishes. Cancel stops it at the next safe point and leaves the unit in ISP mode for the next flash."
                },
            );
        }
    }

    fn unit_list(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            label(ui, "attached");
            ui.label(text(Role::Label, self.units.len().to_string()).color(color::TEXT));
            if let Scan::Scanning = self.scan {
                label(ui, "· scanning");
            }
        });
        if let Scan::Failed(error) = &self.scan {
            edged(
                ui,
                color::FAULT,
                &format!("Could not list USB devices: {error}"),
            );
        }
        ui.add_space(space::S);
        let idle = self.operation.job().is_none();
        let mut clicked = None;
        for unit in &self.units {
            let selected = self.selected.as_deref() == Some(unit.device.instance.as_str());
            let (word, glyph, fill) = identity(unit);
            let response = Frame::new()
                .fill(if selected {
                    color::SURFACE
                } else {
                    Color32::TRANSPARENT
                })
                .inner_margin(Margin::same(space::S as i8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        glyph.show(ui, 20.0, fill.unwrap_or(color::TEXT_2));
                        ui.vertical(|ui| {
                            let name = match &unit.state {
                                State::Bootloader => "LPC55S69 bootloader",
                                State::MiniBootloader => "MCU-Link mini bootloader",
                                _ => unit.serial().unwrap_or("no serial"),
                            };
                            ui.label(text(Role::Data, name).color(color::TEXT));
                            let version = version_of(unit)
                                .map(|v| format!(" {v}"))
                                .unwrap_or_default();
                            ui.label(
                                text(
                                    Role::Label,
                                    format!(
                                        "{word}{version} · {}",
                                        unit.device.location.as_deref().unwrap_or("?")
                                    ),
                                )
                                .color(color::TEXT_2),
                            );
                        });
                    });
                })
                .response;
            if selected {
                let rect = response.rect;
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(rect.min, pos2(rect.min.x + 3.0, rect.max.y)),
                    0.0,
                    color::ACCENT,
                );
            }
            let response = ui
                .interact(
                    response.rect,
                    response.id.with("pick"),
                    egui::Sense::click(),
                )
                .on_hover_text(unit.state.to_string());
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
            self.ended = None;
        }
    }

    fn main(&mut self, ui: &mut Ui, now: f64) {
        if let Some(job) = self.operation.job() {
            Self::working(ui, job, now);
            ui.add_space(space::L);
            if job.cancel.is_cancelled() {
                ui.label(
                    text(Role::Action, "CANCELLING AT THE NEXT SAFE POINT").color(color::TEXT_2),
                );
            } else if slot(ui, "Cancel", None).clicked() {
                job.cancel.cancel();
            }
            return;
        }
        if let Some(ended) = &self.ended {
            Self::result(ui, ended);
            ui.add_space(space::L);
            if slot(ui, "Back", None).clicked() {
                self.ended = None;
            }
            return;
        }
        let Some(unit) = self.selected_unit().cloned() else {
            frame(ui, None, Stroke::new(1.0, color::BORDER), |ui| {
                ui.label(text(Role::Display, "NOTHING").color(color::TEXT_2));
                ui.label(
                    text(
                        Role::Instruction,
                        if self.units.is_empty() {
                            "PLUG IN AN MCU-LINK"
                        } else {
                            "PICK A UNIT"
                        },
                    )
                    .color(color::TEXT),
                );
            });
            return;
        };
        self.unit_slab(ui, &unit);
        ui.add_space(space::M);
        let offer = offer(&unit, &self.units);
        field(ui, "path", |ui| match offer.best() {
            Some(path) => wrapping(
                ui,
                text(Role::Body, format!("{path}. {}", path.describe())).color(color::TEXT),
            ),
            None => wrapping(ui, text(Role::Body, "None.").color(color::TEXT_2)),
        });
        for note in &offer.notes {
            edged(ui, color::ATTENTION, note);
        }
        ui.add_space(space::M);
        self.image_choice(ui, Some(&unit));
        ui.add_space(space::L);

        let action = self.action();
        // A question asked before the worker stopped, or about a unit no longer selected, no longer
        // stands: confirming it would start an update nothing can run, or one on a unit that left.
        let asked_about_another = self.confirming.as_ref().is_some_and(|question| {
            self.selected.as_deref() != Some(question.unit.device.instance.as_str())
        });
        if asked_about_another || matches!(action, Action::Blocked { .. }) {
            self.confirming = None;
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
            edged(ui, color::ATTENTION, &question);
            ui.add_space(space::S);
            ui.horizontal(|ui| {
                let fill = match &action {
                    Action::Ready { fill, .. } => *fill,
                    Action::Blocked { .. } => color::ACCENT,
                };
                if slot(ui, "Confirm", Some(fill)).clicked() {
                    self.start(unit, source, agreed(download.as_ref()), now);
                }
                if slot(ui, "Back", None).clicked() {
                    self.confirming = None;
                }
            });
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
                        unit: Box::new(unit),
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
                    egui::Button::new(
                        text(Role::Action, words.to_uppercase()).color(color::TEXT_2),
                    )
                    .fill(Color32::TRANSPARENT)
                    .stroke(Stroke::new(1.0, color::BORDER))
                    .min_size(vec2(320.0, style::SLOT_HEIGHT)),
                );
                if !reason.is_empty() {
                    wrapping(ui, text(Role::Body, reason).color(color::TEXT_2));
                }
            }
        }
    }

    fn unit_slab(&self, ui: &mut Ui, unit: &Unit) {
        let (word, glyph, fill) = identity(unit);
        let ink = if fill.is_some() {
            color::INK
        } else {
            color::TEXT
        };
        let stroke = if fill.is_some() {
            Stroke::NONE
        } else {
            Stroke::new(1.0, color::BORDER)
        };
        frame(ui, fill, stroke, |ui| {
            ui.horizontal(|ui| {
                glyph.show(ui, 48.0, ink);
                ui.add_space(space::M);
                ui.label(text(Role::Display, word).color(ink));
                if let Some(version) = version_of(unit) {
                    ui.add_space(space::L);
                    ui.label(text(Role::Tally, version).color(ink));
                }
            });
            let subline = match &unit.state {
                State::Bootloader => "WAITING FOR AN IMAGE".to_owned(),
                State::MiniBootloader => "A DIFFERENT BOARD: NOT SUPPORTED".to_owned(),
                State::Unreadable { reason, .. } => {
                    format!("COULD NOT ASK: {}", reason.to_uppercase())
                }
                State::Held { .. } => "ANOTHER PROGRAM HOLDS ITS CMSIS-DAP".to_owned(),
                State::Probe { .. } => match offer(unit, &self.units).best() {
                    Some(
                        Path::OursJumperFree | Path::StockJumperFree | Path::DaplinkRsJumperFree,
                    ) => "UPDATES WITHOUT THE JUMPER",
                    Some(Path::Jumper) => "NEEDS THE ISP JUMPER TO UPDATE",
                    _ => "CANNOT BE UPDATED AS IT IS",
                }
                .to_owned(),
                State::SwdJumper { .. } => match offer(unit, &self.units).best() {
                    Some(Path::OursJumperFree) => {
                        "SWD JUMPER FITTED. UPDATES WITHOUT THE ISP JUMPER"
                    }
                    Some(Path::Jumper) => "SWD JUMPER FITTED. NEEDS THE ISP JUMPER TO UPDATE",
                    _ => "SWD JUMPER FITTED. CANNOT BE UPDATED AS IT IS",
                }
                .to_owned(),
            };
            wrapping(ui, text(Role::Instruction, subline).color(ink));
            let mut facts: Vec<String> = unit.serial().map(str::to_owned).into_iter().collect();
            facts.extend([
                unit.device.location.clone().unwrap_or_else(|| "?".into()),
                format!("{:04x}:{:04x}", unit.device.vid, unit.device.pid),
            ]);
            if let State::Probe { identity, .. } = &unit.state {
                facts.extend(identity.product.clone());
                facts.extend(identity.protocol.as_ref().map(|p| format!("CMSIS-DAP {p}")));
            }
            wrapping(ui, text(Role::Data, facts.join(" · ")).color(ink));
        });
    }

    fn image_choice(&mut self, ui: &mut Ui, unit: Option<&Unit>) {
        let ctx = ui.ctx().clone();
        field(ui, "image", |ui| {
            ui.horizontal(|ui| {
                for (choice, words) in [
                    (Choice::Release, "Release"),
                    (Choice::Stock, "Stock"),
                    (Choice::File, "File"),
                ] {
                    let selected = self.choice == choice;
                    let rich = text(Role::Action, words.to_uppercase()).color(if selected {
                        color::INK
                    } else {
                        color::TEXT
                    });
                    let button = egui::Button::new(rich)
                        .fill(if selected {
                            color::ACCENT
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::new(
                            1.0,
                            if selected {
                                color::ACCENT
                            } else {
                                color::BORDER
                            },
                        ))
                        .selected(selected);
                    if ui.add(button).clicked() {
                        self.choice = choice;
                        self.confirming = None;
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
            let detail = match self.choice {
                Choice::Release => match &self.release {
                    None => "Looking up the latest release.".to_owned(),
                    Some(Err(_)) => "No release to install.".to_owned(),
                    Some(Ok(release)) => {
                        let standing = unit.and_then(Unit::build).map(|build| {
                            match build.compare(&release.tag) {
                                verge_probe::version::Relation::UpToDate => {
                                    "runs this release".to_owned()
                                }
                                verge_probe::version::Relation::OtherVariant => {
                                    format!("runs this release built as {build}")
                                }
                                verge_probe::version::Relation::Behind => {
                                    format!("runs {build}, older")
                                }
                                verge_probe::version::Relation::Ahead => {
                                    format!("runs {build}, newer than this release")
                                }
                                verge_probe::version::Relation::Unknown => {
                                    format!("runs {build}, which carries no version to compare")
                                }
                            }
                        });
                        let mut words = format!(
                            "{} · {} · {} bytes · checked against {} and {}",
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
                    Some(found) => format!("V{} from {}", found.version, found.path.display()),
                    None => "No stock image found in LinkServer's usual places.".to_owned(),
                },
                Choice::File => match &self.picked {
                    None if self.picker.is_some() => "The file dialog is open.".to_owned(),
                    None => "Pick an ELF or S-record, or drop one on the window.".to_owned(),
                    Some(Picked {
                        path,
                        image: Ok(image),
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
                    Some(Picked {
                        path,
                        image: Err(error),
                    }) => format!("{}: {error}", path.display()),
                },
            };
            wrapping(ui, text(Role::Data, detail).color(color::TEXT_2));
        });
        if let Some(notice) = &self.notice {
            edged(ui, color::ATTENTION, notice);
        }
        let release_refused = self.choice == Choice::Release
            && matches!((&self.release, unit.and_then(Unit::build)), (Some(Ok(release)), Some(build))
                if matches!(build.compare(&release.tag), verge_probe::version::Relation::UpToDate | verge_probe::version::Relation::Ahead));
        if unit.is_some() && (release_refused || (self.force && self.choice == Choice::Release)) {
            Frame::new()
                .stroke(Stroke::new(1.0, color::ATTENTION))
                .inner_margin(Margin::same(space::S as i8))
                .show(ui, |ui| {
                    ui.checkbox(
                        &mut self.force,
                        text(Role::Body, "Install anyway: flash this release even though the unit runs the same or a newer build").color(color::TEXT),
                    );
                });
        }
    }

    fn working(ui: &mut Ui, job: &Job, now: f64) {
        frame(ui, None, Stroke::new(2.0, color::TEXT), |ui| {
            ui.horizontal(|ui| {
                glyph::WORKING.show(ui, 48.0, color::TEXT);
                ui.add_space(space::M);
                ui.label(text(Role::Display, "WORKING").color(color::TEXT));
            });
            ui.label(text(Role::Instruction, "DO NOT UNPLUG THE UNIT").color(color::TEXT));
            let mut facts = vec![job.target.clone()];
            facts.extend(job.image.clone());
            facts.push(format!("{:.1} s", now - job.started));
            ui.label(text(Role::Data, facts.join(" · ")).color(color::TEXT_2));
            ui.add_space(space::M);
            if let Some(current) = &job.current {
                ui.horizontal(|ui| {
                    label(ui, &current.step.to_string());
                    if current.step.unit() == StepUnit::Bytes && current.total > 0 {
                        ui.label(
                            text(
                                Role::Data,
                                format!("{} / {}", kib(current.done), kib(current.total)),
                            )
                            .color(color::TEXT),
                        );
                    }
                    ui.label(
                        text(Role::Data, format!("{:.1} s", now - current.since))
                            .color(color::TEXT_2),
                    );
                });
                if current.step.unit() == StepUnit::Bytes && current.total > 0 {
                    let fraction = current.done as f32 / current.total as f32;
                    let (rect, _) = ui
                        .allocate_exact_size(vec2(ui.available_width(), 8.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, color::BORDER);
                    let mut done = rect;
                    done.set_width(rect.width() * fraction.clamp(0.0, 1.0));
                    ui.painter().rect_filled(done, 0.0, color::TEXT);
                }
            }
        });
        ui.add_space(space::M);
        Self::rail(
            ui,
            &job.rail,
            &job.finished,
            job.current.as_ref().map(|c| c.step),
        );
        for note in &job.notes {
            edged(ui, color::ATTENTION, note);
        }
    }

    /// One cell per step: filled when done, with its seconds; a heavy outline for the current one.
    fn rail(ui: &mut Ui, rail: &[Step], finished: &[(Step, f64)], current: Option<Step>) {
        if rail.is_empty() {
            return;
        }
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
                        if seconds.is_some() {
                            ui.painter().rect_filled(rect, 0.0, color::TEXT);
                        } else if current == Some(*step) {
                            ui.painter().rect_stroke(
                                rect,
                                0.0,
                                Stroke::new(2.0, color::ACCENT),
                                egui::StrokeKind::Inside,
                            );
                        } else {
                            ui.painter().rect_stroke(
                                rect,
                                0.0,
                                Stroke::new(1.0, color::BORDER),
                                egui::StrokeKind::Inside,
                            );
                        }
                        response.on_hover_text(step.to_string()).widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Label, true, step.to_string())
                        });
                        label(ui, short(*step));
                        if let Some(t) = seconds {
                            ui.label(text(Role::Data, format!("{t:.1} s")).color(color::TEXT_2));
                        }
                    },
                );
            }
        });
    }

    fn result(ui: &mut Ui, ended: &Ended) {
        let (word, instruction, glyph, fill) = match &ended.result {
            Ok(Outcome::Running(_)) => (
                "DONE",
                "THE UNIT IS BACK".to_owned(),
                glyph::DONE,
                color::OURS,
            ),
            Ok(Outcome::JumperStillFitted) => (
                "FLASHED",
                "UNPLUG IT, TAKE THE JUMPER OFF, PLUG IT BACK IN".to_owned(),
                glyph::ATTENTION,
                color::ATTENTION,
            ),
            Ok(Outcome::NotRestarted { maybe_jumper: false }) => (
                "FLASHED",
                "IT DID NOT RESTART: UNPLUG IT AND PLUG IT BACK IN".to_owned(),
                glyph::ATTENTION,
                color::ATTENTION,
            ),
            Ok(Outcome::NotRestarted { maybe_jumper: true }) => (
                "FLASHED",
                "IT DID NOT RESTART, OR ITS JUMPER IS FITTED: UNPLUG IT, TAKE ANY JUMPER OFF, PLUG IT BACK IN"
                    .to_owned(),
                glyph::ATTENTION,
                color::ATTENTION,
            ),
            Err(error) if error.left_in_isp => (
                "FAILED",
                "THE UNIT MAY BE IN ISP MODE".to_owned(),
                glyph::FAILED,
                color::FAULT,
            ),
            Err(_) => (
                "FAILED",
                "NOTHING WAS FLASHED, OR THE UNIT DID NOT RETURN".to_owned(),
                glyph::FAILED,
                color::FAULT,
            ),
        };
        frame(ui, Some(fill), Stroke::NONE, |ui| {
            ui.horizontal(|ui| {
                glyph.show(ui, 48.0, color::INK);
                ui.add_space(space::M);
                ui.label(text(Role::Display, word).color(color::INK));
            });
            ui.label(text(Role::Instruction, instruction).color(color::INK));
            let mut facts = vec![ended.target.clone()];
            facts.extend(ended.image.clone());
            facts.push(format!("{:.1} s", ended.seconds));
            ui.label(text(Role::Data, facts.join(" · ")).color(color::INK));
            if let Ok(Outcome::Running(unit)) = &ended.result {
                wrapping(
                    ui,
                    text(
                        Role::Data,
                        format!("now {}, which runs {}", unit_name(unit), unit.state),
                    )
                    .color(color::INK),
                );
            }
        });
        ui.add_space(space::M);
        if let Err(error) = &ended.result {
            ui.add(
                egui::Label::new(text(Role::Data, &error.message).color(color::TEXT))
                    .selectable(true),
            );
            if let Some(advice) = error.advice {
                edged(ui, color::ATTENTION, advice);
            }
        }
        if !ended.steps.is_empty() {
            let rail: Vec<Step> = ended.steps.iter().map(|(s, _)| *s).collect();
            Self::rail(ui, &rail, &ended.steps, None);
        }
        for note in &ended.notes {
            edged(ui, color::ATTENTION, note);
        }
    }

    fn log_drawer(&mut self, ui: &mut Ui) {
        let (lines, warnings, errors) = self.log.tail(if self.log_open { 400 } else { 1 });
        let level_color = |level: tracing::Level| match level {
            tracing::Level::ERROR => color::FAULT,
            tracing::Level::WARN => color::ATTENTION,
            tracing::Level::INFO => color::TEXT,
            _ => color::TEXT_2,
        };
        if self.log_open {
            ScrollArea::vertical()
                .max_height(220.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for line in &lines {
                        ui.add(
                            egui::Label::new(
                                text(Role::Data, format!("{:<5} {}", line.level, line.text))
                                    .color(level_color(line.level)),
                            )
                            .selectable(true),
                        );
                    }
                });
        }
        ui.horizontal(|ui| {
            label(ui, "log");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if outline(ui, if self.log_open { "Hide" } else { "Show" }, true).clicked() {
                    self.log_open = !self.log_open;
                }
                ui.label(
                    text(Role::Label, format!("{warnings} WARN · {errors} ERROR"))
                        .color(color::TEXT_2),
                );
                if !self.log_open
                    && let Some(line) = lines.last()
                {
                    ui.add(
                        egui::Label::new(
                            text(Role::Data, format!("{:<5} {}", line.level, line.text))
                                .color(level_color(line.level)),
                        )
                        .truncate(),
                    );
                }
            });
        });
    }
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
            "{fit} This unit is {}. In ISP mode the tool cannot tell its board, and a release says which boards it is for only once it is downloaded: flash it with STOCK, or with a file built for {}.",
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

/// An error message as a sentence: capitalised, with a full stop.
pub fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    let mut out: String = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default();
    out.extend(chars);
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
