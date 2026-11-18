//! `verge power` and `verge energy`: an MCU-Link Pro's target supply and its energy measurement.

pub(crate) mod record;
mod watch;

use std::time::Duration;

use crate::output::outputln;
use crate::question::say;
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use escapement_verge_probe::aux::Aux;
use escapement_verge_probe::bus::BusError;
use escapement_verge_probe::detect::{Unit, detect_named};
use escapement_verge_probe::pro::{Feature, ProLink, check_offered};
use escapement_verge_probe::stream::EndReason;
use escapement_verge_probe::{
    Bus, CalibrationRange, Cancel, CommandError, EnergyAccumulator, EnergyCalibration,
    EnergyCaptureSample, EnergyError, EnergyRanges, EnergyTotal, VergeControl, VergeInfo,
    VergeTransport,
};

/// Pairs a second for a capture the command starts. verge-gui starts at the same rate.
const DEFAULT_RATE: u32 = 25_000;

#[derive(Args)]
pub struct PowerArgs {
    /// Turn the supply on or off. Left out, the tool reads it.
    #[arg(value_enum, ignore_case = true)]
    state: Option<Switch>,
}

/// `--rate`: pairs a second, as 25000 or 25k.
fn parse_rate(text: &str) -> Result<u32, String> {
    let refused =
        || format!("--rate takes 1 to 100,000 pairs a second, as 25000 or 25k, not {text:?}");
    let (digits, scale) = match text.strip_suffix(['k', 'K']) {
        Some(digits) => (digits, 1000),
        None => (text, 1),
    };
    let rate = digits
        .parse::<u32>()
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .ok_or_else(refused)?;
    if (1..=100_000).contains(&rate) {
        Ok(rate)
    } else {
        Err(refused())
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Switch {
    On,
    Off,
}

/// `--range`: escapement_verge_probe::EnergyRanges as the command line spells it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Ranges {
    #[default]
    Auto,
    High,
}

impl Ranges {
    /// What a capture on this setting does, as a clause.
    fn holding(self) -> &'static str {
        match self {
            Ranges::Auto => "switching current ranges automatically",
            Ranges::High => "holding the high range",
        }
    }
}

impl From<Ranges> for EnergyRanges {
    fn from(ranges: Ranges) -> EnergyRanges {
        match ranges {
            Ranges::Auto => EnergyRanges::Auto,
            Ranges::High => EnergyRanges::High,
        }
    }
}

/// `--range`, for the commands that can start a capture.
#[derive(Args, Clone, Copy, Debug, Default)]
pub struct RangeArg {
    /// Which current ranges a capture this starts uses. auto, the default, switches between the
    /// low and high ranges as the current moves: it reads currents down to tens of nA, but each
    /// switch disturbs the reading for about a millisecond. high holds the high range, so the
    /// reading follows the target undisturbed, but currents under about 0.65 mA in the 350 mA
    /// setting read unreliably. Either way, a high-range pair under that is marked under range.
    #[arg(long, value_enum, ignore_case = true)]
    pub range: Option<Ranges>,
}

