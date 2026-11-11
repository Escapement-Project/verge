//! An update's progress as `tracing` spans, drawn as bars by `tracing-indicatif`: one bar over
//! the steps, and one for the step under way.

use escapement_verge_probe::progress::{Progress, Step, Unit};
use indicatif::ProgressStyle;
use tracing::span::EnteredSpan;
use tracing::{Span, info_span};
use tracing_indicatif::span_ext::IndicatifSpanExt;

#[derive(Default)]
pub struct Bars {
    overall: Option<EnteredSpan>,
    current: Option<EnteredSpan>,
}

fn style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template)
        .expect("the bar templates are valid")
        .progress_chars("=> ")
}

impl Bars {
    fn finish_current(&mut self) {
        if self.current.take().is_some()
            && let Some(overall) = &self.overall
        {
            overall.pb_inc(1);
        }
    }
}

impl Bars {
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
        span.pb_set_style(&style("{msg:>22} [{bar:30}] {pos}/{len} steps  {elapsed}"));
        span.pb_set_message("update");
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
                    "{msg:>22} [{bar:30}] {bytes}/{total_bytes}  {binary_bytes_per_sec}",
                ));
                span.pb_set_length(total);
            }
            Unit::None => {
                span.pb_set_style(&style("{msg:>22} {spinner} {elapsed}"));
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
