//! Bench view: a Pro's supply, energy and DIO pins, or a standalone MCU-Link's DIO pins.
//! Acquisition, and the recording's queries and drawing through tickwave, continue independently
//! of frames.

use crate::app::{choice, edged, label, outline, sentence, sentence_case};
use crate::bench_worker::{Command, DioChange, Status, group_name};
use crate::style::{self, Role, space, text};
use crate::worker::{Backend, Request};
use egui::{CornerRadius, Frame, Margin, ScrollArea, Stroke, Ui, vec2};
use escapement_tickwave_egui::view::{
    Answer, ChannelStatistics, MarkerCursor, Recording, Statistics,
};
use escapement_tickwave_egui::{
    Extras, Graph, Palette, Shown, Strip, ViewState, Viewer, Window, Y,
};
use escapement_verge_probe::tickwave::{self as recordings, Snapshots};
use escapement_verge_probe::{BoardKind, EnergyRanges, Unit, pro};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::AtomicBool,
    mpsc::{self, Receiver},
};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quantity {
    Current,
    Voltage,
    Power,
}
impl Quantity {
    fn word(self) -> &'static str {
        match self {
            Self::Current => "Current",
            Self::Voltage => "Voltage",
            Self::Power => "Power",
        }
    }
}
enum FileAction {
    Open,
    Save,
    Export(bool),
    Discard,
}
enum FileResult {
    Open(PathBuf),
    Done(String),
    /// The user closed the file dialog without choosing.
    Cancelled,
    Recovery(Vec<PathBuf>),
    Discarded(PathBuf),
    /// The recording was compacted in place; its view reads the copy.
    Compacted,
    /// A recording a crash cut off, finished by compacting it, to open.
    Repaired(PathBuf),
}

/// What the panel reads from a recording once: its channels, and how fast its ticks run.
struct Channels {
    /// The trace for each quantity, where the recording has it.
    i: Option<usize>,
    v: Option<usize>,
    p: Option<usize>,
    under: Option<usize>,
    lanes: Vec<usize>,
    /// Ticks a second.
    rate: f64,
    timebase: escapement_tickwave_egui::view::format::model::Ratio,
    /// The capture held the high range.
    high_range_only: bool,
}
impl Channels {
    fn read(r: &Recording) -> Self {
        let timebase = r.timebase();
        let header = r
            .sources()
            .first()
            .and_then(|source| recordings::header_from(&source.extra));
        Self {
            i: r.channel("I"),
            v: r.channel("V"),
            p: r.channel("P"),
            under: r.channel("under_range"),
            lanes: (0..4)
                .filter_map(|k| r.channel(&format!("DIO{k}")))
                .collect(),
            rate: timebase.num() as f64 / timebase.den() as f64,
            timebase,
            high_range_only: header.is_some_and(|h| h.high_range_only),
        }
    }
    fn trace(&self, quantity: Quantity) -> Option<usize> {
        match quantity {
            Quantity::Current => self.i,
            Quantity::Voltage => self.v,
            Quantity::Power => self.p,
        }
    }
    fn seconds(&self, ticks: i64) -> f64 {
        ticks as f64 / self.rate
    }
    fn ticks(&self, seconds: f64) -> i64 {
        (seconds * self.rate).round() as i64
    }
}

/// The recording under way, while it is the one open.
struct Live {
    slot: Snapshots,
    /// Where the last snapshot's data ends, and when it was published.
    end: Option<(i64, Instant)>,
}

/// A recording to compact, and the view that reads it meanwhile.
struct Compaction {
    path: PathBuf,
    viewer: Option<Arc<Viewer>>,
}

/// How far a followed window may run ahead of the data, so a stalled stream doesn't scroll on into
/// blank time.
const AHEAD: Duration = Duration::from_millis(50);
/// Markers listed a page at a time.
const MARKER_PAGE: usize = 50;
/// The strip's height under the graph.
const STRIP: f32 = 64.0;

pub struct Bench {
    pub status: Option<Status>,
    connection: Option<String>,
    rate: u32,
    /// The current ranges for a capture the panel starts.
    ranges: EnergyRanges,
    path: Option<PathBuf>,
    /// A recording to open on the next frame, which has the context a view needs.
    to_open: Option<PathBuf>,
    directory: PathBuf,
    recovered: Vec<PathBuf>,
    /// The open recording's view. The file thread holds it too, to let go of the file for a
    /// compaction's rename.
    viewer: Option<Arc<Viewer>>,
    channels: Option<Channels>,
    live: Option<Live>,
    state: ViewState,
    quantity: Quantity,
    marker_after: Option<MarkerCursor>,
    marker_selected: Option<MarkerCursor>,
    /// The window and value range the graph drew last frame.
    shown: Option<Shown>,
    io: Option<Receiver<Result<FileResult, String>>>,
    failed: bool,
    notice: Option<String>,
    start_confirm: bool,
    off_confirm: bool,
    /// The height under the graph, measured on the frame before.
    footer: f32,
    /// Compactions waiting for the file thread, and the one it runs.
    compactions: Vec<Compaction>,
    compacting: Option<PathBuf>,
    /// The worker's latest recording, as its statuses name it, whichever file is shown; whether
    /// it is still being written; and its snapshots.
    recorded: Option<PathBuf>,
    writing: bool,
    recorded_slot: Option<Snapshots>,
    /// The followed window's width, in seconds, which carries over to the next file opened.
    span_seconds: f64,
}