#[derive(Subcommand)]
pub enum EnergyCommand {
    /// Start a capture. It runs in the probe until `stop`, the next start or a restart, so a
    /// debugger can use the probe meanwhile.
    ///
    /// A start clears the total. It can calibrate first, which disconnects J9 from the target for
    /// about 0.2 s: a target powered through J9 loses its supply for that time.
    Start {
        /// Voltage and current pairs a second, 1 to 100,000, as 25000 or 25k.
        #[arg(long, default_value_t = DEFAULT_RATE, value_parser = parse_rate)]
        rate: u32,
        #[command(flatten)]
        range: RangeArg,
    },
    /// Show the newest voltage and current pair.
    Latest,
    /// Show the energy since the last start. A stopped capture keeps its total until the next
    /// start or a restart.
    Total,
    /// Stop the capture and show its total.
    Stop,
    /// Show the points each current range was calibrated at.
    Calibration,
    /// Show the capture live: the newest pair, the total, and graphs of current and power.
    ///
    /// Starts a capture if none runs, and stops only one it started. Over a capture a fault
    /// stopped, it shows that capture's total and starts a new one. Q, Esc or Ctrl-C leaves.
    /// Where the output is not a terminal it prints a line a read instead. Through Verge Aux the
    /// current graph shows every pair, as the lowest and highest in each column; without it, the
    /// newest pair at each read, so it can miss short peaks. The power graph plots each
    /// interval's mean from the total, which counts every pair.
    Watch {
        /// Voltage and current pairs a second, for a capture this starts, as 25000 or 25k.
        #[arg(long, default_value_t = DEFAULT_RATE, value_parser = parse_rate)]
        rate: u32,
        /// How often to read the probe, as 100ms or 1s.
        #[arg(long, value_name = "DURATION", default_value = "100ms", value_parser = humantime::parse_duration)]
        every: Duration,
        /// How far back the graphs reach.
        #[arg(long, value_name = "DURATION", default_value = "30s", value_parser = humantime::parse_duration)]
        window: Duration,
        /// Leave after this long instead of waiting for a key.
        #[arg(long = "for", value_name = "DURATION", value_parser = humantime::parse_duration)]
        limit: Option<Duration>,
        #[command(flatten)]
        range: RangeArg,
    },
    /// Record every pair of a capture to a file: a tickwave recording, a sigrok session or CSV.
    ///
    /// The file's name picks the format: .tickwave for a tickwave recording, which tickwave opens;
    /// .sr for a sigrok session, which PulseView opens; .csv for CSV. A name that says none of
    /// these writes a tickwave recording, or what --csv or --tickwave says. Starts a capture if
    /// none runs, and stops only one it started. Needs Verge Aux, so a debugger can hold the probe
    /// meanwhile. Each pair's current is as the probe's total counts it. In a sigrok session, where
    /// the probe measured nothing, for a moment as it reads the supply or times a debugger's
    /// reads, the last pair repeats and the logic channel GAP is high. Each DIO pin --dio names is
    /// recorded too. Ctrl-C ends the recording.
    Record {
        /// The file to write. `-` writes CSV to stdout.
        #[arg(long, value_name = "FILE")]
        out: std::path::PathBuf,
        /// Voltage and current pairs a second, for a capture this starts, as 25000 or 25k.
        #[arg(long, default_value_t = DEFAULT_RATE, value_parser = parse_rate)]
        rate: u32,
        /// Stop after this long.
        #[arg(long = "for", value_name = "DURATION", value_parser = humantime::parse_duration)]
        limit: Option<Duration>,
        /// Write CSV, as a .csv name does: one line per pair, its sequence number, its time in ns
        /// since the capture started, millivolts, nanoamps and the current range it was read on
        /// (low, high, or under for a pair marked under range), then the level of each DIO pin
        /// recorded. Each edge of those pins is a line of its own, in time order, with only its
        /// time and the levels after it. Markers, if any arrive, are merged in as rows of their
        /// own when recording ends, with marker number and label columns; not into a pipe or
        /// terminal. Voltage and current are read together at the pair's time.
        #[arg(long)]
        csv: bool,
        /// Write a tickwave recording, as a .tickwave name does: voltage, current and power, the
        /// range each pair was read on and its under-range mark, each DIO pin recorded, and
        /// markers. It closes a block at least every second, so a crash loses at most that, and
        /// rewrites the file with full-size blocks once recording ends.
        #[arg(long, conflicts_with = "csv")]
        tickwave: bool,
        /// Record the edges of these DIO pins too: 0 to 3, as `--dio 0,3`, or `dio0` to `dio3`,
        /// or `all`. A tickwave recording gives each a digital channel with its edges' times; a
        /// sigrok session shows each as a logic channel at the pair rate, a pulse shorter than a
        /// pair stretched to one; CSV gives each edge's time.
        #[arg(long, value_name = "PINS", value_delimiter = ',', value_parser = crate::parse_dio_pins)]
        dio: Vec<u8>,
        #[command(flatten)]
        range: RangeArg,
    },
}

