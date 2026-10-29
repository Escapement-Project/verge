//! `verge power` and `verge energy`: an MCU-Link Pro's target supply and its energy measurement.

mod watch;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use tracing_indicatif::indicatif_println;
use verge_probe::detect::{State, detect_named};
use verge_probe::aux::Aux;
use verge_probe::bus::BusError;
use verge_probe::{
    Bus, CalibrationRange, Cancel, CommandError, DapTransport, EnergyAccumulator,
    EnergyCalibration, EnergyCaptureSample, EnergyError, EnergyTotal, Firmware, VergeControl,
    VergeInfo, VergeTransport, dap,
};

#[derive(Args)]
pub struct PowerArgs {
    /// Turn the supply on or off. Left out, the tool reads it.
    #[arg(value_enum)]
    state: Option<Switch>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Switch {
    On,
    Off,
}

#[derive(Subcommand)]
pub enum EnergyCommand {
    /// Start a capture. It runs in the probe until `stop`, the next start or a restart, so a
    /// debugger can use the probe meanwhile.
    ///
    /// A start clears the total. It can calibrate first, which disconnects J9 from the target for
    /// about 0.2 s: a target powered through J9 loses its supply for that time.
    Start {
        /// Voltage and current pairs a second, 1 to 100,000.
        #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..=100_000))]
        rate: u32,
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
    /// Where the output is not a terminal it prints a line a read instead. The current graph
    /// plots the newest pair at each read, so it can miss short peaks; the power graph plots each
    /// interval's mean from the total, which counts every pair.
    Watch {
        /// Voltage and current pairs a second, for a capture this starts.
        #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..=100_000))]
        rate: u32,
        /// How often to read the probe, as 100ms or 1s.
        #[arg(long, default_value = "100ms", value_parser = humantime::parse_duration)]
        every: Duration,
        /// How far back the graphs reach.
        #[arg(long, default_value = "30s", value_parser = humantime::parse_duration)]
        window: Duration,
        /// Leave after this long instead of waiting for a key.
        #[arg(long = "for", value_name = "DURATION", value_parser = humantime::parse_duration)]
        limit: Option<Duration>,
    },
}

/// A Pro over Verge Aux where it has one, so a debugger can hold CMSIS-DAP meanwhile, or over
/// CMSIS-DAP.
type Pro<'a> = VergeControl<'a, dyn VergeTransport + 'a>;

/// What a Pro offers that these commands reach.
#[derive(Clone, Copy)]
enum Feature {
    Supply,
    Energy,
}

impl Feature {
    fn name(self) -> &'static str {
        match self {
            Feature::Supply => "target supply",
            Feature::Energy => "energy measurement",
        }
    }

    fn offered(self, info: &VergeInfo) -> bool {
        match self {
            Feature::Supply => info.supports_target_power(),
            Feature::Energy => info.supports_energy_capture(),
        }
    }

