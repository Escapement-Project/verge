//! An update's progress as `tracing` spans, drawn as bars by `tracing-indicatif`: one bar over
//! the steps, and one for the step under way.

use escapement_verge_probe::progress::{Progress, Step, Unit};
use indicatif::ProgressStyle;
use tracing::span::EnteredSpan;
use tracing::{Span, info_span};
use tracing_indicatif::span_ext::IndicatifSpanExt;

/// The width of the label before each bar: the longest step label, or a batch's port and serial.
const LABEL: usize = 26;

pub struct Bars {
    /// What the bar over the steps says: the operation, or the unit a batch updates.
    name: String,
    overall: Option<EnteredSpan>,
    current: Option<EnteredSpan>,
}

impl Default for Bars {
    fn default() -> Bars {
        Bars::new("update")
    }
}

fn style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(&template.replace("{msg}", &format!("{{msg:>{LABEL}}}")))
        .expect("the bar templates are valid")
        .progress_chars("=> ")
}

impl Bars {
    pub fn new(name: impl Into<String>) -> Bars {
        Bars {
            name: name.into(),
            overall: None,
            current: None,
        }
    }

    /// Names the bar over the steps of the next plan.
    pub fn rename(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    fn finish_current(&mut self) {
        if self.current.take().is_some()
            && let Some(overall) = &self.overall
        {
            overall.pb_inc(1);
        }
    }

    /// Removes the bars, as when one unit of a batch is done.
    pub fn clear(&mut self) {
        self.current.take();
        self.overall.take();
    }
}

impl Drop for Bars {
    fn drop(&mut self) {
        self.clear();
    }
}

impl Progress for Bars {
    fn plan(&mut self, steps: &[Step]) {
        self.finish_current();
        let span = info_span!("update");
        span.pb_set_style(&style("{msg} [{bar:30}] {pos}/{len} steps  {elapsed}"));
        span.pb_set_message(&self.name);
        span.pb_set_length(steps.len() as u64);
        span.pb_start();
        self.overall = Some(span.entered());
    }

    fn start(&mut self, step: Step, total: u64) {
        self.finish_current();
        let span: Span = info_span!("step", step = %step);
        match step.unit() {
            Unit::Bytes => {
                span.pb_set_style(&style(
                    "{msg} [{bar:30}] {bytes}/{total_bytes}  {binary_bytes_per_sec}",
                ));
                span.pb_set_length(total);
            }
            Unit::None => {
                span.pb_set_style(&style("{msg} {spinner} {elapsed}"));
            }
        }
        span.pb_set_message(&step.to_string());
        span.pb_start();
        self.current = Some(span.entered());
    }

    fn advance(&mut self, done: u64) {
        if let Some(current) = &self.current {
            current.pb_set_position(done);
        }
    }

    fn note(&mut self, text: &str) {
        tracing::warn!("{text}");
    }
}

/// A spinner with a line of text that changes, for something that runs until it ends: a batch
/// waiting for units, or a recording. Hidden where stderr is not a terminal.
pub struct Spinner(Span);

impl Spinner {
    pub fn new(text: &str) -> Spinner {
        let span = info_span!("spinner");
        span.pb_set_style(
            &ProgressStyle::with_template("{spinner} {msg}  {elapsed}")
                .expect("the spinner template is valid"),
        );
        span.pb_set_message(text);
        span.pb_start();
        Spinner(span)
    }

    pub fn set(&self, text: &str) {
        self.0.pb_set_message(text);
    }
}