/// A Pro over Verge Aux where it has one, so a debugger can hold CMSIS-DAP meanwhile, or over
/// CMSIS-DAP.
type Pro<'a> = VergeControl<'a, dyn ProLink + 'a>;

/// A unit running Verge on a board with `feature`, whether its CMSIS-DAP is free or a debugger
/// holds it.
fn running_verge_with(unit: &Unit, feature: Feature) -> bool {
    unit.verge()
        .and_then(|ours| ours.board)
        .is_some_and(|board| feature.on(board))
}

/// The unit `serial` names, or the one [`crate::pick_unit`] finds with `feature`, refusing anything
/// else in a sentence that says why.
pub(crate) fn find_pro<B: Bus>(bus: &B, serial: Option<&str>, feature: Feature) -> Result<Unit> {
    let (does, needs) = match feature {
        Feature::Energy => (
            "measures energy",
            "Energy measurement needs an MCU-Link Pro running Verge.",
        ),
        Feature::Supply => (
            "has a target supply",
            "The target supply is an MCU-Link Pro's, reached while it runs Verge.",
        ),
        Feature::Dio => (
            "has DIO pins the tool can reach",
            "The DIO pins need an MCU-Link Pro or a standalone MCU-Link running Verge.",
        ),
    };
    find_pro_where(bus, serial, feature, |_| true, does, needs)
}

/// As [`find_pro`], picking without a serial only among the units `fits` takes. `does` and
/// `needs` say what none of several units did, as [`crate::Wants`] says.
pub(crate) fn find_pro_where<B: Bus>(
    bus: &B,
    serial: Option<&str>,
    feature: Feature,
    fits: impl Fn(&Unit) -> bool,
    does: &'static str,
    needs: &'static str,
) -> Result<Unit> {
    let units = detect_named(bus, serial).context("looking for MCU-Links")?;
    let wants = crate::Wants {
        fits: |unit: &Unit| running_verge_with(unit, feature) && fits(unit),
        does,
        needs,
    };
    let unit = crate::pick_unit(&units, serial, &wants)?;
    escapement_verge_probe::pro::validate(unit, feature)?;
    Ok(unit.clone())
}