impl Bench {
    pub fn new(directory: &Path) -> Self {
        let directory = directory.join("captures");
        let recovery = directory.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(
                recordings::recordings(&recovery)
                    .map(FileResult::Recovery)
                    .map_err(|e| e.to_string()),
            );
        });
        Self {
            status: None,
            connection: None,
            rate: 25_000,
            ranges: EnergyRanges::Auto,
            path: None,
            to_open: None,
            directory,
            recovered: Vec::new(),
            viewer: None,
            channels: None,
            live: None,
            state: following(0),
            quantity: Quantity::Current,
            marker_after: None,
            marker_selected: None,
            shown: None,
            io: Some(rx),
            failed: false,
            notice: None,
            start_confirm: false,
            off_confirm: false,
            footer: 120.0,
            compactions: Vec::new(),
            compacting: None,
            recorded: None,
            writing: false,
            recorded_slot: None,
            span_seconds: 10.0,
        }
    }
    pub fn recording(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.recording)
    }
    pub fn busy(&self) -> bool {
        self.recording() || self.io.is_some() || !self.compactions.is_empty()
    }
    pub fn take_failure(&mut self) -> bool {
        std::mem::take(&mut self.failed)
    }
    pub fn worker_failed(&mut self, message: &str) {
        self.connection = None;
        self.status = None;
        self.start_confirm = false;
        self.off_confirm = false;
        self.notice = Some(message.to_owned());
        // The worker closed the recording as it failed: finish it as a stop would.
        if self.writing
            && let Some(path) = self.recorded.clone()
        {
            self.ended(path);
        }
    }
    #[cfg(test)]
    pub(crate) fn answer(&self) -> Option<Arc<Answer>> {
        self.viewer.as_ref()?.answer()
    }
    #[cfg(test)]
    pub(crate) fn ticks_per_second(&self) -> Option<f64> {
        Some(self.channels.as_ref()?.rate)
    }
    #[cfg(test)]
    pub(crate) fn select(&mut self, selection: Range<i64>) {
        self.state.selection = Some(selection);
    }
    /// The selection's readings, once the answer is for the selection on screen.
    #[cfg(test)]
    pub(crate) fn selection_statistics(&self) -> Option<Statistics> {
        let selection = self.state.selection.as_ref()?;
        self.answer()?
            .statistics
            .get(1)
            .filter(|s| s.interval == *selection)
            .cloned()
    }
    pub fn receive(&mut self, status: Status) {
        if self.connection.as_deref() != Some(status.instance.as_str()) {
            return;
        }
        self.off_confirm = status.confirm_off;
        if let Some(path) = status.path.clone()
            && self.recorded.as_ref() != Some(&path)
        {
            // A recording began: the panel follows it. One that began and ended between two
            // statuses is finished at once.
            if path.parent() == Some(self.directory.as_path()) && !self.recovered.contains(&path) {
                self.recovered.push(path.clone());
                self.recovered.sort();
            }
            self.recorded = Some(path.clone());
            self.recorded_slot = status.snapshots.clone();
            self.writing = status.recording;
            if status.recording {
                self.follow_recording();
            } else {
                self.close();
                self.path = Some(path.clone());
                self.ended(path);
            }
        } else if self.writing
            && !status.recording
            && let Some(path) = self.recorded.clone()
        {
            self.ended(path);
        }
        if let Some(error) = &status.error {
            if self.status.as_ref().and_then(|old| old.error.as_ref()) != Some(error) {
                self.failed = true;
                tracing::error!("bench: {error}");
            }
            self.notice = Some(error.clone());
        }
        self.status = Some(status);
    }
    /// Shows the recording under way, as it is written.
    fn follow_recording(&mut self) {
        let Some(path) = self.recorded.clone() else {
            return;
        };
        self.close();
        self.path = Some(path);
        self.live = self
            .recorded_slot
            .clone()
            .map(|slot| Live { slot, end: None });
    }
    /// The recording at `path` was finished by its writer. Shown, its view reads the finished
    /// file at once, then a compacted copy; shown without a view, the copy opens once made; not
    /// shown, it is compacted without a word. A recording whose header never came left no file.
    fn ended(&mut self, path: PathBuf) {
        self.writing = false;
        self.recorded_slot = None;
        let shown = self.path.as_ref() == Some(&path);
        if shown {
            self.live = None;
        }
        if !path.exists() {
            self.recovered.retain(|p| p != &path);
            if shown {
                self.close();
            }
            return;
        }
        let viewer = self.viewer.clone().filter(|_| shown);
        if let Some(viewer) = &viewer
            && let Err(error) = viewer.reopen()
        {
            // The compaction's own reopen tries again once its copy is in place.
            self.notice = Some(format!("Reading the finished capture: {error}"));
        }
        self.compact(path, viewer);
    }
    /// Lets go of the view of the open recording.
    fn close(&mut self) {
        self.viewer = None;
        self.live = None;
        self.path = None;
        self.shown = None;
        self.marker_after = None;
        self.marker_selected = None;
        if let (Window::Follow { span }, Some(channels)) = (&self.state.window, &self.channels) {
            self.span_seconds = channels.seconds(*span);
        }
        self.channels = None;
        self.state = following(0);
    }
    fn open(&mut self, path: PathBuf) {
        // The recording under way opens as it is written: repairing it would cut it short.
        if self.writing && self.recorded.as_ref() == Some(&path) {
            if self.live.is_none() {
                self.follow_recording();
            }
            return;
        }
        self.close();
        self.path = Some(path.clone());
        self.to_open = Some(path);
    }
    /// Opens `to_open`: a finished recording at once; one a crash cut off once compacting has
    /// finished it.
    fn open_now(&mut self, ctx: &egui::Context) {
        let Some(path) = self.to_open.clone() else {
            return;
        };
        // A second reader would hold the file at the compaction's rename, which Windows refuses:
        // it opens once the copy is in place.
        if self.compacting.as_ref() == Some(&path)
            || self.compactions.iter().any(|c| c.path == path)
        {
            return;
        }
        self.to_open = None;
        match recordings::finished(&path) {
            Ok(true) => match Viewer::open(ctx, &path) {
                Ok(viewer) => self.show_viewer(Arc::new(viewer)),
                Err(error) => self.notice = Some(format!("Opening {}: {error}", path.display())),
            },
            Ok(false) => {
                self.notice = Some("Repairing the capture a crash cut off…".into());
                self.compact(path, None);
            }
            Err(error) => self.notice = Some(format!("Opening {}: {error}", path.display())),
        }
    }
    fn show_viewer(&mut self, viewer: Arc<Viewer>) {
        let channels = viewer.recording(Channels::read);
        if let Window::Follow { span } = &mut self.state.window
            && *span <= 0
        {
            *span = channels.ticks(self.span_seconds);
        }
        self.channels = Some(channels);
        self.viewer = Some(viewer);
    }
    /// Compacts the recording at `path` on the file thread once it is free, `viewer` reading the
    /// original until the copy takes its place, then the copy. Without a view, the copy opens if
    /// it is still the file asked for.
    fn compact(&mut self, path: PathBuf, viewer: Option<Arc<Viewer>>) {
        self.compactions.push(Compaction { path, viewer });
    }
    fn start_compaction(&mut self) {
        if self.io.is_some() || self.compactions.is_empty() {
            return;
        }
        let Compaction { path, viewer } = self.compactions.remove(0);
        self.compacting = Some(path.clone());
        let (send, receive) = mpsc::channel();
        self.io = Some(receive);
        std::thread::spawn(move || {
            let result = compact_beside(&path, viewer.as_deref())
                .map(|()| match viewer {
                    Some(_) => FileResult::Compacted,
                    None => FileResult::Repaired(path),
                })
                .map_err(|error| format!("{error:#}"));
            let _ = send.send(result);
        });
    }
    pub fn disconnect(&mut self, backend: &mut dyn Backend) {
        self.start_confirm = false;
        self.off_confirm = false;
        if self.connection.take().is_some() {
            backend.send(Request::Bench(Command::Disconnect));
        }
        self.status = None;
    }
    fn connect(&mut self, unit: &Unit, backend: &mut dyn Backend) {
        if self.connection.as_deref() == Some(&unit.device.instance) {
            return;
        }
        self.start_confirm = false;
        self.off_confirm = false;
        self.status = None;
        self.connection = Some(unit.device.instance.clone());
        backend.send(Request::Bench(Command::Connect(Box::new(unit.clone()))));
    }
    fn start(&mut self, backend: &mut dyn Backend) {
        // UTC to the millisecond, without the colons Windows refuses in a file name.
        let now = humantime::format_rfc3339_millis(SystemTime::now())
            .to_string()
            .replace(':', "-");
        let path = self.directory.join(format!("capture-{now}.tickwave"));
        backend.send(Request::Bench(Command::Start {
            rate: self.rate,
            ranges: self.ranges,
            path,
        }));
        self.start_confirm = false;
    }
    pub fn tick(&mut self) {
        let received = self.io.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err("File worker stopped before finishing.".into()))
            }
        });
        if let Some(result) = received {
            self.io = None;
            self.compacting = None;
            match result {
                Ok(FileResult::Open(path)) => self.open(path),
                Ok(FileResult::Repaired(path)) => {
                    if self.path.as_ref() == Some(&path) {
                        self.notice = None;
                        self.open(path);
                    }
                }
                Ok(FileResult::Done(message)) => {
                    self.notice = Some(message);
                }
                Ok(FileResult::Cancelled | FileResult::Compacted) => {}
                Ok(FileResult::Recovery(paths)) => self.recovered = paths,
                Ok(FileResult::Discarded(path)) => {
                    self.recovered.retain(|p| p != &path);
                    if self.path.as_ref() == Some(&path) {
                        self.close();
                    }
                    self.notice = Some("Capture discarded.".into());
                }
                Err(error) => {
                    tracing::error!("file operation: {error}");
                    self.notice = Some(error);
                    self.failed = true;
                }
            }
        }
    }
    /// Takes in what came since the last frame: file results, a recording to open, and the
    /// recording under way's latest snapshot.
    fn poll(&mut self, ui: &Ui) {
        self.tick();
        self.start_compaction();
        self.open_now(ui.ctx());
        if let Some(live) = &mut self.live {
            let published = live.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(published) = published {
                // The recorder publishes again when its clock flushes a stalled stream: the same
                // end keeps its first publish time, or the followed window would step back.
                live.end = match (live.end, published.end) {
                    (Some((end, at)), Some(next)) if next == end => Some((end, at)),
                    (_, next) => next.map(|end| (end, published.at)),
                };
                match &self.viewer {
                    Some(viewer) => viewer.update(published.snapshot),
                    None => match self
                        .path
                        .as_deref()
                        .map(|path| Viewer::live(ui.ctx(), path, published.snapshot))
                    {
                        Some(Ok(viewer)) => self.show_viewer(Arc::new(viewer)),
                        Some(Err(error)) => {
                            self.notice = Some(format!("Showing the recording: {error}"));
                            self.live = None;
                        }
                        None => {}
                    },
                }
            }
        }
        if let Some(failure) = self.viewer.as_ref().and_then(|v| v.failure()) {
            self.notice = Some(failure);
        }
    }
    /// The host's now for the graph, while a recording is written: the last snapshot's end and the
    /// time since, at most `AHEAD` past it.
    fn now(&self) -> Option<i64> {
        let (end, at) = self.live.as_ref()?.end?;
        let rate = self.channels.as_ref()?.rate;
        Some(end + (at.elapsed().min(AHEAD).as_secs_f64() * rate).round() as i64)
    }

    /// The view: its controls and readings in a left panel, the graph in the rest.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        units: &[Unit],
        selected: &mut Option<String>,
        backend: &mut dyn Backend,
    ) {
        let face = style::face(ui.ctx());
        egui::Panel::left("bench")
            .exact_size(SIDE)
            .frame(
                Frame::new()
                    .fill(face.surface)
                    .inner_margin(Margin::same(space::M as i8)),
            )
            .show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| self.side(ui, units, selected, backend));
            });
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(face.page)
                    .inner_margin(Margin::same(space::L as i8)),
            )
            .show(ui, |ui| {
                self.poll(ui);
                self.graph(ui);
            });
        self.dialogs(ui, backend);
    }

    fn side(
        &mut self,
        ui: &mut Ui,
        units: &[Unit],
        selected: &mut Option<String>,
        backend: &mut dyn Backend,
    ) {
        let face = style::face(ui.ctx());
        let name = label(ui, "Unit");
        let chosen = units
            .iter()
            .find(|u| Some(&u.device.instance) == selected.as_ref());
        egui::ComboBox::from_id_salt("bench-unit")
            .width(ui.available_width())
            .selected_text(text(
                Role::Code,
                chosen.and_then(Unit::serial).unwrap_or("Select a unit"),
            ))
            .show_ui(ui, |ui| {
                for unit in units {
                    ui.add_enabled_ui(!self.recording(), |ui| {
                        ui.selectable_value(
                            selected,
                            Some(unit.device.instance.clone()),
                            text(Role::Code, unit.serial().unwrap_or("No serial")),
                        );
                    });
                }
            })
            .response
            .labelled_by(name.id);
        match chosen {
            Some(unit) => match pro::validate(unit, pro::Feature::Energy) {
                Ok(()) => self.connect(unit, backend),
                // A standalone MCU-Link, for its DIO pins.
                Err(_) if pro::validate(unit, pro::Feature::Dio).is_ok() => {
                    self.connect(unit, backend);
                }
                Err(error) => {
                    self.disconnect(backend);
                    ui.add_space(space::S);
                    edged(ui, style::Status::Attention, &sentence(&error.to_string()));
                }
            },
            None => self.disconnect(backend),
        }
        match (&self.status, chosen) {
            (Some(status), _) if status.connected => {
                let status = status.clone();
                if status.target_power {
                    self.supply(ui, &status, backend);
                }
                if status.energy {
                    self.acquisition(ui, &status, backend);
                } else {
                    let note = if status.board == Some(BoardKind::Pro) {
                        "This Pro has no energy measurement circuit fitted, so it measures no \
                         energy."
                    } else {
                        "A standalone MCU-Link has DIO pins. Supply control and energy \
                         measurement need a Pro."
                    };
                    ui.add_space(space::S);
                    ui.add(egui::Label::new(text(Role::Body, note).color(face.ink_2)).wrap());
                }
            }
            // The worker dropped the session, after a failure: the notice below says why.
            (Some(_), Some(unit)) => {
                ui.add_space(space::S);
                if outline(ui, "Reconnect", true).clicked() {
                    self.connection = None;
                    self.connect(unit, backend);
                }
            }
            (None, Some(_)) if self.connection.is_some() => {
                ui.add_space(space::S);
                ui.label(text(Role::Body, "Connecting…").color(face.ink_2));
            }
            _ => {}
        }
        if let Some(notice) = self.notice.clone() {
            ui.add_space(space::S);
            edged(ui, style::Status::Attention, &notice);
        }
        self.dio(ui, chosen, backend);
        self.markers(ui);
        self.measurements(ui);
        self.kept(ui);
    }

    fn dio(&mut self, ui: &mut Ui, chosen: Option<&Unit>, backend: &mut dyn Backend) {
        let face = style::face(ui.ctx());
        ui.add_space(space::L);
        label(ui, "DIO");
        let state = self
            .status
            .as_ref()
            .filter(|s| s.connected)
            .and_then(|s| s.dio.clone());
        let Some(state) = state else {
            let reason = self
                .status
                .as_ref()
                .and_then(|s| s.dio_reason.as_ref().map(ToString::to_string))
                .or_else(|| {
                    chosen.and_then(|unit| {
                        pro::validate(unit, pro::Feature::Dio)
                            .err()
                            .map(|error| error.to_string())
                    })
                })
                .unwrap_or_else(|| {
                    "Select a Pro or standalone MCU-Link running Verge to use DIO.".into()
                });
            ui.add_enabled(false, egui::Label::new(text(Role::Body, reason)));
            return;
        };
        if state.pins == 0 {
            ui.add(
                egui::Label::new(
                    text(
                        Role::Body,
                        "A bench tool has given the DIO pins to its command marker. Run verge \
                         restart to give them back.",
                    )
                    .color(face.ink_2),
                )
                .wrap(),
            );
            return;
        }
        // Pins the marker holds are not DIO pins. On the standalone MCU-Link each is a group.
        for group in state
            .groups
            .iter()
            .filter(|&&group| group & state.pins != 0)
        {
            let name = group_name(*group);
            ui.horizontal(|ui| {
                ui.label(text(Role::Code, &name));
                for output in [false, true] {
                    let selected = (state.outputs & group != 0) == output;
                    let words = format!("{name} {}", if output { "output" } else { "input" });
                    let response = choice(ui, if output { "Output" } else { "Input" }, selected);
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), &words)
                    });
                    if response.clicked() && !selected {
                        backend.send(Request::Bench(Command::Dio {
                            change: DioChange {
                                mask: *group,
                                outputs: if output { *group } else { 0 },
                                levels: state.levels,
                            },
                            confirmed: false,
                        }));
                    }
                }
            });
        }
        egui::Grid::new("dio-levels")
            .num_columns(4)
            .spacing([space::S, space::XS])
            .show(ui, |ui| {
                for pin in 0..4 {
                    let bit = 1 << pin;
                    if state.pins & bit == 0 {
                        continue;
                    }
                    let output = state.outputs & bit != 0;
                    ui.label(text(Role::Code, format!("DIO{pin}")));
                    ui.label(text(Role::Code, if output { "out" } else { "in" }).color(face.ink_2));
                    let high = state.read & bit != 0;
                    ui.label(
                        text(Role::Figures, if high { "high" } else { "low" }).color(if high {
                            face.ink
                        } else {
                            face.ink_2
                        }),
                    );
                    if output
                        && ui
                            .small_button(text(
                                Role::Label,
                                if state.levels & bit == 0 {
                                    "Set high"
                                } else {
                                    "Set low"
                                },
                            ))
                            .clicked()
                    {
                        backend.send(Request::Bench(Command::Dio {
                            change: DioChange {
                                mask: bit,
                                outputs: bit,
                                levels: state.levels ^ bit,
                            },
                            confirmed: false,
                        }));
                    }
                    ui.end_row();
                }
            });
        let place = match chosen.and_then(Unit::verge).and_then(|ours| ours.board) {
            Some(BoardKind::Plain) => {
                "DIO0 to DIO3 are J8's pads, left to right with the USB connector on the left. \
                 They work at 3.3 V only. "
            }
            Some(BoardKind::Pro) => "DIO0 to DIO3 are J19 pins 2 to 5. ",
            _ => "",
        };
        ui.add(
            egui::Label::new(
                text(
                    Role::Body,
                    format!("{place}Inputs have pull-downs. Outputs read their drive."),
                )
                .color(face.ink_2),
            )
            .wrap(),
        );
    }

    fn markers(&mut self, ui: &mut Ui) {
        let (Some(answer), Some(channels)) = (self.answer_now(), self.channels.as_ref()) else {
            return;
        };
        let Some(page) = answer.markers.list.as_ref() else {
            return;
        };
        let losses = self.marker_losses();
        if page.total == 0 && losses == 0 {
            return;
        }
        let face = style::face(ui.ctx());
        ui.add_space(space::L);
        label(ui, "Markers");
        ui.label(text(Role::Figures, format!("{} received", page.total)).color(face.ink));
        if losses > 0 {
            ui.label(
                text(
                    Role::Body,
                    format!(
                        "{losses} marker-loss {}. Some markers are missing.",
                        if losses == 1 { "report" } else { "reports" }
                    ),
                )
                .color(face.attention_ink),
            );
        }
        let mut picked = None;
        ScrollArea::vertical()
            .id_salt("marker-list")
            .max_height(150.0)
            .show(ui, |ui| {
                for marker in &page.entries {
                    let a = &marker.annotation;
                    let number = a.values.as_ref().and_then(|v| v.get("number"));
                    let words = [
                        Some(format!("{:.6}s", channels.seconds(marker.cursor.tick))),
                        number.cloned(),
                        a.label.clone(),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                    let response = ui.add(
                        egui::Button::new(text(Role::Code, words))
                            .selected(self.marker_selected == Some(marker.cursor))
                            .wrap(),
                    );
                    if response.clicked() {
                        picked = Some(marker.cursor);
                    }
                    if a.kind.as_deref() == Some("marker-after-loss") {
                        ui.label(
                            text(Role::Body, "Markers lost before this one")
                                .color(face.attention_ink),
                        );
                    }
                }
            });
        if let Some(cursor) = picked {
            self.marker_selected = Some(cursor);
            let width = self.window_width().max(1);
            let start = cursor.tick - width / 2;
            self.state.window = Window::At(start..start + width);
        }
        let next = page.next;
        ui.horizontal(|ui| {
            if outline(ui, "First", self.marker_after.is_some()).clicked() {
                self.marker_after = None;
            }
            if outline(ui, "Next", next.is_some()).clicked() {
                self.marker_after = next;
            }
        });
        let successor = answer
            .markers
            .successor
            .as_ref()
            .filter(|(of, _)| Some(*of) == self.marker_selected)
            .and_then(|(_, next)| next.as_ref());
        if outline(
            ui,
            "Select to next marker",
            self.marker_selected.is_some() && successor.is_some(),
        )
        .clicked()
            && let (Some(start), Some(end)) = (self.marker_selected, successor)
        {
            self.state.selection = Some(start.tick..end.cursor.tick);
        }
    }

    /// Marker-loss reports: counted by the worker while the recording is written, from the
    /// recording once it is finished.
    fn marker_losses(&self) -> u64 {
        if self.live.is_some() {
            return self.status.as_ref().map_or(0, |s| s.marker_losses);
        }
        self.viewer.as_ref().map_or(0, |viewer| {
            viewer.recording(|r| {
                r.sources()
                    .first()
                    .and_then(|s| s.extra.get("marker_losses"))
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            })
        })
    }

    /// The latest answer, for the panel's text.
    fn answer_now(&self) -> Option<Arc<Answer>> {
        self.viewer.as_ref()?.answer()
    }

    /// The window's width on screen, in ticks.
    fn window_width(&self) -> i64 {
        match &self.shown {
            Some(shown) => shown.window.end - shown.window.start,
            None => span(&self.state.window),
        }
    }

    fn supply(&mut self, ui: &mut Ui, status: &Status, backend: &mut dyn Backend) {
        let face = style::face(ui.ctx());
        ui.add_space(space::L);
        label(ui, "Target supply");
        ui.horizontal(|ui| {
            if outline(ui, if status.supply { "Turn off" } else { "Turn on" }, true).clicked() {
                backend.send(Request::Bench(Command::Supply {
                    on: !status.supply,
                    confirmed: false,
                }));
            }
            ui.label(
                text(Role::Figures, if status.supply { "on" } else { "off" })
                    .color(if status.supply { face.ink } else { face.ink_2 }),
            );
        });
        // Without an energy circuit there is nothing to measure it with.
        if !status.energy {
            return;
        }
        ui.label(
            match status.millivolts {
                None => text(Role::Body, "No reading without a capture"),
                Some(mv) => text(
                    Role::Figures,
                    format!("measured {}", si(f64::from(mv) * 1e-3, "V")),
                ),
            }
            .color(face.ink_2),
        );
    }

    fn acquisition(&mut self, ui: &mut Ui, status: &Status, backend: &mut dyn Backend) {
        let face = style::face(ui.ctx());
        ui.add_space(space::L);
        label(ui, "Acquisition");
        if status.recording {
            ui.label(
                match status.rate {
                    None => text(Role::Body, "Waiting for the stream"),
                    Some(rate) => text(Role::Figures, format!("{rate} pairs/s · recording")),
                }
                .color(face.ink),
            );
            if outline(ui, "Stop recording", true).clicked() {
                backend.send(Request::Bench(Command::Stop));
            }
            return;
        }
        ui.add(
            egui::DragValue::new(&mut self.rate)
                .range(1..=100_000)
                .suffix(" pairs/s"),
        );
        ui.horizontal_wrapped(|ui| {
            for rate in [1_000, 25_000, 65_000, 100_000] {
                if choice(ui, &format!("{}k", rate / 1000), self.rate == rate).clicked() {
                    self.rate = rate;
                }
            }
        });
        if !status.capturing {
            ui.horizontal_wrapped(|ui| {
                for (ranges, words) in [
                    (EnergyRanges::Auto, "Auto range"),
                    (EnergyRanges::High, "High range"),
                ] {
                    if choice(ui, words, self.ranges == ranges).clicked() {
                        self.ranges = ranges;
                    }
                }
            });
            ui.add(
                egui::Label::new(
                    text(
                        Role::Body,
                        match self.ranges {
                            EnergyRanges::Auto => {
                                "Reads currents down to tens of nA. Each switch to the low range \
                                 dips the reading for about a millisecond."
                            }
                            EnergyRanges::High => {
                                "Reads the target undisturbed, but currents under about 0.65 mA \
                                 read unreliably and are marked under range."
                            }
                        },
                    )
                    .color(face.ink_2),
                )
                .wrap(),
            );
        }
        let words = if status.capturing {
            "Record running capture"
        } else {
            "Start recording"
        };
        if outline(ui, words, true).clicked() {
            if status.capturing {
                self.start(backend);
            } else {
                self.start_confirm = true;
            }
        }
        if status.capturing {
            ui.add(
                egui::Label::new(
                    text(
                        Role::Body,
                        "A capture is running: recording it keeps its rate.",
                    )
                    .color(face.ink_2),
                )
                .wrap(),
            );
        }
    }

    /// The visible window's readings and the selection's, side by side.
    fn measurements(&mut self, ui: &mut Ui) {
        let (Some(answer), Some(channels)) = (self.answer_now(), self.channels.as_ref()) else {
            return;
        };
        let face = style::face(ui.ctx());
        ui.add_space(space::L);
        label(ui, "Measurements");
        if let Some(selection) = self.state.selection.clone() {
            let mut changed = None;
            ui.horizontal(|ui| {
                let mut start = channels.seconds(selection.start);
                let mut end = channels.seconds(selection.end);
                let from = ui.add(
                    egui::DragValue::new(&mut start)
                        .speed(0.001)
                        .max_decimals(6)
                        .suffix(" s"),
                );
                ui.label(text(Role::Body, "to").color(face.ink_2));
                let to = ui.add(
                    egui::DragValue::new(&mut end)
                        .speed(0.001)
                        .max_decimals(6)
                        .suffix(" s"),
                );
                if from.changed() || to.changed() {
                    let (a, b) = (channels.ticks(start), channels.ticks(end));
                    changed = Some(a.min(b)..a.max(b));
                }
            });
            if let Some(selection) = changed {
                self.state.selection = Some(selection);
            }
            ui.horizontal(|ui| {
                if outline(ui, "Zoom to selection", true).clicked() {
                    self.state.window = Window::At(selection.clone());
                }
                if outline(ui, "Clear", true).clicked() {
                    self.state.selection = None;
                }
            });
        }
        let window = answer.statistics.first();
        // The answer's selection is the one asked; one moved since waits for its own.
        let selection = self.state.selection.as_ref().map(|selection| {
            answer
                .statistics
                .get(1)
                .filter(|statistics| statistics.interval == *selection)
        });
        let extent = answer.extent.clone().unwrap_or(0..0);
        egui::Grid::new("bench-measurements")
            .num_columns(3)
            .spacing(vec2(space::M, space::XS))
            .min_col_width(60.0)
            .show(ui, |ui| {
                ui.label("");
                label(ui, "Window");
                if selection.is_some() {
                    label(ui, "Selection");
                }
                ui.end_row();
                let columns: Vec<Option<&Statistics>> =
                    std::iter::once(window).chain(selection).collect();
                for (name, value) in READINGS {
                    label(ui, &sentence_case(name));
                    for column in &columns {
                        let words = column.map_or_else(
                            || "…".to_owned(),
                            |statistics| value(&Reading::new(statistics, channels)),
                        );
                        ui.label(text(Role::Figures, words).color(face.ink));
                    }
                    ui.end_row();
                }
                label(ui, "Unmeasured");
                for column in &columns {
                    let missing = column.and_then(|statistics| {
                        let current = Reading::new(statistics, channels).current?;
                        let ticks = current.unmeasured(&statistics.interval, &extent);
                        Some(ticks.as_f64() / channels.rate)
                    });
                    ui.label(match missing {
                        Some(s) if s <= 0.0 => text(Role::Figures, "none").color(face.ink_2),
                        Some(s) => text(Role::Figures, si(s, "s")).color(face.attention_ink),
                        None => text(Role::Figures, "…").color(face.ink),
                    });
                }
                ui.end_row();
            });
    }

    /// The open capture's name and what can be done with it, under the graph.
    fn files(&mut self, ui: &mut Ui) {
        let face = style::face(ui.ctx());
        let idle = self.io.is_none() && self.compactions.is_empty();
        let open = self.path.is_some() && self.viewer.is_some();
        // A DIO-only recording has no pairs for the exports' columns.
        let exportable =
            open && !self.recording() && self.channels.as_ref().is_some_and(|c| c.i.is_some());
        ui.horizontal_wrapped(|ui| {
            if let Some(path) = &self.path {
                ui.label(
                    text(
                        Role::Code,
                        path.file_name().unwrap_or_default().to_string_lossy(),
                    )
                    .color(face.ink_2),
                )
                .on_hover_text(path.display().to_string());
                ui.add_space(space::S);
            }
            if outline(ui, "Open…", idle).clicked() {
                self.file(FileAction::Open);
            }
            if outline(ui, "Save as…", idle && open && !self.recording()).clicked() {
                self.file(FileAction::Save);
            }
            ui.add_space(space::S);
            label(
                ui,
                if self.state.selection.is_some() {
                    "Export selection"
                } else {
                    "Export"
                },
            )
            .on_hover_text("Exports keep whole samples at the selection's edges.");
            if outline(ui, "CSV…", idle && exportable).clicked() {
                self.file(FileAction::Export(true));
            }
            if outline(ui, "sigrok…", idle && exportable).clicked() {
                self.file(FileAction::Export(false));
            }
            ui.add_space(space::S);
            if outline(ui, "Discard…", idle && open && !self.recording()).clicked() {
                self.file(FileAction::Discard);
            }
        });
    }

    /// Captures kept in the record directory, which a recording adds to until one is discarded.
    fn kept(&mut self, ui: &mut Ui) {
        if self.recovered.is_empty() {
            return;
        }
        ui.add_space(space::L);
        let name = label(ui, "Kept on disk");
        // A file the file thread is compacting can't take a second reader: Windows refuses the
        // rename while one holds it.
        let idle = self.io.is_none() && self.compactions.is_empty();
        ui.add_enabled_ui(idle, |ui| self.kept_list(ui, name.id));
    }

    fn kept_list(&mut self, ui: &mut Ui, name: egui::Id) {
        egui::ComboBox::from_id_salt("bench-retained")
            .width(ui.available_width())
            .selected_text(text(
                Role::Code,
                format!("{} captures", self.recovered.len()),
            ))
            .show_ui(ui, |ui| {
                for path in self.recovered.clone() {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if ui
                        .selectable_label(self.path.as_ref() == Some(&path), text(Role::Code, name))
                        .clicked()
                    {
                        self.open(path);
                    }
                }
            })
            .response
            .labelled_by(name);
    }

    fn dialogs(&mut self, ui: &mut Ui, backend: &mut dyn Backend) {
        if self.start_confirm {
            egui::Window::new("Start energy capture?")
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.label(text(
                        Role::Body,
                        "Starting clears the probe’s energy total. Calibration can interrupt \
                         target power for about 0.2 seconds.",
                    ));
                    ui.horizontal(|ui| {
                        if outline(ui, "Start capture", true).clicked() {
                            self.start(backend);
                        }
                        if outline(ui, "Cancel", true).clicked() {
                            self.start_confirm = false;
                        }
                    });
                });
        }
        if let Some(status) = self.status.as_ref()
            && let Some(change) = status.confirm_dio
        {
            let group = group_name(change.mask);
            let pins = if change.mask.count_ones() == 1 {
                "this pin"
            } else {
                "these pins"
            };
            let voltage = if status.board == Some(BoardKind::Plain) {
                " J8 drives 3.3 V, which can damage a target that runs at a lower voltage."
            } else {
                ""
            };
            let warning = format!(
                "Check that the target is not driving {pins}. The probe cannot detect \
                 contention.{voltage}"
            );
            egui::Window::new(format!("Drive {group}?"))
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.label(text(Role::Body, warning));
                    ui.horizontal(|ui| {
                        if outline(ui, "Drive pins", true).clicked() {
                            backend.send(Request::Bench(Command::Dio {
                                change,
                                confirmed: true,
                            }));
                        }
                        if outline(ui, "Keep as inputs", true).clicked() {
                            backend.send(Request::Bench(Command::CancelDio));
                        }
                    });
                });
        }
        if self.off_confirm {
            egui::Window::new("Turn off target supply?")
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.label(text(
                        Role::Body,
                        "A capture is running. Turning off the supply removes target power.",
                    ));
                    ui.horizontal(|ui| {
                        if outline(ui, "Turn off supply", true).clicked() {
                            backend.send(Request::Bench(Command::Supply {
                                on: false,
                                confirmed: true,
                            }));
                            self.off_confirm = false;
                        }
                        if outline(ui, "Keep supply on", true).clicked() {
                            self.off_confirm = false;
                            backend.send(Request::Bench(Command::CancelSupply));
                        }
                    });
                });
        }
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let rate = self.channels.as_ref().map(|c| c.rate);
        ui.horizontal_wrapped(|ui| {
            for quantity in [Quantity::Current, Quantity::Voltage, Quantity::Power] {
                if choice(ui, quantity.word(), self.quantity == quantity).clicked()
                    && self.quantity != quantity
                {
                    self.quantity = quantity;
                    self.state.y = Y::Fit;
                }
            }
            ui.add_space(space::M);
            for seconds in [1, 10, 60] {
                let Some(rate) = rate else { break };
                let width = (seconds as f64 * rate).round() as i64;
                let on = span(&self.state.window) == width;
                if choice(ui, &format!("{seconds}s"), on).clicked() {
                    self.state.window = match &self.state.window {
                        Window::Follow { .. } => Window::Follow { span: width },
                        // A window the user moved keeps its end.
                        Window::At(at) => Window::At(at.end - width..at.end),
                    };
                }
            }
            ui.add_space(space::M);
            if choice(ui, "Select", self.state.select_mode).clicked() {
                self.state.select_mode = !self.state.select_mode;
            }
            let fitted = matches!(self.state.y, Y::Fit);
            if choice(ui, "Fit Y", fitted).clicked() {
                self.state.y = match (fitted, &self.shown) {
                    // Off the fit where it stands: the range drawn last frame.
                    (true, Some(shown)) => Y::Fixed {
                        low: shown.y.0,
                        high: shown.y.1,
                    },
                    (true, None) => Y::Fit,
                    (false, _) => Y::Fit,
                };
            }
        });
        ui.add_space(space::S);
    }

    fn graph(&mut self, ui: &mut Ui) {
        let face = style::face(ui.ctx());
        self.toolbar(ui);
        let (Some(viewer), Some(channels)) = (self.viewer.clone(), self.channels.as_ref()) else {
            let size = vec2(
                ui.available_width(),
                (ui.available_height() - 48.0).max(160.0),
            );
            Frame::new()
                .fill(face.surface)
                .stroke(Stroke::new(1.0, face.hair))
                .corner_radius(CornerRadius::same(style::RADIUS))
                .show(ui, |ui| {
                    ui.set_min_size(size);
                    ui.set_max_size(size);
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            text(
                                Role::Body,
                                if self.path.is_some() {
                                    "Loading the capture…"
                                } else if self
                                    .status
                                    .as_ref()
                                    .is_some_and(|s| s.connected && !s.energy)
                                {
                                    "Open a capture."
                                } else {
                                    "Start a recording, or open a capture."
                                },
                            )
                            .color(face.ink_2),
                        );
                    });
                });
            ui.add_space(space::S);
            self.files(ui);
            return;
        };
        let palette = palette(face, ui);
        let trace = channels.trace(self.quantity);
        let lanes = channels.lanes.clone();
        let flag = channels.under;
        let high_range_only = channels.high_range_only;
        let i = channels.i;
        let now = self.now();
        let mut extras = Extras::default();
        extras.marker_list = Some((self.marker_after, MARKER_PAGE));
        extras.successor_of = self.marker_selected;
        let top = ui.cursor().top();
        let strip = if trace.is_some() {
            STRIP + space::S
        } else {
            0.0
        };
        let height = (ui.available_height() - self.footer - strip).max(160.0);
        let graph = match trace {
            Some(trace) => Graph::new(&viewer, &mut self.state, &palette, trace)
                .flag(flag)
                .include_zero(true),
            None => Graph::lanes_only(&viewer, &mut self.state, &palette),
        }
        .lanes(&lanes)
        .extras(extras)
        .now(now)
        .motion(crate::mark::motion(ui.ctx()))
        .height(height);
        let response = ui.add(graph);
        self.shown = Graph::shown(&response);
        if trace.is_some() {
            ui.add_space(space::S);
            ui.add(
                Strip::new(&viewer, &mut self.state, &palette)
                    .include_zero(true)
                    .now(now)
                    .height(STRIP),
            );
        }
        if high_range_only {
            ui.label(
                text(
                    Role::Body,
                    "This capture held the high range: currents under about 0.65 mA read unreliably",
                )
                .color(face.ink_2),
            );
        }
        let (lost, dio_losses) = self.losses(&viewer, i, &lanes);
        if lost > 0 {
            ui.label(
                text(Role::Body, format!("{lost} pairs lost in this capture"))
                    .color(face.attention_ink),
            );
        }
        if dio_losses > 0 {
            ui.label(
                text(
                    Role::Body,
                    format!(
                        "{dio_losses} DIO edge-loss {} in this capture",
                        if dio_losses == 1 { "report" } else { "reports" }
                    ),
                )
                .color(face.attention_ink),
            );
        }
        ui.add_space(space::S);
        self.files(ui);
        // What follows the graph, measured each frame so the graph leaves room for it.
        let footer = ui.cursor().top() - top - height - strip + space::S;
        if (footer - self.footer).abs() > 0.5 {
            self.footer = footer;
            ui.ctx().request_repaint();
        }
    }

    /// Pairs lost, and DIO edge-loss reports: counted by the worker while the recording is
    /// written, from the recording once it is finished.
    fn losses(&self, viewer: &Viewer, i: Option<usize>, lanes: &[usize]) -> (u64, u64) {
        let lost = i.map_or(0, |i| viewer.recording(|r| r.lost(i)));
        let dio = match (&self.live, lanes.first()) {
            (Some(_), _) => self.status.as_ref().map_or(0, |s| s.edge_losses),
            (None, Some(&lane)) => viewer.recording(|r| r.gaps(lane)) as u64,
            (None, None) => 0,
        };
        (lost, dio)
    }

    fn file(&mut self, action: FileAction) {
        if self.io.is_some() {
            return;
        }
        let path = self.path.clone();
        let interval = self.state.selection.clone();
        let (send, receive) = mpsc::channel();
        self.io = Some(receive);
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<FileResult> {
                if matches!(action, FileAction::Open) {
                    let selected = rfd::FileDialog::new()
                        .add_filter("tickwave recording", &["tickwave"])
                        .pick_file();
                    return Ok(selected.map_or(FileResult::Cancelled, FileResult::Open));
                }
                let path = path.ok_or_else(|| anyhow::anyhow!("no capture is open"))?;
                if matches!(action, FileAction::Discard) {
                    let agree = rfd::MessageDialog::new()
                        .set_title("Discard capture?")
                        .set_description("This permanently deletes the opened capture from disk.")
                        .set_buttons(rfd::MessageButtons::OkCancel)
                        .show();
                    if agree == rfd::MessageDialogResult::Ok {
                        std::fs::remove_file(&path)?;
                        return Ok(FileResult::Discarded(path));
                    }
                    return Ok(FileResult::Cancelled);
                }
                let (extension, title) = match action {
                    FileAction::Export(true) => ("csv", "Export CSV"),
                    FileAction::Export(false) => ("sr", "Export sigrok"),
                    _ => ("tickwave", "Save capture"),
                };
                let Some(destination) = rfd::FileDialog::new()
                    .set_title(title)
                    .add_filter(title, &[extension])
                    .save_file()
                else {
                    return Ok(FileResult::Cancelled);
                };
                match action {
                    FileAction::Save => {
                        std::fs::copy(&path, &destination)?;
                    }
                    FileAction::Export(csv) => export(&path, &destination, csv, interval)?,
                    _ => {}
                }
                Ok(FileResult::Done(format!("Saved {}", destination.display())))
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = send.send(result);
        });
    }
}

