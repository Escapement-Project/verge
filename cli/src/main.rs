use std::num::NonZeroUsize;
mod dio_record;
mod energy;
mod output;
mod progress;
mod question;
mod read;

use std::io::IsTerminal;
use std::path::{Path as FsPath, PathBuf};
use std::process::ExitCode;

use crate::output::outputln;
use anyhow::{Context, Result, bail};
use clap::{ArgAction, Args, Parser, Subcommand};
use escapement_verge_probe::batch::{
    ArmError, Armed, Batch, Board, Entry, Filter, Observer, OursFilter, Record, RecordWriter,
    StockFilter, Tally, open_record,
};
use escapement_verge_probe::board;
use escapement_verge_probe::cache::Cache;
use escapement_verge_probe::detect::{Guess, Hardware, PickError, State, detect_named};
use escapement_verge_probe::dio::{self, DioChange, DioError};
use escapement_verge_probe::ids::Role;
use escapement_verge_probe::image::{Image, PAGE};
use escapement_verge_probe::paths::{ISP_IDENTITY_WARNING, USB_PERMISSIONS};
use escapement_verge_probe::pro::{Feature, ProError};
use escapement_verge_probe::progress::{Progress, Step};
use escapement_verge_probe::release::{DEFAULT_REPO, GitHub, Http, ReleaseError, Repo, Ureq};
#[cfg(feature = "simulate")]
use escapement_verge_probe::sim::http::SimHttp;
#[cfg(feature = "simulate")]
use escapement_verge_probe::sim::{SCENARIOS, SimBus};
use escapement_verge_probe::source::{self, ImageVersion, LoadError, Loaded};
use escapement_verge_probe::update::{self, Outcome, RestartError, Timing, UpdateError};
use escapement_verge_probe::usb::UsbBus;
use escapement_verge_probe::version::Relation;
use escapement_verge_probe::version::StockVersion;
use escapement_verge_probe::{
    BoardKind, Bus, Cancel, CommandError, Firmware, Path, Unit, detect, offer,
};
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{EnvFilter, filter_fn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::progress::{Bars, Spinner};
use crate::question::{
    EXIT_NOT_RUNNING, EXIT_STOPPED, Ended, confirm, confirm_at_terminal, interactive, say,
};

const EXAMPLES: &str = "\
Examples:
  verge list               what is attached, and how each probe can be updated
  verge update             install the latest Verge release on the only probe
  verge --serial EV3E7A91C2 update --release v0.3.0
  verge update --stock     put NXP's firmware back, from your LinkServer install
  verge isp flash --file build/mculink_cjtag.elf
                           flash a probe you put into ISP mode with the jumper";

/// Control the Verge firmware, or update it.
///
/// verge updates NXP's firmware as well as Verge. It finds every attached MCU-Link, says what it
/// runs, and picks the best way to update it. A probe running Verge, NXP's firmware V3.122 or
/// later, or daplink-rs restarts into its bootloader when asked, so it needs no jumper. Anything
/// else needs the ISP jumper, fitted before it is plugged in.
///
/// Exit status: 0 when it worked; 1 when it failed or the answer was no; 2 for a command line it
/// could not read; 3 when the probe came back in ISP mode after a flash or a reset, so it does
/// not run an image yet; 130 when Ctrl-C stopped an update or a question. Ctrl-C that ends a recording,
/// a watch or a batch the usual way leaves the status that run gives.
#[derive(Parser)]
#[command(name = "verge", version, after_help = EXAMPLES)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Say more about what is happening. Twice for USB-level detail.
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,

    /// The probe to act on, by serial number. Needed only when more than one attached probe could
    /// be meant and there is no terminal to ask at.
    #[arg(long, global = true)]
    serial: Option<String>,

    /// The GitHub repository Verge releases come from, Escapement-Project/mculink-verge unless
    /// this or VERGE_REPO names another.
    #[arg(long, global = true, value_name = "OWNER/NAME")]
    repo: Option<Repo>,

    /// A GitHub token. Public releases need none; a private repository does.
    #[arg(
        long,
        global = true,
        value_name = "TOKEN",
        env = "GITHUB_TOKEN",
        hide_env_values = true
    )]
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

impl Cli {
    /// The repository releases come from: --repo, else VERGE_REPO, else the project's own.
    /// VERGE_REPO is read here rather than by clap, so that a bad value stops only a command that
    /// uses releases ([`Cli::uses_releases`]).
    fn repo(&self) -> Result<Repo> {
        if let Some(repo) = &self.repo {
            return Ok(repo.clone());
        }
        match std::env::var("VERGE_REPO") {
            Ok(text) if !text.is_empty() => text.parse().map_err(|_| {
                anyhow::anyhow!(
                    "VERGE_REPO is {text:?}; it has to be OWNER/NAME, such as {DEFAULT_REPO}"
                )
            }),
            _ => Ok(Repo::default()),
        }
    }

    fn uses_releases(&self) -> bool {
        matches!(
            self.command,
            Some(
                Command::Update(_)
                    | Command::Batch(_)
                    | Command::Release { .. }
                    | Command::Isp(IspCommand::Flash(_))
            )
        )
    }
}

#[derive(Subcommand)]
enum Command {
    /// Say what every attached MCU-Link runs, and how each can be updated. The default.
    List,
    /// Install a firmware on a probe, the best way its state allows.
    ///
    /// The image is the latest Verge release unless you pick another. A probe already in ISP
    /// mode is flashed as it is.
    Update(UpdateArgs),
    /// Update every probe of a chosen kind as it is plugged in, one at a time, until stopped.
    ///
    /// Choose the image and the probes it takes once, answer one question, then plug probes in
    /// one after another or several on a hub. Each finished probe blinks so you can tell which to
    /// unplug. Ctrl-C stops taking new probes; the one under way finishes. Every probe gets a line
    /// in the record file.
    Batch(BatchArgs),
    /// Work with a probe's ROM bootloader, ISP mode: enter it, flash, reset out of it, ask it.
    #[command(subcommand)]
    Isp(IspCommand),
    /// Show a Verge release and how each attached probe compares to it.
    Release {
        /// A release tag. The latest release when left out.
        tag: Option<String>,
    },
    /// Read target memory through a probe over SWD, JTAG or cJTAG, as probe-rs's read does.
    ///
    /// Prints probe-rs's hex table on stdout and nothing else: each line is its address and eight
    /// values, four for b64. cJTAG works on a probe running Verge.
    Read(read::ReadArgs),
    /// Read an image file and say what it puts into flash. Touches no USB device.
    Inspect {
        /// An ELF or S-record file.
        file: PathBuf,
    },
    /// Drive and read the four DIO pins of a probe running Verge: on an MCU-Link Pro's J19
    /// header, or on a standalone MCU-Link's J8 pads.
    ///
    /// With no options it reads them. --high and --low make pins outputs at that level, and
    /// --input makes them inputs again. On a Pro, DIO0, DIO1 and DIO2 change direction together,
    /// and DIO3 on its own, so a change has to turn a whole group round; there LED6 (SIO) is lit
    /// while any of them is driven. On a standalone MCU-Link each pin changes on its own, and J8
    /// is wired straight to the probe's MCU: 3.3 V only, with no ground pin of its own. The pins
    /// keep their state between debug sessions and go back to inputs when the probe restarts.
    #[command(args_conflicts_with_subcommands = true)]
    Dio(DioCommand),
    /// Restart a probe running Verge, as unplugging it would, and wait for it to come back.
    ///
    /// The request goes to the probe's USB core rather than through CMSIS-DAP, so it works while a
    /// debugger holds the probe and when a CMSIS-DAP command never finishes. Everything the probe
    /// holds is lost: the DIO pins go back to inputs, a Pro's target supply turns off and an energy
    /// capture ends.
    Restart,
    /// Read or switch the target supply of an MCU-Link Pro running Verge.
    ///
    /// The supply feeds the target through J6, at 3.3 V or, with J5 fitted, 1.8 V, up to 400 mA.
    /// It is off whenever the Pro starts. The state shown is the one the firmware set, not a
    /// measured voltage.
    Power(energy::PowerArgs),
    /// Measure a target's voltage, current and energy through an MCU-Link Pro running Verge.
    ///
    /// The probe measures the current that flows from J9 pin 1 to pin 3, whatever supplies it;
    /// `verge power` switches the Pro's own supply. A capture runs in the probe from `start` to
    /// `stop`, so a debugger can use the probe meanwhile. The commands reach the probe through Verge
    /// Aux, so they work while a debugger has it open; a Verge build older than Verge Aux answers
    /// only once the debugger lets go.
    #[command(subcommand)]
    Energy(energy::EnergyCommand),
}

