use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use verge_probe::capture::{Archive, DigitalView, Envelope, Interval, Statistics, View};

pub struct Query {
    pub generation: u64,
    pub path: PathBuf,
    /// The range on screen; `None` follows the capture's end by `live_span`.
    pub interval: Option<Interval>,
    pub live_span: u64,
    pub selection: Option<Interval>,
    pub marker_after: Option<verge_probe::capture::MarkerCursor>,
    pub marker_selected: Option<verge_probe::capture::MarkerCursor>,
    pub pixels: usize,
    pub cancel: Arc<AtomicBool>,
}
pub struct Answer {
    /// The visible range and as much again either side, so a pan or a zoom out has bins to
    /// draw before its own answer comes back.
    pub view: View,
    pub digital: DigitalView,
    pub digital_fallback: Vec<verge_probe::capture::DigitalBin>,
    pub markers: verge_probe::capture::MarkerPage,
    pub marker_list: verge_probe::capture::MarkerPage,
    pub marker_successor: Option<verge_probe::capture::StoredMarker>,
    pub marker_selected: Option<verge_probe::capture::MarkerCursor>,
    pub visible: Interval,
    /// The visible range's own statistics.
    pub window: Statistics,
    pub overview: View,
    /// Beyond `view`, the overview's bins, and the parts of those `view` cuts, read for their own.
    pub fallback: Vec<Envelope>,
    pub selection: Option<(Interval, Statistics)>,
}
struct State {
    request: Option<Query>,
    result: Option<(u64, Result<Answer, String>)>,
    stopped: bool,
}
pub struct Queries {
    shared: Arc<(Mutex<State>, Condvar)>,
    cancel: Option<Arc<AtomicBool>>,
    thread: Option<JoinHandle<()>>,
}
impl Queries {
    pub fn new() -> Self {
        let shared = Arc::new((
            Mutex::new(State {
                request: None,
                result: None,
                stopped: false,
            }),
            Condvar::new(),
        ));
        let work = shared.clone();
        let thread = thread::Builder::new()
            .name("capture-query".into())
            .spawn(move || {
                loop {
                    let (lock, wake) = &*work;
                    let request = {
                        let mut state = lock.lock().expect("query lock");
                        while state.request.is_none() && !state.stopped {
                            state = wake.wait(state).expect("query wait");
                        }
                        if state.stopped {
                            break;
                        }
                        state.request.take().expect("request")
                    };
                    let answer = run(&request).map_err(|error| format!("{error:#}"));
                    let mut state = lock.lock().expect("query lock");
                    if !request.cancel.load(Ordering::Relaxed) {
                        state.result = Some((request.generation, answer));
                    }
                }
            })
            .expect("query worker");
        Self {
            shared,
            cancel: None,
            thread: Some(thread),
        }
    }
    pub fn send(&mut self, query: Query) {
        self.cancel();
        self.cancel = Some(query.cancel.clone());
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("query lock");
        state.result = None;
        state.request = Some(query);
        wake.notify_one();
    }
    pub fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let mut state = self.shared.0.lock().expect("query lock");
        state.result = None;
        state.request = None;
    }
    pub fn take(&self) -> Option<(u64, Result<Answer, String>)> {
        self.shared.0.lock().expect("query lock").result.take()
    }
}
fn run(query: &Query) -> Result<Answer> {
    let archive = Archive::open(&query.path)?;
    let extent = archive.extent()?;
    let visible = query.interval.unwrap_or_else(|| {
        Interval::new(
            extent
                .end_ns
                .saturating_sub(query.live_span)
                .max(extent.start_ns),
            extent.end_ns.max(extent.start_ns + 1),
        )
    });
    let width = visible.duration_ns();
    let around = Interval::new(
        visible.start_ns.saturating_sub(width),
        visible.end_ns.saturating_add(width),
    );
    let view = archive.view(around, query.pixels.saturating_mul(3), &query.cancel)?;
    let digital = archive.digital(around, query.pixels.saturating_mul(3), &query.cancel)?;
    let markers = archive.markers(visible, None, 512, &query.cancel)?;
    let all = Interval::new(0, i64::MAX as u64);
    let marker_list = archive.markers(all, query.marker_after, 24, &query.cancel)?;
    let marker_successor = query
        .marker_selected
        .map(|cursor| {
            archive
                .markers(all, Some(cursor), 1, &query.cancel)
                .map(|page| page.entries.into_iter().next())
        })
        .transpose()?
        .flatten();
    let window = archive.statistics(visible, &query.cancel)?;
    // An overview bin that `around` cuts holds levels from its part inside, so each part outside is
    // read again rather than drawn with the whole bin's.
    let overview = archive.view(extent, 128, &query.cancel)?;
    let mut fallback = Vec::new();
    for bin in &overview.envelopes {
        for interval in outside_intervals(bin.interval, around) {
            if interval == bin.interval {
                fallback.push(bin.clone());
                continue;
            }
            let statistics = archive.statistics(interval, &query.cancel)?;
            if statistics.samples > 0 {
                fallback.push(Envelope {
                    interval,
                    gap: statistics.measured_ns < interval.duration_ns(),
                    statistics,
                });
            }
        }
    }
    let digital_overview = archive.digital(extent, 128, &query.cancel)?;
    let mut digital_fallback = Vec::new();
    for bin in &digital_overview.bins {
        for interval in outside_intervals(bin.interval, around) {
            if interval == bin.interval {
                digital_fallback.push(bin.clone());
            } else {
                digital_fallback.extend(archive.digital(interval, 1, &query.cancel)?.bins);
            }
        }
    }
    let selection = query
        .selection
        .map(|i| archive.statistics(i, &query.cancel).map(|s| (i, s)))
        .transpose()?;
    Ok(Answer {
        view,
        digital,
        digital_fallback,
        markers,
        marker_list,
        marker_successor,
        marker_selected: query.marker_selected,
        visible,
        window,
        overview,
        fallback,
        selection,
    })
}
/// The parts of `interval` before and after `covered`.
fn outside_intervals(interval: Interval, covered: Interval) -> impl Iterator<Item = Interval> {
    [
        Interval::new(interval.start_ns, interval.end_ns.min(covered.start_ns)),
        Interval::new(interval.start_ns.max(covered.end_ns), interval.end_ns),
    ]
    .into_iter()
    .filter(|part| part.duration_ns() > 0)
}
impl Drop for Queries {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.shared.0.lock().expect("query lock").stopped = true;
        self.shared.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
