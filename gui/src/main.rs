//! The desktop window for updating an MCU-Link's firmware.

// No console window on Windows; the log drawer shows what the console would.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
mod glyph;
mod log;
mod style;
#[cfg(test)]
mod tests;
mod worker;

use clap::Parser;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use verge_core::release::{DEFAULT_REPO, GitHub, Repo, Ureq};
#[cfg(feature = "simulate")]
use verge_core::sim::http::SimHttp;
use verge_core::usb::UsbBus;

use crate::app::{Config, UpdaterApp};
use crate::worker::Worker;

/// Update the firmware on an MCU-Link, in a window.
#[derive(Parser)]
#[command(name = "verge-gui", version)]
struct Args {
    /// The GitHub repository releases of ours come from.
    #[arg(long, value_name = "OWNER/NAME", env = "VERGE_REPO", hide_env_values = true, default_value = DEFAULT_REPO)]
    repo: Repo,

    /// A GitHub token. Public releases need none; a private repository does.
    #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
    github_token: Option<String>,

    /// Use a simulated bench instead of USB. For developing the tool.
    #[cfg(feature = "simulate")]
    #[arg(long, hide = true, value_name = "SCENARIO")]
    simulate: Option<String>,

    /// Real seconds per simulated second.
    #[cfg(feature = "simulate")]
    #[arg(long, hide = true, default_value_t = 1.0)]
    simulate_pace: f64,
}

/// The simulated bench, where the `simulate` feature builds one, and nothing otherwise.
#[cfg(feature = "simulate")]
type Simulated = verge_core::sim::SimBus;
#[cfg(not(feature = "simulate"))]
type Simulated = std::convert::Infallible;

/// The simulated bench `--simulate` names, by its name, or `None` for USB.
#[cfg(feature = "simulate")]
fn simulated(args: &Args) -> Option<(String, Simulated)> {
    let name = args.simulate.as_ref()?;
    match Simulated::scenario(name) {
        Some(bus) => Some((name.clone(), bus.paced(args.simulate_pace))),
        None => {
            let names: Vec<&str> = verge_core::sim::SCENARIOS.iter().map(|(n, _)| *n).collect();
            eprintln!(
                "no simulated bench called {name:?}; there are {}",
                names.join(", ")
            );
            std::process::exit(2);
        }
    }
}

#[cfg(not(feature = "simulate"))]
fn simulated(_: &Args) -> Option<(String, Simulated)> {
    None
}

/// The worker on a simulated bench, with a simulated GitHub that has one release of ours.
#[cfg(feature = "simulate")]
fn simulated_worker(bus: Simulated, repo: Repo, repaint: impl Fn() + Send + 'static) -> Worker {
    let http = SimHttp::new(repo.clone());
    http.publish_ours("v0.3.0");
    let key = http.public_key();
    Worker::spawn(
        bus,
        GitHub::new(http, repo, None).with_key(Some(key)),
        repaint,
    )
}

#[cfg(not(feature = "simulate"))]
fn simulated_worker(bus: Simulated, _: Repo, _: impl Fn() + Send + 'static) -> Worker {
    match bus {}
}

/// Where a batch's record file goes unless the user names another: the user's data directory,
/// `~/.local/share/verge` on Linux. The current directory where the system names none.
fn record_dir() -> std::path::PathBuf {
    match dirs::data_dir() {
        Some(data) => data.join("verge"),
        None => {
            tracing::warn!(
                "the system names no data directory, so batch records go in the current one"
            );
            std::env::current_dir().unwrap_or_default()
        }
    }
}

fn main() -> eframe::Result {
    let args = Args::parse();
    let log = log::Log::default();
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")));
    let window =
        log::Capture(log.clone()).with_filter(EnvFilter::new("verge_core=info,verge_gui=info"));
    tracing_subscriber::registry()
        .with(stderr)
        .with(window)
        .init();

    let simulated = simulated(&args);
    let config = Config {
        repo: args.repo.clone(),
        simulated: simulated.as_ref().map(|(name, _)| name.clone()),
        stock_dirs: verge_core::stock::search_dirs(),
        record_dir: record_dir(),
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("verge")
            .with_inner_size([1080.0, 700.0])
            .with_min_inner_size([860.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "verge",
        options,
        Box::new(move |creation| {
            let ctx = creation.egui_ctx.clone();
            style::install(&ctx);
            log.set_repaint(ctx.clone());
            let repaint = {
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            };
            let worker = match simulated {
                Some((_, bus)) => simulated_worker(bus, args.repo.clone(), repaint),
                None => Worker::spawn(
                    UsbBus::default(),
                    GitHub::new(
                        Ureq::default(),
                        args.repo.clone(),
                        args.github_token.clone(),
                    ),
                    repaint,
                ),
            };
            // A signal closes the window the way its close button does, so the guard against
            // closing mid-flash holds for it too.
            let signal_ctx = ctx.clone();
            if let Err(error) = ctrlc::set_handler(move || {
                signal_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }) {
                tracing::debug!("no signal handler: {error}");
            }
            Ok(Box::new(UpdaterApp::new(Box::new(worker), log, config)))
        }),
    )
}
