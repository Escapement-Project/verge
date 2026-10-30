//! Pro bench view. Acquisition and archive queries continue independently of frames.

mod query;
use crate::app::{choice, edged, label, outline, sentence};
use crate::bench_worker::{Command, Status};
use crate::style::{self, Role, color, space, text};
use crate::worker::{Backend, Request};
use egui::{Color32, CornerRadius, Frame, Margin, ScrollArea, Stroke, Ui, vec2};
use egui_plot::{GridInput, GridMark, Line, Plot, Span, uniform_grid_spacer};
use query::{Answer, Queries, Query};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::AtomicBool,
    mpsc::{self, Receiver},
};
use std::time::{Duration, Instant, SystemTime};
use verge_probe::capture::{Archive, Interval, Statistics, View, recovered_sessions};
use verge_probe::{Unit, pro};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quantity {
    Current,
    Voltage,
    Power,
}
impl Quantity {
    /// The unit the graph's values are in.
    fn unit(self) -> &'static str {
        match self {
            Self::Current => "mA",
            Self::Voltage => "V",
            Self::Power => "mW",
        }
    }
    fn word(self) -> &'static str {
        match self {
            Self::Current => "Current",
            Self::Voltage => "Voltage",
            Self::Power => "Power",
        }
    }
    fn extrema(self, s: &Statistics) -> Option<(f64, f64)> {
        Some(match self {
            Self::Current => (f64::from(s.min_na?) * 1e-6, f64::from(s.max_na?) * 1e-6),
            Self::Voltage => (f64::from(s.min_mv?) * 1e-3, f64::from(s.max_mv?) * 1e-3),
            Self::Power => (s.min_mv_na? as f64 * 1e-9, s.max_mv_na? as f64 * 1e-9),
        })
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
}

pub struct Bench {
    pub status: Option<Status>,
    connection: Option<String>,
    rate: u32,
    path: Option<PathBuf>,
    directory: PathBuf,
    recovered: Vec<PathBuf>,
    queries: Queries,
    generation: u64,
    /// The generation of the answer on screen.
    answered: u64,
    /// The last query failed: asking again waits for something to change.
    query_failed: bool,
    pending: bool,
    dirty: bool,
    queried_at: Instant,
    answer: Option<Answer>,
    interval: Option<Interval>,
    selection: Option<Interval>,
    span: u64,
    follow: bool,
    quantity: Quantity,
    select_mode: bool,
    drag_start: Option<u64>,
    reset_bounds: bool,
    y_lock: bool,
    y_bounds: Option<[f64; 2]>,
    y_fit: u8,
    io: Option<Receiver<Result<FileResult, String>>>,
    failed: bool,
    notice: Option<String>,
    start_confirm: bool,
    off_confirm: bool,
    /// The height under the plot, measured on the frame before.
    footer: f32,
}

