//! `verge dio record`: the DIO pins of a board without energy capture (a standalone MCU-Link, or a
//! Pro without its energy circuit), from its DIO stream on Verge Aux (`0x90` mode 15), to a
//! tickwave recording. Any other Pro records its pins beside its energy capture
//! (`verge energy record --dio`).

use std::path::PathBuf;
use std::time::Duration;

use crate::energy::record::Writer;
use crate::output::outputln;
use anyhow::{Context, Result, bail};
use clap::Args;
use escapement_verge_probe::aux::{Aux, AuxLink};
use escapement_verge_probe::pro::{self, Feature};
use escapement_verge_probe::stream::{EndReason, Message};
use escapement_verge_probe::tickwave::{Context as Recording, FileWriter};
use escapement_verge_probe::{BoardKind, Bus, Cancel, VergeControl};

#[derive(Args)]
pub struct RecordArgs {
    /// The tickwave recording to write.
    #[arg(long, value_name = "FILE")]
    out: PathBuf,
    /// Stop after this long. Without it, Ctrl-C ends the recording.
    #[arg(long = "for", value_name = "DURATION", value_parser = humantime::parse_duration)]
    limit: Option<Duration>,
    /// The pins to record: 0 to 3, as `--pins 0,3`, or `dio0` to `dio3`, or `all`, the default.
    #[arg(long, value_name = "PINS", value_delimiter = ',', value_parser = crate::parse_dio_pins)]
    pins: Vec<u8>,
}

/// How long a read waits before the limit and Ctrl-C are checked again.
const SLICE: Duration = Duration::from_millis(50);
/// How long the header, and the end once asked for, may take to come.
const WAIT: Duration = Duration::from_secs(2);

pub fn record<B: Bus>(
    bus: &B,
    args: &RecordArgs,
    serial: Option<&str>,
    cancel: &Cancel,
) -> Result<()> {
    // A Pro that measures energy records its pins with it, so it is no pick here.
    let unit = crate::energy::find_pro_where(bus, serial, Feature::Dio, |unit| {
        unit.verge().is_some_and(|ours| ours.supports_dio_stream())
    })?;
    let name = unit.serial().unwrap_or("the unit");
    let mut aux = pro::open_aux(bus, &unit, Feature::Dio)?;
    let mut probe = VergeControl::new(&mut aux);
    let info = probe
        .info()
        .with_context(|| format!("asking {name} what it runs"))?
        .with_context(|| format!("{name} no longer answers as Verge"))?
        .clone();
    if !info.supports_dio_stream() {
        if info.supports_energy_capture() {
            bail!(
                "{name} records its DIO pins beside its energy capture: use `verge energy record --dio`"
            );
        }
        bail!(
            "{name} runs build {}, which predates DIO recording: update it first",
            info.firmware
        );
    }
    let asked = match args.pins.iter().fold(0, |mask, pin| mask | pin) {
        0 => 0x0F,
        mask => mask,
    };
    // The probe answers before the file is touched, so a refused start leaves an earlier recording
    // there as it was.
    probe
        .start_dio_stream(asked)
        .with_context(|| format!("starting {name}'s DIO stream"))?;
    let instrument = match info.board {
        Some(BoardKind::Pro) => "MCU-Link Pro",
        _ => "MCU-Link",
    };
    let recording = Recording {
        instrument: Some(instrument.into()),
        software: Some(info.firmware.clone()),
        started_utc: Some(
            humantime::format_rfc3339_millis(std::time::SystemTime::now()).to_string(),
        ),
    };
    let writer = match FileWriter::create(&args.out, recording) {
        Ok(file) => Writer::tickwave(file),
        Err(error) => {
            if let Err(ended) = probe.end_dio_stream() {
                tracing::warn!("could not end {name}'s DIO stream: {ended}");
            }
            return Err(error).with_context(|| format!("creating {}", args.out.display()));
        }
    };
    let board = info.board;
    let streamed = stream(
        bus, name, board, &mut probe, &writer, asked, args.limit, cancel,
    );
    let written = writer
        .finish()
        .with_context(|| format!("writing {}", args.out.display()));
    let recorded = match (streamed, written) {
        (Ok(recorded), Ok(())) => recorded,
        (_, Err(error)) | (Err(error), Ok(())) => {
            if let Err(ended) = probe.end_dio_stream() {
                tracing::warn!("could not end {name}'s DIO stream: {ended}");
            }
            return Err(error);
        }
    };
    let pins: Vec<String> = (0..4)
        .filter(|k| recorded.pins & (1 << k) != 0)
        .map(|k| format!("DIO{k} {}", count(recorded.edges[k], "edge")))
        .collect();
    outputln!(
        "{name}: {} over {:.3} s to {}.",
        pins.join(", "),
        recorded.seconds,
        args.out.display()
    );
    if recorded.losses > 0 {
        outputln!(
            "The probe lost edges {}: the recording marks those spans unknown.",
            count(recorded.losses, "time")
        );
    }
    if recorded.reason != EndReason::Asked {
        outputln!("The stream ended before this side asked: another program restarted it.");
    }
    Ok(())
}

