//! The batch view: an image and the units it takes, armed once, then every unit plugged in after
//! that updated on its own, one at a time.

use std::num::NonZeroUsize;

use std::path::{Path as FsPath, PathBuf};

use egui::{Color32, Margin, Stroke, Ui, vec2};
use escapement_verge_probe::batch::{
    self, ArmError, Board, Filter, OursFilter, Record, Settled, StockFilter, Tally,
};
use escapement_verge_probe::paths::ISP_IDENTITY_WARNING;
use escapement_verge_probe::progress::{Cancel, Unit as StepUnit};
use escapement_verge_probe::source::{ImageVersion, Source};
use escapement_verge_probe::version::StockVersion;

use super::{
    Choice, Current, Fill, Picked, STOCK, VergeApp, answers, edged, field, frame, kib, label,
    outline, progress, sentence_case, slot, wrapping,
};
use crate::glyph::{self, Glyph};
use crate::style::{self, Face, Role, Status, space, text};
use crate::worker::{BatchEvent, BatchState, ProgressEvent, Request, arm_error};

fn record_path(dir: &FsPath) -> String {
    let stamp = humantime::format_rfc3339_millis(std::time::SystemTime::now())
        .to_string()
        .replace(':', "");
    dir.join(format!("verge-batch-{stamp}.jsonl"))
        .display()
        .to_string()
}

/// What the user chooses before arming.
pub(in crate::app) struct BatchForm {
    stock: bool,
    stock_below: String,
    ours: bool,
    ours_any: bool,
    isp: bool,
    attached: bool,
    stop_after: bool,
    board: Board,
    count: String,
    record: String,
    dir: PathBuf,
    pub(in crate::app) confirming: Option<Arming>,
}

/// The arming question while it is open. What it says is downloaded first is fixed when it
/// opens, as [`super::Confirming`]'s is, so it holds only for the image it was opened for.
pub(in crate::app) struct Arming {
    source: Source,
    download: Option<String>,
}

impl BatchForm {
    pub(in crate::app) fn new(dir: &FsPath) -> BatchForm {
        BatchForm {
            stock: true,
            stock_below: String::new(),
            ours: true,
            ours_any: false,
            isp: false,
            attached: false,
            stop_after: false,
            board: Board::McuLink,
            count: String::new(),
            record: record_path(dir),
            dir: dir.to_path_buf(),
            confirming: None,
        }
    }

    /// The filter and the count, or why the form does not make one and the field that says so.
    fn filter(&self) -> Result<(Filter, Option<NonZeroUsize>), (Field, String)> {
        let below = self.stock_below.trim();
        let stock = match (self.stock, below.is_empty()) {
            (false, _) => StockFilter::None,
            (true, true) => StockFilter::Any,
            (true, false) => StockFilter::Below(
                StockVersion::parse(below.strip_prefix(['V', 'v']).unwrap_or(below)).ok_or_else(
                    || {
                        (
                            Field::Takes,
                            format!(
                                "\"{below}\" is not a version of {STOCK}: write it like 3.172."
                            ),
                        )
                    },
                )?,
            ),
        };
        let ours = match (self.ours, self.ours_any) {
            (false, _) => OursFilter::None,
            (true, true) => OursFilter::Any,
            (true, false) => OursFilter::Older,
        };
        if stock == StockFilter::None && ours == OursFilter::None && !self.isp {
            return Err((
                Field::Takes,
                "Tick at least one kind of unit for the batch to take.".into(),
            ));
        }
        let limit = if self.stop_after {
            let count: NonZeroUsize = self.count.trim().parse().map_err(|_| {
                (
                    Field::Stop,
                    "Write how many units to stop after, as a positive number.".to_owned(),
                )
            })?;
            Some(count)
        } else {
            None
        };
        Ok((
            Filter {
                board: self.board,
                stock,
                ours,
                bootloaders: self.isp,
                attached: self.attached,
            },
            limit,
        ))
    }
}

/// The part of the form a reason not to arm is about.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Image,
    Takes,
    Board,
    Stop,
}

/// A batch the window armed: running, or ended and still shown.
pub(in crate::app) struct BatchRun {
    name: Option<String>,
    filter: String,
    record: PathBuf,
    state: BatchState,
    records: Vec<Record>,
    current: Option<(String, Option<Current>)>,
    pub(in crate::app) stop: Cancel,
    cancel: Cancel,
    ended: Option<Result<Tally, String>>,
    started: f64,
    /// When it ended, which stops its clock.
    ended_at: Option<f64>,
}

