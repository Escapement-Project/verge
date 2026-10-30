use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use verge_probe::capture::{Archive, Interval, Statistics, View};

pub struct Query {
    pub generation: u64,
    pub path: PathBuf,
    pub interval: Option<Interval>,
    pub live_span: u64,
    pub selection: Option<Interval>,
    pub pixels: usize,
    pub cancel: Arc<AtomicBool>,
}
#[derive(Clone)]
pub struct Answer {
    pub view: View,
    pub overview: View,
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
    let interval = query.interval.unwrap_or_else(|| {
        Interval::new(
            extent
                .end_ns
                .saturating_sub(query.live_span)
                .max(extent.start_ns),
            extent.end_ns.max(extent.start_ns + 1),
        )
    });
    let view = archive.view(interval, query.pixels, &query.cancel)?;
    let overview = archive.view(extent, 128, &query.cancel)?;
    let selection = query
        .selection
        .map(|i| archive.statistics(i, &query.cancel).map(|s| (i, s)))
        .transpose()?;
    Ok(Answer {
        view,
        overview,
        selection,
    })
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
