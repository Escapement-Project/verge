//! `verge energy record`: every pair of a capture, from the Pro's stream on Verge Aux, to a
//! tickwave recording, a sigrok session or a CSV file, with the edges of the DIO pins it asks for.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::progress::Spinner;
use crate::question::say;
use anyhow::{Context, Result, bail};
use escapement_verge_probe::aux::AuxLink;
use escapement_verge_probe::record::{Format, Writer};
use escapement_verge_probe::stream::{
    Began, EndReason, EnergyStream, Message, Reader, ReaderError, Total,
};
use escapement_verge_probe::{Bus, Cancel};

/// What a recording to `out` writes: what its extension names, else what `flag` asks for, else a
/// tickwave recording. `flag` is the format option given, by its name. A flag the extension
/// contradicts is refused, and so is anything but CSV to stdout.
pub fn format_for(out: &Path, flag: Option<(Format, &str)>) -> Result<Format> {
    let format = match (Format::named_by(out), flag) {
        (Some(named), Some((asked, flag))) if named != asked => bail!(
            "{flag} writes {}, and {} names {}: name the file .{}, or leave {flag} out",
            asked.name(),
            out.display(),
            named.name(),
            asked.extension()
        ),
        (Some(named), _) => named,
        (None, Some((asked, _))) => asked,
        (None, None) => Format::Tickwave,
    };
    if is_stdout(out) && format != Format::Csv {
        bail!("--out - writes to stdout, which takes CSV only: add --csv");
    }
    Ok(format)
}

/// Whether `out` names stdout.
pub fn is_stdout(out: &Path) -> bool {
    out.as_os_str() == "-"
}

/// `out` as the summary names it.
pub fn shown(out: &Path) -> String {
    if is_stdout(out) {
        "stdout".to_owned()
    } else {
        out.display().to_string()
    }
}

/// stdout as a file the CSV writer can take.
fn stdout_file() -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    let owned = std::os::fd::AsFd::as_fd(&std::io::stdout()).try_clone_to_owned()?;
    #[cfg(windows)]
    let owned =
        std::os::windows::io::AsHandle::as_handle(&std::io::stdout()).try_clone_to_owned()?;
    Ok(std::fs::File::from(owned))
}

#[derive(Clone)]
pub struct Options {
    pub out: PathBuf,
    pub rate: u32,
    pub limit: Option<Duration>,
    pub format: Format,
    /// The DIO pins whose edges to record, bit `k` for DIO`k`.
    pub dio: u8,
    /// The current ranges for a capture this starts; automatic unless given.
    pub range: Option<super::Ranges>,
}

pub struct Recorded {
    pub total: Total,
    /// Pairs the sequence skipped: messages the probe dropped because nobody read them in time.
    pub lost: u64,
    pub reason: EndReason,
    /// The DIO pins the probe reported, each one's edges (DIO0 first), and how often it lost
    /// some.
    pub dio: u8,
    pub edges: [u64; 4],
    pub edges_lost: u64,
    /// The capture held the high range, as its header says.
    pub high_range_only: bool,
    /// Pairs read under range (Sample::under_range).
    pub under_range: u64,
}

/// A reader's error, in the words of the other energy commands: an old build's refusal named as
/// such, and a start whose reply was lost marked uncertain.
pub(crate) fn reader_error<L: AuxLink>(
    reader: &mut Reader<L>,
    name: &str,
    error: ReaderError,
) -> anyhow::Error {
    match error {
        ReaderError::Start(error) => super::energy_error(
            &mut reader.control(),
            name,
            &format!("starting {name}'s capture"),
            error,
        ),
        ReaderError::Join(error) => super::energy_error(
            &mut reader.control(),
            name,
            &format!("streaming {name}'s capture"),
            error,
        ),
        error => anyhow::Error::new(error).context(format!("recording {name}")),
    }
}