impl BatchRun {
    pub(in crate::app) fn running(&self) -> bool {
        self.ended.is_none()
    }

    /// An update's progress, for the unit under way.
    pub(in crate::app) fn progress(&mut self, event: ProgressEvent, now: f64) {
        let Some((_, current)) = &mut self.current else {
            return;
        };
        match event {
            ProgressEvent::Start { step, total } => {
                *current = Some(Current {
                    step,
                    total,
                    done: 0,
                    since: now,
                });
            }
            ProgressEvent::Advance(done) => {
                if let Some(current) = current {
                    current.done = done;
                }
            }
            _ => {}
        }
    }

    pub(in crate::app) fn handle(&mut self, event: BatchEvent, now: f64) {
        match event {
            BatchEvent::Armed { name, filter } => {
                self.name = Some(name);
                self.filter = filter;
            }
            BatchEvent::Starting(port) => self.current = Some((port, None)),
            BatchEvent::Record(record) => {
                if self
                    .current
                    .as_ref()
                    .is_some_and(|(p, _)| *p == record.port)
                {
                    self.current = None;
                }
                self.records.push(*record);
            }
            BatchEvent::State(state) => self.state = state,
            BatchEvent::Ended(result) => {
                self.current = None;
                self.ended_at = Some(now);
                if let Ok(tally) = &result {
                    self.state.tally = *tally;
                }
                self.ended = Some(result);
            }
        }
    }
}

/// A result's glyph, its chip and the colour of its words, the same family as the unit view's.
/// An update that finished is Verge's identity, so a lume chip, with the words in ink.
fn mark(result: Settled, face: &Face) -> (Glyph, Fill, Color32) {
    let bare = |color| Fill {
        ground: Color32::TRANSPARENT,
        on: color,
    };
    match result {
        Settled::Updated => (glyph::DONE, Fill::lume(face), face.ink),
        Settled::JumperFitted | Settled::NotRestarted => (
            glyph::ATTENTION,
            bare(face.attention_ink),
            face.attention_ink,
        ),
        Settled::Failed => (glyph::FAILED, bare(face.fault_ink), face.fault_ink),
        Settled::Current | Settled::Skipped | Settled::Waiting => {
            (glyph::STOCK, bare(face.ink_2), face.ink_2)
        }
    }
}

impl VergeApp {
    /// The image chosen above, as a batch's source, or why there is none. A batch that cannot arm
    /// with it says so here, as far as the choice tells before the image is loaded: a release's
    /// image is checked against the board once it is downloaded.
    fn batch_source(&self, filter: &Filter) -> Result<(Source, String), (Field, String)> {
        let image = |reason: String| (Field::Image, reason);
        // Where the image does not suit the units ticked, the field to change.
        let refused = |error: &ArmError| {
            let field = match error {
                ArmError::OursUndated => Field::Takes,
                ArmError::NotForBoard { .. } => Field::Board,
                ArmError::Image(_) => Field::Image,
            };
            (field, arm_error(error))
        };
        let (source, what, version) = match self.choice {
            Choice::Release => match &self.release {
                None => return Err(image("Looking up the release.".into())),
                Some(Err(error)) => return Err(image(error.clone())),
                Some(Ok(release)) => (
                    Source::Release(Some(release.tag.clone())),
                    format!("release {}", release.tag),
                    ImageVersion::Ours(release.tag.clone()),
                ),
            },
            Choice::Stock => match &self.stock {
                None => {
                    return Err(image(format!(
                        "No image of {STOCK} in LinkServer's usual places."
                    )));
                }
                Some(found) => (
                    Source::Stock(found.path.parent().map(FsPath::to_path_buf)),
                    format!("{STOCK} V{}", found.version),
                    ImageVersion::Stock(found.version),
                ),
            },
            Choice::File => match &self.picked {
                Some(Picked {
                    path,
                    image: Ok(image),
                    version,
                }) => {
                    batch::check(filter, image, version).map_err(|error| refused(&error))?;
                    (
                        Source::File(path.clone()),
                        path.display().to_string(),
                        version.clone(),
                    )
                }
                Some(Picked {
                    image: Err(error), ..
                }) => return Err(image(error.clone())),
                None => {
                    return Err(image(
                        "Pick an ELF or S-record, or drop one on the window.".into(),
                    ));
                }
            },
        };
        filter
            .check_version(&version)
            .map_err(|error| refused(&error))?;
        Ok((source, what))
    }

    pub(in crate::app) fn batch_view(&mut self, ui: &mut Ui, now: f64) {
        if self.batch().is_some() {
            self.batch_running(ui, now);
        } else {
            self.batch_setup(ui, now);
        }
    }