/// Runs `f` on the unit [`find_pro`] finds.
pub(crate) fn with_pro<B: Bus, T>(
    bus: &B,
    serial: Option<&str>,
    feature: Feature,
    f: impl FnOnce(&str, &mut Pro<'_>) -> Result<T>,
) -> Result<T> {
    let unit = find_pro(bus, serial, feature)?;
    escapement_verge_probe::pro::with_pro(bus, &unit, feature, f)
}

fn bind_attached_capture<T: escapement_verge_probe::pro::ProLink + ?Sized>(
    pro: &mut VergeControl<'_, T>,
) -> Result<()> {
    if pro.tracked_capture_id().is_none() {
        if !pro.awaiting_energy_header() {
            bail!("no acknowledged attach is awaiting its capture header");
        }
        let message = pro.transport().message(Duration::from_secs(2))?;
        let Some(escapement_verge_probe::stream::Message::Header(header)) = message else {
            bail!("the attached stream did not provide its capture header");
        };
        pro.accept_energy_header(&header)?;
    }
    Ok(())
}

/// Whether the firmware keeps a running total.
fn keeps_totals<T: VergeTransport + ?Sized>(pro: &mut VergeControl<'_, T>) -> bool {
    pro.info()
        .ok()
        .flatten()
        .is_some_and(VergeInfo::supports_energy_accumulator)
}

/// Whether a capture runs, where the firmware can say. A build without capture identities, or a
/// probe that does not answer, says nothing.
fn capturing<T: VergeTransport + ?Sized>(pro: &mut VergeControl<'_, T>) -> bool {
    pro.energy_capture_status()
        .is_ok_and(|status| status.running)
}

/// The refusal for a build without the capture commands this tool uses, named plainly as the other
/// refusals for old builds are. The command was not sent.
fn old_build<T: VergeTransport + ?Sized>(
    pro: &mut VergeControl<'_, T>,
    name: &str,
) -> anyhow::Error {
    let build = pro.info().ok().flatten().map_or_else(
        || "a Verge build".to_owned(),
        |info| format!("build {}", info.firmware),
    );
    anyhow::anyhow!(
        "{name} runs {build}, which predates the capture commands this tool uses: update it first"
    )
}

/// An energy command's error, for a command that changes the capture.
fn energy_error<T: VergeTransport + ?Sized, E: std::error::Error + Send + Sync + 'static>(
    pro: &mut VergeControl<'_, T>,
    name: &str,
    doing: &str,
    error: CommandError<E>,
) -> anyhow::Error {
    match error {
        CommandError::Unsupported(_) => old_build(pro, name),
        // Sent, and the answer did not say whether it worked.
        error @ (CommandError::Probe(_) | CommandError::Malformed(_)) => {
            anyhow::Error::new(error).context(format!("{doing}; completion may be uncertain"))
        }
        error => anyhow::Error::new(error).context(doing.to_owned()),
    }
}

pub fn power<B: Bus>(bus: &B, args: &PowerArgs, serial: Option<&str>) -> Result<()> {
    with_pro(bus, serial, Feature::Supply, |name, pro| {
        // A capture keeps measuring a target the supply no longer feeds.
        let capture =
            matches!(args.state, Some(Switch::Off)) && keeps_captures(pro) && capturing(pro);
        let on = match args.state {
            None => pro.read_target_power(),
            Some(state) => pro.set_target_power(matches!(state, Switch::On)),
        }
        .with_context(|| format!("reaching {name}'s target supply"))?;
        outputln!(
            "{name}'s target supply is {}.",
            if on { "on" } else { "off" }
        );
        if capture && !on {
            outputln!("A capture is running; the target now has no supply from the Pro.");
        }
        Ok(())
    })
}

/// Whether the firmware measures energy at all.
fn keeps_captures<T: VergeTransport + ?Sized>(pro: &mut VergeControl<'_, T>) -> bool {
    pro.info()
        .ok()
        .flatten()
        .is_some_and(VergeInfo::supports_energy_capture)
}

