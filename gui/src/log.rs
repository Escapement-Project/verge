//! The window's log: a `tracing` layer that keeps the newest lines for the drawer.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

const KEEP: usize = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// When it came, in local time: `14:26:08`.
    pub time: String,
    pub level: Level,
    pub text: String,
}

impl std::fmt::Display for Line {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {:<5} {}", self.time, self.level, self.text)
    }
}

/// The time of day now in the local time zone, to the second.
fn now() -> String {
    jiff::ZonedDateTime::now().strftime("%H:%M:%S").to_string()
}

#[derive(Default)]
struct Inner {
    lines: VecDeque<Line>,
    warnings: usize,
    errors: usize,
    repaint: Option<egui::Context>,
}

/// The kept lines, shared between the layer and the window.
#[derive(Clone, Default)]
pub struct Log(Arc<Mutex<Inner>>);

impl Log {
    /// Repaints the window when a line of ours arrives.
    pub fn set_repaint(&self, ctx: egui::Context) {
        self.lock().repaint = Some(ctx);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while a line was pushed leaves the buffer whole; keep using it.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Every kept line, oldest first, one to a line of text.
    #[must_use]
    pub fn text(&self) -> String {
        let inner = self.lock();
        let mut out = String::new();
        for line in &inner.lines {
            let _ = writeln!(out, "{line}");
        }
        out
    }

    /// The newest `count` lines, oldest first, and the warning and error counts.
    #[must_use]
    pub fn tail(&self, count: usize) -> (Vec<Line>, usize, usize) {
        let inner = self.lock();
        let skip = inner.lines.len().saturating_sub(count);
        (
            inner.lines.iter().skip(skip).cloned().collect(),
            inner.warnings,
            inner.errors,
        )
    }

    pub fn push(&self, level: Level, text: String) {
        let repaint = {
            let mut inner = self.lock();
            if inner.lines.len() == KEEP {
                inner.lines.pop_front();
            }
            match level {
                Level::WARN => inner.warnings += 1,
                Level::ERROR => inner.errors += 1,
                _ => {}
            }
            inner.lines.push_back(Line {
                time: now(),
                level,
                text,
            });
            inner.repaint.clone()
        };
        if let Some(ctx) = repaint {
            ctx.request_repaint();
        }
    }
}

struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }
}

/// Collects our crates' events into a [`Log`].
///
/// Only our crates: egui and eframe log through `log`, sometimes while egui holds its own lock,
/// and a repaint asked for from inside that lock deadlocks.
pub struct Capture(pub Log);

impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let target = event.metadata().target();
        if !(target.starts_with("verge_gui") || target.starts_with("escapement_verge_probe")) {
            return;
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0.push(*event.metadata().level(), message.0);
    }
}