/// Records the capture that runs, or starts one at `options.rate`, until the limit, Ctrl-C or the
/// capture's end. A capture this started is stopped at the end; one that ran before is left running.
pub fn record<B: Bus, L: AuxLink>(
    bus: &B,
    name: &str,
    reader: &mut Reader<L>,
    options: &Options,
    cancel: &Cancel,
) -> Result<Recorded> {
    let asked = EnergyStream::new(options.rate)
        .ranges(options.range.unwrap_or_default().into())
        .dio(options.dio);
    // The probe answers before the file is touched, so a refused start leaves an earlier
    // recording there as it was.
    let began = reader
        .start_energy(&asked)
        .map_err(|error| reader_error(reader, name, error))?;
    match began {
        Began::Joined if options.range.is_some() => tracing::warn!(
            "{name} already runs a capture, which keeps its current ranges: --range is for a capture this starts"
        ),
        // Only a capture that failed is replaced: a running one is joined.
        Began::Started {
            replaced: Some(total),
            ..
        } => say(&format!(
            "{name}: {}. A new capture replaces it.",
            super::describe_total(name, total)?
        )),
        _ => {}
    }
    let created = if options.format == Format::Csv && is_stdout(&options.out) {
        stdout_file()
            .context("opening stdout")
            .and_then(|file| Ok(Writer::csv(file, &options.out)?))
    } else {
        Writer::create(&options.out, options.format, reader.context()).map_err(Into::into)
    };
    let mut writer = match created {
        Ok(writer) => writer,
        Err(error) => {
            leave(name, reader);
            return Err(error);
        }
    };
    let streamed = stream(bus, name, reader, &mut writer, options, cancel);
    let written = writer.finish();
    // A reader that closes the pipe early ends a recording to stdout, as it ends any other output.
    if is_stdout(&options.out)
        && let Err(error) = &written
        && error.is_broken_pipe()
    {
        leave(name, reader);
        let closed = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        return Err(crate::output::Error::from(closed).into());
    }
    let written = written.with_context(|| format!("writing {}", shown(&options.out)));
    match (streamed, written) {
        (Ok(recorded), Ok(())) => Ok(recorded),
        // The writer's own error says why the stream could not hand it pairs.
        (_, Err(error)) | (Err(error), Ok(())) => {
            leave(name, reader);
            Err(error)
        }
    }
}

/// Leaves the probe as a recording found it: a capture the recording started stops, and the
/// stream of a capture that ran before ends.
fn leave<L: AuxLink>(name: &str, reader: &mut Reader<L>) {
    if let Err(error) = reader.abandon() {
        tracing::warn!("could not leave {name} as it was: {error}");
    }
}

/// What a recording says as it starts, once the probe has said its rate.
pub(crate) fn starting(name: &str, out: &Path, rate: u32, limit: Option<Duration>) -> String {
    let ends = match limit {
        Some(limit) => format!(
            ", for {}. Ctrl-C ends it sooner",
            humantime::format_duration(limit)
        ),
        None => ". Ctrl-C ends it".to_owned(),
    };
    format!(
        "Recording {name} to {} at {rate} pairs a second{ends}.",
        shown(out)
    )
}