pub fn energy<B: Bus>(
    bus: &B,
    command: &EnergyCommand,
    serial: Option<&str>,
    cancel: &Cancel,
) -> Result<()> {
    let energy = Feature::Energy;
    match command {
        EnergyCommand::Start { rate, range } => with_pro(bus, serial, energy, |name, pro| {
            let range = range.range.unwrap_or_default();
            // What the start replaces, said afterwards: the start clears the total.
            let replaced = if capturing(pro) && keeps_totals(pro) {
                pro.read_energy_accumulator().ok()
            } else {
                None
            };
            pro.set_energy_ranges(range.into())
                .with_context(|| format!("setting {name}'s current ranges"))?;
            let start = match pro.start_energy_capture_identified(*rate) {
                Ok(start) => start,
                Err(error) => {
                    return Err(energy_error(
                        pro,
                        name,
                        &format!("starting {name}'s capture"),
                        error,
                    ));
                }
            };
            let actual = start.actual_rate_pairs_per_second;
            let asked = if actual == *rate {
                String::new()
            } else {
                format!(" (asked for {rate})")
            };
            outputln!(
                "{name} captures {actual} pairs a second{asked}, {}.",
                range.holding()
            );
            if let Some(total) = replaced {
                outputln!(
                    "This replaced the capture that was running, and its total: {}.",
                    describe_total(name, total)?
                );
            }
            Ok(())
        }),
        EnergyCommand::Latest => with_pro(bus, serial, energy, |name, pro| {
            let latest = match pro.latest_energy_sample() {
                Err(CommandError::Firmware(EnergyError::DmaError)) => {
                    outputln!(
                        "{name}'s capture failed: the ADC overran, its DMA failed or the probe fell behind reading it. A new start clears the fault."
                    );
                    return Ok(());
                }
                latest => latest.with_context(|| format!("reading {name}'s newest pair"))?,
            };
            match latest {
                EnergyCaptureSample::Sample(sample) => match sample.calibrated {
                    Some(calibrated) => outputln!(
                        "{name}: {}, {} ({}), pair {}.",
                        si(f64::from(calibrated.voltage_millivolts) * 1e-3, "V"),
                        si(f64::from(calibrated.current_nanoamps) * 1e-9, "A"),
                        range_name(calibrated.range_id),
                        sample.raw.sequence
                    ),
                    None => outputln!(
                        "{name}: voltage code {}, current code {}, pair {}, not calibrated.",
                        sample.raw.voltage_code,
                        sample.raw.current_code,
                        sample.raw.sequence
                    ),
                },
                EnergyCaptureSample::NoSample => {
                    outputln!("{name}'s capture has no complete pair yet.");
                }
                EnergyCaptureSample::Stopped => {
                    outputln!("{name} is not capturing: start a capture first.");
                }
                _ => bail!("{name} answered with a reading this tool does not know"),
            }
            Ok(())
        }),
        EnergyCommand::Total => with_pro(bus, serial, energy, |name, pro| {
            let total = pro
                .read_energy_accumulator()
                .with_context(|| format!("reading {name}'s total"))?;
            outputln!("{name}: {}.", describe_total(name, total)?);
            Ok(())
        }),
        EnergyCommand::Stop => with_pro(bus, serial, energy, |name, pro| {
            // After a fault the stop still stops, and answers with the fault.
            let state = match pro.energy_capture_status() {
                Ok(state) => state,
                Err(CommandError::Unsupported(_)) => return Err(old_build(pro, name)),
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("asking {name} whether it captures"));
                }
            };
            let stopped = match state.capture_id {
                Some(id) => pro.stop_energy_capture_if(id),
                None if !state.running => Ok(()),
                None => bail!("{name} gave no identity for its running capture"),
            };
            match stopped {
                Ok(()) | Err(CommandError::Firmware(EnergyError::DmaError)) => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("stopping {name}'s capture"));
                }
            }
            let what = match (state.running, state.result) {
                (true, _) => format!("{name} stopped"),
                (false, Some(EnergyError::DmaError)) => {
                    format!("{name}'s capture had stopped already")
                }
                (false, _) => format!("{name} was not capturing"),
            };
            if !keeps_totals(pro) {
                outputln!("{what}.");
                return Ok(());
            }
            let total = pro
                .read_energy_accumulator()
                .with_context(|| format!("reading {name}'s total"))?;
            outputln!("{what}: {}.", describe_total(name, total)?);
            Ok(())
        }),
        EnergyCommand::Calibration => with_pro(bus, serial, energy, |name, pro| {
            for (range, label) in [
                (CalibrationRange::Low, "Low range"),
                (CalibrationRange::High, "High range"),
            ] {
                let calibration = pro
                    .read_energy_calibration(range)
                    .with_context(|| format!("reading {name}'s calibration"))?;
                let (points, failed) = match &calibration {
                    EnergyCalibration::Calibrated(points) => (points, false),
                    EnergyCalibration::Failed(points) => (points, true),
                    _ => bail!("{name} answered with a calibration this tool does not know"),
                };
                let points = points
                    .iter()
                    .map(|point| {
                        format!(
                            "code {} at {}",
                            point.code,
                            si(f64::from(point.current_nanoamps) * 1e-9, "A")
                        )
                    })
                    .collect::<Vec<_>>();
                let text = match (failed, points.is_empty()) {
                    (false, _) => points.join(", "),
                    (true, true) => "not calibrated: the last calibration failed before its first point, or none ran".to_owned(),
                    (true, false) => format!("the last calibration failed after {}", points.join(", ")),
                };
                outputln!("{label}: {text}.");
            }
            Ok(())
        }),
        EnergyCommand::Watch {
            rate,
            every,
            window,
            limit,
            range,
        } => with_pro(bus, serial, energy, |name, pro| {
            let options = watch::Options {
                rate: *rate,
                every: *every,
                window: *window,
                limit: *limit,
                range: range.range,
            };
            let ended = watch::watch(bus, name, pro, &options, cancel)?;
            let total = match ended.total {
                Some(total) => describe_total(name, total)?,
                None => "its firmware keeps no total".to_owned(),
            };
            if ended.running {
                outputln!("{name}: {total}. The capture keeps running.");
            } else {
                outputln!("{name} stopped: {total}.");
            }
            Ok(())
        }),
        EnergyCommand::Record {
            out,
            rate,
            limit,
            csv,
            tickwave,
            dio,
            range,
        } => {
            let flag = match (csv, tickwave) {
                (true, _) => Some((record::Format::Csv, "--csv")),
                (_, true) => Some((record::Format::Tickwave, "--tickwave")),
                _ => None,
            };
            let options = record::Options {
                out: out.clone(),
                rate: *rate,
                limit: *limit,
                format: record::Format::for_file(out, flag)?,
                software: None,
                dio: dio.iter().fold(0, |mask, pin| mask | pin),
                range: range.range,
            };
            record(bus, serial, &options, cancel)
        }
    }
}