#[derive(Args)]
struct DioCommand {
    #[command(subcommand)]
    action: Option<DioAction>,
    #[command(flatten)]
    args: DioArgs,
}

#[derive(Subcommand)]
enum DioAction {
    /// Record a standalone MCU-Link's DIO pins to a tickwave recording, which tickwave opens.
    ///
    /// Each pin is a digital channel. The probe stamps an edge as its interrupt takes it, a little
    /// after the edge, to 7 ns. There are no energy readings. A Pro without its energy measurement
    /// circuit records the same way. Any other Pro records its pins beside its energy capture, with
    /// `verge energy record --dio`.
    ///
    /// The recording goes through Verge Aux, so a debugger can hold the probe meanwhile, and
    /// `verge dio` can drive the pins through CMSIS-DAP while it records. Where the probe lost
    /// edges, the recording marks the pins unknown.
    ///
    /// A crash loses about the last second. The probe reports its time while the pins are quiet,
    /// so the recording a crash leaves covers quiet stretches too, even before the first edge.
    /// Once recording ends, verge rewrites the file with full-size blocks.
    Record(dio_record::RecordArgs),
}

#[derive(Args)]
struct DioArgs {
    /// Drive these pins high: 0 to 3, as `--high 0,1,2`, or `dio0` to `dio3`, or `all`.
    #[arg(long, value_name = "PINS", value_delimiter = ',', value_parser = dio::parse_pin)]
    high: Vec<u8>,
    /// Drive these pins low.
    #[arg(long, value_name = "PINS", value_delimiter = ',', value_parser = dio::parse_pin)]
    low: Vec<u8>,
    /// Make these pins inputs.
    #[arg(long, value_name = "PINS", value_delimiter = ',', value_parser = dio::parse_pin)]
    input: Vec<u8>,
}

#[derive(Subcommand)]
enum IspCommand {
    /// Restart a probe into its ROM bootloader without the jumper, and leave it there.
    ///
    /// In ISP mode a probe cannot say which board it is, so `verge isp flash` cannot check that a
    /// Verge image is built for it. A probe known to be a board other than the standalone MCU-Link
    /// is restarted only with --stock.
    Enter {
        /// You mean to flash NXP's firmware next, which goes on any board.
        #[arg(long)]
        stock: bool,
    },
    /// Flash the probe that is in ISP mode, as after fitting the jumper.
    ///
    /// With the jumper still fitted, the probe comes back in ISP mode. Then take the jumper off and
    /// run `verge isp reset`.
    Flash(FlashArgs),
    /// Reset a probe out of ISP mode without flashing it, so it starts the image it holds.
    Reset {
        #[command(flatten)]
        port: PortArg,
    },
    /// Ask the bootloader of a probe in ISP mode what it is. Changes nothing.
    Info {
        #[command(flatten)]
        port: PortArg,
    },
}

#[derive(Args)]
struct PortArg {
    /// The probe in ISP mode at this USB port, as `verge list` names it, when several are. Only
    /// where the system ties a bootloader to its port, as Linux does.
    #[arg(long)]
    port: Option<String>,
}

#[derive(Args)]
#[group(multiple = false)]
struct Source {
    /// Verge from a GitHub release: the latest, or the one tagged TAG. The default.
    #[arg(long, value_name = "TAG", num_args = 0..=1, default_missing_value = "latest")]
    release: Option<String>,
    /// An ELF or S-record file, such as a Verge build.
    #[arg(long, value_name = "FILE")]
    file: Option<PathBuf>,
    /// NXP's firmware from your own LinkServer install: the newest found, or the newest in DIR.
    /// The tool never downloads it.
    #[arg(long, value_name = "DIR", num_args = 0..=1)]
    stock: Option<Option<PathBuf>>,
}

#[derive(Args)]
struct UpdateArgs {
    #[command(flatten)]
    source: Source,
    /// Flash without asking first.
    #[arg(short, long)]
    yes: bool,
    /// Install a release even when the probe runs the same or a newer build.
    #[arg(long)]
    force: bool,
}

/// Kinds of probe a batch takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Take {
    /// Probes running NXP's firmware.
    Stock,
    /// Probes running Verge.
    Verge,
    /// Probes that arrive in ISP mode, as after fitting the jumper.
    Isp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum BoardArg {
    /// The standalone MCU-Link.
    McuLink,
    /// The MCU-Link Pro. A Verge image has to declare it.
    McuLinkPro,
}

#[derive(Args)]
struct BatchArgs {
    #[command(flatten)]
    source: Source,
    /// Which probes to take, as `stock,verge`.
    #[arg(
        long,
        value_enum,
        num_args = 1..,
        value_delimiter = ',',
        ignore_case = true,
        default_values_t = [Take::Stock, Take::Verge]
    )]
    take: Vec<Take>,
    /// Take only NXP's firmware older than this version, such as 3.172. Implies --take stock.
    #[arg(long, value_name = "VERSION", value_parser = parse_stock_version)]
    stock_below: Option<StockVersion>,
    /// Take Verge of any build. Without it, only builds older than the image, or carrying no
    /// version, are taken. Implies --take verge.
    #[arg(long)]
    verge_any: bool,
    /// Take probes attached already, not only those plugged in from now on.
    #[arg(long)]
    include_attached: bool,
    /// The board to take.
    #[arg(long, value_enum, ignore_case = true, default_value_t = BoardArg::McuLink)]
    board: BoardArg,
    /// Stop once this many probes are updated.
    #[arg(long, value_name = "N", value_parser = parse_count)]
    count: Option<NonZeroUsize>,
    /// The record file: one JSON line per probe. Defaults to verge-batch-<time>.jsonl here.
    #[arg(long, value_name = "FILE")]
    record: Option<PathBuf>,
    /// Arm without asking first.
    #[arg(short, long)]
    yes: bool,
}

fn parse_count(text: &str) -> std::result::Result<NonZeroUsize, String> {
    match text.parse::<usize>() {
        Ok(n) => NonZeroUsize::new(n).ok_or_else(|| "--count has to be at least 1".to_owned()),
        Err(_) => Err(format!(
            "--count takes a number of probes, such as 20, not {text:?}"
        )),
    }
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

/// A log line as verge prints it: the level, then the message and its fields. The spans around it
/// are left out: a progress bar names its step already.
struct LogLine {
    ansi: bool,
}

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for LogLine
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        context: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        use tracing_subscriber::fmt::FormatFields;
        let level = *event.metadata().level();
        // The colours tracing-subscriber's own format gives each level.
        let colour = match level {
            tracing::Level::ERROR => "31",
            tracing::Level::WARN => "33",
            tracing::Level::INFO => "32",
            tracing::Level::DEBUG => "34",
            tracing::Level::TRACE => "35",
        };
        if self.ansi {
            write!(writer, "\x1b[{colour}m{level:>5}\x1b[0m ")?;
        } else {
            write!(writer, "{level:>5} ")?;
        }
        context.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Whether NO_COLOR asks for no colour: set, and not empty (no-color.org).
fn no_colour() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
}

/// The log target of the error that ends a run.
const EXIT_TARGET: &str = "verge::exit";