impl Bench {
    pub fn new(directory: &Path) -> Self {
        let directory = directory.join("captures");
        let recovery = directory.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(
                recovered_sessions(&recovery)
                    .map(FileResult::Recovery)
                    .map_err(|e| e.to_string()),
            );
        });
        Self {
            status: None,
            connection: None,
            rate: 1000,
            path: None,
            directory,
            recovered: Vec::new(),
            queries: Queries::new(),
            generation: 0,
            answered: 0,
            query_failed: false,
            pending: false,
            dirty: true,
            queried_at: Instant::now(),
            answer: None,
            interval: None,
            selection: None,
            span: 10_000_000_000,
            follow: true,
            quantity: Quantity::Current,
            select_mode: false,
            drag_start: None,
            reset_bounds: true,
            y_lock: false,
            y_bounds: None,
            y_fit: 0,
            io: Some(rx),
            failed: false,
            notice: None,
            start_confirm: false,
            off_confirm: false,
            footer: 120.0,
        }
    }
    pub fn recording(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.recording)
    }
    pub fn busy(&self) -> bool {
        self.recording() || self.io.is_some()
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
        self.invalidate();
    }
    pub fn inspect(&mut self, interval: Interval, selection: Option<Interval>) {
        self.follow = false;
        self.interval = Some(interval);
        self.selection = selection;
        self.reset_bounds = true;
        self.invalidate();
    }
    #[cfg(test)]
    pub fn selection_statistics(&self) -> Option<(Interval, &Statistics)> {
        self.answer
            .as_ref()?
            .selection
            .as_ref()
            .map(|(interval, stats)| (*interval, stats))
    }
    pub fn receive(&mut self, status: Status) {
        if self.connection.as_deref() != Some(status.instance.as_str()) {
            return;
        }
        self.off_confirm = status.confirm_off;
        if self
            .status
            .as_ref()
            .is_some_and(|old| old.recording && !status.recording)
        {
            self.invalidate();
        }
        if let Some(path) = &status.path
            && self.status.as_ref().and_then(|s| s.path.as_ref()) != Some(path)
        {
            if path.parent() == Some(self.directory.as_path()) && !self.recovered.contains(path) {
                self.recovered.push(path.clone());
            }
            self.open(path.clone());
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
    fn open(&mut self, path: PathBuf) {
        self.path = Some(path);
        self.interval = None;
        self.selection = None;
        self.answer = None;
        self.follow = true;
        self.reset_bounds = true;
        self.invalidate();
    }
    fn invalidate(&mut self) {
        self.query_failed = false;
        self.queries.cancel();
        self.generation += 1;
        self.pending = false;
        self.dirty = true;
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
        let path = self.directory.join(format!("capture-{now}.vergecapture"));
        backend.send(Request::Bench(Command::Start {
            rate: self.rate,
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
            match result {
                Ok(FileResult::Open(path)) => self.open(path),
                Ok(FileResult::Done(message)) => {
                    self.notice = Some(message);
                }
                Ok(FileResult::Cancelled) => {}
                Ok(FileResult::Recovery(paths)) => self.recovered = paths,
                Ok(FileResult::Discarded(path)) => {
                    self.recovered.retain(|p| p != &path);
                    if self.path.as_ref() == Some(&path) {
                        self.path = None;
                        self.answer = None;
                        self.selection = None;
                        self.invalidate();
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
    fn poll(&mut self, ui: &Ui) {
        self.tick();
        if let Some((generation, result)) = self.queries.take()
            && generation == self.generation
        {
            self.pending = false;
            match result {
                Ok(answer) => {
                    self.answer = Some(answer);
                    self.answered = generation;
                }
                Err(error) => {
                    self.notice = Some(error);
                    self.query_failed = true;
                }
            }
        }
        let refresh = self.follow
            && self.recording()
            && self.queried_at.elapsed() > Duration::from_millis(250);
        if let Some(path) = self.path.clone()
            && !self.pending
            && (self.dirty || (self.answer.is_none() && !self.query_failed) || refresh)
        {
            self.queried_at = Instant::now();
            self.pending = true;
            self.dirty = false;
            self.queries.send(Query {
                generation: self.generation,
                path,
                interval: if self.follow { None } else { self.interval },
                live_span: self.span,
                selection: self.selection,
                pixels: ui.available_width().max(64.0) as usize,
                cancel: Arc::new(AtomicBool::new(false)),
            });
        }
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }
    /// The view: its controls and readings in a left panel, the graph in the rest.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        units: &[Unit],
        selected: &mut Option<String>,
        backend: &mut dyn Backend,
    ) {
        egui::Panel::left("bench")
            .exact_size(SIDE)
            .frame(
                Frame::new()
                    .fill(color::SUNKEN)
                    .inner_margin(Margin::same(space::M as i8)),
            )
            .show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| self.side(ui, units, selected, backend));
            });
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(color::BACKGROUND)
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
        let name = label(ui, "unit");
        let chosen = units
            .iter()
            .find(|u| Some(&u.device.instance) == selected.as_ref());
        egui::ComboBox::from_id_salt("bench-unit")
            .width(ui.available_width())
            .selected_text(text(
                Role::Data,
                chosen.and_then(Unit::serial).unwrap_or("Select a unit"),
            ))
            .show_ui(ui, |ui| {
                for unit in units {
                    ui.add_enabled_ui(!self.recording(), |ui| {
                        ui.selectable_value(
                            selected,
                            Some(unit.device.instance.clone()),
                            text(Role::Data, unit.serial().unwrap_or("No serial")),
                        );
                    });
                }
            })
            .response
            .labelled_by(name.id);
        match chosen {
            Some(unit) => match pro::validate(unit, pro::Feature::Energy) {
                Ok(()) => self.connect(unit, backend),
                Err(error) => {
                    self.disconnect(backend);
                    ui.add_space(space::S);
                    edged(ui, color::ATTENTION, &sentence(&error.to_string()));
                }
            },
            None => self.disconnect(backend),
        }
        match (&self.status, chosen) {
            (Some(status), _) if status.connected => {
                let status = status.clone();
                self.supply(ui, &status, backend);
                self.acquisition(ui, &status, backend);
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
                ui.label(text(Role::Body, "Connecting…").color(color::TEXT_2));
            }
            _ => {}
        }
        if let Some(notice) = self.notice.clone() {
            ui.add_space(space::S);
            edged(ui, color::ATTENTION, &notice);
        }
        self.measurements(ui);
        self.kept(ui);
        ui.add_space(space::L);
        label(ui, "dio");
        ui.label(text(Role::Body, "DIO awaits firmware support through Aux.").color(color::TEXT_2));
    }

    fn supply(&mut self, ui: &mut Ui, status: &Status, backend: &mut dyn Backend) {
        ui.add_space(space::L);
        label(ui, "target supply");
        ui.horizontal(|ui| {
            if outline(ui, if status.supply { "Turn off" } else { "Turn on" }, true).clicked() {
                backend.send(Request::Bench(Command::Supply {
                    on: !status.supply,
                    confirmed: false,
                }));
            }
            ui.label(
                text(Role::Data, if status.supply { "ON" } else { "OFF" }).color(
                    if status.supply {
                        color::TEXT
                    } else {
                        color::TEXT_2
                    },
                ),
            );
        });
        ui.label(
            text(
                Role::Data,
                status.millivolts.map_or_else(
                    || "no reading without a capture".into(),
                    |mv| format!("measured {}", si(f64::from(mv) * 1e-3, "V")),
                ),
            )
            .color(color::TEXT_2),
        );
    }

    fn acquisition(&mut self, ui: &mut Ui, status: &Status, backend: &mut dyn Backend) {
        ui.add_space(space::L);
        label(ui, "acquisition");
        if status.recording {
            ui.label(
                text(
                    Role::Data,
                    status.rate.map_or_else(
                        || "waiting for the stream".into(),
                        |rate| format!("{rate} pairs/s · recording"),
                    ),
                )
                .color(color::TEXT),
            );
            if outline(ui, "Stop recording", true).clicked() {
                backend.send(Request::Bench(Command::Stop));
            }
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.rate)
                    .range(1..=100_000)
                    .suffix(" pairs/s"),
            );
            for rate in [1_000, 10_000, 100_000] {
                if choice(ui, &format!("{}k", rate / 1000), self.rate == rate).clicked() {
                    self.rate = rate;
                }
            }
        });
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
                    .color(color::TEXT_2),
                )
                .wrap(),
            );
        }
    }

    /// The visible window's readings and the selection's, side by side.
    fn measurements(&mut self, ui: &mut Ui) {
        let Some(answer) = self.answer.clone() else {
            return;
        };
        ui.add_space(space::L);
        label(ui, "measurements");
        if let Some(mut selection) = self.selection {
            ui.horizontal(|ui| {
                let mut start = seconds(selection.start_ns);
                let mut end = seconds(selection.end_ns);
                let from = ui.add(
                    egui::DragValue::new(&mut start)
                        .speed(0.001)
                        .max_decimals(6)
                        .suffix(" s"),
                );
                ui.label(text(Role::Data, "to").color(color::TEXT_2));
                let to = ui.add(
                    egui::DragValue::new(&mut end)
                        .speed(0.001)
                        .max_decimals(6)
                        .suffix(" s"),
                );
                if from.changed() || to.changed() {
                    selection = Interval::new(nanos(start), nanos(end));
                    self.selection = Some(selection);
                    self.invalidate();
                }
            });
            ui.horizontal(|ui| {
                if outline(ui, "Zoom to selection", true).clicked() {
                    self.inspect(selection, Some(selection));
                }
                if outline(ui, "Clear", true).clicked() {
                    self.selection = None;
                    self.invalidate();
                }
            });
        }
        let window = (answer.view.interval, answer.view.statistics.clone());
        let selection = self.selection.map(|selection| match &answer.selection {
            Some((queried, statistics)) if *queried == selection => {
                Some((selection, statistics.clone()))
            }
            // The answer for this selection has not come back yet.
            _ => None,
        });
        egui::Grid::new("bench-measurements")
            .num_columns(3)
            .spacing(vec2(space::S, space::XS))
            .min_col_width(60.0)
            .show(ui, |ui| {
                ui.label("");
                label(ui, "window");
                if selection.is_some() {
                    label(ui, "selection");
                }
                ui.end_row();
                let columns: Vec<Option<&(Interval, Statistics)>> = std::iter::once(Some(&window))
                    .chain(selection.as_ref().map(Option::as_ref))
                    .collect();
                for (name, value) in READINGS {
                    label(ui, name);
                    for column in &columns {
                        let words = column.map_or_else(
                            || "…".to_owned(),
                            |(interval, statistics)| value(*interval, statistics),
                        );
                        ui.label(text(Role::Data, words).color(color::TEXT));
                    }
                    ui.end_row();
                }
                label(ui, "unmeasured");
                for column in &columns {
                    let missing = column.map(|(interval, statistics)| {
                        interval
                            .duration_ns()
                            .saturating_sub(statistics.measured_ns)
                    });
                    ui.label(match missing {
                        Some(0) => text(Role::Data, "none").color(color::TEXT_2),
                        Some(ns) => text(Role::Data, si(seconds(ns), "s")).color(color::ATTENTION),
                        None => text(Role::Data, "…").color(color::TEXT),
                    });
                }
                ui.end_row();
            });
    }

    /// The open capture's name and what can be done with it, under the graph.
    fn files(&mut self, ui: &mut Ui) {
        let idle = self.io.is_none();
        let open = self.path.is_some();
        ui.horizontal_wrapped(|ui| {
            if let Some(path) = &self.path {
                ui.label(
                    text(
                        Role::Data,
                        path.file_name().unwrap_or_default().to_string_lossy(),
                    )
                    .color(color::TEXT_2),
                )
                .on_hover_text(path.display().to_string());
                ui.add_space(space::S);
            }
            if outline(ui, "Open…", idle).clicked() {
                self.file(FileAction::Open);
            }
            if outline(ui, "Save as…", idle && open).clicked() {
                self.file(FileAction::Save);
            }
            ui.add_space(space::S);
            label(
                ui,
                if self.selection.is_some() {
                    "export selection"
                } else {
                    "export"
                },
            )
            .on_hover_text("Exports keep whole samples at the selection's edges.");
            if outline(ui, "CSV…", idle && open).clicked() {
                self.file(FileAction::Export(true));
            }
            if outline(ui, "sigrok…", idle && open).clicked() {
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
        let name = label(ui, "kept on disk");
        egui::ComboBox::from_id_salt("bench-retained")
            .width(ui.available_width())
            .selected_text(text(
                Role::Data,
                format!("{} captures", self.recovered.len()),
            ))
            .show_ui(ui, |ui| {
                for path in self.recovered.clone() {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if ui
                        .selectable_label(self.path.as_ref() == Some(&path), text(Role::Data, name))
                        .clicked()
                    {
                        self.open(path);
                    }
                }
            })
            .response
            .labelled_by(name.id);
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
        ui.horizontal_wrapped(|ui| {
            for quantity in [Quantity::Current, Quantity::Voltage, Quantity::Power] {
                if choice(ui, quantity.word(), self.quantity == quantity).clicked()
                    && self.quantity != quantity
                {
                    self.quantity = quantity;
                    self.y_bounds = None;
                    self.y_fit = 2;
                }
            }
            ui.add_space(space::M);
            if choice(ui, "Live", self.follow).clicked() && !self.follow {
                self.follow = true;
                self.reset_bounds = true;
                self.invalidate();
            }
            if choice(ui, "Inspect", !self.follow).clicked() && self.follow {
                self.follow = false;
                self.interval = self.answer.as_ref().map(|a| a.view.interval);
                self.invalidate();
            }
            ui.add_space(space::M);
            for seconds in [1, 10, 60] {
                let span = seconds * 1_000_000_000;
                if choice(ui, &format!("{seconds}s"), self.follow && self.span == span).clicked() {
                    self.span = span;
                    self.reset_bounds = true;
                    self.interval = self.answer.as_ref().map(|a| {
                        Interval::new(
                            a.view.interval.end_ns.saturating_sub(self.span),
                            a.view.interval.end_ns,
                        )
                    });
                    self.invalidate();
                }
            }
            ui.add_space(space::M);
            if choice(ui, "Select", self.select_mode).clicked() {
                self.select_mode = !self.select_mode;
            }
            if choice(ui, "Lock Y", self.y_lock).clicked() {
                self.y_lock = !self.y_lock;
                self.y_bounds = None;
            }
        });
        ui.add_space(space::S);
    }

    fn graph(&mut self, ui: &mut Ui) {
        self.toolbar(ui);
        let Some(answer) = self.answer.clone() else {
            let size = vec2(
                ui.available_width(),
                (ui.available_height() - 48.0).max(160.0),
            );
            Frame::new()
                .fill(color::SUNKEN)
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
                                } else {
                                    "Start a recording, or open a capture."
                                },
                            )
                            .color(color::TEXT_2),
                        );
                    });
                });
            ui.add_space(space::S);
            self.files(ui);
            return;
        };
        let interval = answer.view.interval;
        let extent = answer.view.extent;
        let quantity = self.quantity;
        let selection = self.selection;
        let follow = self.follow;
        // A new interval's bounds apply once its own answer is drawn, not the one before it.
        let fresh = self.answered == self.generation;
        let reset = self.reset_bounds && fresh;
        let y_bounds = if self.y_fit > 0 { None } else { self.y_bounds };
        let mut pointer = None;
        let plot = Plot::new("energy")
            .height((ui.available_height() - self.footer).max(160.0))
            .x_grid_spacer(uniform_grid_spacer(ladder))
            .y_grid_spacer(uniform_grid_spacer(ladder))
            .x_axis_formatter(|mark, _| tick(mark, "s"))
            .y_axis_formatter(move |mark, _| tick(mark, quantity.unit()))
            .y_axis_min_width(56.0)
            .include_y(0.0)
            .allow_drag(!self.select_mode)
            .auto_bounds([false, !self.y_lock || self.y_fit > 0]);
        let response = plot.show(ui, |plot| {
            plot.set_auto_bounds([false, !self.y_lock || self.y_fit > 0]);
            if follow || reset {
                plot.set_plot_bounds_x(
                    seconds(interval.start_ns)
                        ..=seconds(interval.end_ns.max(interval.start_ns + 1)),
                );
            }
            if let Some([min, max]) = y_bounds {
                plot.set_plot_bounds_y(min..=max);
            }
            draw(plot, &answer.view, quantity, color::ACCENT);
            if let Some(selection) = selection {
                plot.span(
                    Span::new(
                        "Selection",
                        seconds(selection.start_ns)..=seconds(selection.end_ns),
                    )
                    .fill(color::ACCENT.gamma_multiply(0.18)),
                );
            }
            pointer = plot.pointer_coordinate().map(|p| nanos(p.x));
        });
        let top = ui.cursor().top();
        let bounds = Some(*response.transform.bounds());
        if fresh {
            self.reset_bounds = false;
        }
        self.y_fit = self.y_fit.saturating_sub(1);
        if self.y_lock {
            if let Some(bounds) = bounds {
                self.y_bounds = Some([bounds.min()[1], bounds.max()[1]]);
            }
        } else {
            self.y_bounds = None;
        }
        if self.select_mode {
            if response.response.drag_started() {
                self.drag_start = pointer;
                self.inspect(interval, self.selection);
            }
            if response.response.dragged()
                && let (Some(start), Some(end)) = (self.drag_start, pointer)
            {
                self.selection = Some(Interval::new(start.min(end), start.max(end)));
                self.invalidate();
            }
            if response.response.drag_stopped() {
                self.drag_start = None;
            }
        }
        // However the plot's range moved (drag, scroll, wheel or box zoom, in either mode), the
        // range drawn is the one to query. Not while a new interval waits for its answer, which
        // moves the plot to it.
        if self.drag_start.is_none()
            && !self.reset_bounds
            && let Some(bounds) = bounds
        {
            let shown = Interval::new(nanos(bounds.min()[0]), nanos(bounds.max()[0]));
            let wanted = if follow {
                interval
            } else {
                self.interval.unwrap_or(interval)
            };
            if differs(shown, wanted) {
                self.follow = false;
                self.interval = Some(shown);
                self.invalidate();
            }
        }
        self.overview(ui, &answer, interval, extent);
        if answer.view.lost_pairs > 0 {
            ui.label(
                text(
                    Role::Data,
                    format!("{} pairs lost in this capture", answer.view.lost_pairs),
                )
                .color(color::ATTENTION),
            );
        }
        ui.add_space(space::S);
        self.files(ui);
        // What follows the plot, measured each frame so the plot leaves room for it.
        let footer = ui.cursor().top() - top + space::S;
        if (footer - self.footer).abs() > 0.5 {
            self.footer = footer;
            ui.ctx().request_repaint();
        }
    }

    /// The whole capture, small, with the visible window marked and its edges draggable.
    fn overview(&mut self, ui: &mut Ui, answer: &Answer, interval: Interval, extent: Interval) {
        ui.add_space(space::S);
        let mut overview_pointer = None;
        let overview = Plot::new("energy-overview")
            .height(64.0)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .show_axes([true, false])
            .x_grid_spacer(uniform_grid_spacer(ladder))
            .x_axis_formatter(|mark, _| tick(mark, "s"))
            .include_y(0.0)
            .show_grid(false)
            .show(ui, |plot| {
                plot.set_plot_bounds_x(
                    seconds(extent.start_ns)..=seconds(extent.end_ns.max(extent.start_ns + 1)),
                );
                draw(plot, &answer.overview, self.quantity, color::TEXT_2);
                plot.span(
                    Span::new(
                        "Visible",
                        seconds(interval.start_ns)..=seconds(interval.end_ns),
                    )
                    .fill(color::ACCENT.gamma_multiply(0.2)),
                );
                overview_pointer = plot.pointer_coordinate().map(|p| nanos(p.x));
            });
        if (overview.response.clicked() || overview.response.dragged())
            && let Some(center) = overview_pointer
        {
            self.follow = false;
            let width = interval.duration_ns();
            let start = center.saturating_sub(width / 2).max(extent.start_ns);
            self.interval = Some(Interval::new(start, start.saturating_add(width)));
            self.reset_bounds = true;
            self.invalidate();
        }
        for (is_start, time) in [(true, interval.start_ns), (false, interval.end_ns)] {
            let x = overview.transform.position_from_point_x(seconds(time));
            let rect = egui::Rect::from_center_size(
                egui::pos2(x, overview.response.rect.center().y),
                vec2(10.0, overview.response.rect.height()),
            );
            let handle = ui.interact(
                rect,
                ui.scope_id().with(("overview-handle", is_start)),
                egui::Sense::drag(),
            );
            ui.painter().line_segment(
                [rect.center_top(), rect.center_bottom()],
                Stroke::new(2.0, color::ACCENT),
            );
            if handle.dragged()
                && let Some(position) = handle.interact_pointer_pos()
            {
                let time = nanos(overview.transform.value_from_position(position).x)
                    .clamp(extent.start_ns, extent.end_ns);
                self.interval = Some(if is_start {
                    Interval::new(time.min(interval.end_ns.saturating_sub(1)), interval.end_ns)
                } else {
                    Interval::new(interval.start_ns, time.max(interval.start_ns + 1))
                });
                self.follow = false;
                self.reset_bounds = true;
                self.invalidate();
            }
        }
    }

    fn file(&mut self, action: FileAction) {
        if self.io.is_some() {
            return;
        }
        let path = self.path.clone();
        let interval = self.selection;
        let (send, receive) = mpsc::channel();
        self.io = Some(receive);
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<FileResult> {
                if matches!(action, FileAction::Open) {
                    let selected = rfd::FileDialog::new()
                        .add_filter("Verge capture", &["vergecapture"])
                        .pick_file();
                    return Ok(match selected {
                        Some(path) => {
                            Archive::open(&path)?;
                            FileResult::Open(path)
                        }
                        None => FileResult::Cancelled,
                    });
                }
                let path = path.ok_or_else(|| anyhow::anyhow!("no capture is open"))?;
                if matches!(action, FileAction::Discard) {
                    let agree = rfd::MessageDialog::new()
                        .set_title("Discard capture?")
                        .set_description("This permanently deletes the opened capture from disk.")
                        .set_buttons(rfd::MessageButtons::OkCancel)
                        .show();
                    if agree == rfd::MessageDialogResult::Ok {
                        verge_probe::capture::remove(&path)?;
                        return Ok(FileResult::Discarded(path));
                    }
                    return Ok(FileResult::Cancelled);
                }
                let (extension, title) = match action {
                    FileAction::Export(true) => ("csv", "Export CSV"),
                    FileAction::Export(false) => ("sr", "Export sigrok"),
                    _ => ("vergecapture", "Save capture"),
                };
                let Some(destination) = rfd::FileDialog::new()
                    .set_title(title)
                    .add_filter(title, &[extension])
                    .save_file()
                else {
                    return Ok(FileResult::Cancelled);
                };
                let archive = Archive::open(&path)?;
                match action {
                    FileAction::Save => archive.save(&destination)?,
                    FileAction::Export(csv) => archive.export(
                        &destination,
                        csv,
                        interval.unwrap_or(archive.extent()?),
                        &AtomicBool::new(false),
                    )?,
                    _ => {}
                }
                Ok(FileResult::Done(format!("Saved {}", destination.display())))
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = send.send(result);
        });
    }
}
fn seconds(ns: u64) -> f64 {
    ns as f64 * 1e-9
}
fn nanos(seconds: f64) -> u64 {
    (seconds.max(0.0) * 1e9) as u64
}