fn record<B: Bus>(
    bus: &B,
    serial: Option<&str>,
    options: &record::Options,
    cancel: &Cancel,
) -> Result<()> {
    let unit = find_pro(bus, serial, Feature::Energy)?;
    let name = unit.serial().unwrap_or("the unit");
    if !unit.device.aux {
        bail!(
            "{name} runs a Verge build from before Verge Aux, which a recording needs: update it first"
        );
    }
    let link = match bus.open_aux(&unit.device) {
        Ok(link) => link,
        Err(BusError::Busy { .. }) => {
            bail!("another program holds {name}'s Verge Aux, which a recording needs")
        }
        Err(error) => return Err(error).with_context(|| format!("opening {name}'s Verge Aux")),
    };
    let mut aux = Aux::open(link).with_context(|| format!("opening {name}'s Verge Aux"))?;
    let mut pro = VergeControl::new(&mut aux);
    check_offered(name, &mut pro, Feature::Energy)?;
    let options = &record::Options {
        software: unit.verge().map(|ours| ours.firmware.clone()),
        ..options.clone()
    };
    let recorded = record::record(bus, name, &mut pro, options, cancel)?;
    // On stderr: stdout may be the CSV itself.
    let file = record::shown(&options.out);
    let format = match record::Format::named_by(&options.out) {
        None if !record::is_stdout(&options.out) => format!(" ({})", options.format.name()),
        _ => String::new(),
    };
    let energy = si(recorded.total.nanojoules() as f64 * 1e-9, "J");
    let why = match recorded.reason {
        EndReason::Asked => "the capture keeps running",
        EndReason::Stopped => "the capture stopped",
        EndReason::Restarted => "a new start restarted the capture",
        EndReason::Failed => {
            "the capture failed: the ADC overran, its DMA failed or the probe fell behind reading it"
        }
        EndReason::Other(_) => "the stream ended for a reason this tool does not know",
    };
    say(&format!(
        "{name}: {} pairs to {file}{format}, {energy}; {why}.",
        recorded.pairs
    ));
    if recorded.high_range_only {
        say("The capture held the high range.");
    }
    if recorded.under_range > 0 {
        say(&format!(
            "{} pairs were read under range, too near the high range's floor to trust: the file marks them.",
            recorded.under_range
        ));
    }
    if recorded.lost > 0 {
        say(&format!(
            "{} pairs were lost: the probe dropped messages this side did not read in time.",
            recorded.lost
        ));
    }
    let unreported = options.dio & !recorded.dio;
    if unreported != 0 {
        say(&format!(
            "{name} reported no edges on {}: J15 turns the DIO pins off, and SPI takes them.",
            crate::dio_names(unreported)
        ));
    }
    if recorded.dio != 0 {
        say(&edge_counts(recorded.dio, recorded.edges));
    }
    if recorded.edges_lost > 0 {
        say(&format!(
            "Edges were lost {}: the pins changed faster than the probe takes them, SPI had \
             them, or this side did not read in time. The levels during each loss are not known.",
            crate::count(recorded.edges_lost as usize, "time")
        ));
    }
    Ok(())
}