fn init_tracing(verbose: u8) {
    let default = match verbose {
        // nusb logs a claim it could not make as an error of its own, which verge reports in its
        // own words where it matters: a unit another program holds is listed as such, and one not
        // asked for is left alone.
        0 => "warn,nusb=off,escapement_verge_probe=info",
        // The bootloader and USB crates log every packet at info, so they wait for -vv.
        1 => "warn,escapement_verge_probe=debug,verge=debug",
        _ => "debug",
    };
    // The error that ends a run is printed whatever RUST_LOG selects: a script that narrows the
    // log to one crate still needs to see why verge failed.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default))
        .add_directive(
            format!("{EXIT_TARGET}=error")
                .parse()
                .expect("a constant directive"),
        );
    let bars = IndicatifLayer::new();
    let ansi = std::io::stderr().is_terminal() && !no_colour();
    let log = tracing_subscriber::fmt::layer()
        .with_ansi(ansi)
        .event_format(LogLine { ansi })
        .with_writer(bars.get_stderr_writer())
        .with_filter(filter);
    // The level filter is the log's alone: a span it disabled would get no bar.
    let ours = filter_fn(|meta| {
        let target = meta.target();
        meta.is_span()
            && (target.starts_with(env!("CARGO_CRATE_NAME"))
                || target.starts_with("escapement_verge_probe"))
    });
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
    // Ctrl-C is how a recording or a watch through a pipe is meant to end, so it says so plainly.
    let ending = match &cli.command {
        Some(
            Command::Energy(energy::EnergyCommand::Record { .. })
            | Command::Dio(DioCommand {
                action: Some(DioAction::Record(_)),
                ..
            }),
        ) => Some("Ending the recording; press Ctrl-C again to quit without finishing the file."),
        Some(Command::Energy(energy::EnergyCommand::Watch { .. })) => {
            Some("Ending the watch; press Ctrl-C again to quit at once.")
        }
        _ => None,
    };
    let handler = {
        let (cancel, stop) = (cancel.clone(), stop.clone());
        ctrlc::set_handler(move || {
            if question::asking() {
                // The question ends itself, and says so.
            } else if batch && !stop.is_cancelled() {
                stop.cancel();
                tracing::warn!(
                    "taking no new probes; the one under way finishes. Ctrl-C again stops it at the next safe point"
                );
            } else if !cancel.is_cancelled() {
                cancel.cancel();
                match ending {
                    Some(ending) => say(ending),
                    None => tracing::warn!(
                        "stopping at the next safe point; press Ctrl-C again to quit now"
                    ),
                }
            } else {
                std::process::exit(EXIT_STOPPED.into());
            }
        })
    };
    match handler {
        Ok(()) => escapement_verge_probe::hid::block_stop_signals(),
        Err(error) => tracing::debug!("no Ctrl-C handler: {error}"),
    }

    let repo = match cli.repo() {
        Ok(repo) => repo,
        Err(error) if cli.uses_releases() => {
            tracing::error!(target: EXIT_TARGET, "{error:#}");
            return ExitCode::FAILURE;
        }
        Err(_) => Repo::default(),
    };
    let result = simulated(&cli, &repo, &cancel, &stop).unwrap_or_else(|| {
        let github = GitHub::new(Ureq::default(), repo.clone(), cli.github_token.clone())
            .with_cache(Cache::user());
        run(&cli, &UsbBus::default(), &github, &cancel, &stop)
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error)
            if error
                .downcast_ref::<output::Error>()
                .is_some_and(output::Error::is_broken_pipe) =>
        {
            ExitCode::SUCCESS
        }
        Err(error) => match error.downcast::<Ended>() {
            Ok(ended) => {
                if let Some(message) = &ended.message {
                    say(message);
                }
                ExitCode::from(ended.code)
            }
            Err(error) => {
                tracing::error!(target: EXIT_TARGET, "{error:#}");
                if cancel.is_cancelled() {
                    ExitCode::from(EXIT_STOPPED)
                } else {
                    ExitCode::FAILURE
                }
            }
        },
    }
}

/// Runs the command on the simulated bench `--simulate` names, if it names one.
#[cfg(feature = "simulate")]
fn simulated(cli: &Cli, repo: &Repo, cancel: &Cancel, stop: &Cancel) -> Option<Result<()>> {
    let name = cli.simulate.as_ref()?;
    if let Some(Command::Read(args)) = &cli.command {
        return Some(read::check(args).and_then(|()| {
            Err(anyhow::anyhow!(
                "--simulate has no target to read: `read` goes to real probes through probe-rs"
            ))
        }));
    }
    Some(match SimBus::scenario(name) {
        // No cache: a simulated GitHub signs with a key made for the run, so a copy kept from
        // another run would never check out.
        Some(bus) => {
            let http = SimHttp::new(repo.clone());
            http.publish_ours("v0.3.0");
            let key = http.public_key();
            let github = GitHub::new(http, repo.clone(), None).with_key(Some(key));
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
fn simulated(_: &Cli, _: &Repo, _: &Cancel, _: &Cancel) -> Option<Result<()>> {
    None
}

fn run<B: Bus + Sync, H: Http>(
    cli: &Cli,
    bus: &B,
    github: &GitHub<H>,
    cancel: &Cancel,
    stop: &Cancel,
) -> Result<()> {
    let serial = cli.serial.as_deref();
    if let Some(serial) = serial {
        let why = match &cli.command {
            Some(Command::Batch(_)) => {
                Some("`batch` takes every probe of a kind, one after another")
            }
            Some(Command::Inspect { .. }) => Some("`inspect` reads a file and opens no probe"),
            Some(Command::Isp(
                IspCommand::Flash(_) | IspCommand::Reset { .. } | IspCommand::Info { .. },
            )) => Some("a probe in ISP mode reports no serial, so --port picks it instead"),
            _ => None,
        };
        if let Some(why) = why {
            bail!("--serial {serial} picks nothing here: {why}");
        }
    }
    match &cli.command {
        Some(Command::Batch(args)) => batch(bus, github, args, cancel, stop),
        None | Some(Command::List) => list(bus, cli.verbose > 0, serial),
        Some(Command::Dio(DioCommand {
            action: Some(DioAction::Record(args)),
            ..
        })) => dio_record::record(bus, args, serial, cancel),
        Some(Command::Dio(DioCommand { args, .. })) => dio(bus, args, serial),
        Some(Command::Restart) => restart(bus, serial),
        Some(Command::Power(args)) => energy::power(bus, args, serial),
        Some(Command::Energy(command)) => energy::energy(bus, command, serial, cancel),
        Some(Command::Update(args)) => update_unit(bus, github, args, serial, cancel),
        Some(Command::Isp(IspCommand::Enter { stock })) => bootloader(bus, serial, *stock, cancel),
        Some(Command::Isp(IspCommand::Flash(args))) => flash(bus, github, args, cancel),
        Some(Command::Isp(IspCommand::Reset { port })) => reset(bus, port.port.as_deref(), cancel),
        Some(Command::Isp(IspCommand::Info { port })) => {
            rom_info(bus, port.port.as_deref(), cancel)
        }
        Some(Command::Release { tag }) => release(bus, github, tag.as_deref(), serial),
        Some(Command::Inspect { file }) => inspect(file),
        Some(Command::Read(args)) => read::read(args, serial),
    }
}

/// What a command acts on, for picking a unit: the units that `fits`, and what to say when none
/// of several attached units does. `does` follows "No attached unit", and `needs` is a sentence.
struct Wants<F> {
    fits: F,
    does: &'static str,
    needs: &'static str,
}

/// The unit a command acts on: the one `serial` names; else the only attached unit that `wants`;
/// else, at a terminal, the one the user picks from those it wants. Off a terminal, several it
/// wants need `--serial`. A lone unit it does not want is returned, for the command to say why it
/// cannot act on it.
fn pick_unit<'a>(
    units: &'a [Unit],
    serial: Option<&str>,
    wants: &Wants<impl Fn(&Unit) -> bool>,
) -> Result<&'a Unit> {
    let listed = |listed: &[Unit]| -> Vec<&'a Unit> {
        units
            .iter()
            .filter(|unit| {
                listed
                    .iter()
                    .any(|l| l.device.instance == unit.device.instance)
            })
            .collect()
    };
    match detect::pick(units, serial, &wants.fits) {
        Ok(unit) => Ok(unit),
        Err(PickError::NoneFits(_)) if units.len() == 1 => Ok(&units[0]),
        Err(PickError::Several(several)) if interactive() => choose(&listed(&several)),
        Err(PickError::Several(several)) => bail!("{}", self::several(&listed(&several))),
        Err(PickError::NoneFits(units)) => bail!(
            "no attached probe {}: {}. {}",
            wants.does,
            clause_list(&units.iter().map(described).collect::<Vec<_>>()),
            wants.needs
        ),
        Err(error) => Err(anyhow::anyhow!("{error}")),
    }
}

/// The refusal for several units that could be meant, naming each the way it can be picked.
fn several(units: &[&Unit]) -> String {
    let names: Vec<String> = units.iter().map(|unit| unit_name(unit)).collect();
    let list = sentence_list(&names);
    // A unit in ISP mode has no serial; `update` says how to flash one.
    if units.iter().any(|unit| unit.serial().is_some()) {
        format!(
            "{} attached probes could be meant: {list}. Pick one with --serial",
            units.len()
        )
    } else {
        format!(
            "{} attached probes could be meant, and none reports a serial: {list}",
            units.len()
        )
    }
}

/// A unit as a sentence names it: its serial, or where it has none, its state and port.
fn unit_name(unit: &Unit) -> String {
    let port = unit.device.location.as_deref();
    match (unit.serial(), &unit.state, port) {
        (Some(serial), _, _) => serial.to_owned(),
        (None, State::Bootloader, Some(port)) => format!("the probe in ISP mode at {port}"),
        (None, State::MiniBootloader, Some(port)) => {
            format!("the MCU-Link mini in ISP mode at {port}")
        }
        (None, _, Some(port)) => format!("the probe at {port}"),
        (None, _, None) => "a probe that reports no port".to_owned(),
    }
}

/// What a unit is and runs, as a clause: "EV3E7A91C2 is a standalone MCU-Link running Verge,
/// build 14d05d4".
fn described(unit: &Unit) -> String {
    let port = unit.device.location.as_deref().unwrap_or("?");
    match (&unit.state, unit.serial()) {
        (State::Bootloader, _) => format!("the probe at {port} is in ISP mode"),
        (State::MiniBootloader, _) => {
            format!("the probe at {port} is an MCU-Link mini in ISP mode")
        }
        (State::Unreadable { .. }, serial) => format!(
            "the tool could not ask {} what it runs",
            serial.map_or_else(|| format!("the probe at {port}"), str::to_owned)
        ),
        (state, serial) => {
            let name = serial.map_or_else(|| format!("the probe at {port}"), str::to_owned);
            match unit.hardware() {
                Hardware::Unknown => format!("{name} runs {state}"),
                board => format!("{name} is {board} running {state}"),
            }
        }
    }
}

/// Names as a list: "a", "a and b", "a, b and c".
fn sentence_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Clauses as a list: "a", "a, and b", "a, b, and c".
fn clause_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

fn choose<'a>(units: &[&'a Unit]) -> Result<&'a Unit> {
    let items: Vec<String> = units.iter().map(|unit| pick_line(unit)).collect();
    Ok(units[question::choose("Which MCU-Link?", &items)?])
}