/// A view that follows the recording's end, `span` ticks wide.
fn following(span: i64) -> ViewState {
    let mut state = ViewState::default();
    state.window = Window::Follow { span };
    state
}

/// The ticks a window spans.
fn span(window: &Window) -> i64 {
    match window {
        Window::Follow { span } => *span,
        Window::At(at) => at.end - at.start,
    }
}

/// Writes a compacted copy of the recording at `path` beside it while `viewer` reads the
/// original, then lets the view go of the file for the rename and opens it again.
fn compact_beside(path: &Path, viewer: Option<&Viewer>) -> anyhow::Result<()> {
    compact_then(path, viewer, || {})
}

/// [`compact_beside`], calling `at_rename` just before the rename, as a test checks that nothing
/// holds the file there.
fn compact_then(
    path: &Path,
    viewer: Option<&Viewer>,
    at_rename: impl FnOnce(),
) -> anyhow::Result<()> {
    let copy = recordings::compacted(path)?;
    if let Some(viewer) = viewer {
        viewer.release();
    }
    at_rename();
    let persisted = copy.persist();
    // The original where the rename failed: the view reads one or the other.
    if let Some(viewer) = viewer {
        viewer.reopen()?;
    }
    Ok(persisted?)
}

/// Exports the pairs over `interval`, the whole recording without one, to a CSV or a sigrok
/// session at `destination`.
pub(crate) fn export(
    path: &Path,
    destination: &Path,
    csv: bool,
    interval: Option<Range<i64>>,
) -> anyhow::Result<()> {
    let bytes: Arc<dyn escapement_tickwave_egui::view::Bytes> =
        Arc::new(std::fs::File::open(path)?);
    let recording = Recording::open(bytes, escapement_tickwave_egui::view::Cache::new(64 << 20))?;
    let interval = interval.or_else(|| recording.extent()).unwrap_or(0..0);
    recordings::export(
        &recording,
        destination,
        csv,
        interval,
        &AtomicBool::new(false),
    )?;
    Ok(())
}

