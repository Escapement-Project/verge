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
use verge_probe::batch::{
    ArmError, Armed, Batch, Board, Entry, Filter, Observer, OursFilter, Record, StockFilter,
    open_record,
};
use verge_probe::board;
use verge_probe::cache::Cache;
use verge_probe::detect::{Guess, Hardware, State, detect_named};
use verge_probe::ids::Role;
use verge_probe::image::{Image, PAGE};
use verge_probe::paths::ISP_IDENTITY_WARNING;
use verge_probe::progress::{Progress, Step};
use verge_probe::release::{DEFAULT_REPO, GitHub, Http, Repo, Ureq};
#[cfg(feature = "simulate")]
use verge_probe::sim::http::SimHttp;
#[cfg(feature = "simulate")]
use verge_probe::sim::{SCENARIOS, SimBus};
use verge_probe::source::{self, ImageVersion, LoadError, Loaded};
use verge_probe::update::{self, Outcome, Timing, UpdateError};
use verge_probe::usb::UsbBus;
use verge_probe::version::Relation;
use verge_probe::version::StockVersion;
use verge_probe::{Bus, Cancel, Firmware, Path, Unit, dap, detect, offer};

use crate::progress::Bars;

const EXAMPLES: &str = "\
Examples:
  verge                    what is attached, and how each unit can be updated
  verge update             install the latest release of ours on the only unit
  verge update --serial CJ3E7A91C2 --release v0.3.0
  verge update --stock     put NXP's firmware back, from your LinkServer install
  verge isp flash --file build/mculink_cjtag.elf
                           flash a unit you put into ISP mode with the jumper";

/// Update the firmware on an MCU-Link: NXP's stock firmware, or ours from mculink-verge.
///
/// The tool finds every attached MCU-Link, says what it runs, and picks the best way to update
/// it. A unit running ours, or stock V3.122 or later, restarts into its bootloader when asked,
/// so it needs no jumper. Anything else needs the ISP jumper, fitted before it is plugged in.
#[derive(Parser)]
#[command(name = "verge", version, after_help = EXAMPLES)]
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
        env = "VERGE_REPO",
        hide_env_values = true,
        default_value = DEFAULT_REPO
    )]
    repo: Repo,

    /// A GitHub token. Public releases need none; a private repository does.
    #[arg(long, global = true, env = "GITHUB_TOKEN", hide_env_values = true)]
    github_token: Option<String>,

    /// Use a simulated bench instead of USB. For developing the tool.
    #[cfg(feature = "simulate")]
    #[arg(long, global = true, hide = true, value_name = "SCENARIO")]
    simulate: Option<String>,

    /// Real seconds per simulated second.
    #[cfg(feature = "simulate")]
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
    /// Update every unit of a chosen kind as it is plugged in, one at a time, until stopped.
    ///
    /// Choose the image and the units it takes once, answer one question, then plug units in one
    /// after another or several on a hub. Each finished unit blinks so you can tell which to
    /// unplug. Ctrl-C stops taking new units; the one under way finishes. Every unit gets a line
    /// in the record file.
    Batch(BatchArgs),
    /// Work with a unit's ROM bootloader, ISP mode: enter it, flash, reset out of it, ask it.
    #[command(subcommand)]
    Isp(IspCommand),
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
    /// Show or limit the two-pin (cJTAG) scan formats a unit running ours offers a host.
    ///
    /// A host frames the format it prefers of those both the unit and the target have. --oscan1-only leaves it
    /// OScan1, which is what to look at on an analyser, and --formats picks a set, so that a host
    /// can be held to one format to test or compare it. A limit lasts until the unit restarts.
    Cjtag(CjtagArgs),
    /// Drive and read the four DIO pins on an MCU-Link Pro's J19 header, on a unit running ours.
    ///
    /// With no options it reads them. --high and --low make pins outputs at that level, and
    /// --input makes them inputs again. DIO0, DIO1 and DIO2 change direction together, and DIO3
    /// on its own, so a change has to turn a whole group round. The pins keep their state between
    /// debug sessions and go back to inputs when the unit restarts. LED6 (SIO) is lit while any
    /// of them is driven.
    Dio(DioArgs),
}

#[derive(Args)]
struct DioArgs {
    /// The unit, when more than one is attached.
    #[arg(long)]
    serial: Option<String>,
    /// Drive these pins high: 0 to 3, as `--high 0,1`, or `dio0` to `dio3`, or `all`.
    #[arg(long, value_name = "PINS", value_delimiter = ',', value_parser = parse_dio_pins)]
    high: Vec<u8>,
    /// Drive these pins low.
    #[arg(long, value_name = "PINS", value_delimiter = ',', value_parser = parse_dio_pins)]
    low: Vec<u8>,
    /// Make these pins inputs.
    #[arg(long, value_name = "PINS", value_delimiter = ',', value_parser = parse_dio_pins)]
    input: Vec<u8>,
}

