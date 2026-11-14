//! The desktop window for updating an MCU-Link's firmware.

// No console window on Windows; the log drawer shows what the console would.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
mod bench;
mod bench_worker;
mod glyph;
mod log;
mod mark;
mod style;
#[cfg(test)]
mod tests;
// A copy of the design tokens' generated file, kept byte for byte so that the site's
// `design/build.py --check` can compare it. Not every role is drawn.
#[allow(dead_code)]
mod tokens;
mod worker;

use std::sync::Arc;

use clap::Parser;
use escapement_verge_probe::cache::Cache;
use escapement_verge_probe::release::{DEFAULT_REPO, GitHub, Repo, Ureq};
#[cfg(feature = "simulate")]
use escapement_verge_probe::sim::http::SimHttp;
use escapement_verge_probe::usb::UsbBus;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::app::{Config, VergeApp};
use crate::worker::Worker;

/// Update the firmware on an MCU-Link, in a window.
#[derive(Parser)]
#[command(name = "verge-gui", version)]
struct Args {
    /// The GitHub repository Verge releases come from.
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
type Simulated = escapement_verge_probe::sim::SimBus;
#[cfg(not(feature = "simulate"))]
type Simulated = std::convert::Infallible;

/// The simulated bench `--simulate` names, by its name, or `None` for USB.
#[cfg(feature = "simulate")]
fn simulated(args: &Args) -> Option<(String, Simulated)> {
    let name = args.simulate.as_ref()?;
    match Simulated::scenario(name) {
        Some(bus) => Some((name.clone(), bus.paced(args.simulate_pace))),
        None => {
            let names: Vec<&str> = escapement_verge_probe::sim::SCENARIOS
                .iter()
                .map(|(n, _)| *n)
                .collect();
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

/// The window's Wayland app_id and X11 class. The Linux desktop entry is named for it and names it
/// again, so the desktop pairs the window with the entry's name and icon; macOS's bundle identifier
/// is the same with '-' for '_', which Apple does not allow.
const APP_ID: &str = "io.github.escapement_project.Verge";

/// Verge's app icon, the design tokens' 512 px rendering.
fn icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../icons/verge-512.png"))
        .expect("the icon is a PNG; tests::the_icon_decodes checks it")
}

fn main() -> eframe::Result {
    let args = Args::parse();
    let log = log::Log::default();
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")));
    let window = log::Capture(log.clone())
        .with_filter(EnvFilter::new("escapement_verge_probe=info,verge_gui=info"));
    tracing_subscriber::registry()
        .with(stderr)
        .with(window)
        .init();

    let simulated = simulated(&args);
    // A simulated GitHub signs with a key made for the run, so a copy kept from another run
    // would never check out.
    let cache = match simulated {
        Some(_) => None,
        None => Cache::user(),
    };
    let config = Config {
        repo: args.repo.clone(),
        simulated: simulated.as_ref().map(|(name, _)| name.clone()),
        stock_dirs: escapement_verge_probe::stock::search_dirs(),
        record_dir: record_dir(),
        cache: cache.clone(),
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("verge")
            .with_app_id(APP_ID)
            .with_inner_size([1080.0, 700.0])
            .with_min_inner_size([860.0, 560.0])
            .with_drag_and_drop(true)
            .with_icon(Arc::new(icon())),
        ..Default::default()
    };
    eframe::run_native(
        "verge",
        options,
        Box::new(move |creation| {
            let ctx = creation.egui_ctx.clone();
            style::install(&ctx);
            #[cfg(target_os = "linux")]
            follow_desktop_face(&ctx);
            #[cfg(target_os = "linux")]
            if desktop_reduces_motion() {
                mark::set_motion(&ctx, false);
            }
            log.set_repaint(ctx.clone());
            let repaint = {
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            };
            // Without `simulate`, Simulated is Infallible and the first arm cannot run.
            #[cfg_attr(not(feature = "simulate"), allow(unreachable_code))]
            let worker = match simulated {
                Some((_, bus)) => simulated_worker(bus, args.repo.clone(), repaint),
                None => Worker::spawn(
                    UsbBus::default(),
                    GitHub::new(
                        Ureq::default(),
                        args.repo.clone(),
                        args.github_token.clone(),
                    )
                    .with_cache(cache),
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
            Ok(Box::new(VergeApp::new(Box::new(worker), log, config)))
        }),
    )
}

/// The XDG portal's `reduced-motion` setting: 1 when the desktop asks for reduced motion.
#[cfg(target_os = "linux")]
fn portal_reduced_motion() -> zbus::Result<u32> {
    let connection = zbus::blocking::Connection::session()?;
    let reply = connection.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.Settings"),
        "ReadOne",
        &("org.freedesktop.appearance", "reduced-motion"),
    )?;
    let value: zbus::zvariant::OwnedValue = reply.body().deserialize()?;
    Ok(u32::try_from(value)?)
}

/// Whether the desktop asks for reduced motion, read once at start. Without a portal, or with one
/// too old to have the setting, the mark moves.
#[cfg(target_os = "linux")]
fn desktop_reduces_motion() -> bool {
    match portal_reduced_motion() {
        Ok(setting) => setting == 1,
        Err(error) => {
            tracing::debug!("no reduced-motion setting: {error}");
            false
        }
    }
}

/// Day or night as the desktop sets it. winit reports no system theme on Linux, so egui would stay
/// on night there: read the XDG portal's colour-scheme setting at start, then follow its changes
/// for as long as the app runs. Without a portal (no session bus, or no answer) the face stays on
/// night, as before.
#[cfg(target_os = "linux")]
fn follow_desktop_face(ctx: &egui::Context) {
    fn apply(ctx: &egui::Context, mode: dark_light::Mode) {
        match mode {
            dark_light::Mode::Dark => ctx.set_theme(egui::Theme::Dark),
            dark_light::Mode::Light => ctx.set_theme(egui::Theme::Light),
            dark_light::Mode::Unspecified => {}
        }
    }
    match dark_light::detect() {
        Ok(mode) => apply(ctx, mode),
        Err(error) => {
            tracing::debug!("no desktop face: {error}");
            return;
        }
    }
    let watcher = match dark_light::subscribe() {
        Ok(watcher) => watcher,
        Err(error) => {
            tracing::debug!("no desktop face changes: {error}");
            return;
        }
    };
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        for mode in watcher.iter() {
            apply(&ctx, mode);
            ctx.request_repaint();
        }
    });
}
