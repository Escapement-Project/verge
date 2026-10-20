mod progress;

use std::io::IsTerminal;
use std::path::{Path as FsPath, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Args, Parser, Subcommand};
use tracing_indicatif::{IndicatifLayer, indicatif_println};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{EnvFilter, filter_fn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use updater_core::detect::{Guess, State};
use updater_core::image::{Image, PAGE};
use updater_core::release::{DEFAULT_REPO, GitHub, Http, Repo, Ureq};
use updater_core::sim::http::SimHttp;
use updater_core::sim::{SCENARIOS, SimBus};
use updater_core::source::{self, LoadError};
use updater_core::update::{self, Outcome, Timing, UpdateError};
use updater_core::usb::UsbBus;
use updater_core::version::Relation;
use updater_core::{Bus, Cancel, Firmware, Path, Unit, detect, offer};

use crate::progress::Bars;

const EXAMPLES: &str = "\
Examples:
  verge-update                     what is attached, and how each unit can be updated
  verge-update update              install the latest release of ours on the only unit
  verge-update update --serial CJ3E7A91C2 --release v0.3.0
  verge-update update --stock      put NXP's firmware back, from your LinkServer install
  verge-update flash --file build/mculink_cjtag.elf
                                   flash a unit you put into ISP mode with the jumper";

/// Update the firmware on an MCU-Link: NXP's stock firmware, or this repository's.
///
/// The tool finds every attached MCU-Link, says what it runs, and picks the best way to update
/// it. A unit running ours, or stock V3.122 or later, restarts into its bootloader when asked,
/// so it needs no jumper. Anything else needs the ISP jumper, fitted before it is plugged in.
#[derive(Parser)]
#[command(name = "verge-update", version, after_help = EXAMPLES)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Say more about what is happening. Twice for USB-level detail.
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,

    /// The GitHub repository releases of ours come from.
    #[arg(
        long,
        global = true,
        value_name = "OWNER/NAME",
        env = "VERGE_UPDATE_REPO",
        hide_env_values = true,
        default_value = DEFAULT_REPO
    )]
    repo: Repo,

    /// A GitHub token. Public releases need none; a private repository does.
    #[arg(long, global = true, env = "GITHUB_TOKEN", hide_env_values = true)]
    github_token: Option<String>,

    /// Use a simulated bench instead of USB. For developing the tool.
    #[arg(long, global = true, hide = true, value_name = "SCENARIO")]
    simulate: Option<String>,

    /// Real seconds per simulated second.
    #[arg(long, global = true, hide = true, default_value_t = 0.5)]
    simulate_pace: f64,
}

#[derive(Subcommand)]
enum Command {
    /// Say what every attached MCU-Link runs, and how each can be updated. The default.
    List,
    /// Install a firmware on a unit, the best way its state allows.
    ///
    /// The image is the latest release of ours unless you pick another. A unit already in ISP
    /// mode is flashed as it is.
    Update(UpdateArgs),
    /// Flash the unit that is in ISP mode, as after fitting the jumper.
    ///
    /// With the jumper still fitted, the unit comes back in ISP mode. Then unplug it, take the
    /// jumper off, and plug it back in.
    Flash(FlashArgs),
    /// Restart a unit into its ROM bootloader without the jumper, and leave it there.
    Bootloader {
        /// The unit to restart, when more than one is attached.
        #[arg(long)]
        serial: Option<String>,
    },
    /// Show a release of ours and how each attached unit compares to it.
    Release {
        /// A release tag. The latest release when left out.
        tag: Option<String>,
    },
    /// Read an image file and say what it puts into flash. Touches no USB device.
    Inspect {
        /// An ELF or S-record file.
        file: PathBuf,
    },
}

#[derive(Args)]
#[group(multiple = false)]
struct Source {
    /// Ours from a GitHub release: the latest, or the one tagged TAG. The default.
    #[arg(long, value_name = "TAG", num_args = 0..=1, default_missing_value = "latest")]
    release: Option<String>,
    /// An ELF or S-record file, such as a build of ours.
    #[arg(long, value_name = "FILE")]
    file: Option<PathBuf>,
    /// NXP's stock image from your own LinkServer install: the newest found, or the newest in
    /// DIR. The tool never downloads it.
    #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = "")]
    stock: Option<PathBuf>,
}