/// A DIO pin as a mask: `2`, `dio2` in any case, or `all`.
fn parse_dio_pins(text: &str) -> Result<u8, String> {
    if text.eq_ignore_ascii_case("all") {
        return Ok(0x0F);
    }
    let digits = text
        .get(..3)
        .filter(|prefix| prefix.eq_ignore_ascii_case("dio"))
        .map_or(text, |_| &text[3..]);
    match digits.parse::<u8>() {
        Ok(n) if n < 4 && digits.bytes().all(|b| b.is_ascii_digit()) => Ok(1 << n),
        _ => Err(format!(
            "{text} is not a DIO pin: name one as 0 to 3, dio0 to dio3, or all"
        )),
    }
}

/// DIO pins by name, as `DIO0, DIO1`.
fn dio_names(mask: u8) -> String {
    (0..8)
        .filter(|n| mask >> n & 1 != 0)
        .map(|n| format!("DIO{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Args)]
struct CjtagArgs {
    /// The unit, when more than one is attached.
    #[arg(long)]
    serial: Option<String>,
    /// Offer OScan1 alone.
    #[arg(long, conflicts_with_all = ["all", "formats"])]
    oscan1_only: bool,
    /// Offer every format the image frames again.
    #[arg(long, conflicts_with = "formats")]
    all: bool,
    /// Offer these formats, by name or SCNFMT number: `--formats oscan1,oscan5` or `9,13`. OScan1
    /// stays offered whatever the list says, and a format the image does not frame is left out.
    #[arg(long, value_delimiter = ',', value_parser = parse_scan_format)]
    formats: Option<Vec<u32>>,
}

/// A scan format named as `scan_format_name` names it, in any case, or its SCNFMT number.
fn parse_scan_format(text: &str) -> Result<u32, String> {
    if let Ok(number) = text.parse::<u32>() {
        return if number < 32 {
            Ok(number)
        } else {
            Err(format!("SCNFMT {number} is past 31"))
        };
    }
    (0..32)
        .find(|&n| dap::scan_format_name(n).eq_ignore_ascii_case(text))
        .ok_or_else(|| {
            format!("{text} is not a scan format: name one as OScan5 or give its SCNFMT, 13")
        })
}

#[derive(Subcommand)]
enum IspCommand {
    /// Restart a unit into its ROM bootloader without the jumper, and leave it there.
    ///
    /// In ISP mode a unit cannot say which board it is, so `verge isp flash` cannot check that an
    /// image of ours is built for it. A unit known to be another board than the full MCU-Link is
    /// restarted only with --stock.
    Enter {
        /// The unit to restart, when more than one is attached.
        #[arg(long)]
        serial: Option<String>,
        /// You mean to flash NXP's stock image next, which goes on any board.
        #[arg(long)]
        stock: bool,
    },
    /// Flash the unit that is in ISP mode, as after fitting the jumper.
    ///
    /// With the jumper still fitted, the unit comes back in ISP mode. Then take the jumper off and
    /// run `verge isp reset`.
    Flash(FlashArgs),
    /// Reset a unit out of ISP mode without flashing it, so it starts the image it holds.
    Reset {
        #[command(flatten)]
        port: PortArg,
    },
    /// Ask the bootloader of a unit in ISP mode what it is. Changes nothing.
    Info {
        #[command(flatten)]
        port: PortArg,
    },
}

#[derive(Args)]
struct PortArg {
    /// The unit in ISP mode at this USB port, as `verge list` names it, when several are. Only
    /// where the system ties a bootloader to its port, as Linux does.
    #[arg(long)]
    port: Option<String>,
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
    #[arg(long, value_name = "DIR", num_args = 0..=1)]
    stock: Option<Option<PathBuf>>,
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

/// Kinds of unit a batch takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Take {
    /// Units running NXP's stock firmware.
    Stock,
    /// Units running ours.
    Ours,
    /// Units that arrive in ISP mode, as after fitting the jumper.
    Isp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum BoardArg {
    /// The full MCU-Link.
    McuLink,
    /// The MCU-Link Pro. An image of ours has to declare it.
    McuLinkPro,
}

#[derive(Args)]
struct BatchArgs {
    #[command(flatten)]
    source: Source,
    /// Which units to take.
    #[arg(long, value_enum, num_args = 1.., default_values_t = [Take::Stock, Take::Ours])]
    take: Vec<Take>,
    /// Take only stock older than this version, such as 3.172. Implies --take stock.
    #[arg(long, value_name = "VERSION", value_parser = parse_stock_version)]
    stock_below: Option<StockVersion>,
    /// Take ours of any build. Without it, only builds older than the image, or carrying no
    /// version, are taken. Implies --take ours.
    #[arg(long)]
    ours_any: bool,
    /// Take units attached already, not only those plugged in from now on.
    #[arg(long)]
    include_attached: bool,
    /// The board to take.
    #[arg(long, value_enum, default_value_t = BoardArg::McuLink)]
    board: BoardArg,
    /// Stop once this many units are updated.
    #[arg(long, value_name = "N")]
    count: Option<usize>,
    /// The record file: one JSON line per unit. Defaults to verge-batch-<time>.jsonl here.
    #[arg(long, value_name = "FILE")]
    record: Option<PathBuf>,
    /// Arm without asking first.
    #[arg(short, long)]
    yes: bool,
}

fn parse_stock_version(text: &str) -> std::result::Result<StockVersion, String> {
    let text = text.strip_prefix(['V', 'v']).unwrap_or(text);
    StockVersion::parse(text).ok_or_else(|| format!("expected a version like 3.172, got {text:?}"))
}

#[derive(Args)]
struct FlashArgs {
    #[command(flatten)]
    source: Source,
    #[command(flatten)]
    port: PortArg,
    /// Flash without asking first.
    #[arg(short, long)]
    yes: bool,
}

fn init_tracing(verbose: u8) {
    let default = match verbose {
        // nusb logs a claim it could not make as an error of its own, which verge reports in its
        // own words where it matters: a unit another program holds is listed as such, and one not
        // asked for is left alone.
        0 => "warn,nusb=off,verge_probe=info",
        // The bootloader and USB crates log every packet at info, so they wait for -vv.
        1 => "warn,verge_probe=debug,verge=debug",
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
    let stop = Cancel::new();
    let batch = matches!(cli.command, Some(Command::Batch(_)));
    let handler = {
        let (cancel, stop) = (cancel.clone(), stop.clone());
        ctrlc::set_handler(move || {
            if batch && !stop.is_cancelled() {
                stop.cancel();
                tracing::warn!(
                    "taking no new units; the one under way finishes. Ctrl-C again stops it at the next safe point"
                );
            } else if !cancel.is_cancelled() {
                cancel.cancel();
                tracing::warn!("stopping at the next safe point; press Ctrl-C again to quit now");
            } else {
                std::process::exit(130);
            }
        })
    };
    if let Err(error) = handler {
        tracing::debug!("no Ctrl-C handler: {error}");
    }

    let result = simulated(&cli, &cancel, &stop).unwrap_or_else(|| {
        let github = GitHub::new(Ureq::default(), cli.repo.clone(), cli.github_token.clone())
            .with_cache(Cache::user());
        run(&cli, &UsbBus::default(), &github, &cancel, &stop)
    });
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

/// Runs the command on the simulated bench `--simulate` names, if it names one.
#[cfg(feature = "simulate")]
fn simulated(cli: &Cli, cancel: &Cancel, stop: &Cancel) -> Option<Result<()>> {
    let name = cli.simulate.as_ref()?;
    Some(match SimBus::scenario(name) {
        // No cache: a simulated GitHub signs with a key made for the run, so a copy kept from
        // another run would never check out.
        Some(bus) => {
            let http = SimHttp::new(cli.repo.clone());
            http.publish_ours("v0.3.0");
            let key = http.public_key();
            let github = GitHub::new(http, cli.repo.clone(), None).with_key(Some(key));
            run(cli, &bus.paced(cli.simulate_pace), &github, cancel, stop)
        }
        None => Err(anyhow::anyhow!(
            "no simulated bench called {name:?}; there are {}",
            SCENARIOS
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    })
}

/// Without the `simulate` feature there is no simulated bench.
#[cfg(not(feature = "simulate"))]
fn simulated(_: &Cli, _: &Cancel, _: &Cancel) -> Option<Result<()>> {
    None
}

fn run<B: Bus + Sync, H: Http>(
    cli: &Cli,
    bus: &B,
    github: &GitHub<H>,
    cancel: &Cancel,
    stop: &Cancel,
) -> Result<()> {
    match &cli.command {
        Some(Command::Batch(args)) => batch(bus, github, args, cancel, stop),
        None | Some(Command::List) => list(bus, cli.verbose > 0),
        Some(Command::Cjtag(args)) => cjtag(bus, args),
        Some(Command::Dio(args)) => dio(bus, args),
        Some(Command::Update(args)) => update_unit(bus, github, args, cancel),
        Some(Command::Isp(IspCommand::Enter { serial, stock })) => {
            bootloader(bus, serial.as_deref(), *stock, cancel)
        }
        Some(Command::Isp(IspCommand::Flash(args))) => flash(bus, github, args, cancel),
        Some(Command::Isp(IspCommand::Reset { port })) => reset(bus, port.port.as_deref(), cancel),
        Some(Command::Isp(IspCommand::Info { port })) => {
            rom_info(bus, port.port.as_deref(), cancel)
        }
        Some(Command::Release { tag }) => release(bus, github, tag.as_deref()),
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
        let board = match (&unit.state, unit.hardware()) {
            (State::Bootloader, _) => "cannot be told in ISP mode".to_owned(),
            (_, Hardware::Unknown) => "not reported".to_owned(),
            (_, board) => board.to_string(),
        };
        indicatif_println!("{:<10}board: {board}", "");
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
                if let Some(names) = ours.scan_format_names() {
                    field("two-pin", Some(&names.join(", ")));
                }
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
            (None, Some(dir), _) => source::Source::Stock(dir.clone()),
            (None, None, tag) => source::Source::Release(tag.clone()),
        }
    }
}

/// The image to flash, and how to name it to the user. A release is downloaded only once `agree`
/// says yes to what downloading it fetches, as [`source::load_asking`] says.
fn load<H: Http>(
    source: &Source,
    github: &GitHub<H>,
    unit: Option<&Unit>,
    force: bool,
    agree: impl FnOnce(&str) -> Result<()>,
) -> Result<Loaded> {
    let progress = &mut Bars::default();
    source::load_asking(&source.to_core(), github, unit, force, agree, progress)?
        .map_err(|error| load_error(error, github))
}

/// Why an image did not load, with the flags that change it.
fn load_error<H: Http>(error: LoadError, github: &GitHub<H>) -> anyhow::Error {
    match error {
        error @ LoadError::AlreadyRuns { .. } => {
            anyhow::anyhow!("{error}. Pass --force to flash it again")
        }
        error @ LoadError::Newer { .. } => {
            anyhow::anyhow!("{error}. Pass --force to install the release anyway")
        }
        error @ LoadError::NoStock { .. } => anyhow::anyhow!("{error} with --stock DIR"),
        error @ (LoadError::OtherBoard { .. } | LoadError::NotForBoard { .. }) => {
            anyhow::anyhow!("{error} with --stock")
        }
        LoadError::Release(error) => anyhow::Error::new(error)
            .context(format!("looking up releases of ours in {}", github.repo())),
        error => error.into(),
    }
}

/// The question before a release is downloaded for one unit.
fn download_question(yes: bool) -> impl FnOnce(&str) -> Result<()> {
    move |what| confirm(&format!("Download {what}?"), yes)
}

/// The command that flashes the same image again, for advice. A release names its repository
/// where it is not the default, so that the command finds the same release, and its kept copy.
fn again<H: Http>(source: &Source, version: &ImageVersion, github: &GitHub<H>) -> String {
    let core = source.to_core();
    let command = source::flash_command(&core, version);
    match core {
        source::Source::Release(_) if *github.repo() != Repo::default() => {
            format!("{command} --repo {}", github.repo())
        }
        _ => command,
    }
}

fn confirm(question: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!("{question} Pass --yes to go ahead without being asked");
    }
    ask(question)
}

/// Asks at a terminal, for a question `--yes` does not answer.
fn confirm_at_terminal(question: &str) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!("{question} Only an answer at a terminal says yes to this; --yes does not");
    }
    ask(question)
}

fn ask(question: &str) -> Result<()> {
    let go = dialoguer::Confirm::new()
        .with_prompt(question)
        .default(false)
        .interact()?;
    if !go {
        bail!("nothing was flashed");
    }
    Ok(())
}

/// The boards an image of ours is built for, as a sentence. `None` for stock's own image, which
/// goes on any board.
fn built_for(image: &Image, stock: bool) -> Result<Option<String>> {
    if stock {
        return Ok(None);
    }
    Ok(Some(format!(
        "The image is built for {}.",
        board::built_for(&image.boards()?)
    )))
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
            "Flashed. The unit came back in ISP mode, so its ISP pin is still held low: by the ISP jumper, or by a probe on its debug connector. Take the jumper off or unplug the probe, then run `verge isp reset`."
        ),
        Outcome::NotRestarted { maybe_jumper } => indicatif_println!(
            "Flashed and verified, but the unit is still in ISP mode: {}. Then it starts the new image.",
            update::not_restarted(*maybe_jumper)
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
    let units = detect_named(bus, args.serial.as_deref()).context("looking for MCU-Links")?;
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
        // Loaded first, so that a known board is refused here as on the jumper-free path, and the
        // advice names an image checked for it. The question says why, so that a no still tells
        // the user about the jumper.
        let loaded = load(&args.source, github, Some(unit), args.force, |what| {
            confirm(
                &format!(
                    "This unit cannot be updated without the jumper now. Download {what} first, to check that it is built for this unit?"
                ),
                args.yes,
            )
        })?;
        let again = again(&args.source, &loaded.version, github);
        bail!(
            "this unit cannot be updated without the jumper now. {} To use the jumper: unplug the unit, fit the ISP jumper, plug it back in, and run `{again}`. {}",
            offer.notes.join(" "),
            in_isp_mode(unit)
        );
    }
    let stock = args.source.stock.is_some();
    let unknown = source::board_unknown(unit).filter(|_| !stock);
    if let Some(unknown) = unknown {
        if args.yes {
            bail!(
                "{unknown} The tool asks before putting ours on such a unit, and --yes does not answer that: run it without --yes, at a terminal"
            );
        }
        // Before anything is downloaded for a question nobody could answer.
        if !std::io::stdin().is_terminal() {
            bail!(
                "{unknown} Only an answer at a terminal says yes to putting ours on such a unit; --yes does not"
            );
        }
    }
    let loaded = load(
        &args.source,
        github,
        Some(unit),
        args.force,
        download_question(args.yes),
    )?;
    let (image, name) = (&loaded.image, &loaded.name);
    log_image(image);
    let target = unit.serial().unwrap_or("the unit in ISP mode");
    let mut question = format!("Flash {name} onto {target}, {path}?");
    if (path == Path::FlashNow || unknown.is_some())
        && let Some(built_for) = built_for(image, stock)?
    {
        question = format!("{built_for} {question}");
    }
    if path == Path::FlashNow {
        question = format!("{ISP_IDENTITY_WARNING} {question}");
        if args.yes {
            tracing::warn!("{ISP_IDENTITY_WARNING} Flashing it, as --yes says.");
        }
    }
    match unknown {
        Some(unknown) => confirm_at_terminal(&format!("{unknown} {question}"))?,
        None => confirm(&question, args.yes)?,
    }
    let mut bars = Bars::default();
    let outcome = update::install(
        bus,
        unit,
        loaded.payload(),
        &Timing::default(),
        &mut bars,
        cancel,
        cancel,
    );
    drop(bars);
    let again = again(&args.source, &loaded.version, github);
    report(&outcome.map_err(|error| explain(error, &again, Some(unit)))?);
    Ok(())
}

/// What to know about flashing `unit` once it is in ISP mode, where nothing tells its board.
fn in_isp_mode(unit: &Unit) -> String {
    match unit.hardware() {
        Hardware::Board(board) => format!(
            "In ISP mode the tool cannot tell which board a unit is: that image is the one checked for this unit, {}.",
            board.a()
        ),
        _ => "In ISP mode the tool cannot tell which board a unit is, and this unit's firmware did not say either: check that the image is built for it.".to_owned(),
    }
}

fn flash<B: Bus, H: Http>(
    bus: &B,
    github: &GitHub<H>,
    args: &FlashArgs,
    cancel: &Cancel,
) -> Result<()> {
    let port = args.port.port.as_deref();
    let rom = update::flash_target(bus, &bus.devices()?, port)?;
    let loaded = load(
        &args.source,
        github,
        None,
        true,
        download_question(args.yes),
    )?;
    let (image, name) = (&loaded.image, &loaded.name);
    log_image(image);
    let location = rom.location.as_deref().unwrap_or("?");
    let built_for = built_for(image, args.source.stock.is_some())?
        .map(|sentence| format!("{sentence} "))
        .unwrap_or_default();
    if args.yes {
        tracing::warn!("{ISP_IDENTITY_WARNING} {built_for}Flashing it, as --yes says.");
    }
    confirm(
        &format!(
            "{ISP_IDENTITY_WARNING} {built_for}Flash {name} onto the unit in ISP mode at {location}?"
        ),
        args.yes,
    )?;
    let mut bars = Bars::default();
    let outcome = update::flash_now(bus, image, port, &Timing::default(), &mut bars, cancel);
    drop(bars);
    let again = again(&args.source, &loaded.version, github);
    report(&outcome.map_err(|error| explain(error, &again, None))?);
    Ok(())
}

/// A batch's output: bars for the unit under way, a line and a record line for each unit.
struct BatchOutput {
    bars: Bars,
    record: std::io::BufWriter<std::fs::File>,
}

impl Progress for BatchOutput {
    fn plan(&mut self, steps: &[Step]) {
        self.bars.plan(steps);
    }
    fn start(&mut self, step: Step, total: u64) {
        self.bars.start(step, total);
    }
    fn advance(&mut self, done: u64) {
        self.bars.advance(done);
    }
    fn note(&mut self, text: &str) {
        self.bars.note(text);
    }
}

impl Observer for BatchOutput {
    fn starting(&mut self, entry: &Entry) {
        tracing::info!(
            "{}: updating {}",
            entry.port,
            entry.serial_before.as_deref().unwrap_or("the unit")
        );
    }

    fn record(&mut self, record: &Record) {
        self.bars.clear();
        use std::io::Write as _;
        let written = serde_json::to_writer(&mut self.record, record)
            .map_err(std::io::Error::from)
            .and_then(|()| writeln!(self.record))
            .and_then(|()| self.record.flush());
        if let Err(error) = written {
            tracing::error!("the record file: {error}");
        }
        let serial = match (&record.serial_before, &record.serial_after) {
            (Some(before), Some(after)) if before != after => format!("{before} -> {after}"),
            (Some(before), _) => before.clone(),
            (None, Some(after)) => after.clone(),
            (None, None) => "-".to_owned(),
        };
        let firmware = match &record.firmware_after {
            Some(after) => format!("{} -> {after}", record.firmware_before),
            None => record.firmware_before.clone(),
        };
        let reason = record
            .reason
            .as_deref()
            .map(|r| format!(": {r}"))
            .unwrap_or_default();
        let seconds = record
            .seconds
            .map(|s| format!(" in {s:.1} s"))
            .unwrap_or_default();
        indicatif_println!(
            "{:<8} {serial}  {firmware}  {}{seconds}{reason}",
            record.port,
            record.result.as_str().to_uppercase()
        );
    }

    fn unplug(&mut self, port: &str, blinking: bool) {
        if blinking {
            indicatif_println!("{port:<8} blinking: unplug it when you like");
        } else {
            indicatif_println!("{port:<8} done: unplug it when you like");
        }
    }

    fn paused(&mut self, reason: Option<&str>) {
        match reason {
            Some(reason) => tracing::warn!("paused. {reason}"),
            None => tracing::info!("resumed"),
        }
    }
}

fn batch<B: Bus + Sync, H: Http>(
    bus: &B,
    github: &GitHub<H>,
    args: &BatchArgs,
    cancel: &Cancel,
    stop: &Cancel,
) -> Result<()> {
    let stock = if let Some(limit) = args.stock_below {
        StockFilter::Below(limit)
    } else if args.take.contains(&Take::Stock) {
        StockFilter::Any
    } else {
        StockFilter::None
    };
    let ours = if args.ours_any {
        OursFilter::Any
    } else if args.take.contains(&Take::Ours) {
        OursFilter::Older
    } else {
        OursFilter::None
    };
    let filter = Filter {
        board: match args.board {
            BoardArg::McuLink => Board::McuLink,
            BoardArg::McuLinkPro => Board::McuLinkPro,
        },
        stock,
        ours,
        bootloaders: args.take.contains(&Take::Isp),
        attached: args.include_attached,
    };
    let core_source = args.source.to_core();
    filter
        .check_source(&core_source)
        .map_err(|error| arm_refusal(&error))?;
    // One question arms the batch. Where the release has to be downloaded first, it asks before
    // the download, and says so.
    let question = |lead: &str| {
        let question = format!(
            "{lead} onto {}? Each goes through its bootloader on its own, without being asked again.",
            filter.describe()
        );
        if filter.bootloaders {
            format!("{ISP_IDENTITY_WARNING} {question}")
        } else {
            question
        }
    };
    let mut asked = false;
    let Loaded {
        image,
        name,
        version,
        ..
    } = load(&args.source, github, None, true, |what| {
        asked = true;
        confirm(
            &question(&format!("Download {what}, and flash it")),
            args.yes,
        )
    })?;
    if stop.is_cancelled() {
        indicatif_println!("Stopped before the batch was armed. Nothing was flashed.");
        return Ok(());
    }
    let armed = Armed {
        image,
        name,
        version,
        source: core_source,
    };
    armed.check(&filter).map_err(|error| arm_refusal(&error))?;
    let Armed { image, name, .. } = &armed;
    log_image(image);
    let path = args.record.clone().unwrap_or_else(|| {
        let stamp = humantime::format_rfc3339_seconds(std::time::SystemTime::now())
            .to_string()
            .replace(':', "");
        PathBuf::from(format!("verge-batch-{stamp}.jsonl"))
    });
    if filter.bootloaders && args.yes {
        tracing::warn!("{ISP_IDENTITY_WARNING} Taking units in ISP mode, as --yes says.");
    }
    if !asked {
        confirm(&question(&format!("Flash {name}")), args.yes)?;
    }
    let file = open_record(&path)
        .with_context(|| format!("opening the record file {}", path.display()))?;
    let mut output = BatchOutput {
        bars: Bars::default(),
        record: std::io::BufWriter::new(file),
    };
    indicatif_println!(
        "Armed: {name}, for {}. Recording to {}. Plug units in; Ctrl-C stops taking new ones.",
        filter.describe(),
        path.display()
    );
    let mut batch = Batch::arm(
        bus,
        armed,
        filter,
        Timing::default(),
        stop.clone(),
        cancel.clone(),
        args.count,
    )?;
    batch.run_to_end(&mut output)?;
    let tally = batch.tally();
    indicatif_println!("Batch done: {tally}.");
    if tally.failed > 0 {
        bail!(
            "{} units failed; the record file {} says why",
            tally.failed,
            path.display()
        );
    }
    Ok(())
}

/// Why a batch did not arm, with the flags that change it.
fn arm_refusal(error: &ArmError) -> anyhow::Error {
    match error {
        ArmError::OursUndated => anyhow::anyhow!(
            "{error}. Pass --ours-any to take every unit running ours, or leave ours out, as with --take stock"
        ),
        ArmError::NotForBoard { .. } => anyhow::anyhow!(
            "{error}. Pick an image of ours built for it, or NXP's stock image with --stock"
        ),
        ArmError::Image(_) => anyhow::anyhow!("{error}"),
    }
}

fn cjtag<B: Bus>(bus: &B, args: &CjtagArgs) -> Result<()> {
    let units = detect_named(bus, args.serial.as_deref()).context("looking for MCU-Links")?;
    let unit = update::select(&units, args.serial.as_deref())?;
    let serial = unit.serial().unwrap_or("the unit");
    let State::Probe {
        firmware: Firmware::Ours(ours),
        ..
    } = &unit.state
    else {
        bail!(
            "{serial} is {}, not running ours, so it has no two-pin formats to show or limit",
            unit.state
        );
    };
    if ours.scan_formats.is_none() {
        bail!(
            "{serial} runs build {}, older than the scan formats: it frames OScan1 alone",
            ours.build
        );
    }
    let mut link = bus
        .open_dap(&unit.device)
        .with_context(|| format!("opening {serial}"))?;
    let mask = if args.oscan1_only {
        Some(1 << dap::SCNFMT_OSCAN1)
    } else if args.all {
        Some(u32::MAX)
    } else {
        args.formats
            .as_ref()
            .map(|formats| formats.iter().fold(0, |mask, n| mask | 1 << n))
    };
    if let Some(mask) = mask {
        dap::set_scan_formats(&mut link, mask).with_context(|| format!("limiting {serial}"))?;
    }
    let names = dap::identify(&mut link)?
        .and_then(|ours| ours.scan_format_names())
        .unwrap_or_default();
    let until = if args.oscan1_only || args.formats.is_some() {
        ", until it restarts"
    } else {
        ""
    };
    indicatif_println!("{serial} offers two-pin {}{until}.", names.join(", "));
    Ok(())
}

fn dio<B: Bus>(bus: &B, args: &DioArgs) -> Result<()> {
    let fold = |pins: &[u8]| pins.iter().fold(0, |mask, pin| mask | pin);
    let (high, low, input) = (fold(&args.high), fold(&args.low), fold(&args.input));
    for (a, b, names) in [
        (high, low, "--high and --low"),
        (high, input, "--high and --input"),
        (low, input, "--low and --input"),
    ] {
        if a & b != 0 {
            bail!("{} named in both {names}", dio_names(a & b));
        }
    }
    let units = detect_named(bus, args.serial.as_deref()).context("looking for MCU-Links")?;
    let unit = update::select(&units, args.serial.as_deref())?;
    let serial = unit.serial().unwrap_or("the unit");
    let State::Probe {
        firmware: Firmware::Ours(ours),
        ..
    } = &unit.state
    else {
        bail!(
            "{serial} is {}, not running ours, so the tool cannot reach its DIO pins",
            unit.state
        );
    };
    if let Some(board) = ours.board
        && board != dap::Ours::BOARD_MCU_LINK_PRO
    {
        bail!("{serial} is not an MCU-Link Pro, so it has no DIO pins");
    }
    let mut link = bus
        .open_dap(&unit.device)
        .with_context(|| format!("opening {serial}"))?;
    let Some(before) =
        dap::dio(&mut link, 0, 0, 0).with_context(|| format!("reading {serial}'s DIO pins"))?
    else {
        bail!(
            "{serial} runs build {}, which predates the DIO pins: update it first",
            ours.build
        );
    };
    if before.pins == 0 {
        if ours.board == Some(dap::Ours::BOARD_MCU_LINK_PRO) {
            bail!(
                "{serial} started with its USB-SIO jumper, J15, fitted, which turns the DIO pins off. Remove the jumper and replug the unit"
            );
        }
        bail!("{serial} is not an MCU-Link Pro, so it has no DIO pins");
    }
    let mask = high | low | input;
    let after = if mask == 0 {
        before
    } else {
        if let Some(group) = before.split(mask, high | low) {
            bail!(
                "{} change direction together, so make them all outputs or all inputs",
                dio_names(group)
            );
        }
        let after = dap::dio(&mut link, mask, high | low, high)
            .with_context(|| format!("setting {serial}'s DIO pins"))?
            .with_context(|| format!("{serial} no longer knows the DIO command"))?;
        if !after.accepted {
            bail!("{serial} refused the change to {}", dio_names(mask));
        }
        after
    };
    indicatif_println!("{serial}'s DIO pins, on J19 pins 2 to 5:");
    for n in 0..8 {
        let bit = 1 << n;
        if after.pins & bit == 0 {
            continue;
        }
        let level = |on: bool| if on { "high" } else { "low" };
        if after.outputs & bit != 0 {
            indicatif_println!(
                "  DIO{n}  output  driven {}",
                level(after.levels & bit != 0)
            );
        } else {
            indicatif_println!("  DIO{n}  input   reads {}", level(after.read & bit != 0));
        }
    }
    let groups: Vec<String> = after.groups.iter().map(|&g| dio_names(g)).collect();
    indicatif_println!("Direction groups: {}.", groups.join("; "));
    Ok(())
}

fn bootloader<B: Bus>(bus: &B, serial: Option<&str>, stock: bool, cancel: &Cancel) -> Result<()> {
    let units = detect_named(bus, serial).context("looking for MCU-Links")?;
    let unit = update::select(&units, serial)?;
    if let Err(refused) = update::check_entry(unit, stock) {
        bail!(
            "{refused}. To put NXP's stock image on it, run `verge isp enter --stock`, then `verge isp flash --stock`. To put an image of ours built for it on it, run `verge update --file IMAGE` instead, which checks that the image declares this board before anything is flashed"
        );
    }
    let mut bars = Bars::default();
    let rom = update::enter_bootloader(bus, unit, stock, &Timing::default(), &mut bars, cancel)?;
    drop(bars);
    let at = rom.location.as_deref().unwrap_or("?");
    // Beside another unit in ISP mode, the flash has to be told which.
    let several = bus
        .devices()?
        .iter()
        .filter(|d| d.role() == Some(Role::Bootloader))
        .count()
        > 1;
    let flash = if several {
        format!("verge isp flash --port {at}")
    } else {
        "verge isp flash".to_owned()
    };
    match unit.hardware() {
        Hardware::Board(board) if stock => indicatif_println!(
            "The unit, {}, is in ISP mode at {at}. Flash it with `{flash} --stock`.",
            board.a()
        ),
        Hardware::Board(board) => indicatif_println!(
            "The unit, {}, is in ISP mode at {at}, where the tool cannot tell its board. Flash it with `{flash}` and an image built for {}: the question names the boards the image is for.",
            board.a(),
            board.a()
        ),
        _ if stock => {
            indicatif_println!("The unit is in ISP mode at {at}. Flash it with `{flash} --stock`.")
        }
        _ => indicatif_println!(
            "The unit is in ISP mode at {at}. Its firmware did not say which board it is, so check that an image is built for it before you flash it with `{flash}`."
        ),
    }
    Ok(())
}

fn reset<B: Bus>(bus: &B, port: Option<&str>, cancel: &Cancel) -> Result<()> {
    let mut bars = Bars::default();
    let outcome = update::reset_now(bus, port, &Timing::default(), &mut bars, cancel);
    drop(bars);
    match outcome? {
        Outcome::Running(unit) => indicatif_println!(
            "Done: {} at {} runs {}.",
            unit.serial().unwrap_or("the unit"),
            unit.device.location.as_deref().unwrap_or("?"),
            unit.state
        ),
        Outcome::JumperStillFitted => indicatif_println!(
            "The unit came back in ISP mode. Either its ISP pin is still held low, by the ISP jumper or by a probe on its debug connector, or its flash holds no image the ROM starts. Take the jumper off or unplug the probe and reset it again, or flash an image with `verge isp flash`."
        ),
        Outcome::NotRestarted { maybe_jumper } => indicatif_println!(
            "The unit is still in ISP mode: {}.",
            update::not_restarted(maybe_jumper)
        ),
    }
    Ok(())
}

fn rom_info<B: Bus>(bus: &B, port: Option<&str>, cancel: &Cancel) -> Result<()> {
    let (rom, info) = update::rom_info_now(bus, port, &Timing::default(), cancel)?;
    let flash = match info.flash {
        Some((start, size)) => format!("flash from 0x{start:08x}, {} KiB", size / 1024),
        None => "flash not reported".to_owned(),
    };
    indicatif_println!(
        "{}: bootloader {}, {flash}.",
        rom.location.as_deref().unwrap_or("?"),
        info.version
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
        "  {}, {} bytes, checked against {} and {}",
        release.image.name,
        release.image.size,
        release.signature.name,
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
    for chunk in image.chunks() {
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
    match image.declaration() {
        Ok(Some(declaration)) => indicatif_println!(
            "  declares, at 0x{:08x} in format {}, that it runs on {} (boards 0x{:08x})",
            declaration.address,
            declaration.format,
            board::built_for(&image.boards()?),
            declaration.boards
        ),
        Ok(None) => indicatif_println!(
            "  declares no boards: as an image of ours, it goes on the full MCU-Link only"
        ),
        Err(_) => {}
    }
    match image.check_for_mculink() {
        Ok(()) => indicatif_println!("  fits this part's flash and has a vector table"),
        Err(error) => indicatif_println!("  will not be flashed: {error}"),
    }
    Ok(())
}

/// Adds what the user can do next to the errors that can leave a unit in ISP mode or away.
///
/// `again` is the command that flashes the same image, and `unit` the unit as it was before, where
/// it was running, to name its board.
fn explain(error: UpdateError, again: &str, unit: Option<&Unit>) -> anyhow::Error {
    let next = match &error {
        UpdateError::Isp(_) | UpdateError::Cancelled | UpdateError::RomSilent { .. } => {
            Some(match unit.map(Unit::hardware) {
                Some(Hardware::Board(board)) => format!(
                    "The unit, {}, may be in ISP mode with a partial image. There the tool cannot tell its board, so flash it again with the same image, which is built for it: `{again}`.",
                    board.a()
                ),
                _ => format!(
                    "The unit may be in ISP mode with a partial image. `{again}` flashes it again."
                ),
            })
        }
        UpdateError::NoReturn(_) | UpdateError::Ambiguous { .. } => {
            Some("`verge list` shows what is attached now.".to_owned())
        }
        _ => None,
    };
    match next {
        Some(next) => anyhow::anyhow!("{error}. {next}"),
        None => anyhow::Error::new(error),
    }
}