/// Each reported pin's edges, as "400 edges on DIO3. DIO0, DIO1, DIO2 did not change."
fn edge_counts(pins: u8, edges: [u64; 4]) -> String {
    let reported = (0..4).filter(|pin| pins >> pin & 1 != 0);
    let moved: Vec<String> = reported
        .clone()
        .filter(|&pin| edges[pin] > 0)
        .enumerate()
        .map(|(i, pin)| match (i, edges[pin]) {
            (0, 1) => format!("1 edge on DIO{pin}"),
            (0, n) => format!("{n} edges on DIO{pin}"),
            (_, n) => format!("{n} on DIO{pin}"),
        })
        .collect();
    let still = reported
        .filter(|&pin| edges[pin] == 0)
        .fold(0, |mask, pin| mask | 1 << pin);
    let mut sentences = Vec::new();
    if !moved.is_empty() {
        sentences.push(format!("{}.", moved.join(", ")));
    }
    if still != 0 {
        sentences.push(format!("{} did not change.", crate::dio_names(still)));
    }
    sentences.join(" ")
}

/// The total as a clause, as "1.71 mJ over 1000 pairs since the start".
fn describe_total(name: &str, total: EnergyTotal) -> Result<String> {
    let energy = |total: EnergyAccumulator| {
        format!(
            "{} over {} pairs",
            si(total.energy_nanojoules as f64 * 1e-9, "J"),
            total.integrated_pairs
        )
    };
    Ok(match total {
        EnergyTotal::Total(total) => format!("{} since the start", energy(total)),
        EnergyTotal::Failed(total) => format!(
            "the capture failed after {}, when the ADC overran, its DMA failed or the probe fell behind reading it",
            energy(total)
        ),
        EnergyTotal::NoSample => "no calibrated pair yet".to_owned(),
        EnergyTotal::Stopped => "no capture since the probe started".to_owned(),
        _ => bail!("{name} answered with a total this tool does not know"),
    })
}

/// The firmware's range IDs, which name the current range and the jumper setting
/// (host-interface.md, "The range is 1 for the low range in the 350 mA setting").
pub(crate) fn range_name(id: u8) -> &'static str {
    match id {
        1 => "low range, 350 mA setting",
        2 => "high range, 350 mA setting",
        3 => "low range, 50 mA setting",
        4 => "high range, 50 mA setting",
        _ => "range unknown",
    }
}

