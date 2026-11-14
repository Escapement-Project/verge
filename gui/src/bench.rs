//! Pro bench view. Acquisition and archive queries continue independently of frames.

mod query;
use crate::app::{choice, edged, label, outline, sentence, sentence_case};
use crate::bench_worker::{Command, DioChange, Status};
use crate::style::{self, Role, space, text};
use crate::worker::{Backend, Request};
use egui::{Color32, CornerRadius, Frame, Margin, ScrollArea, Stroke, Ui, Vec2, vec2};
use egui_plot::{
    FilledArea, GridInput, GridMark, Line, Plot, PlotUi, Points, Span, uniform_grid_spacer,
};
use escapement_verge_probe::capture::{
    Archive, Envelope, Interval, Statistics, Supported, recovered_sessions,
};
use escapement_verge_probe::{EnergyRanges, Unit, pro};
use query::{Answer, Queries, Query};
use std::ops::RangeInclusive;
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
    fn value(self, s: &Supported) -> f64 {
        let (millivolts, nanoamps) = (f64::from(s.sample.millivolts), f64::from(s.sample.nanoamps));
        match self {
            Self::Current => nanoamps * 1e-6,
            Self::Voltage => millivolts * 1e-3,
            Self::Power => millivolts * nanoamps * 1e-9,
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
}

pub struct Bench {
    pub status: Option<Status>,
    connection: Option<String>,
    rate: u32,
    /// The current ranges for a capture the panel starts.
    ranges: EnergyRanges,
    path: Option<PathBuf>,
    directory: PathBuf,
    recovered: Vec<PathBuf>,
    queries: Queries,
    generation: u64,
    /// The last query failed: asking again waits for something to change.
    query_failed: bool,
    pending: bool,
    dirty: bool,
    queried_at: Instant,
    answer: Option<Arc<Answer>>,
    interval: Option<Interval>,
    selection: Option<Interval>,
    marker_after: Option<escapement_verge_probe::capture::MarkerCursor>,
    marker_selected: Option<escapement_verge_probe::capture::MarkerCursor>,
    span: u64,
    follow: bool,
    quantity: Quantity,
    select_mode: bool,
    drag_start: Option<u64>,
    /// Move the plot to `interval`, or to the live window, on the next frame.
    reset_bounds: bool,
    /// Y follows the visible data until the user moves it.
    y_fit: bool,
    /// The vertical travel of the drag or scroll under way while Y fits.
    travel: f32,
    /// The travel that took Y off the fit, applied on the next frame.
    carry: f32,
    io: Option<Receiver<Result<FileResult, String>>>,
    failed: bool,
    notice: Option<String>,
    start_confirm: bool,
    off_confirm: bool,
    /// The height under the plot, measured on the frame before.
    footer: f32,
    /// The plot as last drawn.
    #[cfg(test)]
    shown: Option<egui_plot::PlotTransform>,
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
            rate: 25_000,
            ranges: EnergyRanges::Auto,
            path: None,
            directory,
            recovered: Vec::new(),
            queries: Queries::new(),
            generation: 0,
            query_failed: false,
            pending: false,
            dirty: true,
            queried_at: Instant::now(),
            answer: None,
            interval: None,
            selection: None,
            marker_after: None,
            marker_selected: None,
            span: 10_000_000_000,
            follow: true,
            quantity: Quantity::Current,
            select_mode: false,
            drag_start: None,
            reset_bounds: true,
            y_fit: true,
            travel: 0.0,
            carry: 0.0,
            io: Some(rx),
            failed: false,
            notice: None,
            start_confirm: false,
            off_confirm: false,
            footer: 120.0,
            #[cfg(test)]
            shown: None,
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
    /// The range on screen: the live window, or where the user moved the plot, which the last
    /// answer may not have caught up with.
    fn current(&self) -> Option<Interval> {
        let answer = self.answer.as_ref()?;
        Some(if self.follow {
            answer.visible
        } else {
            self.interval.unwrap_or(answer.visible)
        })
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
        self.marker_after = None;
        self.marker_selected = None;
        self.interval = None;
        self.selection = None;
        self.answer = None;
        self.follow = true;
        self.reset_bounds = true;
        self.y_fit = true;
        self.invalidate();
    }
    /// What is asked for changed: answers to earlier questions are dropped.
    fn invalidate(&mut self) {
        self.query_failed = false;
        self.queries.cancel();
        self.generation += 1;
        self.pending = false;
        self.dirty = true;
    }
    /// The range moved. The query under way finishes and draws, and the next one asks for where
    /// the range is then: cancelling on every frame of a pan would leave nothing drawn until it
    /// stopped.
    fn requery(&mut self) {
        self.query_failed = false;
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
                Ok(answer) => self.answer = Some(Arc::new(answer)),
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
                marker_after: self.marker_after,
                marker_selected: self.marker_selected,
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
                            .map(|e| e.to_string())
                    })
                })
                .unwrap_or_else(|| "Select an available Pro running Verge to use DIO.".into());
            ui.add_enabled(false, egui::Label::new(text(Role::Body, reason)));
            return;
        };
        for group in &state.groups {
            let name = if *group == 7 { "DIO0–2" } else { "DIO3" };
            ui.horizontal(|ui| {
                ui.label(text(Role::Code, name));
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
        ui.label(
            text(
                Role::Body,
                "Inputs have pull-downs. Outputs read their drive.",
            )
            .color(face.ink_2),
        );
    }

    fn markers(&mut self, ui: &mut Ui) {
        let Some(answer) = self.answer.clone() else {
            return;
        };
        let page = &answer.marker_list;
        if page.total == 0 && page.loss_events == 0 {
            return;
        }
        let face = style::face(ui.ctx());
        ui.add_space(space::L);
        label(ui, "Markers");
        ui.label(text(Role::Figures, format!("{} received", page.total)).color(face.ink));
        if page.loss_events > 0 {
            ui.label(
                text(
                    Role::Body,
                    format!(
                        "{} marker-loss {}. Some markers are missing.",
                        page.loss_events,
                        if page.loss_events == 1 {
                            "report"
                        } else {
                            "reports"
                        }
                    ),
                )
                .color(face.attention_ink),
            );
        }
        ScrollArea::vertical()
            .id_salt("marker-list")
            .max_height(150.0)
            .show(ui, |ui| {
                for entry in &page.entries {
                    let cursor = escapement_verge_probe::capture::MarkerCursor {
                        time_ns: entry.marker.time_ns,
                        id: entry.id,
                    };
                    let words = format!(
                        "{:.6}s · {} · {}",
                        seconds(entry.marker.time_ns),
                        entry.marker.number,
                        escapement_verge_probe::record::marker_label(&entry.marker.label)
                    );
                    let response = ui.add(
                        egui::Button::new(text(Role::Code, words))
                            .selected(self.marker_selected == Some(cursor))
                            .wrap(),
                    );
                    if response.clicked() {
                        self.marker_selected = Some(cursor);
                        let width = self
                            .current()
                            .map_or(self.span, Interval::duration_ns)
                            .max(1);
                        let start = cursor.time_ns.saturating_sub(width / 2);
                        self.inspect(
                            Interval::new(start, start.saturating_add(width)),
                            self.selection,
                        );
                    }
                    if entry.lost_before {
                        ui.label(
                            text(Role::Body, "Markers lost before this one")
                                .color(face.attention_ink),
                        );
                    }
                }
            });
        ui.horizontal(|ui| {
            if outline(ui, "First", self.marker_after.is_some()).clicked() {
                self.marker_after = None;
                self.invalidate();
            }
            if outline(ui, "Next", page.next.is_some()).clicked() {
                self.marker_after = page.next;
                self.invalidate();
            }
        });
        let successor = answer
            .marker_successor
            .as_ref()
            .filter(|_| answer.marker_selected == self.marker_selected);
        if outline(
            ui,
            "Select to next marker",
            self.marker_selected.is_some() && successor.is_some(),
        )
        .clicked()
            && let (Some(start), Some(end)) = (self.marker_selected, successor)
        {
            self.selection = Some(Interval::new(start.time_ns, end.marker.time_ns));
            self.invalidate();
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
                                "Reads the target undisturbed, but currents under about 0.36 mA \
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
        let Some(answer) = self.answer.clone() else {
            return;
        };
        let face = style::face(ui.ctx());
        ui.add_space(space::L);
        label(ui, "Measurements");
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
                ui.label(text(Role::Body, "to").color(face.ink_2));
                let to = ui.add(
                    egui::DragValue::new(&mut end)
                        .speed(0.001)
                        .max_decimals(6)
                        .suffix(" s"),
                );
                if from.changed() || to.changed() {
                    selection = Interval::new(nanos(start), nanos(end));
                    self.selection = Some(selection);
                    self.requery();
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
        let window = (answer.visible, answer.window.clone());
        let selection = self.selection.map(|selection| match &answer.selection {
            Some((queried, statistics)) if *queried == selection => {
                Some((selection, statistics.clone()))
            }
            // The answer for this selection has not come back yet.
            _ => None,
        });
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
                let columns: Vec<Option<&(Interval, Statistics)>> = std::iter::once(Some(&window))
                    .chain(selection.as_ref().map(Option::as_ref))
                    .collect();
                for (name, value) in READINGS {
                    label(ui, &sentence_case(name));
                    for column in &columns {
                        let words = column.map_or_else(
                            || "…".to_owned(),
                            |(interval, statistics)| value(*interval, statistics),
                        );
                        ui.label(text(Role::Figures, words).color(face.ink));
                    }
                    ui.end_row();
                }
                label(ui, "Unmeasured");
                for column in &columns {
                    let missing = column.map(|(interval, statistics)| {
                        unmeasured_ns(*interval, answer.view.extent, statistics)
                    });
                    ui.label(match missing {
                        Some(0) => text(Role::Figures, "none").color(face.ink_2),
                        Some(ns) => {
                            text(Role::Figures, si(seconds(ns), "s")).color(face.attention_ink)
                        }
                        None => text(Role::Figures, "…").color(face.ink),
                    });
                }
                ui.end_row();
            });
    }

    /// The open capture's name and what can be done with it, under the graph.
    fn files(&mut self, ui: &mut Ui) {
        let face = style::face(ui.ctx());
        let idle = self.io.is_none();
        let open = self.path.is_some();
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
            if outline(ui, "Save as…", idle && open).clicked() {
                self.file(FileAction::Save);
            }
            ui.add_space(space::S);
            label(
                ui,
                if self.selection.is_some() {
                    "Export selection"
                } else {
                    "Export"
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
        let name = label(ui, "Kept on disk");
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
        if let Some(change) = self.status.as_ref().and_then(|s| s.confirm_dio) {
            let group = if change.mask & 7 != 0 {
                "DIO0–2"
            } else {
                "DIO3"
            };
            let warning = "Check that the target is not driving these pins. The probe cannot detect contention.";
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
        ui.horizontal_wrapped(|ui| {
            for quantity in [Quantity::Current, Quantity::Voltage, Quantity::Power] {
                if choice(ui, quantity.word(), self.quantity == quantity).clicked()
                    && self.quantity != quantity
                {
                    self.quantity = quantity;
                    self.y_fit = true;
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
                self.interval = self.current();
                self.invalidate();
            }
            ui.add_space(space::M);
            for seconds in [1, 10, 60] {
                let span = seconds * 1_000_000_000;
                if choice(ui, &format!("{seconds}s"), self.follow && self.span == span).clicked() {
                    self.span = span;
                    self.reset_bounds = true;
                    self.interval = self.current().map(|current| {
                        Interval::new(current.end_ns.saturating_sub(self.span), current.end_ns)
                    });
                    self.invalidate();
                }
            }
            ui.add_space(space::M);
            if choice(ui, "Select", self.select_mode).clicked() {
                self.select_mode = !self.select_mode;
            }
            if choice(ui, "Fit Y", self.y_fit).clicked() {
                self.y_fit = !self.y_fit;
            }
        });
        ui.add_space(space::S);
    }

    fn graph(&mut self, ui: &mut Ui) {
        let face = style::face(ui.ctx());
        // The plots stand on the surface, as the measurements page's charts do.
        ui.visuals_mut().extreme_bg_color = face.surface;
        self.toolbar(ui);
        let Some(answer) = self.answer.clone() else {
            // A selection drag under way ends with the plot it was on.
            self.drag_start = None;
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
        let visible = answer.visible;
        let extent = answer.view.extent;
        let quantity = self.quantity;
        let selection = self.selection;
        let follow = self.follow;
        let target = if follow {
            Some(visible)
        } else {
            self.reset_bounds.then(|| self.interval.unwrap_or(visible))
        };
        let fit = self.y_fit;
        let carry = std::mem::take(&mut self.carry);
        let mut fitted = None;
        let mut pointer = None;
        let plot = Plot::new("energy")
            .height((ui.available_height() - self.footer).max(160.0))
            .x_grid_spacer(uniform_grid_spacer(ladder))
            .y_grid_spacer(uniform_grid_spacer(ladder))
            .x_axis_formatter(|mark, _| tick(mark, "s"))
            .y_axis_formatter(move |mark, _| tick(mark, quantity.unit()))
            .y_axis_min_width(56.0)
            .grid_color(face.hair)
            .auto_bounds(false)
            .allow_double_click_reset(false)
            // The wheel zooms time; dragging the Y axis zooms Y.
            .allow_zoom([true, false])
            // While Y fits, a drag or the wheel moves time only, until it passes the detent.
            .allow_drag([!self.select_mode, !self.select_mode && !fit])
            .allow_scroll([true, !fit]);
        let response = plot.show(ui, |plot| {
            let x = match target {
                Some(interval) => {
                    let x = seconds(interval.start_ns)
                        ..=seconds(interval.end_ns.max(interval.start_ns + 1));
                    plot.set_plot_bounds_x(x.clone());
                    x
                }
                None => plot.plot_bounds().range_x(),
            };
            if fit {
                let rows = label_rows(&answer.markers);
                let height = plot.transform().frame().height();
                fitted =
                    y_range(&answer, quantity, &x).map(|range| label_headroom(range, rows, height));
                if let Some(range) = fitted.clone() {
                    plot.set_plot_bounds_y(range);
                }
            } else if carry != 0.0 {
                let shift = -f64::from(carry) * plot.transform().dvalue_dpos()[1];
                plot.translate_bounds(vec2(0.0, shift as f32));
            }
            lost_bands(plot, &answer.overview.lost, face);
            let verge = face.series_verge;
            draw(plot, &answer.fallback, quantity, verge);
            match &answer.view.samples {
                Some(samples) => draw_samples(plot, samples, quantity, verge, &x),
                None => draw(plot, &answer.view.envelopes, quantity, verge),
            }
            if let Some(selection) = selection {
                plot.span(
                    Span::new(
                        "Selection",
                        seconds(selection.start_ns)..=seconds(selection.end_ns),
                    )
                    .fill(face.wash()),
                );
            }
            pointer = plot.pointer_coordinate().map(|p| nanos(p.x));
        });
        let top = ui.cursor().top();
        let bounds = *response.transform.bounds();
        #[cfg(test)]
        {
            self.shown = Some(response.transform);
        }
        self.reset_bounds = false;
        self.y_gesture(ui, &response.response, fitted, bounds.range_y());
        let shown = Interval::new(nanos(bounds.min()[0]), nanos(bounds.max()[0]));
        if self.select_mode {
            if response.response.drag_started() {
                self.drag_start = pointer;
                self.inspect(shown, self.selection);
            }
            if response.response.dragged()
                && let (Some(start), Some(end)) = (self.drag_start, pointer)
            {
                self.selection = Some(Interval::new(start.min(end), start.max(end)));
                self.requery();
            }
            if response.response.drag_stopped() {
                self.drag_start = None;
            }
        }
        // However the plot's range moved (drag, scroll, wheel or box zoom, in either mode), the
        // range drawn is the one to query.
        if self.drag_start.is_none() {
            let wanted = if follow {
                visible
            } else {
                self.interval.unwrap_or(visible)
            };
            if differs(shown, wanted) {
                self.follow = false;
                self.interval = Some(shown);
                self.requery();
            }
        }
        self.draw_markers(ui, &answer.markers, &response.transform);
        self.digital_lanes(
            ui,
            &answer.digital,
            &answer.digital_fallback,
            &response.transform,
        );
        self.overview(ui, &answer, self.current().unwrap_or(visible), extent);
        if answer.high_range_only {
            ui.label(
                text(
                    Role::Body,
                    "This capture held the high range: currents under about 0.36 mA read unreliably",
                )
                .color(face.ink_2),
            );
        }
        if answer.view.lost_pairs > 0 {
            ui.label(
                text(
                    Role::Body,
                    format!("{} pairs lost in this capture", answer.view.lost_pairs),
                )
                .color(face.attention_ink),
            );
        }
        if answer.digital.loss_events > 0 {
            ui.label(
                text(
                    Role::Body,
                    format!(
                        "{} DIO edge-loss {} in this capture",
                        answer.digital.loss_events,
                        if answer.digital.loss_events == 1 {
                            "report"
                        } else {
                            "reports"
                        }
                    ),
                )
                .color(face.attention_ink),
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

    fn draw_markers(
        &self,
        ui: &mut Ui,
        page: &escapement_verge_probe::capture::MarkerPage,
        transform: &egui_plot::PlotTransform,
    ) {
        let face = style::face(ui.ctx());
        let frame = *transform.frame();
        let painter = ui.painter().with_clip_rect(frame);
        let mut occupied = [frame.left() - 1.0; LABEL_ROWS];
        // Drawn once every line is, so that no marker's line crosses another's label.
        let mut labels = Vec::new();
        for entry in &page.entries {
            let x = transform
                .position_from_point(&egui_plot::PlotPoint::new(
                    seconds(entry.marker.time_ns),
                    0.0,
                ))
                .x;
            if x < frame.left() || x > frame.right() {
                continue;
            }
            let line = [egui::pos2(x, frame.top()), egui::pos2(x, frame.bottom())];
            painter.line_segment(line, Stroke::new(3.0, face.surface));
            painter.line_segment(line, Stroke::new(1.0, face.ink));
            let label = format!(
                "{} {}",
                entry.marker.number,
                escapement_verge_probe::record::marker_label(&entry.marker.label)
            );
            let galley =
                painter.layout_no_wrap(label.clone(), egui::FontId::monospace(11.0), face.ink);
            let left = (x + 3.0)
                .min(frame.right() - galley.size().x)
                .max(frame.left());
            if let Some((row, right)) = occupied
                .iter_mut()
                .enumerate()
                .find(|(_, right)| **right < left)
            {
                let pos = egui::pos2(left, frame.top() + LABEL_TOP + row as f32 * LABEL_ROW);
                *right = left + galley.size().x + 8.0;
                labels.push((pos, galley));
            }
            ui.interact(
                egui::Rect::from_min_max(
                    egui::pos2(x - 3.0, frame.top()),
                    egui::pos2(x + 3.0, frame.bottom()),
                ),
                ui.scope_id().with(("marker", entry.id)),
                egui::Sense::hover(),
            )
            .on_hover_text(format!("{:.6}s · {label}", seconds(entry.marker.time_ns)));
        }
        for (pos, galley) in labels {
            painter.rect_filled(
                egui::Rect::from_min_size(pos, galley.size()),
                0,
                face.surface,
            );
            painter.galley(pos, galley, face.ink);
        }
        if page.next.is_some() {
            painter.text(
                frame.left_bottom() + vec2(6.0, -6.0),
                egui::Align2::LEFT_BOTTOM,
                "More markers: zoom in or use the list",
                egui::FontId::proportional(12.0),
                face.ink_2,
            );
        }
    }

    fn digital_lanes(
        &self,
        ui: &mut Ui,
        digital: &escapement_verge_probe::capture::DigitalView,
        fallback: &[escapement_verge_probe::capture::DigitalBin],
        transform: &egui_plot::PlotTransform,
    ) {
        if digital.pins == 0 {
            return;
        }
        let face = style::face(ui.ctx());
        let count = digital.pins.count_ones() as f32;
        let (rect, _) = ui.allocate_exact_size(
            vec2(ui.available_width(), count * 24.0),
            egui::Sense::hover(),
        );
        let frame = *transform.frame();
        let clip = egui::Rect::from_min_max(
            egui::pos2(frame.left(), rect.top()),
            egui::pos2(frame.right(), rect.bottom()),
        );
        let painter = ui.painter().with_clip_rect(clip);
        let x = |time| {
            transform
                .position_from_point(&egui_plot::PlotPoint::new(seconds(time), 0.0))
                .x
        };
        let mut row = 0;
        for pin in 0..4 {
            let bit = 1 << pin;
            if digital.pins & bit == 0 {
                continue;
            }
            let top = rect.top() + row as f32 * 24.0 + 3.0;
            row += 1;
            ui.painter().text(
                egui::pos2(frame.left() - 8.0, top + 7.0),
                egui::Align2::RIGHT_CENTER,
                format!("DIO{pin}"),
                egui::FontId::monospace(11.0),
                face.ink_2,
            );
            let mut previous: Option<(f32, f32)> = None;
            for bin in fallback.iter().chain(&digital.bins) {
                let left = x(bin.interval.start_ns);
                let right = x(bin.interval.end_ns);
                if right < clip.left() || left > clip.right() {
                    previous = None;
                    continue;
                }
                if bin.gap || (bin.high | bin.low) & bit == 0 {
                    previous = None;
                    if bin.gap {
                        painter.rect_filled(
                            egui::Rect::from_min_max(
                                egui::pos2(left, top),
                                egui::pos2(right, top + 14.0),
                            ),
                            0,
                            face.attention_fill.gamma_multiply(0.2),
                        );
                    }
                    continue;
                }
                let high = bin.high & bit != 0;
                let low = bin.low & bit != 0;
                let y = top + if high { 0.0 } else { 14.0 };
                if let Some((last_x, last_y)) = previous
                    && (last_x - left).abs() < 0.5
                {
                    painter.line_segment(
                        [egui::pos2(left, last_y), egui::pos2(left, y)],
                        Stroke::new(1.0, face.series_verge),
                    );
                }
                if high && low {
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(left, top),
                            egui::pos2(right.max(left + 1.0), top + 14.0),
                        ),
                        0,
                        face.series_verge.gamma_multiply(0.6),
                    );
                    previous = None;
                } else {
                    painter.line_segment(
                        [egui::pos2(left, y), egui::pos2(right, y)],
                        Stroke::new(1.0, face.series_verge),
                    );
                    previous = Some((right, y));
                }
            }
        }
    }

    /// Takes Y off the fit once the user moves it: a drag or the wheel past the detent, a drag on
    /// the Y axis, or a box zoom. A double click fits it again.
    fn y_gesture(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        fitted: Option<RangeInclusive<f64>>,
        shown: RangeInclusive<f64>,
    ) {
        if response.double_clicked() {
            self.y_fit = true;
            return;
        }
        if !self.y_fit {
            self.travel = 0.0;
            return;
        }
        if let Some(fitted) = fitted
            && (fitted.start() - shown.start()).abs() + (fitted.end() - shown.end()).abs()
                > (fitted.end() - fitted.start()) * 1e-6
        {
            self.y_fit = false;
            return;
        }
        let dragging = response.dragged_by(egui::PointerButton::Primary) && !self.select_mode;
        let scroll = if response.contains_pointer() {
            ui.input(|i| i.smooth_scroll_delta)
        } else {
            Vec2::ZERO
        };
        let delta = scroll
            + if dragging {
                response.drag_delta()
            } else {
                Vec2::ZERO
            };
        if !dragging && scroll == Vec2::ZERO {
            self.travel = 0.0;
        } else if delta.y.abs() > delta.x.abs() {
            self.travel += delta.y;
            if self.travel.abs() > DETENT {
                self.y_fit = false;
                self.carry = std::mem::take(&mut self.travel);
            }
        }
    }

    /// The whole capture, small, with the visible window marked and its edges draggable.
    fn overview(&mut self, ui: &mut Ui, answer: &Answer, interval: Interval, extent: Interval) {
        let face = style::face(ui.ctx());
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
                lost_bands(plot, &answer.overview.lost, face);
                draw(
                    plot,
                    &answer.overview.envelopes,
                    self.quantity,
                    face.series_verge,
                );
                // Unnamed: 64 points leave no room for its label.
                plot.span(
                    Span::new("", seconds(interval.start_ns)..=seconds(interval.end_ns))
                        .fill(face.wash()),
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
            self.requery();
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
                Stroke::new(2.0, face.ink),
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
                self.requery();
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
                        escapement_verge_probe::capture::remove(&path)?;
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
/// Vertical travel, in points, before a drag or the wheel takes Y off the fit, so a pan along time
/// that wanders a little leaves Y fitted.
const DETENT: f32 = 12.0;

/// The opacity of a column some of whose pairs were lost, or of a pair read under range, against a
/// whole one.
const PARTLY_MEASURED: f32 = 0.35;

/// The time in `interval` that the capture spans and has no reading for. A window reaching before
/// the capture's start or past its end has nothing to measure there.
fn unmeasured_ns(interval: Interval, extent: Interval, statistics: &Statistics) -> u64 {
    let start = interval.start_ns.max(extent.start_ns);
    let end = interval.end_ns.min(extent.end_ns);
    end.saturating_sub(start)
        .saturating_sub(statistics.measured_ns)
}

/// Marker labels sit in rows across the top of the plot.
const LABEL_ROWS: usize = 3;
const LABEL_TOP: f32 = 4.0;
const LABEL_ROW: f32 = 16.0;

/// The label rows to keep clear: all of them once the capture has a marker, so the fitted range
/// does not change as markers scroll in and out of view.
fn label_rows(page: &escapement_verge_probe::capture::MarkerPage) -> usize {
    if page.total > 0 { LABEL_ROWS } else { 0 }
}

/// `range` raised so that its top lies below `rows` of marker labels in a plot `height` px tall.
fn label_headroom(range: RangeInclusive<f64>, rows: usize, height: f32) -> RangeInclusive<f64> {
    let band = f64::from(LABEL_TOP + rows as f32 * LABEL_ROW);
    let height = f64::from(height);
    if rows == 0 || height <= 2.0 * band {
        return range;
    }
    let share = band / height;
    let (low, high) = (*range.start(), *range.end());
    low..=(high - low * share) / (1.0 - share)
}

/// The Y range that shows every bin in `x` and zero, with a margin.
fn y_range(
    answer: &Answer,
    quantity: Quantity,
    x: &RangeInclusive<f64>,
) -> Option<RangeInclusive<f64>> {
    let (start, end) = (nanos(*x.start()), nanos(*x.end()));
    let (low, high) = answer
        .view
        .envelopes
        .iter()
        .chain(&answer.fallback)
        .filter(|bin| bin.interval.end_ns > start && bin.interval.start_ns < end)
        .filter_map(|bin| quantity.extrema(&bin.statistics))
        .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))?;
    let (low, high) = (low.min(0.0), high.max(0.0));
    let margin = if high > low { (high - low) * 0.05 } else { 1.0 };
    Some(low - margin..=high + margin)
}

/// A line as drawn: every point kept. egui_plot otherwise merges points less than a pixel apart into
/// a filled band, which turns a step's vertical edge into a wedge reaching the next pixel.
fn trace<'a>(line: Line<'a>, color: Color32) -> Line<'a> {
    line.color(color).width(1.2).allow_band(false)
}

/// Each bin's range, from its least reading to its greatest, filled and edged. A pulse narrower
/// than a bin still reaches its height in the bin that holds it, where a bin's mean would depend on
/// how much of the pulse falls inside. A bin not wholly measured is a run of its own, so a gap
/// narrower than a column still breaks the line. It draws pale if pairs were lost in it, and at
/// full strength if the capture only paused, as it does while a debugger holds the conversions.
fn draw(plot: &mut PlotUi<'_>, bins: &[Envelope], quantity: Quantity, color: Color32) {
    let mut first = 0;
    for next in 1..=bins.len() {
        let joined = next < bins.len()
            && !bins[next - 1].gap
            && !bins[next].gap
            && bins[next - 1].interval.end_ns == bins[next].interval.start_ns;
        if joined {
            continue;
        }
        let color = if bins[first].lost {
            color.gamma_multiply(PARTLY_MEASURED)
        } else {
            color
        };
        let mut xs = Vec::new();
        let mut lows = Vec::new();
        let mut highs = Vec::new();
        for bin in &bins[first..next] {
            let Some((low, high)) = quantity.extrema(&bin.statistics) else {
                continue;
            };
            for time in [bin.interval.start_ns, bin.interval.end_ns] {
                xs.push(seconds(time));
                lows.push(low);
                highs.push(high);
            }
        }
        first = next;
        if xs.is_empty() {
            continue;
        }
        plot.add(
            FilledArea::new("Range", &xs, &lows, &highs)
                .fill_color(color.gamma_multiply(0.45))
                .allow_hover(false),
        );
        for (name, edge) in [("Least", &lows), ("Greatest", &highs)] {
            let points: Vec<_> = xs.iter().zip(edge.iter()).map(|(&x, &y)| [x, y]).collect();
            plot.line(trace(Line::new(name, points), color));
        }
    }
}

/// A band over each span of lost pairs, so that a loss reads apart from a pause at any zoom: both
/// break the trace, and only a loss is marked. Spans less than a pixel apart share one band, so a
/// capture that lost pairs thousands of times draws no more bands than the plot has pixels.
fn lost_bands(plot: &mut PlotUi<'_>, lost: &[Interval], face: &style::Face) {
    let pixel = plot.transform().dvalue_dpos()[0].abs();
    let shown = plot.plot_bounds().range_x();
    let mut bands: Vec<(f64, f64)> = Vec::new();
    for span in lost {
        let (start, end) = (seconds(span.start_ns), seconds(span.end_ns));
        if end < *shown.start() || start > *shown.end() {
            continue;
        }
        match bands.last_mut() {
            Some(band) if start - band.1 < pixel => band.1 = end,
            _ => bands.push((start, end)),
        }
    }
    for (start, end) in bands {
        // Unnamed, so egui_plot draws no label over the marker rows.
        plot.span(Span::new("", start..=end).fill(face.attention_fill.gamma_multiply(0.2)));
    }
}

/// Each sample held for the time it covers, with a mark where it starts once the marks are far
/// enough apart to tell. A sample read under range, too near the high range's floor to trust,
/// draws pale.
fn draw_samples(
    plot: &mut PlotUi<'_>,
    samples: &[Supported],
    quantity: Quantity,
    color: Color32,
    x: &RangeInclusive<f64>,
) {
    let pale = color.gamma_multiply(PARTLY_MEASURED);
    let mut held = Vec::new();
    let mut held_under = false;
    let mut previous_end = None;
    for sample in samples {
        let under = sample.sample.under_range;
        if (previous_end != Some(sample.sample.time_ns) || under != held_under) && !held.is_empty()
        {
            plot.line(trace(
                Line::new("Samples", std::mem::take(&mut held)),
                if held_under { pale } else { color },
            ));
        }
        held_under = under;
        let value = quantity.value(sample);
        held.push([seconds(sample.sample.time_ns), value]);
        held.push([seconds(sample.end_ns), value]);
        previous_end = Some(sample.end_ns);
    }
    if !held.is_empty() {
        plot.line(trace(
            Line::new("Samples", held),
            if held_under { pale } else { color },
        ));
    }
    let shown = samples
        .iter()
        .filter(|s| (x.start()..=x.end()).contains(&&seconds(s.sample.time_ns)))
        .count();
    if shown > 0 && f64::from(plot.transform().frame().width()) / shown as f64 >= 6.0 {
        for (under, color) in [(false, color), (true, pale)] {
            let marks: Vec<_> = samples
                .iter()
                .filter(|s| s.sample.under_range == under)
                .map(|s| [seconds(s.sample.time_ns), quantity.value(s)])
                .collect();
            if !marks.is_empty() {
                plot.points(Points::new("Samples", marks).color(color).radius(2.5));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};
    use escapement_verge_probe::capture::CaptureWriter;
    use escapement_verge_probe::stream::{End, EndReason, Header, Message, Pair, Pairs};

    /// `count` pairs at `rate`, each pair's current code from `code`; 65535 is 350 mA.
    fn capture(path: &Path, rate: u32, count: u32, code: impl Fn(u32) -> u16) {
        capture_with_dio(path, rate, count, code, false);
    }
    fn capture_with_dio(path: &Path, rate: u32, count: u32, code: impl Fn(u32) -> u16, dio: bool) {
        capture_events(path, rate, count, code, dio, &[]);
    }
    fn capture_events(
        path: &Path,
        rate: u32,
        count: u32,
        code: impl Fn(u32) -> u16,
        dio: bool,
        markers: &[escapement_verge_probe::stream::Marker],
    ) {
        let header: Header = serde_json::from_value(serde_json::json!({"clock_hz":rate,"period":1,"setting_ma":350,"full_scale_mv":3300,"first_sequence":0,"previous_low":true,
            "low":[{"code":0,"current_nanoamps":0},{"code":65535,"current_nanoamps":350000000}],"high":[],"dio_mask":if dio {15} else {0},"dio_levels":if code(0)>1000 {8} else {0},"edge_resolution_ns":11})).unwrap();
        let period = 1_000_000_000 / u64::from(rate);
        let mut writer = CaptureWriter::create(path).unwrap();
        writer.ingest(Message::Header(header)).unwrap();
        let mut last_levels = if code(0) > 1000 { 8 } else { 0 };
        let mut sent_ns = 0;
        for first in (0..count).step_by(99) {
            let pairs = (first..(first + 99).min(count))
                .map(|i| Pair {
                    voltage_code: 32768,
                    current_code: code(i),
                    low: true,
                    under_range: false,
                })
                .collect();
            writer
                .ingest(Message::Pairs(Pairs::new(
                    0,
                    first,
                    u64::from(first) * period,
                    pairs,
                )))
                .unwrap();
            if dio {
                let mut edges = Vec::new();
                for i in first..(first + 99).min(count) {
                    let levels = if code(i) > 1000 { 8 } else { 0 };
                    if levels != last_levels {
                        edges.push(escapement_verge_probe::stream::Edge {
                            time_ns: u64::from(i) * period,
                            levels,
                        });
                        last_levels = levels;
                    }
                }
                if !edges.is_empty() {
                    let newest = edges.last().unwrap().time_ns;
                    // Edges lost before the message holding pair 20,050: unknown from the last edge
                    // sent to what would have been this message's first, which the loss stands for.
                    if first <= 20_050 && first + 99 > 20_050 {
                        let lost = edges.remove(0);
                        writer
                            .ingest(Message::DioLost(escapement_verge_probe::stream::DioLost {
                                from_ns: sent_ns,
                                to_ns: lost.time_ns,
                                levels: lost.levels,
                            }))
                            .unwrap();
                    }
                    if !edges.is_empty() {
                        writer
                            .ingest(Message::Edges(escapement_verge_probe::stream::Edges::new(
                                0, edges,
                            )))
                            .unwrap();
                    }
                    sent_ns = newest;
                }
            }
        }
        for (index, chunk) in markers.chunks(11).enumerate() {
            writer
                .ingest(Message::Markers(
                    escapement_verge_probe::stream::Markers::new(
                        if index == 0 {
                            escapement_verge_probe::stream::Markers::LOST
                        } else {
                            0
                        },
                        chunk.to_vec(),
                    ),
                ))
                .unwrap();
        }
        writer
            .ingest(Message::End(End {
                reason: EndReason::Stopped,
                last_sequence: count - 1,
            }))
            .unwrap();
        writer.finish().unwrap();
    }
    /// Like [`capture`], with the pairs in `lost` never arriving.
    fn capture_lossy(
        path: &Path,
        rate: u32,
        count: u32,
        code: impl Fn(u32) -> u16,
        lost: &[std::ops::Range<u32>],
    ) {
        capture_gaps(path, rate, count, code, lost, &[], 0);
    }
    /// Like [`capture_lossy`], with the conversions also held for `hold_ns` before each pair in
    /// `held`, as a debugger connecting does: time jumps, and no pair goes missing.
    fn capture_gaps(
        path: &Path,
        rate: u32,
        count: u32,
        code: impl Fn(u32) -> u16,
        lost: &[std::ops::Range<u32>],
        held: &[u32],
        hold_ns: u64,
    ) {
        let header: Header = serde_json::from_value(serde_json::json!({"clock_hz":rate,"period":1,"setting_ma":350,"full_scale_mv":3300,"first_sequence":0,"previous_low":true,
            "low":[{"code":0,"current_nanoamps":0},{"code":65535,"current_nanoamps":350000000}],"high":[],"dio_mask":0,"dio_levels":0,"edge_resolution_ns":11})).unwrap();
        let period = 1_000_000_000 / u64::from(rate);
        let time = |i: u32| {
            u64::from(i) * period + held.iter().filter(|&&at| at <= i).count() as u64 * hold_ns
        };
        let mut writer = CaptureWriter::create(path).unwrap();
        writer.ingest(Message::Header(header)).unwrap();
        let mut first = 0;
        while first < count {
            if let Some(gap) = lost.iter().find(|gap| gap.contains(&first)) {
                first = gap.end;
                continue;
            }
            let next_break = lost
                .iter()
                .map(|gap| gap.start)
                .chain(held.iter().copied())
                .filter(|&start| start > first)
                .min()
                .unwrap_or(count);
            let end = (first + 99).min(count).min(next_break);
            let pairs = (first..end)
                .map(|i| Pair {
                    voltage_code: 32768,
                    current_code: code(i),
                    low: true,
                    under_range: false,
                })
                .collect();
            let flags = if held.contains(&first) {
                Pairs::HELD
            } else {
                0
            };
            writer
                .ingest(Message::Pairs(Pairs::new(flags, first, time(first), pairs)))
                .unwrap();
            first = end;
        }
        writer
            .ingest(Message::End(End {
                reason: EndReason::Stopped,
                last_sequence: count - 1,
            }))
            .unwrap();
        writer.finish().unwrap();
    }
    /// 20 s at 1,000 pairs a second, with a 30 ms pulse each second.
    fn fixture(path: &Path) {
        capture(
            path,
            1000,
            20_000,
            |i| if i % 1000 < 30 { 3000 } else { 350 },
        );
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
            // A frame's time, so two clicks a few frames apart make a double click.
            .with_step_dt(1.0 / 60.0)
            .with_context_setup(crate::style::install);
        if wgpu {
            builder = builder.wgpu();
        }
        builder.build_ui_state(
            |ui, b: &mut Bench| b.show(ui, &[], &mut None, &mut crate::worker::Manual::default()),
            bench,
        )
    }
    /// The middle of the plot, right of the 300-point panel and under the toolbar.
    const PLOT: egui::Pos2 = egui::pos2(700.0, 300.0);
    fn press(h: &mut Harness<'_, Bench>, pos: egui::Pos2, pressed: bool) {
        button(h, egui::PointerButton::Primary, pos, pressed);
    }
    fn button(
        h: &mut Harness<'_, Bench>,
        button: egui::PointerButton,
        pos: egui::Pos2,
        pressed: bool,
    ) {
        h.event(egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Default::default(),
        });
        h.step();
    }
    /// Drags across the plot by `by`, in `steps` frames, letting queries run between them.
    fn drag(h: &mut Harness<'_, Bench>, by: Vec2, steps: u32) {
        drag_with(h, egui::PointerButton::Primary, by, steps, |_| {});
    }
    /// As `drag`, with `with`, calling `during` before the button is released.
    fn drag_with(
        h: &mut Harness<'_, Bench>,
        with: egui::PointerButton,
        by: Vec2,
        steps: u32,
        during: impl FnOnce(&mut Harness<'_, Bench>),
    ) {
        h.event(egui::Event::PointerMoved(PLOT));
        h.step();
        button(h, with, PLOT, true);
        for step in 1..=steps {
            h.event(egui::Event::PointerMoved(
                PLOT + by * step as f32 / steps as f32,
            ));
            h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
        during(h);
        button(h, with, PLOT + by, false);
    }
    fn shown(h: &Harness<'_, Bench>) -> egui_plot::PlotTransform {
        h.state().shown.expect("a plot")
    }
    /// Steps until the answer on screen is for `visible`.
    fn answered(h: &mut Harness<'_, Bench>, visible: Interval) -> Arc<Answer> {
        for _ in 0..400 {
            h.step();
            if let Some(answer) = &h.state().answer
                && answer.visible == visible
            {
                return answer.clone();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("no answer for {visible:?}");
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
    fn fitted_range_leaves_the_label_rows_above_the_trace() {
        assert_eq!(label_headroom(0.0..=16.0, 0, 200.0), 0.0..=16.0);
        assert_eq!(label_headroom(0.0..=16.0, 3, 80.0), 0.0..=16.0);
        let range = label_headroom(-1.0..=16.0, 3, 208.0);
        let band = f64::from(LABEL_TOP + 3.0 * LABEL_ROW);
        let below_top = (range.end() - 16.0) / (range.end() - range.start()) * 208.0;
        assert!((below_top - band).abs() < 1e-9, "{below_top}");
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
        h.get_by_label("Clear").click();
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
                marker_after: None,
                marker_selected: None,
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
    /// Renders the frame `h` drew to `name` in `dir`, then the next in the day face.
    fn shoot(h: &mut Harness<'_, Bench>, dir: &Path, name: &str) {
        let stem = name.trim_end_matches(".png");
        h.render()
            .unwrap()
            .save(dir.join(format!("{stem}-night.png")))
            .unwrap();
        h.ctx.set_theme(egui::Theme::Light);
        h.step();
        h.render()
            .unwrap()
            .save(dir.join(format!("{stem}-day.png")))
            .unwrap();
        h.ctx.set_theme(egui::Theme::Dark);
        h.step();
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
        shoot(&mut h, &dir, "bench.png");
        let visible = Interval::new(5_000_000_000, 5_040_000_000);
        h.state_mut().inspect(visible, None);
        answered(&mut h, visible);
        h.step();
        shoot(&mut h, &dir, "bench-samples.png");
        // 2 ms of every 10 at 10,000 pairs a second, for 20 s.
        let path = dir.join("dense.vergecapture");
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        capture(&path, 10_000, 200_000, |i| {
            if i % 100 < 20 { 3000 } else { 350 }
        });
        let mut bench = Bench::new(&dir);
        bench.open(path);
        let mut h = harness(bench, true);
        wait(&mut h);
        h.step();
        shoot(&mut h, &dir, "bench-dense.png");
        for (name, span) in [("1s", 1_000_000_000), ("100ms", 100_000_000)] {
            let visible = Interval::new(5_000_000_000, 5_000_000_000 + span);
            h.state_mut().inspect(visible, None);
            answered(&mut h, visible);
            h.step();
            let file = format!("bench-dense-{name}.png");
            shoot(&mut h, &dir, &file);
        }
        // 2 ms of every 10 at 1,000 pairs a second, with noise on both levels.
        let path = dir.join("marker.vergecapture");
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        capture_events(
            &path,
            1000,
            60_000,
            noisy_marker,
            true,
            &[
                escapement_verge_probe::stream::Marker {
                    time_ns: 20_050_000_000,
                    number: 7,
                    label: b"wake, read sensor".to_vec(),
                },
                escapement_verge_probe::stream::Marker {
                    time_ns: 20_200_000_000,
                    number: 8,
                    label: b"transmit".to_vec(),
                },
                escapement_verge_probe::stream::Marker {
                    time_ns: 21_300_000_000,
                    number: 9,
                    label: b"sleep".to_vec(),
                },
                escapement_verge_probe::stream::Marker {
                    time_ns: 26_000_000_000,
                    number: 10,
                    label: b"label with all thirty-two bytes!!".to_vec(),
                },
            ],
        );
        let archive = Archive::open(&path).unwrap();
        for csv in [true, false] {
            let out = dir.join(if csv { "marker.csv" } else { "marker.sr" });
            if out.exists() {
                std::fs::remove_file(&out).unwrap();
            }
            archive
                .export(
                    &out,
                    csv,
                    Interval::new(20_000_000_000, 20_500_000_000),
                    &AtomicBool::new(false),
                )
                .unwrap();
        }
        let mut bench = Bench::new(&dir);
        bench.open(path);
        let mut h = harness(bench, true);
        wait(&mut h);
        for (name, span) in [
            ("10s", 10_000_000_000),
            ("2s", 2_000_000_000),
            ("500ms", 500_000_000),
        ] {
            let visible = Interval::new(20_000_000_000, 20_000_000_000 + span);
            h.state_mut().inspect(visible, None);
            answered(&mut h, visible);
            h.step();
            let file = format!("bench-marker-{name}.png");
            shoot(&mut h, &dir, &file);
        }
        // The same shape with pairs lost: one, three, and 300 in a run.
        let path = dir.join("lossy.vergecapture");
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        capture_lossy(
            &path,
            1000,
            60_000,
            noisy_marker,
            &[20_503..20_506, 21_007..21_008, 24_000..24_300],
        );
        let mut bench = Bench::new(&dir);
        bench.open(path);
        let mut h = harness(bench, true);
        wait(&mut h);
        for (name, start, span) in [
            ("10s", 20_000_000_000, 10_000_000_000),
            ("2s", 20_000_000_000, 2_000_000_000),
            ("500ms", 20_300_000_000, 500_000_000),
            ("end", 53_300_000_000, 7_000_000_000),
        ] {
            let visible = Interval::new(start, start + span);
            h.state_mut().inspect(visible, None);
            answered(&mut h, visible);
            h.step();
            let file = format!("bench-lossy-{name}.png");
            shoot(&mut h, &dir, &file);
        }
        // A pan past what the answer covers, before the next answer lands: high until 10.05 s,
        // then low.
        let path = dir.join("step.vergecapture");
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        capture(&path, 1000, 20_000, |i| if i < 10_050 { 3000 } else { 350 });
        let mut bench = Bench::new(&dir);
        bench.open(path);
        let mut h = harness(bench, true);
        wait(&mut h);
        let visible = Interval::new(9_900_000_000, 10_000_000_000);
        h.state_mut().inspect(visible, None);
        answered(&mut h, visible);
        h.state_mut().interval = Some(Interval::new(10_000_000_000, 10_200_000_000));
        h.state_mut().reset_bounds = true;
        h.step();
        shoot(&mut h, &dir, "bench-pan.png");
        // A debug session's shape: 10,000 pairs a second, held 4 ms every 0.3 to 0.8 s while a
        // debugger connects, and pairs lost twice, 0.6 ms from 15.12 s and 150 ms from 22 s.
        let path = dir.join("held.vergecapture");
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        let mut held = Vec::new();
        let mut at = 2000u32;
        while at < 300_000 {
            held.push(at);
            at += 3000 + (at.wrapping_mul(2_654_435_761) >> 16) % 5000;
        }
        capture_gaps(
            &path,
            10_000,
            300_000,
            |i| noisy_marker(i / 10) + ((i.wrapping_mul(40_503) >> 12) % 8) as u16,
            &[151_234..151_240, 220_000..221_500],
            &held,
            4_000_000,
        );
        let pause = held.iter().find(|&&at| at > 160_000).copied().unwrap();
        let pause_ns = u64::from(pause) * 100_000
            + held.iter().filter(|&&at| at < pause).count() as u64 * 4_000_000;
        let lost_ns =
            151_234 * 100_000 + held.iter().filter(|&&at| at < 151_234).count() as u64 * 4_000_000;
        let mut bench = Bench::new(&dir);
        bench.open(path);
        let mut h = harness(bench, true);
        wait(&mut h);
        for (name, start, span) in [
            ("all", 0, 32_000_000_000),
            ("2s", 15_000_000_000, 2_000_000_000),
            ("loss-20ms", lost_ns - 10_000_000, 20_000_000),
            ("pause-20ms", pause_ns - 12_000_000, 20_000_000),
        ] {
            let visible = Interval::new(start, start + span);
            h.state_mut().inspect(visible, None);
            answered(&mut h, visible);
            h.step();
            let file = format!("bench-held-{name}.png");
            shoot(&mut h, &dir, &file);
        }
    }
    /// 2 ms of every 10, with noise on both levels.
    fn noisy_marker(i: u32) -> u16 {
        let noise = (i.wrapping_mul(2_654_435_761) >> 24) as u16;
        if i % 10 < 2 {
            2900 + noise / 2
        } else {
            340 + noise / 16
        }
    }
    #[test]
    fn marker_pick_centres_view_and_selects_to_next_across_pages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("markers.vergecapture");
        let markers: Vec<_> = (0..30)
            .map(|number| escapement_verge_probe::stream::Marker {
                time_ns: u64::from(number + 1) * 1_000_000_000,
                number,
                label: format!("step {number}").into_bytes(),
            })
            .collect();
        capture_events(&path, 1000, 40_000, |_| 350, true, &markers);
        let mut bench = Bench::new(dir.path());
        bench.open(path);
        let mut h = harness(bench, false);
        wait(&mut h);
        assert_eq!(
            h.state().answer.as_ref().unwrap().marker_list.entries.len(),
            24
        );
        h.step();
        h.get_by_label("24.000000s · 23 · step 23").scroll_to_me();
        for _ in 0..20 {
            h.step();
        }
        h.get_by_label("24.000000s · 23 · step 23").click();
        h.step();
        let wanted = h.state().interval.unwrap();
        assert_eq!(wanted, Interval::new(19_000_000_000, 29_000_000_000));
        answered(&mut h, wanted);
        h.get_by_label("Select to next marker").scroll_to_me();
        h.step();
        h.get_by_label("Select to next marker").click();
        h.step();
        for _ in 0..400 {
            h.step();
            if h.state()
                .answer
                .as_ref()
                .unwrap()
                .selection
                .as_ref()
                .is_some_and(|(i, _)| *i == Interval::new(24_000_000_000, 25_000_000_000))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let selection = h
            .state()
            .answer
            .as_ref()
            .unwrap()
            .selection
            .as_ref()
            .unwrap();
        assert_eq!(selection.0, Interval::new(24_000_000_000, 25_000_000_000));
        assert_eq!(selection.1.samples, 1000);
        h.get_by_label("Next").scroll_to_me();
        h.step();
        h.get_by_label("Next").click();
        h.step();
        for _ in 0..400 {
            h.step();
            if h.state()
                .answer
                .as_ref()
                .unwrap()
                .marker_list
                .entries
                .first()
                .is_some_and(|m| m.marker.number == 24)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            h.state().answer.as_ref().unwrap().marker_list.entries.len(),
            6
        );
        assert_eq!(h.state().answer.as_ref().unwrap().marker_list.total, 30);
    }

    #[test]
    fn zooming_the_plot_queries_the_range_it_shows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.vergecapture");
        fixture(&path);
        let mut h = harness(state(&path), false);
        wait(&mut h);
        h.step();
        let before = h.state().answer.as_ref().unwrap().visible;
        // In select mode, where only dragging is the selection's, the wheel still zooms.
        h.state_mut().select_mode = true;
        h.event(egui::Event::PointerMoved(PLOT));
        h.step();
        h.event(egui::Event::Zoom(2.0));
        h.step();
        let wanted = h.state().interval.expect("an interval of its own");
        assert!(!h.state().follow);
        assert!(
            wanted.duration_ns() < before.duration_ns() * 3 / 4,
            "{wanted:?} from {before:?}"
        );
        answered(&mut h, wanted);
    }
    #[test]
    fn moving_the_plot_up_keeps_it_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.vergecapture");
        fixture(&path);
        let mut h = harness(state(&path), false);
        wait(&mut h);
        h.step();
        let before = shown(&h);
        assert!(h.state().y_fit);
        h.event(egui::Event::PointerMoved(PLOT));
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, 80.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Default::default(),
        });
        for _ in 0..60 {
            h.step();
        }
        assert!(!h.state().y_fit);
        let moved = shown(&h);
        assert!(
            moved.bounds().min()[1] > before.bounds().min()[1],
            "{:?} from {:?}",
            moved.bounds(),
            before.bounds()
        );
        // A new answer leaves the range where the wheel left it.
        let earlier = h.state().answer.clone().unwrap();
        h.state_mut().requery();
        for _ in 0..400 {
            h.step();
            if !Arc::ptr_eq(h.state().answer.as_ref().unwrap(), &earlier) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!Arc::ptr_eq(h.state().answer.as_ref().unwrap(), &earlier));
        h.step();
        assert_eq!(shown(&h).bounds().range_y(), moved.bounds().range_y());
        // A double click fits it again.
        h.event(egui::Event::PointerMoved(PLOT));
        for _ in 0..2 {
            press(&mut h, PLOT, true);
            press(&mut h, PLOT, false);
        }
        h.step();
        assert!(h.state().y_fit);
        assert_eq!(shown(&h).bounds().range_y(), before.bounds().range_y());
    }
    #[test]
    fn a_drag_along_time_leaves_y_fitted_and_a_vertical_one_moves_it_from_its_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.vergecapture");
        fixture(&path);
        let mut h = harness(state(&path), false);
        wait(&mut h);
        h.step();
        let before = shown(&h);
        drag(&mut h, vec2(-200.0, 6.0), 10);
        assert!(h.state().y_fit);
        assert!(!h.state().follow);
        let panned = shown(&h);
        assert!(panned.bounds().min()[0] > before.bounds().min()[0]);
        drag(&mut h, vec2(0.0, 60.0), 6);
        h.step();
        assert!(!h.state().y_fit);
        let moved = shown(&h);
        // All 60 points, the detent's included.
        let points = (moved.bounds().min()[1] - panned.bounds().min()[1]) / moved.dvalue_dpos()[1];
        assert!((points + 60.0).abs() < 1.0, "{points}");
    }
    #[test]
    fn a_box_zoom_leaves_y_alone_until_it_lands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.vergecapture");
        fixture(&path);
        let mut h = harness(state(&path), false);
        wait(&mut h);
        h.step();
        let before = shown(&h);
        drag_with(
            &mut h,
            egui::PointerButton::Secondary,
            vec2(40.0, 120.0),
            10,
            |h| {
                assert!(h.state().y_fit);
                assert_eq!(shown(h).bounds().range_y(), before.bounds().range_y());
            },
        );
        h.step();
        assert!(!h.state().y_fit);
        assert_ne!(shown(&h).bounds().range_y(), before.bounds().range_y());
    }
    #[test]
    fn a_selection_started_straight_after_a_zoom_keeps_the_zoom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.vergecapture");
        fixture(&path);
        let mut h = harness(state(&path), false);
        wait(&mut h);
        h.step();
        h.state_mut().select_mode = true;
        h.event(egui::Event::PointerMoved(PLOT));
        h.step();
        h.event(egui::Event::Zoom(2.0));
        h.step();
        let zoomed = shown(&h).bounds().range_x();
        drag_with(
            &mut h,
            egui::PointerButton::Primary,
            vec2(60.0, 0.0),
            3,
            |_| {},
        );
        h.step();
        // To the nanosecond the selection's interval holds.
        let after = shown(&h).bounds().range_x();
        assert!(
            (after.start() - zoomed.start()).abs() < 1e-6
                && (after.end() - zoomed.end()).abs() < 1e-6,
            "{after:?} after zooming to {zoomed:?}"
        );
        let selection = h.state().selection.expect("a selection");
        assert!(
            selection.start_ns >= nanos(*zoomed.start())
                && selection.end_ns <= nanos(*zoomed.end())
        );
    }
    #[test]
    fn answers_arrive_while_the_plot_is_still_moving() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.vergecapture");
        fixture(&path);
        let mut h = harness(state(&path), false);
        wait(&mut h);
        h.step();
        h.event(egui::Event::PointerMoved(PLOT));
        h.step();
        press(&mut h, PLOT, true);
        let mut answers = std::collections::HashSet::new();
        for step in 1..=40 {
            h.event(egui::Event::PointerMoved(
                PLOT - vec2(5.0 * step as f32, 0.0),
            ));
            h.step();
            std::thread::sleep(Duration::from_millis(5));
            let visible = h.state().answer.as_ref().unwrap().visible;
            answers.insert((visible.start_ns, visible.end_ns));
        }
        press(&mut h, PLOT - vec2(200.0, 0.0), false);
        assert!(
            answers.len() > 3,
            "{} answers during the drag",
            answers.len()
        );
    }
    #[test]
    fn zoomed_in_the_answer_holds_samples_and_the_overview_fills_around_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.vergecapture");
        capture_with_dio(
            &path,
            1000,
            20_000,
            |i| if i % 1000 < 30 { 3000 } else { 350 },
            true,
        );
        let mut h = harness(state(&path), false);
        wait(&mut h);
        let whole = h.state().answer.clone().unwrap();
        assert!(whole.view.samples.is_none());
        let visible = Interval::new(5_000_000_000, 5_050_000_000);
        h.state_mut().inspect(visible, None);
        let answer = answered(&mut h, visible);
        // 50 ms visible, and 50 ms either side.
        assert_eq!(answer.view.samples.as_ref().map(Vec::len), Some(150));
        let covered = answer.view.interval;
        assert!(
            answer
                .fallback
                .iter()
                .all(|bin| bin.interval.end_ns <= covered.start_ns
                    || bin.interval.start_ns >= covered.end_ns)
        );
        let extent = answer.view.extent;
        assert_eq!(
            answer.fallback.first().unwrap().interval.start_ns,
            extent.start_ns
        );
        assert_eq!(
            answer.fallback.last().unwrap().interval.end_ns,
            extent.end_ns
        );
        assert!(!answer.digital_fallback.is_empty());
        assert!(
            answer
                .digital_fallback
                .iter()
                .all(|bin| bin.interval.end_ns <= covered.start_ns
                    || bin.interval.start_ns >= covered.end_ns)
        );
        assert_eq!(answer.window.samples, 50);
    }
    #[test]
    fn the_overview_beside_the_answer_holds_only_its_own_levels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step.vergecapture");
        // High until 10.05 s, then low, with DIO3 following. The overview's bins are 156.25 ms, so
        // the one from 10.0 s holds both levels.
        capture_with_dio(
            &path,
            1000,
            20_000,
            |i| if i < 10_050 { 3000 } else { 350 },
            true,
        );
        let mut h = harness(state(&path), false);
        wait(&mut h);
        // The answer covers 9.8 s to 10.1 s: the cut bin's part after it is low throughout.
        let visible = Interval::new(9_900_000_000, 10_000_000_000);
        h.state_mut().inspect(visible, None);
        let answer = answered(&mut h, visible);
        assert_eq!(answer.view.interval.end_ns, 10_100_000_000);
        let after = |bin_start: &dyn Fn(u64) -> bool| {
            answer
                .fallback
                .iter()
                .find(|bin| bin_start(bin.interval.start_ns))
                .unwrap()
                .clone()
        };
        let beside = after(&|start| start == 10_100_000_000);
        let later = after(&|start| start > 10_100_000_000);
        assert_eq!(beside.statistics.max_na, later.statistics.max_na);
        assert!(!beside.gap);
        let digital = answer
            .digital_fallback
            .iter()
            .find(|bin| bin.interval.start_ns == 10_100_000_000)
            .unwrap();
        assert_eq!((digital.high & 8, digital.low & 8), (0, 8));
    }
    #[test]
    fn a_cut_part_of_the_overview_keeps_its_own_gap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lossy.vergecapture");
        // Pairs lost after 10.1 s, inside the overview bin from 10.0 s, and at 15 s; none before.
        capture_lossy(
            &path,
            1000,
            20_000,
            |_| 350,
            &[10_120..10_125, 15_000..15_001],
        );
        let mut h = harness(state(&path), false);
        wait(&mut h);
        let visible = Interval::new(9_900_000_000, 10_000_000_000);
        h.state_mut().inspect(visible, None);
        let answer = answered(&mut h, visible);
        let part = |start| {
            answer
                .fallback
                .iter()
                .find(|bin| bin.interval.start_ns == start)
                .unwrap()
        };
        assert!(part(10_100_000_000).gap && part(10_100_000_000).lost);
        assert!(!part(9_687_500_000).gap);
    }
    #[test]
    fn a_pause_leaves_a_gap_and_only_lost_pairs_are_lost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("held.vergecapture");
        // Held for 4 ms before pair 10,120, inside the overview bin the window below cuts, and
        // before 15,000; pairs lost from 17,000 (17.008 s to 17.011 s, after both holds) and at
        // 18,000 (18.008 s to 18.009 s).
        capture_gaps(
            &path,
            1000,
            20_000,
            |_| 350,
            &[17_000..17_003, 18_000..18_001],
            &[10_120, 15_000],
            4_000_000,
        );
        let mut h = harness(state(&path), false);
        wait(&mut h);
        let visible = Interval::new(9_900_000_000, 10_000_000_000);
        h.state_mut().inspect(visible, None);
        let answer = answered(&mut h, visible);
        let cut = answer
            .fallback
            .iter()
            .find(|bin| bin.interval.start_ns == 10_100_000_000)
            .unwrap();
        assert!(cut.gap && !cut.lost);
        let at = |time: u64| {
            answer
                .overview
                .envelopes
                .iter()
                .find(|bin| bin.interval.start_ns <= time && time < bin.interval.end_ns)
                .unwrap()
        };
        for pause in [10_122_000_000, 15_006_000_000] {
            assert!(at(pause).gap && !at(pause).lost);
        }
        assert!(at(17_009_000_000).lost && at(18_008_500_000).lost);
        let spans: Vec<_> = answer
            .overview
            .lost
            .iter()
            .map(|span| (span.start_ns, span.end_ns))
            .collect();
        assert_eq!(
            spans,
            [
                (17_008_000_000, 17_011_000_000),
                (18_008_000_000, 18_009_000_000)
            ]
        );
        let lost = [
            (17_008_000_000, 17_011_000_000),
            (18_008_000_000, 18_009_000_000),
        ];
        assert!(
            answer
                .overview
                .envelopes
                .iter()
                .filter(|bin| bin.lost)
                .all(|bin| {
                    lost.iter().any(|&(start, end)| {
                        bin.interval.start_ns < end && bin.interval.end_ns > start
                    })
                })
        );
    }
    #[test]
    fn a_window_past_the_capture_counts_only_the_capture_as_unmeasured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.vergecapture");
        // 2 s of pairs, five of them lost before 1.1 s.
        capture_lossy(&path, 1000, 2000, |_| 350, &[500..502, 1000..1003]);
        let mut h = harness(state(&path), false);
        wait(&mut h);
        let visible = Interval::new(1_500_000_000, 2_300_000_000);
        h.state_mut().inspect(visible, None);
        let answer = answered(&mut h, visible);
        assert_eq!(
            unmeasured_ns(visible, answer.view.extent, &answer.window),
            0
        );
        let whole = Interval::new(0, 2_300_000_000);
        h.state_mut().inspect(whole, None);
        let answer = answered(&mut h, whole);
        assert_eq!(
            unmeasured_ns(whole, answer.view.extent, &answer.window),
            5_000_000
        );
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
        let unit = escapement_verge_probe::detect(
            &escapement_verge_probe::sim::SimBus::scenario("ours-pro").unwrap(),
        )
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