/// A unit as the picker lists it: port, serial, board and what it runs.
fn pick_line(unit: &Unit) -> String {
    let location = unit.device.location.as_deref().unwrap_or("?");
    let serial = unit.serial().unwrap_or("-");
    let board = match (&unit.state, unit.hardware()) {
        (State::Bootloader, _) | (_, Hardware::Unknown) => "-".to_owned(),
        (_, hardware) => hardware.name(),
    };
    format!("{location:<8} {serial:<14} {board:<20} {}", unit.state)
}

fn list<B: Bus>(bus: &B, verbose: bool, serial: Option<&str>) -> Result<()> {
    let units = detect(bus).context("looking for MCU-Links")?;
    if let Some(serial) = serial
        && !units.iter().any(|unit| unit.serial() == Some(serial))
    {
        bail!("no attached MCU-Link has serial {serial}");
    }
    if units.is_empty() {
        outputln!("No MCU-Link is attached.");
        if cfg!(target_os = "linux") {
            outputln!(
                "If one is plugged in, check that your user may open it: see the udev rules at {USB_PERMISSIONS}"
            );
        }
        return Ok(());
    }
    let shown: Vec<&Unit> = units
        .iter()
        .filter(|unit| serial.is_none_or(|serial| unit.serial() == Some(serial)))
        .collect();
    // One grid: each label under the serials, each value under the states.
    let location = |unit: &Unit| unit.device.location.clone().unwrap_or_else(|| "?".into());
    let named = |unit: &Unit| unit.serial().unwrap_or("-").to_owned();
    let indent = shown
        .iter()
        .map(|unit| location(unit).len())
        .fold(8, usize::max);
    let label = shown
        .iter()
        .map(|unit| named(unit).len())
        .fold(14, usize::max);
    // On a terminal a value wraps under itself; through a pipe each stays on its line.
    let column = indent + 1 + label + 1;
    let width = std::io::stdout().is_terminal().then(|| {
        usize::from(dialoguer::console::Term::stdout().size().1).max(column + 20) - column
    });
    for unit in shown {
        outputln!(
            "{:<indent$} {:<label$} {}",
            location(unit),
            named(unit),
            unit.state
        );
        let row = |name: &str, value: &str| -> Result<()> {
            let lines = match width {
                Some(width) => textwrap::wrap(value, width),
                None => vec![value.into()],
            };
            for (n, line) in lines.iter().enumerate() {
                let name = if n == 0 { name } else { "" };
                outputln!("{:indent$} {name:<label$} {line}", "");
            }
            Ok(())
        };
        let board = match unit.state {
            State::Bootloader => "cannot be told in ISP mode".to_owned(),
            _ => unit.hardware().name(),
        };
        row("board", &board)?;
        if verbose {
            if let State::Probe { identity, .. } = &unit.state {
                let field = |name: &str, value: Option<&str>| row(name, value.unwrap_or("-"));
                field("vendor", identity.vendor.as_deref())?;
                field("product", identity.product.as_deref())?;
                field("protocol", identity.protocol.as_deref())?;
                field("firmware", identity.firmware_version.as_deref())?;
                let packets = identity
                    .packet_count
                    .zip(identity.packet_size)
                    .map(|(count, size)| format!("{count} of {size} bytes"));
                field("packets", packets.as_deref())?;
            }
            // A unit a debugger holds answered over Verge Aux, which says no more than this.
            if let Some(ours) = unit.verge() {
                row("capabilities", &ours.flag_names().join(", "))?;
                if let Some(names) = ours.scan_format_names() {
                    let names = if names.is_empty() {
                        "none".to_owned()
                    } else {
                        names.join(", ")
                    };
                    row("cJTAG", &names)?;
                }
            }
        }
        let offer = offer(unit, &units);
        let mut notes = offer.notes.iter();
        match offer.paths.as_slice() {
            // The first note says why there is no path.
            [] => row(
                "update",
                notes.next().map_or("not possible as it is", String::as_str),
            )?,
            [best, rest @ ..] => {
                row("update", &format!("{best}. {}", best.describe()))?;
                for other in rest {
                    row("or", &other.to_string())?;
                }
            }
        }
        for note in notes {
            row("note", note)?;
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
        .map_err(|error| load_error(error, github.repo()))
}

/// Why an image did not load, with the flags that change it.
fn load_error(error: LoadError, repo: &Repo) -> anyhow::Error {
    match error {
        error @ LoadError::AlreadyRuns { .. } => {
            anyhow::anyhow!("{error}. Pass --force to flash it again")
        }
        error @ LoadError::Newer { .. } => {
            anyhow::anyhow!("{error}. Pass --force to install the release anyway")
        }
        error @ LoadError::NoStock { .. } => anyhow::anyhow!("{error} with --stock DIR"),
        error @ LoadError::OtherBoard { .. } => anyhow::anyhow!("{error} with --stock"),
        error @ LoadError::NotForBoard { .. } => anyhow::anyhow!(
            "{error} with --stock, or a Verge build that declares it with --file IMAGE"
        ),
        // It names the repository itself.
        LoadError::Release(error @ ReleaseError::NotFound { tag: None, .. }) => anyhow::anyhow!(
            "{error}. Install your own build with `verge update --file IMAGE`, or NXP's firmware with --stock"
        ),
        LoadError::Release(error @ ReleaseError::NotFound { .. }) => error.into(),
        LoadError::Release(error @ ReleaseError::TokenRefused(_)) => {
            anyhow::anyhow!("{error}: unset GITHUB_TOKEN, or pass a valid one with --github-token")
        }
        LoadError::Release(error) => {
            let doing = match error {
                ReleaseError::NotFound { .. }
                | ReleaseError::Refused(_)
                | ReleaseError::TokenRefused(_)
                | ReleaseError::Unreachable(_)
                | ReleaseError::Json(_) => "looking up Verge releases in",
                // The lookup's and the download's, and its message names the URL.
                ReleaseError::Http(_) => "getting a release from",
                // After the lookup found the release.
                _ => "installing a release from",
            };
            anyhow::Error::new(error).context(format!("{doing} {repo}"))
        }
        error => error.into(),
    }
}

/// The question before a release is downloaded for one unit.
fn download_question(yes: bool) -> impl FnOnce(&str) -> Result<()> {
    move |what| confirm(&[], &format!("Download {what}?"), yes)
}

/// The command that flashes the same image again, for advice. A release names its repository
/// where it is not the default, so that the command finds the same release, and its kept copy.
fn again<H: Http>(source: &Source, version: &ImageVersion, github: &GitHub<H>) -> String {
    source::flash_command_in(&source.to_core(), version, github.repo())
}

/// The boards a Verge image is built for, as a sentence. `None` for NXP's own image, which goes
/// on any board.
fn built_for(image: &Image, version: &ImageVersion) -> Result<Option<String>> {
    if version.is_stock() {
        return Ok(None);
    }
    Ok(Some(format!("The image {}.", board::image_is_for(image)?)))
}

/// Says how a flashed unit came back. One that came back in ISP mode does not run the image yet:
/// that ends the run with [`EXIT_NOT_RUNNING`]. `before` is the unit's serial before the flash,
/// where it had one.
fn report(outcome: &Outcome, before: Option<&str>) -> Result<()> {
    match outcome {
        Outcome::Running(unit) => {
            let after = unit.serial();
            let at = unit.device.location.as_deref().unwrap_or("?");
            match (before, after) {
                (Some(before), Some(after)) if before != after => outputln!(
                    "Done: {before} at {at} now runs {}, and reports serial {after}.",
                    unit.state
                ),
                _ => outputln!(
                    "Done: {} at {at} now runs {}.",
                    after.or(before).unwrap_or("the probe"),
                    unit.state
                ),
            }
            return Ok(());
        }
        Outcome::JumperStillFitted => outputln!(
            "Flashed. The probe came back in ISP mode, so its ISP pin is still held low: by the ISP jumper, or by another probe on its debug connector. Take the jumper off or unplug the other probe, then run `verge isp reset`."
        ),
        Outcome::NotRestarted { maybe_jumper } => outputln!(
            "Flashed and verified, but the probe is still in ISP mode: {}. Then it starts the new image.",
            update::not_restarted(*maybe_jumper)
        ),
    }
    Err(Ended::quietly(EXIT_NOT_RUNNING).into())
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

/// The jumper's steps for `unit`, once a no left the image unchecked: "an image built for a
/// standalone MCU-Link".
fn image_for(unit: &Unit) -> String {
    match unit.hardware() {
        Hardware::Board(board) => format!("an image built for {}", board.a()),
        _ => "an image built for it".to_owned(),
    }
}

fn update_unit<B: Bus, H: Http>(
    bus: &B,
    github: &GitHub<H>,
    args: &UpdateArgs,
    serial: Option<&str>,
    cancel: &Cancel,
) -> Result<()> {
    let units = detect_named(bus, serial).context("looking for MCU-Links")?;
    let wants = Wants {
        fits: |_: &Unit| true,
        does: "can be updated",
        needs: "",
    };
    let unit = pick_unit(&units, serial, &wants).map_err(|refused| {
        match isp_flash_hint(&units, &args.source, github) {
            Some(hint) => anyhow::anyhow!("{refused}. {hint}"),
            None => refused,
        }
    })?;
    let offer = offer(unit, &units);
    let Some(path) = offer.best() else {
        let refused = anyhow::anyhow!(
            "{} cannot be updated as it is. {}",
            unit_name(unit),
            offer.notes.join(" ")
        );
        return Err(match isp_flash_hint(&units, &args.source, github) {
            Some(hint) if !unit.is_bootloader() => anyhow::anyhow!("{refused} {hint}."),
            _ => refused,
        });
    };
    if path == Path::Jumper {
        // Loaded first, so that a known board is refused here as on the jumper-free path, and the
        // advice names an image checked for it. A no still says how to use the jumper.
        let lead = "This probe cannot be updated without the jumper now. verge downloads the release first, to check that it is built for this probe.";
        let notes = offer.notes.join(" ");
        let loaded = load(&args.source, github, Some(unit), args.force, |what| {
            confirm(&[lead.to_owned()], &format!("Download {what}?"), args.yes).map_err(|no| {
                match no.downcast_ref::<Ended>() {
                    Some(ended) if ended.code == 1 => Ended::new(
                        format!(
                            "Nothing was downloaded. {notes} To use the jumper: unplug the probe, fit the ISP jumper, plug it back in, and run `verge isp flash` with {}.",
                            image_for(unit)
                        ),
                        1,
                    )
                    .into(),
                    _ => no,
                }
            })
        })?;
        let again = again(&args.source, &loaded.version, github);
        bail!(
            "this probe cannot be updated without the jumper now. {notes} To use the jumper: unplug the probe, fit the ISP jumper, plug it back in, and run `{again}`. {}",
            in_isp_mode(unit)
        );
    }
    let stock = args.source.stock.is_some();
    let unknown = source::board_unknown(unit).filter(|_| !stock);
    if let Some(unknown) = unknown {
        if args.yes {
            bail!(
                "{unknown} The tool asks before putting Verge on such a probe, and --yes does not answer that: run it without --yes, at a terminal"
            );
        }
        // Before anything is downloaded for a question nobody could answer.
        if !std::io::stdin().is_terminal() {
            bail!(
                "{unknown} Only an answer at a terminal says yes to putting Verge on such a probe; --yes does not"
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
    if cancel.is_cancelled() {
        return Err(UpdateError::Stopped.into());
    }
    let (image, name) = (&loaded.image, &loaded.name);
    log_image(image);
    let question = match path {
        Path::FlashNow => format!("Flash {name} onto the probe in ISP mode?"),
        _ => format!(
            "Flash {name} onto {} without the jumper?",
            unit.serial().unwrap_or("the probe")
        ),
    };
    let mut lead = Vec::new();
    if path == Path::FlashNow {
        lead.push(ISP_IDENTITY_WARNING.to_owned());
        if args.yes {
            tracing::warn!("{ISP_IDENTITY_WARNING} Flashing it, as --yes says.");
        }
    }
    if let Some(unknown) = unknown {
        lead.push(unknown.to_owned());
    }
    if (path == Path::FlashNow || unknown.is_some())
        && let Some(built_for) = built_for(image, &loaded.version)?
    {
        lead.push(built_for);
    }
    match unknown {
        Some(_) => confirm_at_terminal(&lead, &question)?,
        None => confirm(&lead, &question, args.yes)?,
    }
    let mut bars = Bars::new("update");
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
    report(
        &outcome.map_err(|error| explain(error, Some(&again), Some(unit)))?,
        unit.serial(),
    )
}

/// The command that flashes a unit in ISP mode, which has no serial to pick it by, when `units`
/// hold one.
fn isp_flash_hint<H: Http>(units: &[Unit], source: &Source, github: &GitHub<H>) -> Option<String> {
    let isp: Vec<&Unit> = units
        .iter()
        .filter(|unit| matches!(unit.state, State::Bootloader))
        .collect();
    if units.iter().any(|unit| unit.state == State::MiniBootloader) {
        return (!isp.is_empty()).then(|| {
            "An MCU-Link mini is in ISP mode too, and a flash needs one bootloader alone on the bus: unplug the mini first".to_owned()
        });
    }
    let command = again(source, &ImageVersion::Unknown, github);
    match isp.as_slice() {
        [] => None,
        [unit] => Some(match unit.device.location.as_deref() {
            Some(port) => format!(
                "The probe in ISP mode, at {port}, has no serial: flash it with `{command} --port {port}`"
            ),
            None => format!("The probe in ISP mode has no serial: flash it with `{command}`"),
        }),
        several => {
            let ports: Vec<String> = several
                .iter()
                .filter_map(|unit| unit.device.location.clone())
                .collect();
            match ports.first() {
                Some(first) => Some(format!(
                    "The probes in ISP mode, at {}, have no serial: `{command} --port {first}` flashes one",
                    sentence_list(&ports)
                )),
                None => Some(format!(
                    "The probes in ISP mode have no serial: flash one with `{command} --port PORT`, with the port `verge list` names"
                )),
            }
        }
    }
}

/// What to know about flashing `unit` once it is in ISP mode, where nothing tells its board.
fn in_isp_mode(unit: &Unit) -> String {
    match unit.hardware() {
        Hardware::Board(board) => format!(
            "In ISP mode the tool cannot tell which board a probe is: that image is the one checked for this probe, {}.",
            board.a()
        ),
        _ => "In ISP mode the tool cannot tell which board a probe is, and this probe's firmware did not say either: check that the image is built for it.".to_owned(),
    }
}

/// A refusal from a command for a unit in ISP mode, with `verge isp enter` where none is in ISP
/// mode and an attached unit can go there without the jumper.
fn isp_refusal<B: Bus>(bus: &B, error: UpdateError) -> anyhow::Error {
    if !matches!(error, UpdateError::NoneInIsp) {
        return error.into();
    }
    let units = detect(bus).unwrap_or_default();
    let enters: Vec<&Unit> = units
        .iter()
        .filter(|unit| {
            unit.serial().is_some()
                && update::check_entry(unit, false).is_ok()
                && offer(unit, &units).paths.iter().any(|path| {
                    matches!(
                        path,
                        Path::OursJumperFree | Path::StockJumperFree | Path::DaplinkRsJumperFree
                    )
                })
        })
        .collect();
    match enters.as_slice() {
        [] => error.into(),
        [unit] => anyhow::anyhow!(
            "no probe is in ISP mode. `verge isp enter` puts {} there without the jumper",
            unit.serial().unwrap_or("the probe")
        ),
        several => anyhow::anyhow!(
            "no probe is in ISP mode. `verge --serial SERIAL isp enter` puts one there without the jumper: {}",
            sentence_list(
                &several
                    .iter()
                    .filter_map(|unit| unit.serial().map(str::to_owned))
                    .collect::<Vec<_>>()
            )
        ),
    }
}

fn flash<B: Bus, H: Http>(
    bus: &B,
    github: &GitHub<H>,
    args: &FlashArgs,
    cancel: &Cancel,
) -> Result<()> {
    let port = args.port.port.as_deref();
    let rom = update::flash_target(bus, &bus.devices()?, port)
        .map_err(|error| isp_refusal(bus, error))?;
    let loaded = load(
        &args.source,
        github,
        None,
        true,
        download_question(args.yes),
    )?;
    if cancel.is_cancelled() {
        return Err(UpdateError::Stopped.into());
    }
    let (image, name) = (&loaded.image, &loaded.name);
    log_image(image);
    let location = rom.location.as_deref().unwrap_or("?");
    let mut lead = vec![ISP_IDENTITY_WARNING.to_owned()];
    lead.extend(built_for(image, &loaded.version)?);
    if args.yes {
        tracing::warn!("{} Flashing it, as --yes says.", lead.join(" "));
    }
    confirm(
        &lead,
        &format!("Flash {name} onto the probe in ISP mode at {location}?"),
        args.yes,
    )?;
    let mut bars = Bars::new("isp flash");
    let outcome = update::flash_now(bus, image, port, &Timing::default(), &mut bars, cancel);
    drop(bars);
    let again = again(&args.source, &loaded.version, github);
    report(
        &outcome.map_err(|error| explain(error, Some(&again), None))?,
        None,
    )
}

/// A batch's output: bars for the unit under way, a line and a record line for each unit, and a
/// spinner while it waits for units.
struct BatchOutput {
    bars: Bars,
    idle: Option<Spinner>,
    record: RecordWriter<std::io::BufWriter<std::fs::File>>,
    record_error: Option<std::io::Error>,
    output_error: Option<output::Error>,
    stop: Cancel,
}

impl BatchOutput {
    fn line(&mut self, args: std::fmt::Arguments<'_>) {
        if self.output_error.is_none()
            && let Err(error) = output::line(args)
        {
            self.output_error = Some(error);
            self.stop.cancel();
        }
    }
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
        self.idle = None;
        let serial = entry.serial_before.as_deref().unwrap_or("in ISP mode");
        tracing::info!("{}: updating {serial}", entry.port);
        self.bars.rename(format!("{} {serial}", entry.port));
    }

    fn idle(&mut self, tally: &Tally) {
        let counts = [
            (tally.updated, "updated"),
            (tally.current, "current"),
            (tally.skipped, "skipped"),
            (tally.failed, "failed"),
        ];
        let said: Vec<String> = counts
            .iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, what)| format!("{n} {what}"))
            .collect();
        let text = if said.is_empty() {
            "waiting for probes".to_owned()
        } else {
            format!("waiting for probes: {}", said.join(", "))
        };
        match &self.idle {
            Some(spinner) => spinner.set(&text),
            None => self.idle = Some(Spinner::new(&text)),
        }
    }

    fn record(&mut self, record: &Record) {
        self.bars.clear();
        if self.record_error.is_none()
            && let Err(error) = self.record.write(record)
        {
            tracing::error!("the record file: {error}");
            self.record_error = Some(error);
            self.stop.cancel();
        }
        let serial = match (&record.serial_before, &record.serial_after) {
            (Some(before), Some(after)) if before != after => format!("{before} → {after}"),
            (Some(before), _) => before.clone(),
            (None, Some(after)) => after.clone(),
            (None, None) => "-".to_owned(),
        };
        let firmware = match &record.firmware_after {
            Some(after) => format!("{} → {after}", record.firmware_before),
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
        self.line(format_args!(
            "{:<8} {serial:<14}  {firmware}  {}{seconds}{reason}",
            record.port,
            record.result.as_str().to_uppercase()
        ));
    }

    fn unplug(&mut self, port: &str, blinking: bool) {
        if blinking {
            self.line(format_args!("{port:<8} blinking: unplug it when you like"));
        } else {
            self.line(format_args!("{port:<8} done: unplug it when you like"));
        }
    }

    fn paused(&mut self, reason: Option<&str>) {
        match reason {
            Some(reason) => say(&format!("Paused. {reason}")),
            None => say("Resumed."),
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
    let ours = if args.verge_any {
        OursFilter::Any
    } else if args.take.contains(&Take::Verge) {
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
    let mut lead = Vec::new();
    if filter.bootloaders {
        lead.push(ISP_IDENTITY_WARNING.to_owned());
    }
    lead.push("The batch flashes each probe it takes without asking again.".to_owned());
    let question = |what: &str| format!("{what} onto {}?", filter.describe());
    let mut asked = false;
    let loaded = load(&args.source, github, None, true, |what| {
        asked = true;
        confirm(
            &lead,
            &question(&format!("Download {what}, and flash it")),
            args.yes,
        )
    })?;
    if stop.is_cancelled() {
        outputln!("Stopped before the batch was armed. Nothing was flashed.");
        return Ok(());
    }
    let armed = Armed::new(loaded, core_source, github.repo().clone());
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
        tracing::warn!("{ISP_IDENTITY_WARNING} Taking probes in ISP mode, as --yes says.");
    }
    if !asked {
        confirm(&lead, &question(&format!("Flash {name}")), args.yes)?;
    }
    let file = open_record(&path)
        .with_context(|| format!("opening the record file {}", path.display()))?;
    let mut output = BatchOutput {
        bars: Bars::default(),
        idle: None,
        record: RecordWriter::new(std::io::BufWriter::new(file)),
        record_error: None,
        output_error: None,
        stop: stop.clone(),
    };
    outputln!(
        "Armed: {name}, for {}. Recording to {}. Plug probes in; Ctrl-C stops taking new ones.",
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
    let result = batch.run_to_end(&mut output);
    output.idle = None;
    if let Some(error) = output.record_error {
        return Err(error).context("writing the batch record file");
    }
    result?;
    let tally = batch.tally();
    let ended = if stop.is_cancelled() {
        "stopped"
    } else {
        "done"
    };
    output.line(format_args!("Batch {ended}: {tally}."));
    if tally.failed > 0 {
        bail!(
            "{} failed; the record file {} says why",
            count(tally.failed, "probe"),
            path.display()
        );
    }
    match output.output_error {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}

/// "1 probe", "2 probes".
fn count(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// How often, as "once" or "3 times".
fn times(n: usize) -> String {
    if n == 1 {
        "once".to_owned()
    } else {
        format!("{n} times")
    }
}

/// Why a batch did not arm, with the flags that change it.
fn arm_refusal(error: &ArmError) -> anyhow::Error {
    match error {
        ArmError::OursUndated => anyhow::anyhow!(
            "{error}. Pass --verge-any to take every probe running Verge, or leave Verge out, as with --take stock"
        ),
        ArmError::NotForBoard { .. } => anyhow::anyhow!(
            "{error}. Pick a Verge image built for it, or NXP's firmware with --stock"
        ),
        ArmError::Image(_) => anyhow::anyhow!("{error}"),
    }
}

fn dio<B: Bus>(bus: &B, args: &DioArgs, serial: Option<&str>) -> Result<()> {
    let fold = |pins: &[u8]| pins.iter().fold(0, |mask, pin| mask | pin);
    let (high, low, input) = (fold(&args.high), fold(&args.low), fold(&args.input));
    for (a, b, names) in [
        (high, low, "--high and --low"),
        (high, input, "--high and --input"),
        (low, input, "--low and --input"),
    ] {
        if a & b != 0 {
            bail!("{} named in both {names}", dio::names(a & b));
        }
    }
    let change = DioChange::default().high(high).low(low).input(input);
    energy::with_pro(bus, serial, Feature::Dio, |serial, pro| {
        let pro_board = pro
            .info()
            .ok()
            .flatten()
            .and_then(|info| info.board)
            .is_none_or(|board| board == BoardKind::Pro);
        let after = match pro.change_dio(&change) {
            Ok(after) => after,
            Err(DioError::Off) => bail!(
                "{serial} started with its USB-SIO jumper, J15, fitted, which turns the DIO pins off. Take J15 off, then unplug the probe and plug it back in"
            ),
            Err(DioError::Split(group)) => bail!(
                "{} change direction together, so make them all outputs or all inputs",
                dio::names(group)
            ),
            Err(DioError::Command(CommandError::Firmware(_))) => {
                bail!(
                    "{serial} refused the change to {}",
                    dio::names(change.pins())
                )
            }
            Err(error) if change.pins() == 0 => {
                return Err(error).with_context(|| format!("reading {serial}'s DIO pins"));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("setting {serial}'s DIO pins"));
            }
        };
        let header = if pro_board {
            "J19 pins 2 to 5"
        } else {
            "J8 pins 1 to 4"
        };
        outputln!("{serial}'s DIO pins, on {header}:");
        for n in 0..8 {
            let bit = 1 << n;
            if after.pins & bit == 0 {
                continue;
            }
            let level = |on: bool| if on { "high" } else { "low" };
            if after.outputs & bit != 0 {
                outputln!(
                    "  DIO{n}  output  driven {}",
                    level(after.levels & bit != 0)
                );
            } else {
                outputln!("  DIO{n}  input   reads {}", level(after.read & bit != 0));
            }
        }
        let groups: Vec<String> = after
            .groups
            .iter()
            .map(|&g| g & after.pins)
            .filter(|&g| g != 0)
            .map(dio::names)
            .collect();
        outputln!("Direction groups: {}.", groups.join("; "));
        Ok(())
    })
}

fn restart<B: Bus>(bus: &B, serial: Option<&str>) -> Result<()> {
    let units = detect_named(bus, serial).context("looking for MCU-Links")?;
    let wants = Wants {
        fits: Unit::takes_restart,
        does: "runs Verge",
        needs: "`verge restart` restarts only a probe running Verge.",
    };
    let unit = pick_unit(&units, serial, &wants)?;
    let name = unit.serial().unwrap_or("the probe");
    if !unit.takes_restart() {
        match &unit.state {
            State::Unreadable { reason, .. } if reason == detect::HELD => bail!(
                "the tool could not ask {name} what it runs ({reason}), so it sends no restart request. Close that program, or unplug the probe"
            ),
            State::Unreadable { reason, .. } => bail!(
                "the tool could not ask {name} what it runs ({reason}), so it sends no restart request: only Verge takes one"
            ),
            state => bail!("{name} runs {state}, not Verge, so it takes no restart request"),
        }
    }
    let losing = what_a_restart_ends(bus, unit);
    match update::restart(bus, unit) {
        Ok(()) => {
            outputln!("{name} restarted.{losing}");
            Ok(())
        }
        Err(RestartError::Open(error)) => {
            Err(anyhow::Error::new(error).context(format!("opening {name}")))
        }
        Err(RestartError::Request(error)) => {
            Err(anyhow::Error::new(error).context(format!("asking {name} to restart")))
        }
        Err(RestartError::Look(error)) => {
            Err(anyhow::Error::new(error).context("looking for MCU-Links"))
        }
        Err(RestartError::NoReturn) => bail!(
            "{name} did not come back within {} s. `verge list` shows what is attached",
            update::RESTART_RETURNS.as_secs()
        ),
    }
}

/// What a restart of `unit` ends that the user may not expect, as sentences after "restarted.":
/// a running capture and a target supply that is on. Empty where nothing is, and where the unit
/// does not answer: the restart goes ahead either way.
fn what_a_restart_ends<B: Bus>(bus: &B, unit: &Unit) -> String {
    let Some(board) = unit.verge().and_then(|ours| ours.board) else {
        return String::new();
    };
    let mut ends = String::new();
    if Feature::Energy.on(board)
        && let Ok(Ok(status)) =
            escapement_verge_probe::pro::with_pro(bus, unit, Feature::Energy, |_, pro| {
                Ok::<_, ProError>(pro.energy_capture_status())
            })
        && status.running
    {
        ends.push_str(" Its energy capture ended.");
    }
    if Feature::Supply.on(board)
        && let Ok(Ok(true)) =
            escapement_verge_probe::pro::with_pro(bus, unit, Feature::Supply, |_, pro| {
                Ok::<_, ProError>(pro.read_target_power())
            })
    {
        ends.push_str(" Its target supply is off.");
    }
    ends
}

/// The refusal for `isp enter` where every unit is in ISP mode already, with the commands that
/// work on them.
fn already_in_isp<B: Bus>(bus: &B, units: &[&Unit]) -> anyhow::Error {
    let ports: Vec<String> = units
        .iter()
        .map(|unit| unit.device.location.clone().unwrap_or_else(|| "?".into()))
        .collect();
    match ports.as_slice() {
        [port] => anyhow::anyhow!(
            "the probe at {port} is in ISP mode already. `verge isp flash` flashes it; `verge isp reset` starts the image it holds"
        ),
        [first, ..] if bus.tells_bootloaders_apart() => anyhow::anyhow!(
            "{} probes are in ISP mode already, at {}, and report no serial. `verge isp flash --port {first}` flashes one; `verge isp reset --port {first}` starts the image it holds",
            ports.len(),
            sentence_list(&ports)
        ),
        _ => anyhow::anyhow!(
            "{} probes are in ISP mode already, at {}. Unplug all but one, then `verge isp flash` flashes it",
            ports.len(),
            sentence_list(&ports)
        ),
    }
}

fn bootloader<B: Bus>(bus: &B, serial: Option<&str>, stock: bool, cancel: &Cancel) -> Result<()> {
    let units = detect_named(bus, serial).context("looking for MCU-Links")?;
    let running = |unit: &Unit| matches!(unit.state, State::Probe { .. });
    let in_isp: Vec<&Unit> = units.iter().filter(|unit| unit.is_bootloader()).collect();
    if serial.is_none() && !in_isp.is_empty() && !units.iter().any(running) {
        return Err(already_in_isp(bus, &in_isp));
    }
    let wants = Wants {
        fits: running,
        does: "runs a firmware",
        needs: "`verge isp enter` sends a probe running a firmware into ISP mode.",
    };
    let unit = pick_unit(&units, serial, &wants)?;
    if unit.state == State::MiniBootloader {
        bail!(
            "{}: verge does not work with the MCU-Link mini",
            unit_name(unit)
        );
    }
    if let Err(refused) = update::check_entry(unit, stock) {
        bail!(
            "{refused}. To put NXP's firmware on it, run `verge isp enter --stock`, then `verge isp flash --stock`. For a Verge image built for this board, run `verge update --file IMAGE` instead, which checks that the image declares the board before anything is flashed"
        );
    }
    let mut bars = Bars::new("isp enter");
    let rom = update::enter_bootloader(bus, unit, stock, &Timing::default(), &mut bars, cancel)
        .map_err(|error| explain(error, None, Some(unit)));
    drop(bars);
    let rom = rom?;
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
        Hardware::Board(board) if stock => outputln!(
            "The probe, {}, is in ISP mode at {at}. Flash it with `{flash} --stock`.",
            board.a()
        ),
        Hardware::Board(board) => outputln!(
            "The probe, {}, is in ISP mode at {at}, where the tool cannot tell its board. Flash it with `{flash}` and an image built for {}. Before it flashes, `{flash}` names the boards the image is built for.",
            board.a(),
            board.a()
        ),
        _ if stock => {
            outputln!("The probe is in ISP mode at {at}. Flash it with `{flash} --stock`.")
        }
        _ => outputln!(
            "The probe is in ISP mode at {at}. Its firmware did not say which board it is, so check that an image is built for it before you flash it with `{flash}`."
        ),
    }
    Ok(())
}

fn reset<B: Bus>(bus: &B, port: Option<&str>, cancel: &Cancel) -> Result<()> {
    let mut bars = Bars::new("isp reset");
    let outcome = update::reset_now(bus, port, &Timing::default(), &mut bars, cancel);
    drop(bars);
    let outcome = outcome.map_err(|error| match error {
        UpdateError::NoneInIsp => {
            anyhow::anyhow!("no probe is in ISP mode, so there is nothing to reset")
        }
        error => explain(error, None, None),
    })?;
    match outcome {
        Outcome::Running(unit) => {
            outputln!(
                "Done: {} at {} runs {}.",
                unit.serial().unwrap_or("the probe"),
                unit.device.location.as_deref().unwrap_or("?"),
                unit.state
            );
            return Ok(());
        }
        Outcome::JumperStillFitted => outputln!(
            "The probe came back in ISP mode. Either its ISP pin is still held low, by the ISP jumper or by another probe on its debug connector, or its flash holds no image the ROM starts. Take the jumper off or unplug the other probe and reset it again, or flash an image with `verge isp flash`."
        ),
        Outcome::NotRestarted { maybe_jumper } => outputln!(
            "The probe is still in ISP mode: {}.",
            update::not_restarted(maybe_jumper)
        ),
    }
    Err(Ended::quietly(EXIT_NOT_RUNNING).into())
}

fn rom_info<B: Bus>(bus: &B, port: Option<&str>, cancel: &Cancel) -> Result<()> {
    let (rom, info) = update::rom_info_now(bus, port, &Timing::default(), cancel).map_err(
        |error| match error {
            UpdateError::NoneInIsp => isp_refusal(bus, error),
            error => explain(error, None, None),
        },
    )?;
    let flash = match info.flash {
        Some((start, size)) => format!("flash from 0x{start:08x}, {} KiB", size / 1024),
        None => "flash not reported".to_owned(),
    };
    outputln!(
        "{}: bootloader {}, {flash}.",
        rom.location.as_deref().unwrap_or("?"),
        info.version
    );
    Ok(())
}

fn release<B: Bus, H: Http>(
    bus: &B,
    github: &GitHub<H>,
    tag: Option<&str>,
    serial: Option<&str>,
) -> Result<()> {
    // Before anything is printed, as `list` does.
    let units = detect(bus).context("looking for MCU-Links")?;
    if let Some(serial) = serial
        && !units.iter().any(|unit| unit.serial() == Some(serial))
    {
        bail!("no attached MCU-Link has serial {serial}");
    }
    let release = match tag {
        None => github.latest(),
        Some(tag) => github.tagged(tag),
    }
    .map_err(|error| load_error(LoadError::Release(error), github.repo()))?;
    outputln!(
        "{}  {}",
        release.tag,
        release.published.as_deref().unwrap_or("")
    );
    outputln!("  {}", release.page);
    outputln!(
        "  {}, {} bytes, signed in {}, checksum in {}",
        release.image.name,
        release.image.size,
        release.signature.name,
        release.checksum.name
    );
    outputln!(
        "  The image says which boards it is built for: verge checks that once it downloads it."
    );
    if !github.has_key() {
        outputln!(
            "This build of verge has no release key, so it installs no release: use --file or --stock."
        );
    }
    for unit in units
        .into_iter()
        .filter(|unit| serial.is_none_or(|serial| unit.serial() == Some(serial)))
    {
        let standing = match (unit.build(), &unit.state) {
            (Some(build), _) => match build.compare(&release.tag) {
                Relation::UpToDate => "runs this release".to_owned(),
                Relation::OtherVariant => format!("runs this release, built as {build}"),
                Relation::Behind => format!("runs {build}, older: an update is available"),
                Relation::Ahead => format!("runs {build}, newer than this release"),
                Relation::Unknown => format!("runs {build}, which carries no version to compare"),
            },
            (
                None,
                State::Probe {
                    firmware: Firmware::DaplinkRs { .. },
                    ..
                },
            ) => "runs daplink-rs; installing this release replaces it".to_owned(),
            (None, State::Probe { .. }) => {
                "runs NXP's firmware; installing this release replaces it".to_owned()
            }
            (
                None,
                State::Unreadable {
                    guess: Guess::Ours, ..
                },
            ) => "probably runs Verge; the tool could not ask which build".to_owned(),
            (None, state) => state.to_string(),
        };
        let location = unit.device.location.as_deref().unwrap_or("?");
        let serial = unit.serial().unwrap_or("-");
        outputln!("{location:<8} {serial:<14} {standing}");
    }
    Ok(())
}

fn inspect(file: &FsPath) -> Result<()> {
    let image = Image::from_path(file)?;
    let file_name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    outputln!("{}: {}", file.display(), image.format);
    if let Some(header) = &image.header {
        outputln!("  header {header:?}");
    }
    for chunk in image.chunks() {
        outputln!(
            "  0x{:08x}..0x{:08x}  {} bytes",
            chunk.address,
            chunk.end(),
            chunk.data.len()
        );
    }
    let runs = image.page_runs();
    let pages: usize = runs.iter().map(|r| r.data.len() / PAGE as usize).sum();
    outputln!(
        "  {} bytes into flash: {} of {PAGE} bytes to erase and write, in {}",
        image.data_len(),
        count(pages, "page"),
        count(runs.len(), "run")
    );
    outputln!("  sha256 {}", image.sha256_hex());
    match image.declaration() {
        Ok(Some(declaration)) => outputln!(
            "  declares, at 0x{:08x} in format {}, that it runs on {} (boards 0x{:08x})",
            declaration.address,
            declaration.format,
            board::built_for(&image.boards()?),
            declaration.boards
        ),
        Ok(None) if file_name.starts_with("MCU-MINI_") => outputln!(
            "  is NXP's firmware for the MCU-Link mini, by its name: verge does not work with the mini"
        ),
        Ok(None) if let Some(version) = escapement_verge_probe::stock::version_of(&file_name) => {
            outputln!(
                "  is NXP's firmware V{version}, by its name: it goes on any board NXP's firmware supports"
            )
        }
        Ok(None) => outputln!("  {}", board::image_is_for(&image)?),
        Err(_) => {}
    }
    match image.check_for_mculink() {
        Ok(()) => outputln!("  fits this part's flash and has a vector table"),
        Err(error) => outputln!("  will not be flashed: {error}"),
    }
    Ok(())
}

/// Adds what the user can do next to the errors that can leave a unit in ISP mode or away.
///
/// `again` is the command that flashes the same image, and `unit` the unit as it was before, where
/// it was running, to name its board.
fn explain(error: UpdateError, again: Option<&str>, unit: Option<&Unit>) -> anyhow::Error {
    let board = match unit.map(Unit::hardware) {
        Some(Hardware::Board(board)) => Some(board),
        _ => None,
    };
    let next = match (&error, again) {
        (UpdateError::Cancelled(left), _) => left.recovery(again, board),
        // The bootloader never answered, so nothing was written.
        (UpdateError::RomSilent { .. }, _) => Some(update::ROM_SILENT_ADVICE.to_owned()),
        (UpdateError::Isp(_), Some(again)) => Some(match board {
            Some(board) => format!(
                "The probe may be in ISP mode with a partial image. It said it was {} before, and in ISP mode the tool cannot ask it, so flash it again with the same image, which is built for it: `{again}`.",
                board.a()
            ),
            None => format!(
                "The probe may be in ISP mode with a partial image. `{again}` flashes it again."
            ),
        }),
        (UpdateError::NoReturn(_) | UpdateError::Ambiguous { .. }, _) => {
            Some("`verge list` shows what is attached now.".to_owned())
        }
        (UpdateError::OtherId { .. }, _) => Some(
            "To bring it back, fit the ISP jumper and flash an image that uses an ID the tool looks for."
                .to_owned(),
        ),
        _ => None,
    };
    match next {
        Some(next) => anyhow::anyhow!("{error}. {next}"),
        None => anyhow::Error::new(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_of_times_says_once_for_one() {
        assert_eq!(times(1), "once");
        assert_eq!(times(2), "2 times");
        assert_eq!(times(0), "0 times");
    }

    /// A release the lookup found and the tool then refused is not a failed lookup.
    #[test]
    fn a_refused_release_is_not_called_a_lookup() {
        use escapement_verge_probe::release::HttpError;
        let repo = Repo::default();
        let http = || HttpError::Status {
            status: 502,
            url: "https://api.github.com/".into(),
        };
        // A release that is not there names the repository itself, once.
        let missing = format!(
            "{:#}",
            load_error(
                LoadError::Release(ReleaseError::NotFound {
                    repo: repo.to_string(),
                    tag: Some("v9.9.9".into()),
                }),
                &repo
            )
        );
        assert_eq!(missing.matches(&repo.to_string()).count(), 1, "{missing}");
        let cases = [
            (ReleaseError::Refused(403), "looking up Verge releases in"),
            (
                ReleaseError::Unreachable(HttpError::Transport("no route".into())),
                "looking up Verge releases in",
            ),
            (
                ReleaseError::Json("eof".into()),
                "looking up Verge releases in",
            ),
            (ReleaseError::Http(http()), "getting a release from"),
            (
                ReleaseError::SignatureForAnotherRelease {
                    name: "mculink-verge-v0.0.1.s19.minisig".into(),
                    signed: "v0.0.0".into(),
                    tag: "v0.0.1".into(),
                },
                "installing a release from",
            ),
        ];
        for (error, doing) in cases {
            let message = format!("{:#}", load_error(LoadError::Release(error), &repo));
            assert!(
                message.starts_with(&format!("{doing} {repo}: ")),
                "{message}"
            );
        }
    }
}