/// Reads the stream into `writer` until it ends, asking it to end at the limit or Ctrl-C.
fn stream<B: Bus, L: AuxLink>(
    bus: &B,
    name: &str,
    reader: &mut Reader<L>,
    writer: &mut Writer,
    options: &Options,
    cancel: &Cancel,
) -> Result<Recorded> {
    let deadline = options.limit.map(|limit| bus.now() + limit);
    let mut spinner: Option<Spinner> = None;
    let mut messages =
        reader.messages(|| cancel.is_cancelled() || deadline.is_some_and(|at| bus.now() >= at));
    while let Some(message) = messages.next() {
        let message = message
            .map_err(|error| anyhow::Error::new(error).context(format!("recording {name}")))?;
        match &message {
            Message::Header(header) => {
                say(&starting(
                    name,
                    &options.out,
                    header.whole_rate(),
                    options.limit,
                ));
                spinner = Some(Spinner::new("recording"));
            }
            Message::Pairs(_) => {
                if let Some(spinner) = &spinner {
                    let total = messages.reader().counts().total;
                    spinner.set(&format!(
                        "recording  {} pairs  {}",
                        total.pairs,
                        super::si(total.nanojoules() as f64 * 1e-9, "J")
                    ));
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
        .with_context(|| format!("{name}'s stream stopped before its end"))?;
    Ok(Recorded {
        total: counts.total,
        lost: counts.lost,
        reason: end.reason,
        dio: counts.pins,
        edges: counts.edges,
        edges_lost: counts.edge_losses,
        high_range_only: reader.header().is_some_and(|header| header.high_range_only),
        under_range: counts.under_range,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use escapement_verge_probe::aux::Aux;
    use escapement_verge_probe::dap::LinkError;
    use escapement_verge_probe::sim::{SimAux, SimBus};
    use escapement_verge_probe::{EnergyCaptureSample, UsbDevice};

    /// A probe that turns the start down, as one whose calibration failed would.
    struct RefuseStart(SimAux);

    impl AuxLink for RefuseStart {
        fn write(&mut self, request: &[u8]) -> Result<(), LinkError> {
            let mut request = request.to_vec();
            if request.starts_with(&[0x90, 9]) {
                request[2..6].fill(0);
            }
            self.0.write(&request)
        }

        fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
            self.0.read(timeout)
        }

        fn now(&self) -> Duration {
            self.0.now()
        }
    }

    fn unit(scenario: &str) -> (SimBus, UsbDevice) {
        let bus = SimBus::scenario(scenario).unwrap();
        let device = escapement_verge_probe::detect(&bus)
            .unwrap()
            .remove(0)
            .device;
        (bus, device)
    }

    fn options(out: &Path, csv: bool) -> Options {
        Options {
            out: out.to_owned(),
            rate: 1000,
            limit: Some(Duration::from_millis(300)),
            format: if csv { Format::Csv } else { Format::Sigrok },
            dio: 0,
            range: Default::default(),
        }
    }

    fn aux_log(bus: &SimBus) -> Vec<String> {
        bus.log()
            .into_iter()
            .filter_map(|line| line.split_once("AUX ").map(|(_, hex)| hex.to_owned()))
            .collect()
    }

    /// A recording of a capture that ran before it ends only its own stream, whether it ran its
    /// course or its file could not be made.
    #[test]
    fn zero_duration_and_file_failure_end_an_attached_stream() {
        for file_failure in [false, true] {
            let (bus, device) = unit("ours-pro-energy");
            let dir = tempfile::tempdir().unwrap();
            let out = if file_failure {
                dir.path().to_owned()
            } else {
                dir.path().join("empty.csv")
            };
            let mut reader = Reader::new(Aux::open(bus.open_aux(&device).unwrap()).unwrap());
            let result = record(
                &bus,
                "unit",
                &mut reader,
                &Options {
                    limit: Some(Duration::ZERO),
                    ..options(&out, true)
                },
                &Cancel::new(),
            );
            assert_eq!(result.is_err(), file_failure);
            assert!(matches!(
                reader.control().latest_energy_sample().unwrap(),
                EnergyCaptureSample::Sample(_)
            ));
            let log = aux_log(&bus);
            assert!(log.iter().any(|hex| hex.starts_with("900d")), "{log:?}");
            assert!(
                !log.iter()
                    .any(|hex| hex.starts_with("900b") || hex == "9002"),
                "{log:?}"
            );
        }
    }

    #[test]
    fn a_refused_start_leaves_an_earlier_recording_alone() {
        let (bus, device) = unit("ours-pro");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("run.sr");
        std::fs::write(&out, b"an earlier recording").unwrap();
        let mut reader =
            Reader::new(Aux::open(RefuseStart(bus.open_aux(&device).unwrap())).unwrap());
        let refused = record(
            &bus,
            "the unit",
            &mut reader,
            &options(&out, false),
            &Cancel::new(),
        );
        assert!(refused.is_err());
        assert_eq!(std::fs::read(&out).unwrap(), b"an earlier recording");
    }

    /// A file that stops taking pairs ends the recording with its own error, and the capture the
    /// recording started stops.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_full_disk_ends_the_recording_and_its_capture() {
        let (bus, device) = unit("ours-pro");
        let mut reader = Reader::new(Aux::open(bus.open_aux(&device).unwrap()).unwrap());
        let options = Options {
            limit: Some(Duration::from_secs(2)),
            ..options(Path::new("/dev/full"), true)
        };
        let error = record(&bus, "the unit", &mut reader, &options, &Cancel::new())
            .err()
            .expect("the disk is full");
        assert!(format!("{error:#}").contains("No space left"), "{error:#}");
        let log = aux_log(&bus);
        assert!(
            log.last()
                .is_some_and(|hex| hex.starts_with("900b") && hex.len() == 52),
            "{log:?}"
        );
    }

    /// A recording names what it records in each pair: their count, the range held, and the pins.
    #[test]
    fn a_recording_counts_what_it_wrote() {
        let (bus, device) = unit("ours-pro");
        let dir = tempfile::tempdir().unwrap();
        let mut reader = Reader::new(Aux::open(bus.open_aux(&device).unwrap()).unwrap());
        let recorded = record(
            &bus,
            "the unit",
            &mut reader,
            &Options {
                dio: 0b0101,
                range: Some(super::super::Ranges::High),
                ..options(&dir.path().join("run.csv"), true)
            },
            &Cancel::new(),
        )
        .unwrap();
        assert!(recorded.total.pairs > 100, "{}", recorded.total.pairs);
        assert_eq!(recorded.reason, EndReason::Stopped);
        assert!(recorded.high_range_only);
        assert_eq!(recorded.dio, 0b0101);
        let lines = std::fs::read_to_string(dir.path().join("run.csv"))
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with(','))
            .count();
        assert_eq!(lines as u64, recorded.total.pairs + 1);
    }
}