/// A tick's value in `unit`, with as many decimals as its step needs.
fn tick(mark: GridMark, unit: &str) -> String {
    let decimals = (-mark.step_size.log10().floor()).max(0.0) as usize;
    format!("{:.decimals$} {unit}", mark.value)
}

/// Whether two ranges are apart by more than the plot's float conversions move one.
fn differs(a: Interval, b: Interval) -> bool {
    let slack = (a.duration_ns().max(b.duration_ns()) / 10_000).max(1);
    a.start_ns.abs_diff(b.start_ns) > slack || a.end_ns.abs_diff(b.end_ns) > slack
}

/// The left panel's width, the Units view's.
const SIDE: f32 = 300.0;

/// Grid steps of one, two or five times a power of ten: the finest visible, the next, and the one
/// two further on, which egui_plot labels where its ticks are far enough apart. Its powers of ten
/// leave a short window or a narrow range with a single labelled tick.
fn ladder(input: GridInput) -> [f64; 3] {
    let next = |at_least: f64| {
        let power = 10f64.powf(at_least.log10().floor());
        [1.0, 2.0, 5.0, 10.0]
            .into_iter()
            .map(|m| m * power)
            .find(|step| *step >= at_least * (1.0 - 1e-9))
            .unwrap_or(10.0 * power)
    };
    let fine = next(input.base_step_size.abs().max(f64::MIN_POSITIVE));
    let medium = next(fine * 1.5);
    [fine, medium, next(next(medium * 1.5) * 1.5)]
}

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

