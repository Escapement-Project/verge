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
use updater_core::release::{DEFAULT_REPO, GitHub, Repo, Ureq};
use updater_core::sim::SimBus;
use updater_core::sim::http::SimHttp;
use updater_core::usb::UsbBus;

use crate::app::{Config, UpdaterApp};
use crate::worker::Worker;

/// Update the firmware on an MCU-Link, in a window.
#[derive(Parser)]
#[command(name = "verge-update-gui", version)]
struct Args {
    /// The GitHub repository releases of ours come from.
    #[arg(long, value_name = "OWNER/NAME", env = "VERGE_UPDATE_REPO", hide_env_values = true, default_value = DEFAULT_REPO)]
    repo: Repo,

    /// A GitHub token. Public releases need none; a private repository does.
    #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
    github_token: Option<String>,

    /// Use a simulated bench instead of USB. For developing the tool.
    #[arg(long, hide = true, value_name = "SCENARIO")]
    simulate: Option<String>,

    /// Real seconds per simulated second.
    #[arg(long, hide = true, default_value_t = 1.0)]
    simulate_pace: f64,
}

fn main() -> eframe::Result {
    let args = Args::parse();
    let log = log::Log::default();
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")));
    let window = log::Capture(log.clone())
        .with_filter(EnvFilter::new("updater_core=info,verge_update_gui=info"));
    tracing_subscriber::registry()
        .with(stderr)
        .with(window)
        .init();

    let simulated = match &args.simulate {
        Some(name) => match SimBus::scenario(name) {
            Some(bus) => Some((name.clone(), bus.paced(args.simulate_pace))),
            None => {
                let names: Vec<&str> = updater_core::sim::SCENARIOS
                    .iter()
                    .map(|(n, _)| *n)
                    .collect();
                eprintln!(
                    "no simulated bench called {name:?}; there are {}",
                    names.join(", ")
                );
                std::process::exit(2);
            }
        },
        None => None,
    };
    let config = Config {
        repo: args.repo.clone(),
        simulated: simulated.as_ref().map(|(name, _)| name.clone()),
        stock_dirs: updater_core::stock::search_dirs(),
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("verge-update")
            .with_inner_size([1080.0, 700.0])
            .with_min_inner_size([860.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "verge-update",
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
                Some((_, bus)) => {
                    let http = SimHttp::new(args.repo.clone());
                    http.publish_ours("v0.3.0");
                    Worker::spawn(bus, GitHub::new(http, args.repo.clone(), None), repaint)
                }
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