fn count(n: u64, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

struct Recorded {
    /// The pins the probe reported, and each one's edges, DIO0 first.
    pins: u8,
    edges: [u64; 4],
    /// How often the probe lost edges.
    losses: u64,
    seconds: f64,
    reason: EndReason,
}

/// Reads the stream into `writer` until it ends, asking it to end at the limit or Ctrl-C.
#[allow(clippy::too_many_arguments)]
fn stream<B: Bus, L: AuxLink>(
    bus: &B,
    name: &str,
    board: Option<BoardKind>,
    probe: &mut VergeControl<'_, Aux<L>>,
    writer: &Writer,
    asked: u8,
    limit: Option<Duration>,
    cancel: &Cancel,
) -> Result<Recorded> {
    let began = bus.now();
    let deadline = limit.map(|limit| began + limit);
    let mut header = None;
    let mut asked_at = None;
    let (mut levels, mut edges, mut losses) = (0u8, [0u64; 4], 0u64);
    loop {
        if header.is_none() && bus.now().saturating_sub(began) >= WAIT {
            bail!("{name} did not send its DIO stream's header");
        }
        if asked_at.is_none()
            && header.is_some()
            && (cancel.is_cancelled() || deadline.is_some_and(|deadline| bus.now() >= deadline))
        {
            probe
                .end_dio_stream()
                .with_context(|| format!("ending {name}'s DIO stream"))?;
            asked_at = Some(bus.now());
        }
        let message = probe
            .transport()
            .message(SLICE)
            .with_context(|| format!("reading {name}'s DIO stream"))?;
        let Some(message) = message else {
            if asked_at.is_some_and(|asked| bus.now().saturating_sub(asked) > WAIT) {
                bail!("{name} did not end its DIO stream when asked");
            }
            continue;
        };
        // What came before the header was a stream this one replaced.
        if header.is_none() && !matches!(message, Message::DioHeader(_)) {
            continue;
        }
        // Before the writer sees it: a message the recording can't take ends it with this reason,
        // and with no header written the writer leaves no file.
        if let Message::Header(_) | Message::Pairs(_) = message {
            bail!("{name} streamed energy pairs, not its DIO pins");
        }
        if let Message::DioHeader(started) = &message {
            if header.is_some() {
                bail!("{name} began a second DIO stream before ending the first");
            }
            if started.dio_mask == 0 {
                // A Pro reports none while an SPI transfer has them; the standalone MCU-Link's
                // only take is a bench tool's command marker.
                if board == Some(BoardKind::Pro) {
                    bail!(
                        "{name} reports none of the pins asked for: an SPI transfer had them as the stream started. Record again once nothing is using SPI"
                    );
                }
                bail!(
                    "{name} reports none of the pins asked for: a bench tool has given them to its command marker. Run `verge restart` to give them back"
                );
            }
            if started.dio_mask != asked {
                tracing::warn!(
                    "{name} reports {} of the pins asked for: a bench tool holds the rest",
                    crate::dio_names(started.dio_mask)
                );
            }
            levels = started.dio_levels;
            header = Some(*started);
        }
        writer.message(&message)?;
        match message {
            Message::DioHeader(_) => {}
            Message::Edges(message) => {
                let mask = header.map_or(0, |header| header.dio_mask);
                for edge in &message.edges {
                    let moved = (levels ^ edge.levels) & mask;
                    for (pin, count) in edges.iter_mut().enumerate() {
                        *count += u64::from(moved >> pin & 1);
                    }
                    levels = edge.levels;
                }
            }
            Message::DioLost(lost) => {
                losses += 1;
                levels = lost.levels;
            }
            Message::End(end) => {
                return Ok(Recorded {
                    pins: header.map_or(0, |header| header.dio_mask),
                    edges,
                    losses,
                    seconds: end.time_ns.unwrap_or(0) as f64 * 1e-9,
                    reason: end.reason,
                });
            }
            // A DIO stream carries no markers: 0x94 records them beside energy capture alone.
            // Pairs were refused above. A DIO time tells the writer how far the stream has got.
            Message::Markers(_) | Message::Header(_) | Message::Pairs(_) | Message::DioTime(_) => {}
        }
    }
}