/// tickwave's palette in the window's face: Verge's trace colour, the face's rules and inks, and
/// the figures in the window's figures face.
fn palette(face: &style::Face, ui: &Ui) -> Palette {
    let mut p = Palette::from_visuals(ui.visuals());
    p.trace = Some(face.series_verge);
    p.surface = face.surface;
    p.hair = face.hair;
    p.ink = face.ink;
    p.ink_2 = face.ink_2;
    p.attention_fill = face.attention_fill;
    p.attention_ink = face.attention_ink;
    p.wash = face.wash();
    p.figures = Role::Figures.font();
    p.labels = Role::Code.font();
    p
}

/// The left panel's width, the Units view's.
const SIDE: f32 = 300.0;

/// `value` in `unit`, with the SI prefix that leaves one to three digits before the point.
fn si(value: f64, unit: &str) -> String {
    const PREFIXES: [(f64, &str); 6] = [
        (1e3, "k"),
        (1.0, ""),
        (1e-3, "m"),
        (1e-6, "µ"),
        (1e-9, "n"),
        (1e-12, "p"),
    ];
    let size = value.abs();
    let (scale, prefix) = PREFIXES
        .into_iter()
        .find(|(scale, _)| size >= *scale)
        .unwrap_or(PREFIXES[PREFIXES.len() - 1]);
    if size == 0.0 {
        return format!("0 {unit}");
    }
    let scaled = value / scale;
    let decimals = if scaled.abs() >= 100.0 {
        1
    } else if scaled.abs() >= 10.0 {
        2
    } else {
        3
    };
    format!("{scaled:.decimals$} {prefix}{unit}")
}