    fn batch_setup(&mut self, ui: &mut Ui, now: f64) {
        let face = style::face(ui.ctx());
        // The strip says why.
        let stopped = self.worker_error.is_some();
        if stopped {
            ui.disable();
        }
        let ready = self.batch_form.filter().and_then(|(filter, limit)| {
            self.batch_source(&filter)
                .map(|(source, what)| (source, what, filter, limit))
        });
        // Why the batch cannot arm, under the field it is about as well as at the foot.
        let refused = ready.as_ref().err().cloned();
        let note = |ui: &mut Ui, here: Field| {
            if let Some((field, reason)) = &refused
                && *field == here
            {
                edged(ui, Status::Attention, reason);
            }
        };
        frame(ui, None, Stroke::new(1.0, face.rule), |ui| {
            ui.horizontal(|ui| {
                glyph::WAITING.show(ui, 48.0, face.ink);
                ui.add_space(space::M);
                ui.label(text(Role::Display, "Batch").color(face.ink));
            });
            wrapping(
                ui,
                text(Role::Instruction, "One image for every unit plugged in").color(face.ink),
            );
            wrapping(
                ui,
                text(
                    Role::Body,
                    "Choose the image and the units it takes, and arm it once. Then plug units in, one after another or several on a hub. Each goes through its bootloader on its own, one at a time, and blinks when it is done.",
                )
                .color(face.ink_2),
            );
        });
        ui.add_space(space::M);
        self.image_choice(ui, None);
        note(ui, Field::Image);
        ui.add_space(space::S);
        let form = &mut self.batch_form;
        field(ui, "Takes", |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut form.stock, text(Role::Body, STOCK).color(face.ink));
                ui.add_enabled_ui(form.stock, |ui| {
                    label(ui, "only below");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.stock_below)
                            .hint_text("any")
                            .desired_width(96.0),
                    );
                });
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut form.ours, text(Role::Body, "Verge").color(face.ink));
                ui.add_enabled_ui(form.ours, |ui| {
                    ui.checkbox(
                        &mut form.ours_any,
                        text(
                            Role::Body,
                            "of any build, not only those older than the image",
                        )
                        .color(face.ink),
                    );
                });
            });
            ui.checkbox(
                &mut form.isp,
                text(
                    Role::Body,
                    "Units that arrive in ISP mode, as after fitting the jumper",
                )
                .color(face.ink),
            );
            ui.checkbox(
                &mut form.attached,
                text(
                    Role::Body,
                    "Units attached now, not only those plugged in from here on",
                )
                .color(face.ink),
            );
            note(ui, Field::Takes);
        });
        field(ui, "Board", |ui| {
            ui.horizontal(|ui| {
                for board in Board::ALL {
                    ui.radio_value(
                        &mut form.board,
                        board,
                        text(Role::Body, board.to_string()).color(face.ink),
                    );
                }
            });
            note(ui, Field::Board);
        });
        field(ui, "Stop", |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(
                    &mut form.stop_after,
                    text(Role::Body, "after").color(face.ink),
                );
                ui.add_enabled(
                    form.stop_after,
                    egui::TextEdit::singleline(&mut form.count).desired_width(48.0),
                );
                ui.label(text(Role::Body, "units are updated").color(face.ink));
            });
            note(ui, Field::Stop);
        });
        field(ui, "Record", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut form.record)
                    .font(Role::Code.style())
                    .desired_width(ui.available_width().min(640.0)),
            );
            wrapping(
                ui,
                text(Role::Body, "One JSON line per unit: when, its port and serial, the firmware before and after, and the result.")
                    .color(face.ink_2),
            );
        });
        ui.add_space(space::L);

        let (source, what, filter, limit) = match ready {
            Ok(ready) if !stopped => ready,
            refused => {
                let words = if stopped {
                    "Worker stopped"
                } else {
                    "Cannot arm"
                };
                ui.add_enabled(
                    false,
                    egui::Button::new(text(Role::Action, words).color(face.ink_2))
                        .min_size(vec2(320.0, crate::style::SLOT_HEIGHT)),
                );
                if let Err((_, reason)) = refused {
                    wrapping(ui, text(Role::Body, reason).color(face.ink_2));
                }
                return;
            }
        };
        // A question asked about another image no longer stands.
        if self
            .batch_form
            .confirming
            .as_ref()
            .is_some_and(|arming| arming.source != source)
        {
            self.batch_form.confirming = None;
        }
        if let Some(Arming { download, .. }) = &self.batch_form.confirming {
            let download = download.clone();
            let lead = match &download {
                Some(download) => format!("Download {download}, and flash it"),
                None => format!("Flash {what}"),
            };
            let mut question = format!(
                "{lead} onto {}? Each goes through its bootloader on its own, without being asked again, until you stop the batch.",
                filter.describe()
            );
            if filter.bootloaders {
                question = format!("{ISP_IDENTITY_WARNING} {question}");
            }
            edged(ui, Status::Attention, &question);
            ui.add_space(space::S);
            match answers(ui, "Confirm", Fill::lume(face), false) {
                Some(true) => {
                    let record = PathBuf::from(self.batch_form.record.trim());
                    let stop = Cancel::new();
                    let cancel = Cancel::new();
                    self.operation = super::Operation::Batch(BatchRun {
                        name: None,
                        filter: filter.describe(),
                        record: record.clone(),
                        state: BatchState::default(),
                        records: Vec::new(),
                        current: None,
                        stop: stop.clone(),
                        cancel: cancel.clone(),
                        ended: None,
                        started: now,
                        ended_at: None,
                    });
                    self.batch_form.confirming = None;
                    self.backend.send(Request::Batch {
                        source,
                        filter,
                        download: super::agreed(download.as_ref()),
                        limit,
                        record,
                        stop,
                        cancel,
                    });
                }
                Some(false) => self.batch_form.confirming = None,
                None => {}
            }
            return;
        }
        if slot(ui, "Arm batch", Some(Fill::lume(face))).clicked() {
            self.batch_form.confirming = Some(Arming {
                download: self.download(&source),
                source,
            });
        }
    }

    fn batch_running(&mut self, ui: &mut Ui, now: f64) {
        let Some(run) = self.batch() else {
            return;
        };
        let face = style::face(ui.ctx());
        let fault = Some(Fill::status(Status::Fault, face));
        let (word, instruction, glyph, fill) = match &run.ended {
            Some(Ok(_)) => ("Batch", "Ended", glyph::DONE, Some(Fill::lume(face))),
            Some(Err(_)) if run.name.is_none() => ("Batch", "Not armed", glyph::FAILED, fault),
            Some(Err(_)) => ("Batch", "Stopped by a fault", glyph::FAILED, fault),
            None if run.state.paused.is_some() => ("Batch", "Paused", glyph::ATTENTION, None),
            None if run.stop.is_cancelled() => (
                "Batch",
                "Stopping: the unit under way finishes first",
                glyph::WORKING,
                None,
            ),
            None => ("Batch", "Plug units in", glyph::WORKING, None),
        };
        let ink = fill.map_or(face.ink, |fill| fill.on);
        let stroke = if fill.is_some() {
            Stroke::NONE
        } else {
            Stroke::new(2.0, face.ink)
        };
        let armed = run.name.is_some();
        let mut back = false;
        frame(ui, fill.map(|fill| fill.ground), stroke, |ui| {
            ui.horizontal(|ui| {
                glyph.show(ui, 48.0, ink);
                ui.add_space(space::M);
                ui.label(text(Role::Display, word).color(ink));
            });
            wrapping(ui, text(Role::Instruction, instruction).color(ink));
            // A batch that never armed loaded no image.
            let mut facts: Vec<String> = match (&run.name, &run.ended) {
                (Some(name), _) => vec![name.clone()],
                (None, None) => vec!["loading the image".into()],
                (None, Some(_)) => Vec::new(),
            };
            facts.push(run.filter.clone());
            facts.push(format!(
                "{:.0} s",
                run.ended_at.unwrap_or(now) - run.started
            ));
            wrapping(ui, text(Role::Code, facts.join(" · ")).color(ink));
            let record = match (&run.ended, armed) {
                (Some(_), false) => "record file: nothing was recorded".to_owned(),
                _ => format!("record file: {}", run.record.display()),
            };
            wrapping(ui, text(Role::Code, record).color(ink));
            // The batch's own controls stay here, above everything that grows.
            ui.add_space(space::M);
            // Drawn in the slab's own ink, which reads on its fill.
            let on_slab = |ui: &mut Ui, words: &str| {
                ui.add_sized(
                    vec2(320.0, crate::style::SLOT_HEIGHT),
                    egui::Button::new(text(Role::Action, words).color(ink))
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(1.0, ink)),
                )
            };
            if run.running() {
                if !run.stop.is_cancelled() {
                    if on_slab(ui, "Stop").clicked() {
                        run.stop.cancel();
                    }
                } else if run.current.is_some() {
                    if outline(ui, "Cancel the unit under way", !run.cancel.is_cancelled())
                        .clicked()
                    {
                        run.cancel.cancel();
                    }
                } else {
                    ui.label(text(Role::Body, "Stopping: no unit under way.").color(ink));
                }
            } else if on_slab(ui, "Back").clicked() {
                back = true;
            }
        });
        if back {
            self.last_batch = None;
            self.batch_form.record = record_path(&self.batch_form.dir);
            return;
        }
        let Some(run) = self.batch() else {
            return;
        };
        ui.add_space(space::M);
        let tally = run.state.tally;
        ui.horizontal(|ui| {
            for (name, count) in [
                ("updated", tally.updated),
                ("current", tally.current),
                ("skipped", tally.skipped),
                ("failed", tally.failed),
                ("waiting", tally.waiting),
            ] {
                ui.allocate_ui_with_layout(
                    vec2(112.0, 72.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_width(112.0);
                        let words = text(Role::Tally, count.to_string());
                        let response = match (name, count) {
                            ("failed", n) if n > 0 => ui.label(words.color(face.fault_ink)),
                            // Verge's identity is a fill, never text: the count sits on lume.
                            ("updated", n) if n > 0 => {
                                let fill = Fill::lume(face);
                                egui::Frame::new()
                                    .fill(fill.ground)
                                    .corner_radius(egui::CornerRadius::same(style::RADIUS))
                                    .inner_margin(Margin::symmetric(space::XS as i8, 0))
                                    .show(ui, |ui| ui.label(words.color(fill.on)))
                                    .inner
                            }
                            _ => ui.label(words.color(face.ink)),
                        };
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::Label,
                                true,
                                format!("{count} {name}"),
                            )
                        });
                        label(ui, &sentence_case(name));
                    },
                );
            }
        });
        if let Some(reason) = &run.state.paused {
            edged(ui, Status::Attention, reason);
        }
        if let Some(Err(error)) = &run.ended {
            edged(ui, Status::Fault, error);
        }
        if let Some((port, current)) = &run.current {
            ui.add_space(space::S);
            field(ui, "Now", |ui| {
                let mut words = format!("updating {port}");
                if let Some(current) = current {
                    words.push_str(&format!(" · {}", current.step));
                    if current.step.unit() == StepUnit::Bytes && current.total > 0 {
                        words.push_str(&format!(
                            " · {} / {}",
                            kib(current.done),
                            kib(current.total)
                        ));
                    }
                }
                ui.label(text(Role::Code, words).color(face.ink));
                if let Some(current) = current
                    && current.step.unit() == StepUnit::Bytes
                    && current.total > 0
                {
                    progress(ui, current.done as f32 / current.total as f32, 640.0);
                }
            });
        }
        if !run.state.waiting.is_empty() {
            field(ui, "Waiting", |ui| {
                let names: Vec<String> = run
                    .state
                    .waiting
                    .iter()
                    .map(|(port, serial)| match serial {
                        Some(serial) => format!("{port} ({serial})"),
                        None => port.clone(),
                    })
                    .collect();
                wrapping(ui, text(Role::Code, names.join(", ")).color(face.ink));
            });
        }
        ui.add_space(space::M);
        if run.records.is_empty() {
            label(ui, "No unit yet");
        }
        for record in run.records.iter().rev() {
            egui::Frame::new()
                .fill(face.surface)
                .inner_margin(Margin::same(space::S as i8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (glyph, chip, tint) = mark(record.result, face);
                        glyph.chip(ui, 20.0, chip.ground, chip.on);
                        ui.label(text(Role::Code, &record.port).color(face.ink));
                        let serial = match (&record.serial_before, &record.serial_after) {
                            (Some(a), Some(b)) if a != b => format!("{a} -> {b}"),
                            (Some(a), _) => a.clone(),
                            (None, Some(b)) => b.clone(),
                            (None, None) => "-".into(),
                        };
                        ui.label(text(Role::Code, serial).color(face.ink));
                        let firmware = match &record.firmware_after {
                            Some(after) => format!("{} -> {after}", record.firmware_before),
                            None => record.firmware_before.clone(),
                        };
                        ui.label(text(Role::Code, firmware).color(face.ink_2));
                        ui.label(
                            text(Role::Label, sentence_case(record.result.as_str())).color(tint),
                        );
                    });
                    if let Some(reason) = &record.reason {
                        wrapping(ui, text(Role::Body, reason).color(face.ink_2));
                    }
                });
        }
    }
}