    /// Why a unit running Verge does not offer it.
    fn missing(self, name: &str, info: &VergeInfo) -> String {
        match self {
            // Firmware that reports extended capabilities (format 6) offers energy capture on
            // every Pro whose HW_VER_5 strap is low (mculink-verge PRO.md).
            Feature::Energy if info.version >= 6 => format!(
                "{name} reports no energy measurement: its HW_VER_5 strap says the circuit is not fitted"
            ),
            _ => format!(
                "{name} runs build {}, which predates its {}: update it first",
                info.firmware,
                self.name()
            ),
        }
    }
}

/// Runs `f` on the Pro `serial` names, or the one [`crate::pick_unit`] finds, refusing anything
/// else in a sentence that says why.
fn with_pro<B: Bus, T>(
    bus: &B,
    serial: Option<&str>,
    feature: Feature,
    f: impl FnOnce(&str, &mut Pro<'_>) -> Result<T>,
) -> Result<T> {
    let what = feature.name();
    let units = detect_named(bus, serial).context("looking for MCU-Links")?;
    let unit = crate::pick_unit(&units, serial, crate::pro_running_ours)?;
    let name = unit.serial().unwrap_or("the unit");
    if let State::Unreadable { reason, .. } = &unit.state {
        bail!("the tool could not ask {name} what it runs: {reason}");
    }
    let State::Probe {
        firmware: Firmware::Ours(ours),
        ..
    } = &unit.state
    else {
        bail!(
            "{name} is {}, not running Verge, so the tool cannot reach its {what}",
            unit.state
        );
    };
    if let Some(board) = ours.board.map(verge_probe::BoardKind::number)
        && board != dap::Ours::BOARD_MCU_LINK_PRO
    {
        bail!("{name} is not an MCU-Link Pro, so it has no {what}");
    }
    if unit.device.aux {
        match bus.open_aux(&unit.device) {
            Ok(link) => {
                let mut aux = Aux::open(link).with_context(|| format!("opening {name}'s Verge Aux"))?;
                return run(name, &mut aux, feature, f);
            }
            Err(BusError::Busy { .. }) => {
                tracing::info!("another program holds {name}'s Verge Aux, so the tool uses CMSIS-DAP");
            }
            Err(error) => return Err(error).with_context(|| format!("opening {name}'s Verge Aux")),
        }
    }
    let mut link = bus
        .open_dap(&unit.device)
        .with_context(|| format!("opening {name}"))?;
    let mut transport = DapTransport(&mut link);
    run(name, &mut transport, feature, f)
}

fn run<T>(
    name: &str,
    transport: &mut dyn VergeTransport,
    feature: Feature,
    f: impl FnOnce(&str, &mut Pro<'_>) -> Result<T>,
) -> Result<T> {
    let mut pro = VergeControl::new(transport);
    let info = pro
        .info()
        .with_context(|| format!("asking {name} what it runs"))?
        .with_context(|| format!("{name} no longer answers as Verge"))?;
    if !feature.offered(info) {
        bail!("{}", feature.missing(name, info));
    }
    f(name, &mut pro)
}

/// Whether the firmware keeps a running total.
fn keeps_totals(pro: &mut Pro<'_>) -> bool {
    pro.info()
        .ok()
        .flatten()
        .is_some_and(VergeInfo::supports_energy_accumulator)
}

pub fn power<B: Bus>(bus: &B, args: &PowerArgs, serial: Option<&str>) -> Result<()> {
    with_pro(bus, serial, Feature::Supply, |name, pro| {
        let on = match args.state {
            None => pro.read_target_power(),
            Some(state) => pro.set_target_power(matches!(state, Switch::On)),
        }
        .with_context(|| format!("reaching {name}'s target supply"))?;
        indicatif_println!(
            "{name}'s target supply is {}.",
            if on { "on" } else { "off" }
        );
        Ok(())
    })
}

pub fn energy<B: Bus>(
    bus: &B,
    command: &EnergyCommand,
    serial: Option<&str>,
    cancel: &Cancel,
) -> Result<()> {
    let energy = Feature::Energy;
    match command {
        EnergyCommand::Start { rate } => with_pro(bus, serial, energy, |name, pro| {
            let start = pro
                .start_energy_capture(*rate)
                .with_context(|| format!("starting {name}'s capture"))?;
            let actual = start.actual_rate_pairs_per_second;
            let asked = if actual == *rate {
                String::new()
            } else {
                format!(" (asked for {rate})")
            };
            indicatif_println!("{name} captures {actual} pairs a second{asked}.");
            Ok(())
        }),
        EnergyCommand::Latest => with_pro(bus, serial, energy, |name, pro| {
            let latest = match pro.latest_energy_sample() {
                Err(CommandError::Firmware(EnergyError::DmaError)) => {
                    indicatif_println!(
                        "{name}'s capture failed: the ADC overran or its DMA failed. A new start clears the fault."
                    );
                    return Ok(());
                }
                latest => latest.with_context(|| format!("reading {name}'s newest pair"))?,
            };
            match latest {
                EnergyCaptureSample::Sample(sample) => match sample.calibrated {
                    Some(calibrated) => indicatif_println!(
                        "{name}: {}, {} ({}), pair {}.",
                        si(f64::from(calibrated.voltage_millivolts) * 1e-3, "V"),
                        si(f64::from(calibrated.current_nanoamps) * 1e-9, "A"),
                        range_name(calibrated.range_id),
                        sample.raw.sequence
                    ),
                    None => indicatif_println!(
                        "{name}: voltage code {}, current code {}, pair {}, not calibrated.",
                        sample.raw.voltage_code,
                        sample.raw.current_code,
                        sample.raw.sequence
                    ),
                },
                EnergyCaptureSample::NoSample => {
                    indicatif_println!("{name}'s capture has no complete pair yet.");
                }
                EnergyCaptureSample::Stopped => {
                    indicatif_println!("{name} is not capturing: start a capture first.");
                }
                _ => bail!("{name} answered with a reading this tool does not know"),
            }
            Ok(())
        }),
        EnergyCommand::Total => with_pro(bus, serial, energy, |name, pro| {
            let total = pro
                .read_energy_accumulator()
                .with_context(|| format!("reading {name}'s total"))?;
            indicatif_println!("{name}: {}.", describe_total(name, total)?);
            Ok(())
        }),
        EnergyCommand::Stop => with_pro(bus, serial, energy, |name, pro| {
            // After a fault the stop still stops, and answers with the fault.
            match pro.stop_energy_capture() {
                Ok(()) | Err(CommandError::Firmware(EnergyError::DmaError)) => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("stopping {name}'s capture"));
                }
            }
            if !keeps_totals(pro) {
                indicatif_println!("{name} stopped.");
                return Ok(());
            }
            let total = pro
                .read_energy_accumulator()
                .with_context(|| format!("reading {name}'s total"))?;
            indicatif_println!("{name} stopped: {}.", describe_total(name, total)?);
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
                indicatif_println!("{label}: {text}.");
            }
            Ok(())
        }),
        EnergyCommand::Watch {
            rate,
            every,
            window,
            limit,
        } => with_pro(bus, serial, energy, |name, pro| {
            let options = watch::Options {
                rate: *rate,
                every: *every,
                window: *window,
                limit: *limit,
            };
            let ended = watch::watch(bus, name, pro, &options, cancel)?;
            let total = match ended.total {
                Some(total) => describe_total(name, total)?,
                None => "its firmware keeps no total".to_owned(),
            };
            if ended.running {
                indicatif_println!("{name}: {total}. The capture keeps running.");
            } else {
                indicatif_println!("{name} stopped: {total}.");
            }
            Ok(())
        }),
    }
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
            "the capture failed after {}, when the ADC overran or its DMA failed",
            energy(total)
        ),
        EnergyTotal::NoSample => "no calibrated pair yet".to_owned(),
        EnergyTotal::Stopped => "no capture since the probe started".to_owned(),
        _ => bail!("{name} answered with a total this tool does not know"),
    })
}

/// The firmware's range IDs, which name the jumper setting and the gain.
pub(crate) fn range_name(id: u8) -> &'static str {
    match id {
        1 => "350 mA, high gain",
        2 => "350 mA, low gain",
        3 => "50 mA, high gain",
        4 => "50 mA, low gain",
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
    use super::si;

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