/// One column of readings: an interval's statistics, by the channels the readings take.
struct Reading<'a> {
    interval: &'a Range<i64>,
    rate: f64,
    timebase: escapement_tickwave_egui::view::format::model::Ratio,
    current: Option<&'a ChannelStatistics>,
    voltage: Option<&'a ChannelStatistics>,
    power: Option<&'a ChannelStatistics>,
}
impl<'a> Reading<'a> {
    fn new(statistics: &'a Statistics, channels: &Channels) -> Self {
        let of = |c: Option<usize>| c.and_then(|c| statistics.channels.get(c)?.as_ref());
        Self {
            interval: &statistics.interval,
            rate: channels.rate,
            timebase: channels.timebase,
            current: of(channels.i),
            voltage: of(channels.v),
            power: of(channels.p),
        }
    }
}

/// A reading's figure, from one column of statistics.
type Value = fn(&Reading) -> String;

/// The measurement rows: a name, and the value from one column's statistics.
const READINGS: [(&str, Value); 9] = [
    ("duration", |r| {
        si((r.interval.end - r.interval.start) as f64 / r.rate, "s")
    }),
    ("pairs", |r| {
        r.current.map_or("…".into(), |c| c.samples.to_string())
    }),
    ("min", |r| {
        r.current
            .and_then(|c| c.min)
            .map_or("…".into(), |a| si(a, "A"))
    }),
    ("mean", |r| {
        r.current
            .and_then(ChannelStatistics::mean)
            .map_or("…".into(), |a| si(a, "A"))
    }),
    ("peak", |r| {
        r.current
            .and_then(|c| c.max)
            .map_or("…".into(), |a| si(a, "A"))
    }),
    ("voltage", |r| {
        r.voltage
            .and_then(ChannelStatistics::mean)
            .map_or("…".into(), |v| si(v, "V"))
    }),
    ("power", |r| {
        r.power
            .and_then(ChannelStatistics::mean)
            .map_or("…".into(), |w| si(w, "W"))
    }),
    ("charge", |r| {
        r.current
            .map_or("…".into(), |c| si(c.integral(r.timebase), "C"))
    }),
    ("energy", |r| {
        r.power
            .map_or("…".into(), |p| si(p.integral(r.timebase), "J"))
    }),
];

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use escapement_verge_probe::stream::{
        DioHeader, Edge, Edges, End, EndReason, Header, Marker, Markers, Message, Pair, Pairs,
    };
    use escapement_verge_probe::tickwave::{Context, FileWriter};

    /// `count` pairs at `rate`, each pair's current code from `code` (65535 is 350 mA), DIO3 high
    /// while the code is over 1000 where `dio`, and `markers` after the pairs, the first message
    /// of them reporting a loss.
    fn messages(
        rate: u32,
        count: u32,
        code: impl Fn(u32) -> u16,
        dio: bool,
        markers: &[Marker],
    ) -> Vec<Message> {
        let header: Header = serde_json::from_value(serde_json::json!({"clock_hz":rate,"period":1,"setting_ma":350,"full_scale_mv":3300,"first_sequence":0,"previous_low":true,
            "low":[{"code":0,"current_nanoamps":0},{"code":65535,"current_nanoamps":350000000}],"high":[],"dio_mask":if dio {15} else {0},"dio_levels":if code(0)>1000 {8} else {0},"edge_resolution_ns":11})).unwrap();
        let period = 1_000_000_000 / u64::from(rate);
        let mut out = vec![Message::Header(header)];
        let mut levels = if code(0) > 1000 { 8 } else { 0 };
        for first in (0..count).step_by(99) {
            let range = first..(first + 99).min(count);
            let pairs = range
                .clone()
                .map(|i| Pair {
                    voltage_code: 32768,
                    current_code: code(i),
                    low: true,
                    under_range: false,
                })
                .collect();
            out.push(Message::Pairs(Pairs::new(
                0,
                first,
                u64::from(first) * period,
                pairs,
            )));
            if dio {
                let mut edges = Vec::new();
                for i in range {
                    let now = if code(i) > 1000 { 8 } else { 0 };
                    if now != levels {
                        edges.push(Edge {
                            time_ns: u64::from(i) * period,
                            levels: now,
                        });
                        levels = now;
                    }
                }
                if !edges.is_empty() {
                    out.push(Message::Edges(Edges::new(0, edges)));
                }
            }
        }
        for (k, chunk) in markers.chunks(11).enumerate() {
            let flags = if k == 0 { Markers::LOST } else { 0 };
            out.push(Message::Markers(Markers::new(flags, chunk.to_vec())));
        }
        out.push(Message::End(End {
            reason: EndReason::Stopped,
            last_sequence: count - 1,
            time_ns: None,
        }));
        out
    }
    fn write(path: &Path, messages: &[Message]) {
        let mut writer = FileWriter::create(path, Context::default()).unwrap();
        for message in messages {
            writer.ingest(message).unwrap();
        }
        writer.finish().unwrap();
    }
    /// 20 s at 1,000 pairs a second, with a 30 ms pulse each second.
    fn pulses(i: u32) -> u16 {
        if i % 1000 < 30 { 3000 } else { 350 }
    }
    fn harness(bench: Bench) -> Harness<'static, Bench> {
        Harness::builder()
            .with_size(egui::vec2(1080.0, 700.0))
            .with_step_dt(1.0 / 60.0)
            .with_context_setup(crate::style::install)
            .build_ui_state(
                |ui, b: &mut Bench| {
                    b.show(ui, &[], &mut None, &mut crate::worker::Manual::default())
                },
                bench,
            )
    }
    /// The panel with a picked simulated Pro, as in use: its statuses stand from frame to frame.
    type Rig = (Bench, Vec<Unit>, Option<String>, crate::worker::Manual);
    fn rig(mut bench: Bench) -> (Harness<'static, Rig>, String) {
        let unit = escapement_verge_probe::detect(
            &escapement_verge_probe::sim::SimBus::scenario("ours-pro-energy").unwrap(),
        )
        .unwrap()
        .remove(0);
        let instance = unit.device.instance.clone();
        bench.connection = Some(instance.clone());
        let h = Harness::builder()
            .with_size(egui::vec2(1080.0, 700.0))
            .with_step_dt(1.0 / 60.0)
            .with_context_setup(crate::style::install)
            .build_ui_state(
                |ui, (bench, units, selected, backend): &mut Rig| {
                    bench.show(ui, units, selected, backend);
                },
                (
                    bench,
                    vec![unit],
                    Some(instance.clone()),
                    crate::worker::Manual::default(),
                ),
            );
        (h, instance)
    }
    /// What a harness holds the panel in.
    trait Panel {
        fn bench(&self) -> &Bench;
    }
    impl Panel for Bench {
        fn bench(&self) -> &Bench {
            self
        }
    }
    impl Panel for Rig {
        fn bench(&self) -> &Bench {
            &self.0
        }
    }
    /// Steps until `done`, failing after 2,000 frames.
    fn until<S: Panel>(h: &mut Harness<'_, S>, what: &str, done: impl Fn(&Bench) -> bool) {
        for _ in 0..2000 {
            h.step();
            if done(h.state().bench()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let b = h.state().bench();
        panic!(
            "{what}: notice {:?}, io {}, compactions {}, answer {}, busy {:?}, failure {:?}",
            b.notice,
            b.io.is_some(),
            b.compactions.len(),
            b.answer_now().is_some(),
            b.viewer.as_ref().map(|v| v.busy()),
            b.viewer.as_ref().and_then(|v| v.failure())
        );
    }
    /// Steps until the view has answered and asks for nothing more.
    fn settle<S: Panel>(h: &mut Harness<'_, S>) {
        until(h, "an answer", |b| {
            b.io.is_none()
                && b.viewer
                    .as_ref()
                    .is_some_and(|v| v.answer().is_some() && !v.busy())
        });
        h.step();
    }
    fn opened(dir: &Path, messages: &[Message]) -> Harness<'static, Bench> {
        let path = dir.join("captures").join("demo.tickwave");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write(&path, messages);
        let mut bench = Bench::new(dir);
        bench.open(path);
        let mut h = harness(bench);
        settle(&mut h);
        h
    }
    fn click(h: &mut Harness<'_, Bench>, label: &str) {
        h.get_by_label(label).click();
        h.step();
    }
    fn seconds(h: &Harness<'_, Bench>, s: f64) -> i64 {
        h.state().channels.as_ref().unwrap().ticks(s)
    }

    #[test]
    fn a_capture_opens_on_its_last_ten_seconds_with_their_readings() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = opened(dir.path(), &messages(1000, 20_000, pulses, false, &[]));
        let answer = h.state().answer().unwrap();
        let ten = seconds(&h, 10.0);
        let end = answer.extent.as_ref().unwrap().end;
        assert_eq!(answer.window, end - ten..end);
        let i = h.state().channels.as_ref().unwrap().i.unwrap();
        let window = answer.statistics[0].channels[i].as_ref().unwrap();
        assert_eq!(window.samples, 10_000);
        h.step();
        assert!(h.query_by_label("10000").is_some(), "the pairs reading");
        // The figures the readings take from: amps, and a charge that is their integral.
        let mean = window.mean().unwrap();
        assert!(mean > 0.0018 && mean < 0.0025, "{mean}");
        assert!(
            h.query_by_label(&si(
                window.integral(h.state().channels.as_ref().unwrap().timebase),
                "C"
            ))
            .is_some()
        );
    }

    #[test]
    fn a_selection_reads_its_own_statistics_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = opened(dir.path(), &messages(1000, 20_000, pulses, false, &[]));
        let selection = seconds(&h, 12.0)..seconds(&h, 14.0);
        h.state_mut().select(selection.clone());
        until(&mut h, "the selection's answer", |b| {
            b.selection_statistics().is_some()
        });
        let i = h.state().channels.as_ref().unwrap().i.unwrap();
        let statistics = h.state().selection_statistics().unwrap();
        assert_eq!(statistics.channels[i].as_ref().unwrap().samples, 2000);
        h.step();
        assert!(h.query_by_label("Selection").is_some());
        click(&mut h, "Zoom to selection");
        assert_eq!(h.state().state.window, Window::At(selection));
        click(&mut h, "Clear");
        assert_eq!(h.state().state.selection, None);
    }

    #[test]
    fn fit_y_goes_off_at_the_range_drawn_and_back_on() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = opened(dir.path(), &messages(1000, 20_000, pulses, false, &[]));
        let drawn = h.state().shown.as_ref().unwrap().y;
        click(&mut h, "Fit Y");
        assert_eq!(
            h.state().state.y,
            Y::Fixed {
                low: drawn.0,
                high: drawn.1
            }
        );
        click(&mut h, "Fit Y");
        assert_eq!(h.state().state.y, Y::Fit);
    }

    #[test]
    fn a_span_preset_follows_at_its_width_or_keeps_a_moved_windows_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = opened(dir.path(), &messages(1000, 20_000, pulses, false, &[]));
        click(&mut h, "1s");
        assert_eq!(
            h.state().state.window,
            Window::Follow {
                span: seconds(&h, 1.0)
            }
        );
        let moved = seconds(&h, 5.0)..seconds(&h, 15.0);
        h.state_mut().state.window = Window::At(moved);
        h.step();
        click(&mut h, "1s");
        assert_eq!(
            h.state().state.window,
            Window::At(seconds(&h, 14.0)..seconds(&h, 15.0))
        );
    }

    #[test]
    fn each_quantity_draws_its_own_channel() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = opened(dir.path(), &messages(1000, 20_000, pulses, false, &[]));
        // Up to 16 mA of current, against half of 3.3 V.
        assert!(h.state().shown.as_ref().unwrap().y.1 < 0.1);
        h.get_by_role_and_label(egui::Role::Button, "Voltage")
            .click();
        h.step();
        settle(&mut h);
        let y = h.state().shown.as_ref().unwrap().y;
        assert!(y.0 <= 1.65 && y.1 >= 1.65, "{y:?}");
    }

    #[test]
    fn the_markers_list_centres_one_and_selects_to_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let markers: Vec<Marker> = (0..3)
            .map(|n| Marker {
                time_ns: 2_000_000_000 * (u64::from(n) + 1),
                number: 40 + n,
                label: format!("step {n}").into_bytes(),
            })
            .collect();
        let mut h = opened(dir.path(), &messages(1000, 20_000, pulses, false, &markers));
        h.step();
        assert!(h.query_by_label("3 received").is_some());
        assert!(
            h.query_by_label("1 marker-loss report. Some markers are missing.")
                .is_some()
        );
        click(&mut h, "2.000000s · 40 · step 0");
        let at = seconds(&h, 2.0);
        match &h.state().state.window {
            Window::At(w) => assert_eq!((w.start + w.end) / 2, at, "{w:?}"),
            other => panic!("{other:?}"),
        }
        until(&mut h, "the successor", |b| {
            b.answer_now()
                .is_some_and(|a| a.markers.successor.is_some())
        });
        click(&mut h, "Select to next marker");
        assert_eq!(h.state().state.selection, Some(at..seconds(&h, 4.0)));
    }

    #[test]
    fn a_live_recording_is_followed_then_shown_finished_and_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("captures").join("live.tickwave");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let all = messages(1000, 20_000, pulses, false, &[]);
        let (head, tail) = all.split_at(all.len() / 2);
        let mut writer = FileWriter::create(&path, Context::default()).unwrap();
        writer.ingest(&head[0]).unwrap();
        let mut bench = Bench::new(dir.path());
        bench.connection = Some("unit".into());
        let mut status = Status {
            instance: "unit".into(),
            connected: true,
            recording: true,
            path: Some(path.clone()),
            snapshots: Some(writer.snapshots()),
            ..Status::default()
        };
        bench.receive(status.clone());
        let mut h = harness(bench);
        // The panel takes the header's snapshot; the stream's pairs follow, a frame apart.
        h.step();
        for message in &head[1..] {
            writer.ingest(message).unwrap();
            h.step();
        }
        until(&mut h, "the first half", |b| {
            b.answer_now().is_some_and(|a| a.extent.is_some())
                && b.now().is_some()
                && b.viewer.as_ref().is_some_and(|v| !v.busy())
        });
        let half = h.state().answer().unwrap().extent.as_ref().unwrap().end;
        for message in &tail[..tail.len() - 1] {
            writer.ingest(message).unwrap();
            h.step();
        }
        until(&mut h, "the newer data", |b| {
            b.answer_now()
                .and_then(|a| a.extent.clone())
                .is_some_and(|e| e.end > half)
        });
        writer.ingest(tail.last().unwrap()).unwrap();
        assert!(writer.close().unwrap());
        status.recording = false;
        status.snapshots = None;
        // A unit no one picked is disconnected each frame; this one stands for a picked one.
        h.state_mut().connection = Some("unit".into());
        h.state_mut().receive(status);
        h.step();
        assert!(h.state().now().is_none(), "no now once finished");
        until(&mut h, "the compaction", |b| {
            b.io.is_none() && b.compactions.is_empty()
        });
        settle(&mut h);
        assert!(recordings::finished(&path).unwrap());
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        // Compacted: compacting it again changes nothing.
        let again = dir.path().join("again.tickwave");
        std::fs::copy(&path, &again).unwrap();
        recordings::compact(&again).unwrap();
        assert_eq!(
            std::fs::read(&again).unwrap(),
            std::fs::read(&path).unwrap()
        );
        let i = h.state().channels.as_ref().unwrap().i.unwrap();
        let answer = h.state().answer().unwrap();
        assert_eq!(
            answer.statistics[0].channels[i].as_ref().unwrap().samples,
            10_000
        );
        assert_eq!(h.state().notice, None);
    }

    #[test]
    fn a_capture_a_crash_cut_off_is_repaired_then_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("captures").join("cut.tickwave");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write(&path, &messages(1000, 20_000, pulses, false, &[]));
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() * 2 / 3]).unwrap();
        let mut bench = Bench::new(dir.path());
        bench.open(path.clone());
        let mut h = harness(bench);
        h.step();
        settle(&mut h);
        assert!(recordings::finished(&path).unwrap());
        assert_eq!(h.state().notice, None);
        assert!(h.state().recovered.contains(&path), "listed as kept");
    }

    #[test]
    fn a_dio_only_recording_draws_its_lanes_and_offers_no_export() {
        let dir = tempfile::tempdir().unwrap();
        let clock = 150_000_000u64;
        let header: DioHeader = serde_json::from_value(serde_json::json!({
            "clock_hz": clock, "dio_mask": 15, "dio_levels": 0, "edge_resolution_ns": 7
        }))
        .unwrap();
        let mut stream = vec![Message::DioHeader(header)];
        for k in 1..=40u64 {
            stream.push(Message::Edges(Edges::new(
                0,
                vec![Edge {
                    time_ns: k * 50_000_000,
                    levels: if k % 2 == 1 { 8 } else { 0 },
                }],
            )));
        }
        stream.push(Message::End(End {
            reason: EndReason::Stopped,
            last_sequence: 0,
            time_ns: Some(2_100_000_000),
        }));
        let mut h = opened(dir.path(), &stream);
        let channels = h.state().channels.as_ref().unwrap();
        assert_eq!((channels.i, channels.lanes.len()), (None, 4));
        h.step();
        let csv = h.get_by_label("CSV…");
        assert!(csv.accesskit_node().is_disabled());
    }
    /// A status from the picked unit while it records to `path`, or once it has stopped.
    fn recording_status(instance: &str, path: &Path, slot: Option<Snapshots>) -> Status {
        Status {
            instance: instance.into(),
            connected: true,
            target_power: true,
            energy: true,
            board: Some(BoardKind::Pro),
            recording: slot.is_some(),
            path: Some(path.to_owned()),
            snapshots: slot,
            ..Status::default()
        }
    }
    /// Compacting the file again changes nothing.
    fn assert_compacted(path: &Path) {
        let again = path.with_extension("again");
        std::fs::copy(path, &again).unwrap();
        recordings::compact(&again).unwrap();
        assert_eq!(
            std::fs::read(&again).unwrap(),
            std::fs::read(path).unwrap(),
            "{path:?}"
        );
        std::fs::remove_file(again).unwrap();
    }

    /// While a recording is written: picking it from the list keeps it live, exports wait,
    /// the loss counts are the worker's, a stalled stream's republished snapshot doesn't move the
    /// followed window back, and now runs at most 50 ms past the data. At the stop: the view
    /// reads the whole finished file before its compaction, and the window counts as busy until
    /// that is done.
    #[test]
    fn the_recording_under_way() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("captures").join("live.tickwave");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let all = messages(1000, 20_000, pulses, false, &[]);
        let (head, tail) = all.split_at(all.len() / 2);
        let mut writer = FileWriter::create(&path, Context::default()).unwrap();
        writer.ingest(&head[0]).unwrap();
        let (mut h, instance) = rig(Bench::new(dir.path()));
        let mut status = recording_status(&instance, &path, Some(writer.snapshots()));
        status.marker_losses = 2;
        status.edge_losses = 3;
        h.state_mut().0.receive(status.clone());
        h.step();
        for message in &head[1..] {
            writer.ingest(message).unwrap();
            h.step();
        }
        until(&mut h, "the first half", |b| {
            b.now().is_some() && b.answer_now().is_some_and(|a| a.extent.is_some())
        });
        assert!(h.state().0.recovered.contains(&path), "listed as kept");
        h.step();
        assert!(h.get_by_label("CSV…").accesskit_node().is_disabled());
        assert!(
            h.query_by_label("3 DIO edge-loss reports in this capture")
                .is_some()
        );
        until(&mut h, "the marker list", |b| {
            b.answer_now().is_some_and(|a| a.markers.list.is_some())
        });
        assert!(
            h.query_by_label("2 marker-loss reports. Some markers are missing.")
                .is_some()
        );

        // Picked from the list, it stays live; nothing repairs it under the writer.
        h.state_mut().0.open(path.clone());
        h.step();
        assert!(h.state().0.live.is_some() && h.state().0.compactions.is_empty());
        assert_eq!(h.state().0.path.as_ref(), Some(&path));

        // The writer's clock publishes the same end again: the window doesn't step back.
        until(&mut h, "an end", |b| {
            b.live.as_ref().is_some_and(|l| l.end.is_some())
        });
        std::thread::sleep(Duration::from_millis(30));
        let before = h.state().0.now().unwrap();
        writer.flush().unwrap();
        h.step();
        assert!(h.state().0.now().unwrap() >= before);
        // Now runs at most 50 ms past the end it was given.
        let (end, _) = h.state().0.live.as_ref().unwrap().end.unwrap();
        h.state_mut().0.live.as_mut().unwrap().end =
            Some((end, Instant::now() - Duration::from_secs(1)));
        let cap = (0.05 * h.state().0.channels.as_ref().unwrap().rate).round() as i64;
        assert_eq!(h.state().0.now(), Some(end + cap));

        // The rest goes in without the panel taking it, then the writer finishes.
        for message in tail {
            writer.ingest(message).unwrap();
        }
        assert!(writer.close().unwrap());
        // Hold the file thread, so the view must read the finished file before any compaction.
        let (hold, held) = mpsc::channel();
        h.state_mut().0.io = Some(held);
        h.state_mut()
            .0
            .receive(recording_status(&instance, &path, None));
        assert!(h.state().0.busy(), "busy until compacted");
        assert!(h.state().0.now().is_none());
        // Statuses keep naming the stopped recording: it is finished once.
        h.state_mut()
            .0
            .receive(recording_status(&instance, &path, None));
        assert_eq!(h.state().0.compactions.len(), 1);
        h.step();
        let kept = h
            .get_all_by_label("Kept on disk")
            .find(|n| n.accesskit_node().role() == egui::accesskit::Role::ComboBox)
            .expect("the kept list");
        assert!(
            kept.accesskit_node().is_disabled(),
            "kept list off while compacting"
        );
        until(&mut h, "the whole file before compacting", |b| {
            b.answer_now()
                .and_then(|a| a.extent.clone())
                .is_some_and(|e| e.end >= 20_000)
        });
        hold.send(Ok(FileResult::Cancelled)).unwrap();
        until(&mut h, "the compaction", |b| !b.busy());
        settle(&mut h);
        assert_compacted(&path);
        assert_eq!(h.state().0.notice, None);
    }

    #[test]
    fn a_file_opened_while_recording_stays_and_the_recording_is_still_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let captures = dir.path().join("captures");
        std::fs::create_dir_all(&captures).unwrap();
        let other = captures.join("other.tickwave");
        write(&other, &messages(1000, 5_000, pulses, false, &[]));
        let path = captures.join("live.tickwave");
        let all = messages(1000, 5_000, pulses, false, &[]);
        let mut writer = FileWriter::create(&path, Context::default()).unwrap();
        writer.ingest(&all[0]).unwrap();
        let (mut h, instance) = rig(Bench::new(dir.path()));
        let live = recording_status(&instance, &path, Some(writer.snapshots()));
        h.state_mut().0.receive(live.clone());
        h.state_mut().0.open(other.clone());
        settle(&mut h);
        for message in &all[1..all.len() - 1] {
            writer.ingest(message).unwrap();
        }
        h.state_mut().0.receive(live);
        h.step();
        assert_eq!(
            h.state().0.path.as_ref(),
            Some(&other),
            "the next status keeps it open"
        );
        // Picked again, the recording shows live; then the other file again.
        h.state_mut().0.open(path.clone());
        assert!(h.state().0.live.is_some());
        assert_eq!(h.state().0.path.as_ref(), Some(&path));
        h.state_mut().0.open(other.clone());
        settle(&mut h);
        writer.ingest(all.last().unwrap()).unwrap();
        assert!(writer.close().unwrap());
        h.state_mut()
            .0
            .receive(recording_status(&instance, &path, None));
        assert!(h.state().0.busy());
        until(&mut h, "the compaction", |b| !b.busy());
        assert_compacted(&path);
        assert_eq!(h.state().0.path.as_ref(), Some(&other));
    }

    #[test]
    fn a_recording_that_came_and_went_between_statuses_is_listed_compacted_and_shown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("captures").join("short.tickwave");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut writer = FileWriter::create(&path, Context::default()).unwrap();
        for message in &messages(1000, 3_000, pulses, false, &[]) {
            writer.ingest(message).unwrap();
        }
        assert!(writer.close().unwrap());
        // Another, longer file is shown when it comes and goes.
        let other = dir.path().join("captures").join("other.tickwave");
        write(&other, &messages(1000, 5_000, pulses, false, &[]));
        let (mut h, instance) = rig(Bench::new(dir.path()));
        h.state_mut().0.open(other);
        settle(&mut h);
        h.state_mut()
            .0
            .receive(recording_status(&instance, &path, None));
        assert!(h.state().0.recovered.contains(&path));
        until(&mut h, "the copy, shown", |b| {
            !b.busy() && b.viewer.is_some()
        });
        settle(&mut h);
        assert_compacted(&path);
        assert_eq!(h.state().0.path.as_ref(), Some(&path));
        let end = h.state().0.answer().unwrap().extent.clone().unwrap().end;
        assert_eq!(end, 3_000, "the graph draws the recording named");
    }

    /// A recording picked while its compaction is queued opens once the copy is in place, so no
    /// second reader holds the file at the rename.
    #[test]
    fn a_file_opens_only_once_its_compaction_is_done() {
        let dir = tempfile::tempdir().unwrap();
        let captures = dir.path().join("captures");
        std::fs::create_dir_all(&captures).unwrap();
        let other = captures.join("other.tickwave");
        write(&other, &messages(1000, 5_000, pulses, false, &[]));
        let path = captures.join("live.tickwave");
        let all = messages(1000, 5_000, pulses, false, &[]);
        let mut writer = FileWriter::create(&path, Context::default()).unwrap();
        writer.ingest(&all[0]).unwrap();
        let (mut h, instance) = rig(Bench::new(dir.path()));
        h.state_mut()
            .0
            .receive(recording_status(&instance, &path, Some(writer.snapshots())));
        h.state_mut().0.open(other);
        settle(&mut h);
        for message in &all[1..] {
            writer.ingest(message).unwrap();
        }
        assert!(writer.close().unwrap());
        let (hold, held) = mpsc::channel();
        h.state_mut().0.io = Some(held);
        h.state_mut()
            .0
            .receive(recording_status(&instance, &path, None));
        // Not shown, it is compacted without the view of the file that is.
        assert!(h.state().0.compactions[0].viewer.is_none());
        h.state_mut().0.open(path.clone());
        for _ in 0..5 {
            h.step();
        }
        assert!(
            h.state().0.viewer.is_none(),
            "opened under its queued compaction"
        );
        hold.send(Ok(FileResult::Cancelled)).unwrap();
        until(&mut h, "the copy, then the view", |b| {
            !b.busy() && b.viewer.is_some()
        });
        settle(&mut h);
        assert_compacted(&path);
        assert_eq!(h.state().0.path.as_ref(), Some(&path));
    }

    #[test]
    fn a_file_waits_for_the_compaction_running_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("captures").join("busy.tickwave");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write(&path, &messages(1000, 3_000, pulses, false, &[]));
        let mut h = harness(Bench::new(dir.path()));
        h.step();
        let (done, running) = mpsc::channel();
        h.state_mut().io = Some(running);
        h.state_mut().compacting = Some(path.clone());
        h.state_mut().open(path.clone());
        for _ in 0..5 {
            h.step();
        }
        assert!(
            h.state().viewer.is_none(),
            "opened under its running compaction"
        );
        done.send(Ok(FileResult::Compacted)).unwrap();
        settle(&mut h);
        assert_eq!(h.state().path.as_ref(), Some(&path));
    }

    #[test]
    fn a_followed_span_keeps_its_seconds_across_files_at_other_rates() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = opened(dir.path(), &messages(1000, 20_000, pulses, false, &[]));
        click(&mut h, "1s");
        let faster = dir.path().join("captures").join("faster.tickwave");
        write(&faster, &messages(2000, 20_000, pulses, false, &[]));
        h.state_mut().open(faster);
        settle(&mut h);
        assert_eq!(h.state().state.window, Window::Follow { span: 2000 });
    }

    #[test]
    fn a_recording_whose_header_never_came_leaves_nothing_shown_or_listed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("captures").join("empty.tickwave");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let writer = FileWriter::create(&path, Context::default()).unwrap();
        let (mut h, instance) = rig(Bench::new(dir.path()));
        h.state_mut()
            .0
            .receive(recording_status(&instance, &path, Some(writer.snapshots())));
        h.step();
        assert!(!writer.close().unwrap());
        h.state_mut()
            .0
            .receive(recording_status(&instance, &path, None));
        h.step();
        let b = &h.state().0;
        assert_eq!(
            (b.path.as_ref(), b.recovered.contains(&path)),
            (None, false)
        );
        assert!(!b.busy());
    }

    /// Nothing holds the file when the copy is renamed over it, as Windows needs: counted in
    /// this process's open files, which the viewer's reader is among.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_compaction_renames_with_the_file_let_go() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("closed.tickwave");
        let mut writer = FileWriter::create(&path, Context::default()).unwrap();
        for message in &messages(1000, 3_000, pulses, false, &[]) {
            writer.ingest(message).unwrap();
        }
        assert!(writer.close().unwrap());
        let viewer = Viewer::open(&egui::Context::default(), &path).unwrap();
        let held = || {
            std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(|fd| std::fs::read_link(fd.unwrap().path()).ok())
                .filter(|target| *target == path)
                .count()
        };
        assert!(held() > 0, "the viewer reads the file");
        compact_then(&path, Some(&viewer), || {
            assert_eq!(held(), 0, "held at the rename")
        })
        .unwrap();
        assert_compacted(&path);
    }

    #[test]
    fn a_cancelled_dialog_keeps_the_notice() {
        let dir = tempfile::tempdir().unwrap();
        let mut bench = Bench::new(dir.path());
        bench.tick();
        bench.notice = Some("disk full".into());
        let (send, receive) = mpsc::channel();
        bench.io = Some(receive);
        send.send(Ok(FileResult::Cancelled)).unwrap();
        bench.tick();
        assert_eq!(bench.notice.as_deref(), Some("disk full"));
    }

    #[test]
    fn file_activity_blocks_close_and_reports_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut bench = Bench::new(dir.path());
        let (send, receive) = mpsc::channel();
        bench.io = Some(receive);
        assert!(bench.busy());
        send.send(Err("disk full".into())).unwrap();
        bench.tick();
        assert!(!bench.busy());
        assert!(bench.take_failure());
        assert_eq!(bench.notice.as_deref(), Some("disk full"));
        let (send, receive) = mpsc::channel();
        bench.io = Some(receive);
        drop(send);
        bench.tick();
        assert!(!bench.busy());
        assert!(bench.take_failure());
        // A compaction waiting for the file thread blocks closing too.
        bench.compact(dir.path().join("x.tickwave"), None);
        assert!(bench.busy());
    }

    #[test]
    fn switching_devices_clears_confirmations_and_a_failure_finishes_the_recording() {
        let dir = tempfile::tempdir().unwrap();
        let mut bench = Bench::new(dir.path());
        bench.start_confirm = true;
        bench.off_confirm = true;
        bench.connection = Some("old".into());
        let unit = escapement_verge_probe::detect(
            &escapement_verge_probe::sim::SimBus::scenario("ours-pro").unwrap(),
        )
        .unwrap()
        .remove(0);
        bench.connect(&unit, &mut crate::worker::Manual::default());
        assert!(!bench.start_confirm);
        assert!(!bench.off_confirm);
        let path = bench.directory.join("new.tickwave");
        std::fs::create_dir_all(&bench.directory).unwrap();
        let mut writer = FileWriter::create(&path, Context::default()).unwrap();
        for message in &messages(1000, 2_000, pulses, false, &[]) {
            writer.ingest(message).unwrap();
        }
        let slot = writer.snapshots();
        bench.receive(recording_status(&unit.device.instance, &path, Some(slot)));
        assert!(bench.recovered.contains(&path));
        assert_eq!(bench.path.as_ref(), Some(&path));
        assert!(writer.close().unwrap());
        // The worker failed and closed the recording: the panel finishes it as a stop.
        bench.worker_failed("gone");
        assert!(!bench.recording());
        assert_eq!(bench.path.as_ref(), Some(&path));
        assert_eq!(bench.compactions.len(), 1);
    }

    /// A pin the bench marker holds is not offered, and with all four held the panel says why.
    #[test]
    fn marker_held_pins_are_left_out() {
        use escapement_verge_probe::sim::SimBus;
        let bus = SimBus::scenario("ours-plain").unwrap();
        let unit = escapement_verge_probe::detect(&bus).unwrap().remove(0);
        let mut dio = pro::with_pro(&bus, &unit, pro::Feature::Dio, |_, pro| {
            anyhow::Ok(pro.access_dio(0, 0, 0)?)
        })
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let rig = (
            Bench::new(dir.path()),
            vec![unit.clone()],
            Some(unit.device.instance.clone()),
            crate::worker::Manual::default(),
        );
        let mut h = Harness::builder()
            .with_size(egui::vec2(1080.0, 700.0))
            .with_context_setup(style::install)
            .build_ui_state(
                |ui, (bench, units, selected, backend): &mut Rig| {
                    bench.show(ui, units, selected, backend);
                },
                rig,
            );
        h.step();
        for (pins, offered) in [(0x0f, 0x0f), (0x0a, 0x0a), (0, 0)] {
            dio.pins = pins;
            h.state_mut().0.receive(Status {
                instance: unit.device.instance.clone(),
                connected: true,
                board: Some(BoardKind::Plain),
                dio: Some(dio.clone()),
                ..Status::default()
            });
            h.step();
            for pin in 0..4 {
                let shown = h.query_by_label(&format!("DIO{pin} output")).is_some();
                assert_eq!(shown, offered & 1 << pin != 0, "pins {pins:#x}, DIO{pin}");
            }
            let told = h
                .query_all_by_label_contains("command marker")
                .next()
                .is_some();
            assert_eq!(told, pins == 0, "pins {pins:#x}");
        }
    }

    /// Frame time, data-to-screen latency and memory while a recording is written at 25,000 and at
    /// 100,000 pairs a second, for 30 s each, the frames paced at 60 a second; `VERGE_FIGURES_RATES`
    /// (comma-separated) and `VERGE_FIGURES_SECONDS` change them, and memory is sampled every 10 s.
    /// A measurement, not a check: build for release and hold the process to one core, as verge's
    /// AGENTS.md asks.
    ///
    ///     taskset -c 0 cargo test --release -p escapement-verge-gui live_figures -- --ignored --nocapture
    #[test]
    #[ignore = "a measurement: run in release on one core"]
    fn live_figures() {
        let rates: Vec<u32> = std::env::var("VERGE_FIGURES_RATES")
            .map_or(vec![25_000, 100_000], |v| {
                v.split(',').map(|r| r.trim().parse().unwrap()).collect()
            });
        let seconds: u32 =
            std::env::var("VERGE_FIGURES_SECONDS").map_or(30, |v| v.parse().unwrap());
        for rate in rates {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("captures").join("live.tickwave");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let all = messages(rate, rate * seconds, pulses, true, &[]);
            let mut writer = FileWriter::create(&path, Context::default()).unwrap();
            writer.ingest(&all[0]).unwrap();
            let slot = writer.snapshots();
            // Each message as the writer takes it in: when, and the tick its last pair ends at.
            let (sent, arrived) = mpsc::channel::<(Instant, i64)>();
            let start = Instant::now();
            let feeder = std::thread::spawn(move || {
                let mut end = 0;
                for message in &all[1..] {
                    if let Message::Pairs(pairs) = message {
                        let first = i64::from(pairs.first_sequence);
                        let due = Duration::from_secs_f64(first as f64 / f64::from(rate));
                        std::thread::sleep(due.saturating_sub(start.elapsed()));
                        end = first + pairs.pairs.len() as i64;
                    }
                    writer.ingest(message).unwrap();
                    let _ = sent.send((Instant::now(), end));
                }
                writer.close().unwrap();
            });
            // A picked unit, so the panel keeps its status as it does in use.
            let units = escapement_verge_probe::detect(
                &escapement_verge_probe::sim::SimBus::scenario("ours-pro-energy").unwrap(),
            )
            .unwrap();
            let instance = units[0].device.instance.clone();
            let mut selected = Some(instance.clone());
            let mut bench = Bench::new(dir.path());
            bench.connection = Some(instance.clone());
            bench.receive(Status {
                instance,
                connected: true,
                recording: true,
                path: Some(path.clone()),
                snapshots: Some(slot),
                ..Status::default()
            });
            let mut h = Harness::builder()
                .with_size(egui::vec2(1080.0, 700.0))
                .with_context_setup(crate::style::install)
                .wgpu()
                .build_ui_state(
                    move |ui, b: &mut Bench| {
                        b.show(
                            ui,
                            &units,
                            &mut selected,
                            &mut crate::worker::Manual::default(),
                        )
                    },
                    bench,
                );
            let rss = || {
                std::fs::read_to_string("/proc/self/status")
                    .unwrap()
                    .lines()
                    .find_map(|l| l.strip_prefix("VmRSS:"))
                    .and_then(|v| v.trim().trim_end_matches(" kB").parse::<u64>().ok())
                    .unwrap_or(0)
            };
            let rss_start = rss();
            let mut arrivals: Vec<(Instant, i64)> = Vec::new();
            let (mut frames, mut latencies) = (Vec::new(), Vec::new());
            let mut drawn = 0;
            let mut samples = Vec::new();
            let frame = Duration::from_secs_f64(1.0 / 60.0);
            let mut next = Instant::now();
            while start.elapsed() < Duration::from_secs(u64::from(seconds) - 1) {
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
                next += frame;
                let t = Instant::now();
                if samples.len() < (start.elapsed().as_secs() / 10) as usize {
                    samples.push(rss() / 1024);
                }
                h.step();
                h.render().unwrap();
                let done = Instant::now();
                frames.push((done - t).as_secs_f64() * 1e3);
                arrivals.extend(arrived.try_iter());
                let end = h
                    .state()
                    .answer()
                    .and_then(|a| a.extent.clone())
                    .map_or(0, |e| e.end);
                if end > drawn {
                    drawn = end;
                    // The message that brought the newest pair drawn.
                    if let Some((at, _)) = arrivals.iter().find(|(_, e)| *e >= end) {
                        latencies.push((done - *at).as_secs_f64() * 1e3);
                    }
                }
            }
            feeder.join().unwrap();
            let rss_end = rss();
            let pct = |v: &mut Vec<f64>, p: f64| {
                v.sort_by(f64::total_cmp);
                v.get(((v.len() as f64 - 1.0) * p).round() as usize)
                    .copied()
                    .unwrap_or(f64::NAN)
            };
            eprintln!("{rate} pairs/s: RSS every 10 s {samples:?} MiB");
            eprintln!(
                "{rate} pairs/s: frame {:.2} / {:.2} ms (median / p95, {} frames); new data to the \
                 screen {:.1} / {:.1} ms ({} updates); RSS {} -> {} MiB",
                pct(&mut frames, 0.5),
                pct(&mut frames, 0.95),
                frames.len(),
                pct(&mut latencies, 0.5),
                pct(&mut latencies, 0.95),
                latencies.len(),
                rss_start / 1024,
                rss_end / 1024,
            );
        }
    }
}