/// `value` in `unit` with an SI prefix and three significant figures, as 20.0 µA or 1.71 mJ.
pub(crate) fn si(value: f64, unit: &str) -> String {
    const PREFIXES: [(f64, &str); 4] = [(1e-9, "n"), (1e-6, "µ"), (1e-3, "m"), (1.0, "")];
    if value == 0.0 {
        return format!("0 {unit}");
    }
    let magnitude = value.abs();
    let mut index = PREFIXES
        .iter()
        .rposition(|(scale, _)| magnitude >= *scale)
        .unwrap_or(0);
    // 999.96 µA would print as 1000 µA; take it to the next prefix instead.
    if magnitude / PREFIXES[index].0 >= 999.5 && index + 1 < PREFIXES.len() {
        index += 1;
    }
    let (scale, prefix) = PREFIXES[index];
    let scaled = value / scale;
    let decimals = match scaled.abs() {
        x if x >= 99.95 => 0,
        x if x >= 9.995 => 1,
        _ => 2,
    };
    format!("{scaled:.decimals$} {prefix}{unit}")
}

#[cfg(test)]
mod tests {
    use super::{EnergyCommand, RangeArg, Ranges, edge_counts, energy, si};
    use escapement_verge_probe::stream::{Header, Message};
    use escapement_verge_probe::{Bus, Cancel, VergeControl};
    use std::time::Duration;

    /// Starts a capture on a simulated Pro with `energy start`, after another host asked for the
    /// high range, and returns the header of a stream of it.
    fn start(range: Option<Ranges>) -> Header {
        let bus = escapement_verge_probe::sim::SimBus::scenario("ours-pro").unwrap();
        let device = escapement_verge_probe::detect(&bus)
            .unwrap()
            .remove(0)
            .device;
        let mut aux =
            escapement_verge_probe::aux::Aux::open(bus.open_aux(&device).unwrap()).unwrap();
        VergeControl::new(&mut aux)
            .set_energy_ranges(escapement_verge_probe::EnergyRanges::High)
            .unwrap();
        drop(aux);
        let command = EnergyCommand::Start {
            rate: 1000,
            range: RangeArg { range },
        };
        energy(&bus, &command, None, &Cancel::new()).unwrap();
        let mut aux =
            escapement_verge_probe::aux::Aux::open(bus.open_aux(&device).unwrap()).unwrap();
        VergeControl::new(&mut aux)
            .stream_energy_capture_identified(0)
            .unwrap();
        loop {
            match aux.message(Duration::from_secs(1)).unwrap() {
                Some(Message::Header(header)) => return header,
                Some(_) => {}
                None => panic!("no header"),
            }
        }
    }

    #[test]
    fn edge_counts_name_each_pins_own() {
        assert_eq!(
            edge_counts(0x0F, [0, 0, 0, 400]),
            "400 edges on DIO3. DIO0, DIO1, DIO2 did not change."
        );
        assert_eq!(
            edge_counts(0x09, [3, 7, 0, 400]),
            "3 edges on DIO0, 400 on DIO3."
        );
        assert_eq!(edge_counts(0x08, [0, 0, 0, 1]), "1 edge on DIO3.");
        assert_eq!(edge_counts(0x05, [0; 4]), "DIO0, DIO2 did not change.");
    }

    /// `energy start` asks for its range every time, so an earlier request by another host does
    /// not decide it.
    #[test]
    fn a_start_uses_the_range_it_was_given() {
        assert!(start(Some(Ranges::High)).high_range_only);
        assert!(!start(Some(Ranges::Auto)).high_range_only);
        assert!(!start(None).high_range_only);
    }

    #[test]
    fn si_keeps_three_significant_figures_across_prefixes() {
        assert_eq!(si(20e-6, "A"), "20.0 µA");
        assert_eq!(si(5e-3, "A"), "5.00 mA");
        assert_eq!(si(1.7094e-3, "J"), "1.71 mJ");
        assert_eq!(si(3.3, "V"), "3.30 V");
        assert_eq!(si(999.96e-6, "A"), "1.00 mA");
        assert_eq!(si(12e-12, "A"), "0.01 nA");
        assert_eq!(si(-250e-9, "A"), "-250 nA");
        assert_eq!(si(1500.0, "J"), "1500 J");
        assert_eq!(si(0.0, "A"), "0 A");
    }
}