#[derive(Args)]
struct UpdateArgs {
    #[command(flatten)]
    source: Source,
    /// The unit to update, when more than one is attached.
    #[arg(long)]
    serial: Option<String>,
    /// Flash without asking first.
    #[arg(short, long)]
    yes: bool,
    /// Install a release even when the unit runs the same or a newer build.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct FlashArgs {
    #[command(flatten)]
    source: Source,
    /// Flash without asking first.
    #[arg(short, long)]
    yes: bool,
}

fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "warn,updater_core=info",
        1 => "info",
        _ => "debug",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let bars = IndicatifLayer::new();
    let log = tracing_subscriber::fmt::layer()
        .with_ansi(std::io::stderr().is_terminal())
        .without_time()
        .with_target(false)
        .with_writer(bars.get_stderr_writer())
        .with_filter(filter);
    // The level filter is the log's alone: a span it disabled would get no bar.
    let ours =
        filter_fn(|meta| meta.is_span() && meta.target().starts_with(env!("CARGO_CRATE_NAME")));
    tracing_subscriber::registry()
        .with(log)
        .with(bars.with_filter(ours))
        .init();
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let cancel = Cancel::new();
    let handler = {
        let cancel = cancel.clone();
        ctrlc::set_handler(move || {
            if cancel.is_cancelled() {
                std::process::exit(130);
            }
            cancel.cancel();
            tracing::warn!("stopping at the next safe point; press Ctrl-C again to quit now");
        })
    };
    if let Err(error) = handler {
        tracing::debug!("no Ctrl-C handler: {error}");
    }

    let result = match &cli.simulate {
        Some(name) => match SimBus::scenario(name) {
            Some(bus) => {
                let http = SimHttp::new(cli.repo.clone());
                http.publish_ours("v0.3.0");
                run(&cli, &bus.paced(cli.simulate_pace), http, &cancel)
            }
            None => Err(anyhow::anyhow!(
                "no simulated bench called {name:?}; there are {}",
                SCENARIOS
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
        None => run(&cli, &UsbBus::default(), Ureq::default(), &cancel),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error:#}");
            if cancel.is_cancelled() {
                ExitCode::from(130)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn run<B: Bus, H: Http>(cli: &Cli, bus: &B, http: H, cancel: &Cancel) -> Result<()> {
    let github = GitHub::new(http, cli.repo.clone(), cli.github_token.clone());
    match &cli.command {
        None | Some(Command::List) => list(bus, cli.verbose > 0),
        Some(Command::Update(args)) => update_unit(bus, &github, args, cancel),
        Some(Command::Flash(args)) => flash(bus, &github, args, cancel),
        Some(Command::Bootloader { serial }) => bootloader(bus, serial.as_deref(), cancel),
        Some(Command::Release { tag }) => release(bus, &github, tag.as_deref()),
        Some(Command::Inspect { file }) => inspect(file),
    }
}

fn describe_unit(unit: &Unit) -> String {
    let location = unit.device.location.as_deref().unwrap_or("?");
    let serial = unit.serial().unwrap_or("-");
    format!("{location:<8} {serial:<14} {}", unit.state)
}

fn list<B: Bus>(bus: &B, verbose: bool) -> Result<()> {
    let units = detect(bus).context("looking for MCU-Links")?;
    if units.is_empty() {
        indicatif_println!("No MCU-Link is attached.");
        if cfg!(target_os = "linux") {
            indicatif_println!(
                "If one is plugged in, check that your user may open it: see the udev rules in the README."
            );
        }
        return Ok(());
    }
    for unit in &units {
        indicatif_println!("{}", describe_unit(unit));
        if verbose && let State::Probe { identity, firmware } = &unit.state {
            let field = |name: &str, value: Option<&str>| {
                indicatif_println!("{:<10}{name:<13} {}", "", value.unwrap_or("-"));
            };
            field("vendor", identity.vendor.as_deref());
            field("product", identity.product.as_deref());
            field("protocol", identity.protocol.as_deref());
            field("firmware", identity.firmware_version.as_deref());
            let packets = identity
                .packet_count
                .zip(identity.packet_size)
                .map(|(count, size)| format!("{count} of {size} bytes"));
            field("packets", packets.as_deref());
            if let Firmware::Ours(ours) = firmware {
                field("capabilities", Some(&ours.flag_names().join(", ")));
            }
        }
        let offer = offer(unit, &units);
        match offer.paths.as_slice() {
            [] => indicatif_println!("{:<10}cannot be updated as it is", ""),
            [best, rest @ ..] => {
                indicatif_println!("{:<10}update: {best}. {}", "", best.describe());
                for other in rest {
                    indicatif_println!("{:<10}or: {other}", "");
                }
            }
        }
        for note in &offer.notes {
            indicatif_println!("{:<10}{note}", "");
        }
    }
    Ok(())
}

impl Source {
    fn to_core(&self) -> source::Source {
        match (&self.file, &self.stock, &self.release) {
            (Some(file), _, _) => source::Source::File(file.clone()),
            (None, Some(dir), _) if dir.as_os_str().is_empty() => source::Source::Stock(None),
            (None, Some(dir), _) => source::Source::Stock(Some(dir.clone())),
            (None, None, tag) => source::Source::Release(tag.clone()),
        }
    }
}

/// The image to flash, and how to name it to the user.
fn load<H: Http>(
    source: &Source,
    github: &GitHub<H>,
    unit: Option<&Unit>,
    force: bool,
) -> Result<(Image, String)> {
    match source::load(&source.to_core(), github, unit, force, &mut Bars::default()) {
        Ok(loaded) => Ok((loaded.image, loaded.name)),
        Err(error @ LoadError::AlreadyRuns { .. }) => {
            bail!("{error}. Pass --force to flash it again")
        }
        Err(error @ LoadError::Newer { .. }) => {
            bail!("{error}. Pass --force to install the release anyway")
        }
        Err(error @ LoadError::NoStock { .. }) => bail!("{error} with --stock DIR"),
        Err(LoadError::Release(error)) => Err(anyhow::Error::new(error))
            .with_context(|| format!("looking up releases of ours in {}", github.repo())),
        Err(error) => Err(error.into()),
    }
}

fn confirm(question: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!("{question} Pass --yes to go ahead without being asked");
    }
    let go = dialoguer::Confirm::new()
        .with_prompt(question)
        .default(false)
        .interact()?;
    if !go {
        bail!("nothing was flashed");
    }
    Ok(())
}

fn report(outcome: &Outcome) {
    match outcome {
        Outcome::Running(unit) => indicatif_println!(
            "Done: {} at {} now runs {}.",
            unit.serial().unwrap_or("the unit"),
            unit.device.location.as_deref().unwrap_or("?"),
            unit.state
        ),
        Outcome::JumperStillFitted => indicatif_println!(
            "Flashed. The unit came back in ISP mode, so the jumper is still fitted: unplug it, take the jumper off, and plug it back in."
        ),
    }
}

fn log_image(image: &Image) {
    let pages: usize = image
        .page_runs()
        .iter()
        .map(|r| r.data.len() / PAGE as usize)
        .sum();
    tracing::info!(
        "{} bytes in {pages} pages, sha256 {}",
        image.data_len(),
        image.sha256_hex()
    );
}

fn update_unit<B: Bus, H: Http>(
    bus: &B,
    github: &GitHub<H>,
    args: &UpdateArgs,
    cancel: &Cancel,
) -> Result<()> {
    let units = detect(bus).context("looking for MCU-Links")?;
    let unit = update::select(&units, args.serial.as_deref())?;
    let offer = offer(unit, &units);
    let Some(path) = offer.best() else {
        bail!(
            "{} cannot be updated as it is. {}",
            describe_unit(unit),
            offer.notes.join(" ")
        );
    };
    if path == Path::Jumper {
        bail!(
            "this unit has no jumper-free update. {} To use the jumper: unplug the unit, fit the ISP jumper, plug it back in, and run `verge-update flash`",
            offer.notes.join(" ")
        );
    }
    let (image, name) = load(&args.source, github, Some(unit), args.force)?;
    log_image(&image);
    let target = unit.serial().unwrap_or("the unit in ISP mode");
    confirm(&format!("Flash {name} onto {target}, {path}?"), args.yes)?;
    let mut bars = Bars::default();
    let outcome = update::install(bus, unit, &image, &Timing::default(), &mut bars, cancel);
    drop(bars);
    report(&outcome.map_err(explain)?);
    Ok(())
}

fn flash<B: Bus, H: Http>(
    bus: &B,
    github: &GitHub<H>,
    args: &FlashArgs,
    cancel: &Cancel,
) -> Result<()> {
    let rom = update::only_bootloader(&bus.devices()?)?;
    let (image, name) = load(&args.source, github, None, true)?;
    log_image(&image);
    let location = rom.location.as_deref().unwrap_or("?");
    confirm(
        &format!("Flash {name} onto the unit in ISP mode at {location}?"),
        args.yes,
    )?;
    let mut bars = Bars::default();
    let outcome = update::flash_now(bus, &image, &Timing::default(), &mut bars, cancel);
    drop(bars);
    report(&outcome.map_err(explain)?);
    Ok(())
}

fn bootloader<B: Bus>(bus: &B, serial: Option<&str>, cancel: &Cancel) -> Result<()> {
    let units = detect(bus).context("looking for MCU-Links")?;
    let unit = update::select(&units, serial)?;
    let mut bars = Bars::default();
    let rom = update::enter_bootloader(bus, unit, &Timing::default(), &mut bars, cancel)
        .map_err(explain)?;
    drop(bars);
    indicatif_println!(
        "The unit is in ISP mode at {}. Flash it with `verge-update flash`.",
        rom.location.as_deref().unwrap_or("?")
    );
    Ok(())
}

fn release<B: Bus, H: Http>(bus: &B, github: &GitHub<H>, tag: Option<&str>) -> Result<()> {
    let release = match tag {
        None => github.latest(),
        Some(tag) => github.tagged(tag),
    }
    .with_context(|| format!("looking up releases of ours in {}", github.repo()))?;
    indicatif_println!(
        "{}  {}",
        release.tag,
        release.published.as_deref().unwrap_or("")
    );
    indicatif_println!("  {}", release.page);
    indicatif_println!(
        "  {}, {} bytes, checked against {}",
        release.image.name,
        release.image.size,
        release.checksum.name
    );
    for unit in detect(bus).context("looking for MCU-Links")? {
        let standing = match (unit.build(), &unit.state) {
            (Some(build), _) => match build.compare(&release.tag) {
                Relation::UpToDate => "runs this release".to_owned(),
                Relation::OtherVariant => format!("runs this release, built as {build}"),
                Relation::Behind => format!("runs {build}, older: an update is available"),
                Relation::Ahead => format!("runs {build}, newer than this release"),
                Relation::Unknown => format!("runs {build}, which carries no version to compare"),
            },
            (None, State::Probe { .. }) => {
                "runs stock; installing this release replaces it".to_owned()
            }
            (
                None,
                State::Unreadable {
                    guess: Guess::Ours, ..
                },
            ) => "probably runs ours; the tool could not ask which build".to_owned(),
            (None, state) => state.to_string(),
        };
        let location = unit.device.location.as_deref().unwrap_or("?");
        let serial = unit.serial().unwrap_or("-");
        indicatif_println!("{location:<8} {serial:<14} {standing}");
    }
    Ok(())
}

fn inspect(file: &FsPath) -> Result<()> {
    let image = Image::from_path(file)?;
    indicatif_println!("{}: {}", file.display(), image.format);
    if let Some(header) = &image.header {
        indicatif_println!("  header {header:?}");
    }
    for chunk in &image.chunks {
        indicatif_println!(
            "  0x{:08x}..0x{:08x}  {} bytes",
            chunk.address,
            chunk.end(),
            chunk.data.len()
        );
    }
    let runs = image.page_runs();
    let pages: usize = runs.iter().map(|r| r.data.len() / PAGE as usize).sum();
    indicatif_println!(
        "  {} bytes into flash: {pages} pages of {PAGE} bytes to erase and write, in {} runs",
        image.data_len(),
        runs.len()
    );
    indicatif_println!("  sha256 {}", image.sha256_hex());
    match image.check_for_mculink() {
        Ok(()) => indicatif_println!("  fits this part's flash and has a vector table"),
        Err(error) => indicatif_println!("  will not be flashed: {error}"),
    }
    Ok(())
}

/// Adds what the user can do next to the errors that can leave a unit in ISP mode or away.
fn explain(error: UpdateError) -> anyhow::Error {
    let next = match &error {
        UpdateError::Isp(_) | UpdateError::Cancelled | UpdateError::RomSilent { .. } => Some(
            "The unit may be in ISP mode with a partial image. `verge-update flash` flashes it again.",
        ),
        UpdateError::NoReturn(_) => Some("`verge-update list` shows what is attached now."),
        _ => None,
    };
    match next {
        Some(next) => anyhow::anyhow!("{error}. {next}"),
        None => anyhow::Error::new(error),
    }
}