type Reading = fn(Interval, &Statistics) -> String;

/// The measurement rows: a name, and the value from an interval and its statistics.
const READINGS: [(&str, Reading); 9] = [
    ("duration", |interval, _| {
        si(seconds(interval.duration_ns()), "s")
    }),
    ("pairs", |_, s| s.samples.to_string()),
    ("min", |_, s| {
        s.min_na
            .map_or("…".into(), |na| si(f64::from(na) * 1e-9, "A"))
    }),
    ("mean", |_, s| {
        s.mean_current_a().map_or("…".into(), |a| si(a, "A"))
    }),
    ("peak", |_, s| {
        s.max_na
            .map_or("…".into(), |na| si(f64::from(na) * 1e-9, "A"))
    }),
    ("voltage", |_, s| {
        s.mean_voltage_v().map_or("…".into(), |v| si(v, "V"))
    }),
    ("power", |_, s| {
        s.mean_power_w().map_or("…".into(), |w| si(w, "W"))
    }),
    ("charge", |_, s| si(s.charge_c(), "C")),
    ("energy", |_, s| si(s.energy_j(), "J")),
];
fn draw(plot: &mut egui_plot::PlotUi<'_>, view: &View, quantity: Quantity, color: Color32) {
    let mut points = Vec::with_capacity(view.envelopes.len() * 4);
    let mut previous_end = None;
    for bin in &view.envelopes {
        let Some((low, high)) = quantity.extrema(&bin.statistics) else {
            continue;
        };
        if (bin.gap || previous_end != Some(bin.interval.start_ns)) && !points.is_empty() {
            plot.line(
                Line::new("Measured envelope", std::mem::take(&mut points))
                    .color(color)
                    .width(1.2),
            );
        }
        let x = seconds(bin.interval.start_ns + bin.interval.duration_ns() / 2);
        if !bin.gap {
            points.push([seconds(bin.interval.start_ns), low]);
        }
        points.push([x, low]);
        points.push([x, high]);
        if !bin.gap {
            points.push([seconds(bin.interval.end_ns), high]);
        }
        if bin.gap {
            plot.line(
                Line::new("Partial coverage", std::mem::take(&mut points))
                    .color(color)
                    .width(1.2),
            );
            previous_end = None;
        } else {
            previous_end = Some(bin.interval.end_ns);
        }
    }
    if !points.is_empty() {
        plot.line(
            Line::new("Measured envelope", points)
                .color(color)
                .width(1.2),
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};
    use verge_probe::capture::CaptureWriter;
    use verge_probe::stream::{End, EndReason, Header, Message, Pair, Pairs};

    fn fixture(path: &Path) {
        let header:Header=serde_json::from_value(serde_json::json!({"rate":1000,"setting_ma":350,"full_scale_mv":3300,"first_sequence":0,"previous_low":true,
            "low":[{"code":0,"current_nanoamps":0},{"code":65535,"current_nanoamps":350000000}],"high":[],"dio_mask":0,"dio_levels":0,"edge_resolution_ns":0})).unwrap();
        let mut writer = CaptureWriter::create(path).unwrap();
        writer.ingest(Message::Header(header)).unwrap();
        for first in (0..20000u32).step_by(99) {
            let pairs = (first..(first + 99).min(20000))
                .map(|i| Pair {
                    voltage_code: 32768,
                    current_code: if i % 1000 < 30 { 3000 } else { 350 },
                    low: true,
                })
                .collect();
            writer
                .ingest(Message::Pairs(Pairs::new(
                    0,
                    first,
                    u64::from(first) * 1_000_000,
                    pairs,
                )))
                .unwrap();
        }
        writer
            .ingest(Message::End(End {
                reason: EndReason::Stopped,
                last_sequence: 19999,
            }))
            .unwrap();
        writer.finish().unwrap();
    }
    fn state(path: &Path) -> Bench {
        let mut bench = Bench::new(path.parent().unwrap());
        bench.open(path.to_owned());
        bench.selection = Some(Interval::new(12_000_000_000, 14_000_000_000));
        bench.invalidate();
        bench
    }
    fn harness(bench: Bench, wgpu: bool) -> Harness<'static, Bench> {
        let mut builder = Harness::builder()
            .with_size(egui::vec2(1080.0, 700.0))
            .with_context_setup(crate::style::install);
        if wgpu {
            builder = builder.wgpu();
        }
        builder.build_ui_state(
            |ui, b: &mut Bench| b.show(ui, &[], &mut None, &mut crate::worker::Manual::default()),
            bench,
        )
    }
    fn wait(h: &mut Harness<'_, Bench>) {
        for _ in 0..400 {
            h.step();
            if h.state().answer.is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("archive query did not complete");
    }
    #[test]
    fn selection_statistics_stay_fixed_when_graph_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.vergecapture");
        fixture(&path);
        let mut h = harness(state(&path), false);
        wait(&mut h);
        let expected = h.state().answer.as_ref().unwrap().selection.clone();
        assert_eq!(expected.as_ref().unwrap().1.samples, 2000);
        h.state_mut().span = 1_000_000_000;
        h.state_mut().invalidate();
        for _ in 0..100 {
            h.step();
            std::thread::sleep(Duration::from_millis(2));
            if !h.state().pending {
                break;
            }
        }
        assert_eq!(h.state().answer.as_ref().unwrap().selection, expected);
        h.get_by_label("CLEAR").click();
        h.step();
        assert!(h.state().selection.is_none());
    }
    #[test]
    fn superseded_queries_cannot_replace_a_new_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.vergecapture");
        fixture(&path);
        let mut queries = Queries::new();
        for generation in 1..=20 {
            queries.send(Query {
                generation,
                path: path.clone(),
                interval: None,
                live_span: 10_000_000_000,
                selection: Some(Interval::new(
                    generation * 1_000_000,
                    generation * 1_000_000 + 1_000_000,
                )),
                pixels: 64,
                cancel: Arc::new(AtomicBool::new(false)),
            });
        }
        for _ in 0..400 {
            if let Some((generation, result)) = queries.take() {
                assert_eq!(generation, 20);
                assert_eq!(result.unwrap().selection.unwrap().0.start_ns, 20_000_000);
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("no latest query result");
    }
    #[test]
    #[ignore = "renders Bench screenshots with wgpu"]
    fn bench_picture() {
        let dir = PathBuf::from(std::env::var("VERGE_GUI_SHOTS").expect("VERGE_GUI_SHOTS"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("demo.vergecapture");
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        fixture(&path);
        let mut h = harness(state(&path), true);
        wait(&mut h);
        h.step();
        h.render().unwrap().save(dir.join("bench.png")).unwrap();
    }
    #[test]
    fn zooming_the_plot_queries_the_range_it_shows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.vergecapture");
        fixture(&path);
        let mut h = harness(state(&path), false);
        wait(&mut h);
        h.step();
        let before = h.state().answer.as_ref().unwrap().view.interval;
        // In select mode, where only dragging is the selection's, the wheel still zooms.
        h.state_mut().select_mode = true;
        // The middle of the plot, right of the 300-point panel and under the toolbar.
        h.event(egui::Event::PointerMoved(egui::pos2(700.0, 300.0)));
        h.step();
        h.event(egui::Event::Zoom(2.0));
        h.step();
        let wanted = h.state().interval.expect("an interval of its own");
        assert!(!h.state().follow);
        assert!(
            wanted.duration_ns() < before.duration_ns() * 3 / 4,
            "{wanted:?} from {before:?}"
        );
        for _ in 0..200 {
            h.step();
            if h.state().answered == h.state().generation {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(h.state().answer.as_ref().unwrap().view.interval, wanted);
    }
    #[test]
    fn a_capture_that_cannot_be_read_is_asked_for_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.vergecapture");
        std::fs::write(&path, b"").unwrap();
        let mut h = harness(state(&path), false);
        for _ in 0..200 {
            h.step();
            if h.state().query_failed {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(h.state().query_failed);
        for _ in 0..5 {
            h.step();
        }
        assert!(!h.state().pending);
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
    }
    #[test]
    fn switching_devices_clears_confirmations_and_keeps_native_capture() {
        let dir = tempfile::tempdir().unwrap();
        let mut bench = Bench::new(dir.path());
        bench.start_confirm = true;
        bench.off_confirm = true;
        bench.connection = Some("old".into());
        let unit = verge_probe::detect(&verge_probe::sim::SimBus::scenario("ours-pro").unwrap())
            .unwrap()
            .remove(0);
        bench.connect(&unit, &mut crate::worker::Manual::default());
        assert!(!bench.start_confirm);
        assert!(!bench.off_confirm);
        let path = bench.directory.join("new.vergecapture");
        bench.receive(Status {
            instance: unit.device.instance.clone(),
            connected: true,
            recording: true,
            path: Some(path.clone()),
            ..Status::default()
        });
        assert!(bench.recovered.contains(&path));
        assert_eq!(bench.path.as_ref(), Some(&path));
        let generation = bench.generation;
        bench.receive(Status {
            instance: unit.device.instance,
            connected: true,
            recording: false,
            path: Some(path.clone()),
            ..Status::default()
        });
        assert!(bench.generation > generation);
        bench.worker_failed("gone");
        assert!(!bench.recording());
        assert_eq!(bench.path.as_ref(), Some(&path));
    }
}
