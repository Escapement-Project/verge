//! The batch view: an image and the units it takes, armed once, then every unit plugged in after
//! that updated on its own, one at a time.

use std::path::{Path as FsPath, PathBuf};

use egui::{Color32, Margin, Stroke, Ui, vec2};
use verge_core::batch::{self, Board, Filter, OursFilter, Record, Settled, StockFilter, Tally};
use verge_core::paths::ISP_IDENTITY_WARNING;
use verge_core::progress::{Cancel, Unit as StepUnit};
use verge_core::source::{ImageVersion, Source};
use verge_core::version::StockVersion;

use super::{
    Choice, Current, Picked, VergeApp, edged, field, frame, kib, label, outline, slot, wrapping,
};
use crate::glyph::{self, Glyph};
use crate::style::{Role, color, space, text};
use crate::worker::{BatchState, Event, Request, arm_error};

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

    /// The filter and the count, or why the form does not make one.
    fn filter(&self) -> Result<(Filter, Option<usize>), String> {
        let below = self.stock_below.trim();
        let stock = match (self.stock, below.is_empty()) {
            (false, _) => StockFilter::None,
            (true, true) => StockFilter::Any,
            (true, false) => StockFilter::Below(
                StockVersion::parse(below.strip_prefix(['V', 'v']).unwrap_or(below)).ok_or_else(
                    || format!("\"{below}\" is not a stock version; write it like 3.172."),
                )?,
            ),
        };
        let ours = match (self.ours, self.ours_any) {
            (false, _) => OursFilter::None,
            (true, true) => OursFilter::Any,
            (true, false) => OursFilter::Older,
        };
        if stock == StockFilter::None && ours == OursFilter::None && !self.isp {
            return Err("Tick at least one kind of unit for the batch to take.".into());
        }
        let limit = if self.stop_after {
            let count: usize = self
                .count
                .trim()
                .parse()
                .map_err(|_| "Write how many units to stop after, as a number.".to_owned())?;
            if count == 0 {
                return Err("Stopping after no units would take none.".into());
            }
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
}

impl BatchRun {
    pub(in crate::app) fn running(&self) -> bool {
        self.ended.is_none()
    }

    /// An update's progress, for the unit under way.
    pub(in crate::app) fn progress(&mut self, event: Event, now: f64) {
        let Some((_, current)) = &mut self.current else {
            return;
        };
        match event {
            Event::Start { step, total } => {
                *current = Some(Current {
                    step,
                    total,
                    done: 0,
                    since: now,
                });
            }
            Event::Advance(done) => {
                if let Some(current) = current {
                    current.done = done;
                }
            }
            _ => {}
        }
    }

    pub(in crate::app) fn handle(&mut self, event: Event, _now: f64) {
        match event {
            Event::BatchArmed { name, filter } => {
                self.name = Some(name);
                self.filter = filter;
            }
            Event::BatchStarting(port) => self.current = Some((port, None)),
            Event::BatchRecord(record) => {
                if self
                    .current
                    .as_ref()
                    .is_some_and(|(p, _)| *p == record.port)
                {
                    self.current = None;
                }
                self.records.push(*record);
            }
            Event::BatchState(state) => self.state = state,
            Event::BatchEnded(result) => {
                self.current = None;
                if let Ok(tally) = &result {
                    self.state.tally = *tally;
                }
                self.ended = Some(result);
            }
            _ => {}
        }
    }
}

/// A result's glyph and colour, the same family as the unit view's.
fn mark(result: Settled) -> (Glyph, Color32) {
    match result {
        Settled::Updated => (glyph::DONE, color::OURS),
        Settled::JumperFitted | Settled::NotRestarted => (glyph::ATTENTION, color::ATTENTION),
        Settled::Failed => (glyph::FAILED, color::FAULT),
        Settled::Current | Settled::Skipped | Settled::Waiting => (glyph::STOCK, color::TEXT_2),
    }
}

impl VergeApp {
    /// The image chosen above, as a batch's source, or why there is none. A batch that cannot arm
    /// with it says so here, as far as the choice tells before the image is loaded: a release's
    /// image is checked against the board once it is downloaded.
    fn batch_source(&self, filter: &Filter) -> Result<(Source, String), String> {
        let (source, what, version) = match self.choice {
            Choice::Release => match &self.release {
                None => return Err("Looking up the latest release.".into()),
                Some(Err(error)) => return Err(error.clone()),
                Some(Ok(release)) => (
                    Source::Release(Some(release.tag.clone())),
                    format!("release {}", release.tag),
                    ImageVersion::Ours(release.tag.clone()),
                ),
            },
            Choice::Stock => match &self.stock {
                None => return Err("No stock image in LinkServer's usual places.".into()),
                Some(found) => (
                    Source::Stock(found.path.parent().map(FsPath::to_path_buf)),
                    format!("stock V{}", found.version),
                    ImageVersion::Stock(found.version),
                ),
            },
            Choice::File => match &self.picked {
                Some(Picked {
                    path,
                    image: Ok(image),
                }) => {
                    batch::check(filter, image, &ImageVersion::Unknown)
                        .map_err(|error| arm_error(&error))?;
                    (
                        Source::File(path.clone()),
                        path.display().to_string(),
                        ImageVersion::Unknown,
                    )
                }
                Some(Picked {
                    image: Err(error), ..
                }) => return Err(error.clone()),
                None => return Err("Pick an ELF or S-record, or drop one on the window.".into()),
            },
        };
        filter
            .check_version(&version)
            .map_err(|error| arm_error(&error))?;
        Ok((source, what))
    }

    pub(in crate::app) fn batch_view(&mut self, ui: &mut Ui, now: f64) {
        if self.batch.is_some() {
            self.batch_running(ui, now);
        } else {
            self.batch_setup(ui, now);
        }
    }

    fn batch_setup(&mut self, ui: &mut Ui, now: f64) {
        frame(ui, None, Stroke::new(1.0, color::BORDER), |ui| {
            ui.horizontal(|ui| {
                glyph::WAITING.show(ui, 48.0, color::TEXT);
                ui.add_space(space::M);
                ui.label(text(Role::Display, "BATCH").color(color::TEXT));
            });
            wrapping(
                ui,
                text(Role::Instruction, "ONE IMAGE FOR EVERY UNIT PLUGGED IN").color(color::TEXT),
            );
            wrapping(
                ui,
                text(
                    Role::Body,
                    "Choose the image and the units it takes, and arm it once. Then plug units in, one after another or several on a hub. Each goes through its bootloader on its own, one at a time, and blinks when it is done.",
                )
                .color(color::TEXT_2),
            );
        });
        ui.add_space(space::M);
        self.image_choice(ui, None);
        ui.add_space(space::S);
        let form = &mut self.batch_form;
        field(ui, "takes", |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(
                    &mut form.stock,
                    text(Role::Body, "Stock").color(color::TEXT),
                );
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
                ui.checkbox(&mut form.ours, text(Role::Body, "Ours").color(color::TEXT));
                ui.add_enabled_ui(form.ours, |ui| {
                    ui.checkbox(
                        &mut form.ours_any,
                        text(
                            Role::Body,
                            "of any build, not only those older than the image",
                        )
                        .color(color::TEXT),
                    );
                });
            });
            ui.checkbox(
                &mut form.isp,
                text(
                    Role::Body,
                    "Units that arrive in ISP mode, as after fitting the jumper",
                )
                .color(color::TEXT),
            );
            ui.checkbox(
                &mut form.attached,
                text(
                    Role::Body,
                    "Units attached now, not only those plugged in from here on",
                )
                .color(color::TEXT),
            );
        });
        field(ui, "board", |ui| {
            ui.horizontal(|ui| {
                for board in Board::ALL {
                    ui.radio_value(
                        &mut form.board,
                        board,
                        text(Role::Body, board.to_string()).color(color::TEXT),
                    );
                }
            });
        });
        field(ui, "stop", |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(
                    &mut form.stop_after,
                    text(Role::Body, "after").color(color::TEXT),
                );
                ui.add_enabled(
                    form.stop_after,
                    egui::TextEdit::singleline(&mut form.count).desired_width(48.0),
                );
                ui.label(text(Role::Body, "units are updated").color(color::TEXT));
            });
        });
        field(ui, "record", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut form.record)
                    .desired_width(ui.available_width().min(640.0)),
            );
            wrapping(
                ui,
                text(Role::Body, "One JSON line per unit: when, its port and serial, the firmware before and after, and the result.")
                    .color(color::TEXT_2),
            );
        });
        ui.add_space(space::L);

        let ready = self.batch_form.filter().and_then(|(filter, limit)| {
            self.batch_source(&filter)
                .map(|(source, what)| (source, what, filter, limit))
        });
        let (source, what, filter, limit) = match ready {
            Ok(ready) => ready,
            Err(reason) => {
                ui.add_enabled(
                    false,
                    egui::Button::new(text(Role::Action, "CANNOT ARM").color(color::TEXT_2))
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(1.0, color::BORDER))
                        .min_size(vec2(320.0, crate::style::SLOT_HEIGHT)),
                );
                wrapping(ui, text(Role::Body, reason).color(color::TEXT_2));
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
            edged(ui, color::ATTENTION, &question);
            ui.add_space(space::S);
            ui.horizontal(|ui| {
                if slot(ui, "Confirm", Some(color::OURS)).clicked() {
                    let record = PathBuf::from(self.batch_form.record.trim());
                    let stop = Cancel::new();
                    let cancel = Cancel::new();
                    self.batch = Some(BatchRun {
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
                    });
                    self.batch_form.confirming = None;
                    self.backend.send(Request::Batch {
                        source: source.clone(),
                        filter: filter.clone(),
                        download: super::agreed(download.as_ref()),
                        limit,
                        record,
                        stop,
                        cancel,
                    });
                }
                if slot(ui, "Back", None).clicked() {
                    self.batch_form.confirming = None;
                }
            });
            return;
        }
        if slot(ui, "Arm batch", Some(color::OURS)).clicked() {
            self.batch_form.confirming = Some(Arming {
                download: self.download(&source),
                source,
            });
        }
    }

    fn batch_running(&mut self, ui: &mut Ui, now: f64) {
        let Some(run) = &self.batch else {
            return;
        };
        let (word, instruction, glyph, fill) = match &run.ended {
            Some(Ok(_)) => ("BATCH", "ENDED", glyph::DONE, Some(color::OURS)),
            Some(Err(_)) if run.name.is_none() => {
                ("BATCH", "NOT ARMED", glyph::FAILED, Some(color::FAULT))
            }
            Some(Err(_)) => (
                "BATCH",
                "STOPPED BY A FAULT",
                glyph::FAILED,
                Some(color::FAULT),
            ),
            None if run.state.paused.is_some() => ("BATCH", "PAUSED", glyph::ATTENTION, None),
            None if run.stop.is_cancelled() => (
                "BATCH",
                "STOPPING: THE UNIT UNDER WAY FINISHES FIRST",
                glyph::WORKING,
                None,
            ),
            None => ("BATCH", "PLUG UNITS IN", glyph::WORKING, None),
        };
        let ink = if fill.is_some() {
            color::INK
        } else {
            color::TEXT
        };
        let stroke = if fill.is_some() {
            Stroke::NONE
        } else {
            Stroke::new(2.0, color::TEXT)
        };
        frame(ui, fill, stroke, |ui| {
            ui.horizontal(|ui| {
                glyph.show(ui, 48.0, ink);
                ui.add_space(space::M);
                ui.label(text(Role::Display, word).color(ink));
            });
            wrapping(ui, text(Role::Instruction, instruction).color(ink));
            let mut facts = vec![
                run.name
                    .clone()
                    .unwrap_or_else(|| "loading the image".into()),
            ];
            facts.push(run.filter.clone());
            facts.push(format!("{:.0} s", now - run.started));
            wrapping(ui, text(Role::Data, facts.join(" · ")).color(ink));
            wrapping(
                ui,
                text(Role::Data, format!("recording to {}", run.record.display())).color(ink),
            );
        });
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
                        let tint = match (name, count) {
                            ("failed", n) if n > 0 => color::FAULT,
                            ("updated", n) if n > 0 => color::OURS,
                            _ => color::TEXT,
                        };
                        let response = ui.label(text(Role::Tally, count.to_string()).color(tint));
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::Label,
                                true,
                                format!("{count} {name}"),
                            )
                        });
                        label(ui, name);
                    },
                );
            }
        });
        if let Some(reason) = &run.state.paused {
            edged(ui, color::ATTENTION, reason);
        }
        if let Some(Err(error)) = &run.ended {
            edged(ui, color::FAULT, error);
        }
        if let Some((port, current)) = &run.current {
            ui.add_space(space::S);
            field(ui, "now", |ui| {
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
                ui.label(text(Role::Data, words).color(color::TEXT));
                if let Some(current) = current
                    && current.step.unit() == StepUnit::Bytes
                    && current.total > 0
                {
                    let fraction = current.done as f32 / current.total as f32;
                    let (rect, _) = ui.allocate_exact_size(
                        vec2(ui.available_width().min(640.0), 8.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(rect, 0.0, color::BORDER);
                    let mut done = rect;
                    done.set_width(rect.width() * fraction.clamp(0.0, 1.0));
                    ui.painter().rect_filled(done, 0.0, color::TEXT);
                }
            });
        }
        if !run.state.waiting.is_empty() {
            field(ui, "waiting", |ui| {
                let names: Vec<String> = run
                    .state
                    .waiting
                    .iter()
                    .map(|(port, serial)| match serial {
                        Some(serial) => format!("{port} ({serial})"),
                        None => port.clone(),
                    })
                    .collect();
                wrapping(ui, text(Role::Data, names.join(", ")).color(color::TEXT));
            });
        }
        ui.add_space(space::M);
        if run.records.is_empty() {
            label(ui, "no unit yet");
        }
        for record in run.records.iter().rev() {
            egui::Frame::new()
                .fill(color::SURFACE)
                .inner_margin(Margin::same(space::S as i8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (glyph, tint) = mark(record.result);
                        glyph.show(ui, 20.0, tint);
                        ui.label(text(Role::Data, &record.port).color(color::TEXT));
                        let serial = match (&record.serial_before, &record.serial_after) {
                            (Some(a), Some(b)) if a != b => format!("{a} -> {b}"),
                            (Some(a), _) => a.clone(),
                            (None, Some(b)) => b.clone(),
                            (None, None) => "-".into(),
                        };
                        ui.label(text(Role::Data, serial).color(color::TEXT));
                        let firmware = match &record.firmware_after {
                            Some(after) => format!("{} -> {after}", record.firmware_before),
                            None => record.firmware_before.clone(),
                        };
                        ui.label(text(Role::Data, firmware).color(color::TEXT_2));
                        ui.label(
                            text(Role::Label, record.result.as_str().to_uppercase()).color(tint),
                        );
                    });
                    if let Some(reason) = &record.reason {
                        wrapping(ui, text(Role::Body, reason).color(color::TEXT_2));
                    }
                });
        }
        ui.add_space(space::L);
        if run.running() {
            if run.stop.is_cancelled() {
                if outline(ui, "Cancel the unit under way", !run.cancel.is_cancelled()).clicked() {
                    run.cancel.cancel();
                }
            } else if slot(ui, "Stop", None).clicked() {
                run.stop.cancel();
            }
        } else if slot(ui, "Back", None).clicked() {
            self.batch = None;
            self.batch_form.record = record_path(&self.batch_form.dir);
        }
    }
}
