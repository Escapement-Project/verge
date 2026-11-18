//! `verge dio record`: the DIO pins of a board without energy capture (a standalone MCU-Link, or a
//! Pro without its energy circuit), from its DIO stream on Verge Aux (`0x90` mode 15), to a
//! tickwave recording. Any other Pro records its pins beside its energy capture
//! (`verge energy record --dio`).

use std::path::PathBuf;
use std::time::Duration;

use crate::energy::record::is_stdout;
use crate::progress::Spinner;
use crate::question::say;
use anyhow::{Context, Result, bail};
use clap::Args;
use escapement_verge_probe::aux::AuxLink;
use escapement_verge_probe::dio;
use escapement_verge_probe::pro::{self, Feature};
use escapement_verge_probe::record::{Format, Writer};
use escapement_verge_probe::stream::{EndReason, Message, Reader};
use escapement_verge_probe::{BoardKind, Bus, Cancel};

#[derive(Args)]
pub struct RecordArgs {
    /// The tickwave recording to write. A .sr or .csv name is refused.
    #[arg(long, value_name = "FILE")]
    out: PathBuf,
    /// Stop after this long. Without it, Ctrl-C ends the recording.
    #[arg(long = "for", value_name = "DURATION", value_parser = humantime::parse_duration)]
    limit: Option<Duration>,
    /// The pins to record: 0 to 3, as `--pins 0,3`, or `dio0` to `dio3`, or `all`, the default.
    #[arg(long, value_name = "PINS", value_delimiter = ',', value_parser = dio::parse_pin)]
    pins: Vec<u8>,
}

pub fn record<B: Bus>(
    bus: &B,
    args: &RecordArgs,
    serial: Option<&str>,
    cancel: &Cancel,
) -> Result<()> {
    // Before any unit is touched. It writes only tickwave, which a name may not contradict.
    match Format::named_by(&args.out) {
        Some(Format::Tickwave) | None if !is_stdout(&args.out) => {}
        Some(named) => bail!(
            "{} names {}, and `dio record` writes a tickwave recording: name it .tickwave",
            args.out.display(),
            named.name()
        ),
        None => bail!(
            "`dio record` writes a tickwave recording, which cannot go to stdout: name a file"
        ),
    }
    // A Pro that measures energy records its pins with it, so it is no pick here.
    let unit = crate::energy::find_pro_where(
        bus,
        serial,
        Feature::Dio,
        |unit| unit.verge().is_some_and(|ours| ours.supports_dio_stream()),
        "records its DIO pins on their own",
        "`verge dio record` needs a standalone MCU-Link, or a Pro without its energy circuit, running Verge.",
    )?;
    let name = unit.serial().unwrap_or("the unit");
    let mut reader = Reader::new(pro::open_aux(bus, &unit, Feature::Dio)?);
    let info = reader
        .control()
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
    reader
        .start_dio(asked)
        .with_context(|| format!("starting {name}'s DIO stream"))?;
    let mut writer = match Writer::create(&args.out, Format::Tickwave, reader.context()) {
        Ok(writer) => writer,
        Err(error) => {
            leave(name, &mut reader);
            return Err(error).with_context(|| format!("creating {}", args.out.display()));
        }
    };
    let streamed = stream(
        bus,
        name,
        info.board,
        &mut reader,
        &mut writer,
        asked,
        args,
        cancel,
    );
    let written = writer
        .finish()
        .with_context(|| format!("writing {}", args.out.display()));
    let recorded = match (streamed, written) {
        (Ok(recorded), Ok(())) => recorded,
        (_, Err(error)) | (Err(error), Ok(())) => {
            leave(name, &mut reader);
            return Err(error);
        }
    };
    let pins: Vec<String> = (0..4)
        .filter(|k| recorded.pins & (1 << k) != 0)
        .map(|k| format!("DIO{k} {}", count(recorded.edges[k], "edge")))
        .collect();
    let format = match Format::named_by(&args.out) {
        None => " (a tickwave recording)",
        Some(_) => "",
    };
    say(&format!(
        "{name}: {} over {:.3} s to {}{format}.",
        pins.join(", "),
        recorded.seconds,
        args.out.display()
    ));
    if recorded.losses > 0 {
        say(&format!(
            "The probe lost edges {}: the recording marks those spans unknown.",
            count(recorded.losses, "time")
        ));
    }
    if recorded.reason != EndReason::Asked {
        say("The stream ended before this side asked: another program restarted it.");
    }
    Ok(())
}

/// Ends the DIO stream, as the recording found the probe without one.
fn leave<L: AuxLink>(name: &str, reader: &mut Reader<L>) {
    if let Err(error) = reader.abandon() {
        tracing::warn!("could not end {name}'s DIO stream: {error}");
    }
}

fn count(n: u64, what: &str) -> String {
    crate::count(usize::try_from(n).unwrap_or(usize::MAX), what)
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
    reader: &mut Reader<L>,
    writer: &mut Writer,
    asked: u8,
    args: &RecordArgs,
    cancel: &Cancel,
) -> Result<Recorded> {
    let limit = args.limit;
    let deadline = limit.map(|limit| bus.now() + limit);
    let mut spinner: Option<Spinner> = None;
    let mut messages =
        reader.messages(|| cancel.is_cancelled() || deadline.is_some_and(|at| bus.now() >= at));
    while let Some(message) = messages.next() {
        let message = message.map_err(|error| {
            anyhow::Error::new(error).context(format!("recording {name}'s DIO pins"))
        })?;
        match &message {
            // Before the writer sees it: with no header written, the writer leaves no file.
            Message::DioHeader(started) => {
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
                        dio::names(started.dio_mask)
                    );
                }
                let ends = match limit {
                    Some(limit) => format!(
                        ", for {}. Ctrl-C ends it sooner",
                        humantime::format_duration(limit)
                    ),
                    None => ". Ctrl-C ends it".to_owned(),
                };
                say(&format!(
                    "Recording {name}'s {} to {}{ends}.",
                    dio::names(started.dio_mask),
                    args.out.display()
                ));
                spinner = Some(Spinner::new("recording"));
            }
            Message::Edges(_) => {
                if let Some(spinner) = &spinner {
                    let counts = messages.reader().counts();
                    let shown: Vec<String> = (0..4)
                        .filter(|pin| counts.pins >> pin & 1 != 0)
                        .map(|pin| format!("DIO{pin} {}", count(counts.edges[pin], "edge")))
                        .collect();
                    spinner.set(&format!("recording  {}", shown.join("  ")));
                }
            }
            _ => {}
        }
        writer.send(message)?;
    }
    drop(spinner);
    let counts = reader.counts();
    let end = counts
        .end
        .with_context(|| format!("{name}'s DIO stream stopped before its end"))?;
    Ok(Recorded {
        pins: counts.pins,
        edges: counts.edges,
        losses: counts.edge_losses,
        seconds: end.time_ns.unwrap_or(0) as f64 * 1e-9,
        reason: end.reason,
    })
}
