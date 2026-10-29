//! `verge energy record`: every pair of a capture, from the Pro's stream on Verge Aux, to a sigrok
//! session or a CSV file.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tracing_indicatif::indicatif_println;
use verge_probe::aux::{Aux, AuxLink};
use verge_probe::stream::{Converter, EndReason, Message, Sample, Total};
use verge_probe::{Bus, Cancel, CommandError, EnergyCaptureSample, EnergyError, VergeControl};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

pub struct Options {
    pub out: PathBuf,
    pub rate: u32,
    pub limit: Option<Duration>,
    pub csv: bool,
}

/// How long a read waits before the limit and Ctrl-C are checked again.
const SLICE: Duration = Duration::from_millis(50);
/// How long the end may take to come once this side asked for it.
const END_WAIT: Duration = Duration::from_secs(2);

pub struct Recorded {
    pub pairs: u64,
    pub total: Total,
    /// Pairs the sequence skipped: messages the probe dropped because nobody read them in time.
    pub lost: u64,
    pub reason: EndReason,
}

/// Records the capture that runs, or starts one at `options.rate`, until the limit, Ctrl-C or the
/// capture's end. A capture this started is stopped at the end; one that ran before is left running.
pub fn record<B: Bus, L: AuxLink>(
    bus: &B,
    name: &str,
    pro: &mut VergeControl<'_, Aux<L>>,
    options: &Options,
    cancel: &Cancel,
) -> Result<Recorded> {
    let fault = |error: &CommandError<EnergyError>| {
        matches!(error, CommandError::Firmware(EnergyError::DmaError))
    };
    let running = match pro.latest_energy_sample() {
        Err(ref error) if fault(error) => {
            // The start below clears what the failed capture kept, so show it first.
            let kept = pro
                .read_energy_accumulator()
                .with_context(|| format!("reading {name}'s total"))?;
            indicatif_println!(
                "{name}: {}. A new capture replaces it.",
                super::describe_total(name, kept)?
            );
            false
        }
        Ok(EnergyCaptureSample::Stopped) => false,
        Ok(_) => true,
        Err(error) => return Err(error).with_context(|| format!("reading {name}'s capture")),
    };
    // The probe answers before the file is touched, so a refused start leaves an earlier
    // recording there as it was.
    if running {
        pro.stream_energy_capture()
            .with_context(|| format!("streaming {name}'s capture"))?;
    } else if let Err(start) = pro.start_energy_stream(options.rate) {
        // The reply can be lost after the probe started, so stop what this may have started.
        let start = anyhow!(start).context(format!("starting {name}'s capture"));
        return Err(match pro.stop_energy_capture() {
            Err(stop) if !fault(&stop) => {
                anyhow!("{start:#}; stopping it again failed too: {stop}")
            }
            _ => start,
        });
    }
    let started = !running;
    let sink = match Sink::create(&options.out, options.csv) {
        Ok(sink) => sink,
        Err(error) => {
            leave(name, pro, started);
            return Err(error);
        }
    };
    let writer = Writer::spawn(sink);
    let streamed = stream(bus, name, pro, &writer, started, options.limit, cancel);
    let written = writer
        .finish()
        .with_context(|| format!("writing {}", options.out.display()));
    match (streamed, written) {
        (Ok(recorded), Ok(())) => Ok(recorded),
        // The writer's own error says why the stream could not hand it pairs.
        (_, Err(error)) | (Err(error), Ok(())) => {
            leave(name, pro, started);
            Err(error)
        }
    }
}

/// Leaves the probe as a recording found it: a capture the recording started stops, and the
/// stream of a capture that ran before ends.
fn leave<L: AuxLink>(name: &str, pro: &mut VergeControl<'_, Aux<L>>, started: bool) {
    let left = if started {
        pro.stop_energy_capture()
    } else {
        pro.end_energy_stream()
    };
    if let Err(left) = left {
        tracing::warn!("could not leave {name} as it was: {left}");
    }
}

/// Pairs between the one expected next and `first`, as the sequence counts them.
fn skipped(next: u32, first: u32) -> u64 {
    u64::try_from(first.wrapping_sub(next).cast_signed()).unwrap_or(0)
}

/// Reads the stream into `writer` until it ends, asking it to end at the limit or Ctrl-C.
fn stream<B: Bus, L: AuxLink>(
    bus: &B,
    name: &str,
    pro: &mut VergeControl<'_, Aux<L>>,
    writer: &Writer,
    started: bool,
    limit: Option<Duration>,
    cancel: &Cancel,
) -> Result<Recorded> {
    let deadline = limit.map(|limit| bus.now() + limit);
    let mut converter: Option<Converter> = None;
    let mut next_sequence = None;
    let mut lost = 0u64;
    let mut asked_at = None;
    loop {
        if asked_at.is_none()
            && (cancel.is_cancelled() || deadline.is_some_and(|deadline| bus.now() >= deadline))
        {
            if started {
                pro.stop_energy_capture()
                    .with_context(|| format!("stopping {name}'s capture"))?;
            } else {
                pro.end_energy_stream()
                    .with_context(|| format!("ending {name}'s stream"))?;
            }
            asked_at = Some(bus.now());
        }
        let message = pro
            .transport()
            .message(SLICE)
            .with_context(|| format!("reading {name}'s stream"))?;
        let Some(message) = message else {
            if asked_at.is_some_and(|asked| bus.now().saturating_sub(asked) > END_WAIT) {
                bail!("{name} did not end the stream when asked");
            }
            continue;
        };
        match message {
            Message::Header(header) => {
                if converter.is_some() {
                    bail!("{name} began a second stream before ending the first");
                }
                writer.send(Output::Begin(header.rate))?;
                next_sequence = Some(header.first_sequence);
                converter = Some(Converter::new(&header));
            }
            // Pairs and an end before the header are a stream this one replaced.
            Message::Pairs(pairs) => {
                let Some(converter) = converter.as_mut() else {
                    continue;
                };
                if let Some(next) = next_sequence {
                    lost += skipped(next, pairs.first_sequence);
                }
                next_sequence = Some(pairs.first_sequence.wrapping_add(pairs.pairs.len() as u32));
                converter.add_message(&pairs);
                writer.send(Output::Pairs(converter.take_samples()))?;
            }
            Message::End(end) => {
                let Some(mut converter) = converter else {
                    continue;
                };
                // The probe keeps room for the end when it drops pairs, so pairs lost just before
                // it show only in its last sequence.
                if let Some(next) = next_sequence {
                    lost += skipped(next, end.last_sequence.wrapping_add(1));
                }
                converter.end();
                writer.send(Output::Pairs(converter.take_samples()))?;
                let total = converter.total();
                return Ok(Recorded {
                    pairs: total.pairs,
                    total,
                    lost,
                    reason: end.reason,
                });
            }
            // This firmware reports no edges yet.
            Message::Edges(_) => {}
        }
    }
}

/// What the read loop hands the writer.
enum Output {
    Begin(u32),
    Pairs(Vec<Sample>),
}

/// The file, written on a thread of its own. Writing a sigrok chunk takes long enough that the
/// probe's 16 messages would fill meanwhile and pairs be lost.
struct Writer {
    send: mpsc::Sender<Output>,
    thread: thread::JoinHandle<Result<()>>,
}

impl Writer {
    fn spawn(mut sink: Sink) -> Writer {
        let (send, receive) = mpsc::channel();
        let thread = thread::spawn(move || {
            for output in receive {
                match output {
                    Output::Begin(rate) => sink.begin(rate)?,
                    Output::Pairs(samples) => sink.samples(&samples)?,
                }
            }
            sink.finish()
        });
        Writer { send, thread }
    }

    /// Fails once the writer has stopped on an error, which [`Writer::finish`] returns.
    fn send(&self, output: Output) -> Result<()> {
        self.send
            .send(output)
            .map_err(|_| anyhow!("the file stopped taking pairs"))
    }

    /// Waits for everything sent to be written, and the file finished.
    fn finish(self) -> Result<()> {
        drop(self.send);
        self.thread
            .join()
            .map_err(|_| anyhow!("the file's writer panicked"))?
    }
}

/// Where the pairs go.
pub enum Sink {
    Csv(BufWriter<File>),
    Sigrok(Box<Sigrok>),
}

impl Sink {
    fn create(path: &Path, csv: bool) -> Result<Sink> {
        let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let file = BufWriter::new(file);
        Ok(if csv {
            Sink::Csv(file)
        } else {
            Sink::Sigrok(Box::new(Sigrok::new(file)?))
        })
    }

    /// The stream began at `rate` pairs a second.
    fn begin(&mut self, rate: u32) -> Result<()> {
        match self {
            Sink::Csv(file) => writeln!(file, "sequence,time_ns,millivolts,nanoamps")?,
            Sink::Sigrok(sigrok) => sigrok.rate = rate,
        }
        Ok(())
    }

    fn samples(&mut self, samples: &[Sample]) -> Result<()> {
        match self {
            Sink::Csv(file) => {
                for s in samples {
                    writeln!(
                        file,
                        "{},{},{},{}",
                        s.sequence, s.time_ns, s.millivolts, s.nanoamps
                    )?;
                }
            }
            Sink::Sigrok(sigrok) => {
                for sample in samples {
                    sigrok.push(sample)?;
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<()> {
        match self {
            Sink::Csv(mut file) => file.flush()?,
            Sink::Sigrok(sigrok) => sigrok.finish()?,
        }
        Ok(())
    }
}

/// A sigrok session file (srzip, format 2), as libsigrok's own srzip output writes it: the
/// voltage in V and the current in mA as analog channels, and a logic channel GAP that is high
/// where the probe measured nothing, across which the last pair is repeated.
pub struct Sigrok {
    zip: ZipWriter<BufWriter<File>>,
    rate: u32,
    last: Option<Sample>,
    volts: Vec<f32>,
    milliamps: Vec<f32>,
    logic: Vec<u8>,
    chunks: u32,
}

/// Samples a chunk file holds: 4 MiB of each analog channel.
const CHUNK: usize = 1 << 20;
const GAP: u8 = 1;
/// The channels' indices: logic first, then analog, as libsigrok numbers them.
const CHANNELS: [(&str, &str); 3] = [("probe1", "GAP"), ("analog2", "V"), ("analog3", "mA")];

impl Sigrok {
    fn new(file: BufWriter<File>) -> Result<Sigrok> {
        let mut zip = ZipWriter::new(file);
        zip.start_file("version", options())?;
        zip.write_all(b"2")?;
        Ok(Sigrok {
            zip,
            rate: 0,
            last: None,
            volts: Vec::new(),
            milliamps: Vec::new(),
            logic: Vec::new(),
            chunks: 0,
        })
    }

    fn put(&mut self, sample: &Sample, logic: u8) -> Result<()> {
        self.volts.push(sample.millivolts as f32 / 1e3);
        self.milliamps.push(sample.nanoamps as f32 / 1e6);
        self.logic.push(logic);
        if self.logic.len() == CHUNK {
            self.flush()?;
        }
        Ok(())
    }

    fn push(&mut self, sample: &Sample) -> Result<()> {
        if let Some(last) = self.last {
            // Periods since the last pair, to the nearest: more than one is a pause, a hold or
            // pairs lost.
            let rate = u128::from(self.rate.max(1));
            let elapsed = u128::from(sample.time_ns.saturating_sub(last.time_ns));
            let periods = (elapsed * rate + 500_000_000) / 1_000_000_000;
            for _ in 1..periods {
                self.put(&last, GAP)?;
            }
        }
        self.put(sample, 0)?;
        self.last = Some(*sample);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.logic.is_empty() {
            return Ok(());
        }
        self.chunks += 1;
        let chunk = self.chunks;
        self.zip.start_file(format!("logic-1-{chunk}"), options())?;
        self.zip.write_all(&self.logic)?;
        for (index, values) in [(2, &self.volts), (3, &self.milliamps)] {
            self.zip
                .start_file(format!("analog-1-{index}-{chunk}"), options())?;
            let bytes: Vec<u8> = values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect();
            self.zip.write_all(&bytes)?;
        }
        self.volts.clear();
        self.milliamps.clear();
        self.logic.clear();
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        self.flush()?;
        // libsigrok reads the keys in order: the totals make the channels the names then rename.
        let mut metadata = format!(
            "[global]\nsigrok version=0.5.2\n\n[device 1]\ncapturefile=logic-1\ntotal probes=1\n\
             samplerate={}\ntotal analog=2\n",
            samplerate(self.rate)
        );
        for (key, name) in CHANNELS {
            metadata.push_str(&format!("{key}={name}\n"));
        }
        metadata.push_str("unitsize=1\n");
        self.zip.start_file("metadata", options())?;
        self.zip.write_all(metadata.as_bytes())?;
        self.zip.finish()?.flush()?;
        Ok(())
    }
}

fn options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
}

/// A rate as libsigrok's `sr_samplerate_string` writes it, which its parser reads back.
fn samplerate(rate: u32) -> String {
    match rate {
        0 => "0 Hz".to_owned(),
        r if r % 1_000_000 == 0 => format!("{} MHz", r / 1_000_000),
        r if r % 1_000 == 0 => format!("{} kHz", r / 1_000),
        r => format!("{r} Hz"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn sample(sequence: u32, time_ms: u64, millivolts: u32, nanoamps: u32) -> Sample {
        Sample {
            sequence,
            time_ns: time_ms * 1_000_000,
            millivolts,
            nanoamps,
        }
    }

    fn read(zip: &mut zip::ZipArchive<File>, name: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        zip.by_name(name).unwrap().read_to_end(&mut bytes).unwrap();
        bytes
    }

    fn floats(bytes: &[u8]) -> Vec<f32> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect()
    }

    /// A pause between pairs 3 and 4 repeats pair 3 across it with GAP high, so the time axis
    /// stays true.
    #[test]
    fn a_gap_repeats_the_last_pair_with_gap_high() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gap.sr");
        let mut sink = Sink::create(&path, false).unwrap();
        sink.begin(1000).unwrap();
        sink.samples(&[
            sample(1, 0, 3300, 20_000),
            sample(2, 1, 3300, 5_000_000),
            sample(3, 2, 3290, 1_000),
            sample(4, 5, 3280, 2_000),
            sample(5, 6, 3280, 3_000),
        ])
        .unwrap();
        sink.finish().unwrap();

        let mut zip = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        assert_eq!(read(&mut zip, "version"), b"2");
        assert_eq!(read(&mut zip, "logic-1-1"), [0, 0, 0, 1, 1, 0, 0]);
        assert_eq!(
            floats(&read(&mut zip, "analog-1-2-1")),
            [3.3, 3.3, 3.29, 3.29, 3.29, 3.28, 3.28]
        );
        assert_eq!(
            floats(&read(&mut zip, "analog-1-3-1")),
            [0.02, 5.0, 0.001, 0.001, 0.001, 0.002, 0.003]
        );
        let metadata = String::from_utf8(read(&mut zip, "metadata")).unwrap();
        assert!(metadata.contains("samplerate=1 kHz\n"), "{metadata}");
    }

    /// Past a chunk's worth, the samples go on in the next chunk file of each channel.
    #[test]
    fn a_long_recording_is_written_in_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.sr");
        let mut sink = Sink::create(&path, false).unwrap();
        sink.begin(100_000).unwrap();
        let samples: Vec<Sample> = (0..CHUNK as u32 + 3)
            .map(|n| Sample {
                sequence: n,
                time_ns: u64::from(n) * 10_000,
                millivolts: 1800,
                nanoamps: n,
            })
            .collect();
        sink.samples(&samples).unwrap();
        sink.finish().unwrap();

        let mut zip = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        assert_eq!(read(&mut zip, "logic-1-1").len(), CHUNK);
        assert_eq!(read(&mut zip, "logic-1-2").len(), 3);
        assert_eq!(
            floats(&read(&mut zip, "analog-1-3-2")),
            [
                CHUNK as f32 / 1e6,
                (CHUNK + 1) as f32 / 1e6,
                (CHUNK + 2) as f32 / 1e6
            ]
        );
        assert!(zip.by_name("logic-1-3").is_err());
    }

    /// Plays back stream messages, as a probe whose stream ends by itself would send them. `None`
    /// is a read that times out, as the one that ends the drain on opening. Writes go nowhere.
    struct Playback(std::collections::VecDeque<Option<Vec<u8>>>);

    impl AuxLink for Playback {
        fn write(&mut self, _: &[u8]) -> Result<(), verge_probe::dap::LinkError> {
            Ok(())
        }

        fn read(&mut self, _: Duration) -> Result<Vec<u8>, verge_probe::dap::LinkError> {
            self.0
                .pop_front()
                .flatten()
                .ok_or(verge_probe::dap::LinkError::Timeout)
        }
    }

    fn header() -> Vec<u8> {
        let mut header = vec![0xA1, 1];
        header.extend_from_slice(&1000u32.to_le_bytes());
        header.push(1);
        header.extend_from_slice(&3300u16.to_le_bytes());
        header.extend_from_slice(&1u32.to_le_bytes());
        header.push(1);
        // Both ranges: two points, then two empty slots.
        let mut table = vec![2];
        for (code, nanoamps) in [(0u16, 0u32), (60_000, 60_000), (0, 0), (0, 0)] {
            table.extend_from_slice(&code.to_le_bytes());
            table.extend_from_slice(&nanoamps.to_le_bytes());
        }
        header.extend_from_slice(&table);
        header.extend_from_slice(&table);
        header.extend_from_slice(&[0; 4]);
        header
    }

    fn pairs(first_sequence: u32, count: u16) -> Vec<u8> {
        let mut message = vec![0xA0, 0];
        message.extend_from_slice(&first_sequence.to_le_bytes());
        message.extend_from_slice(&(u64::from(first_sequence - 1) * 1_000_000).to_le_bytes());
        message.extend_from_slice(&count.to_le_bytes());
        for _ in 0..count {
            message.extend_from_slice(&[0x00, 0x80, 0x10, 0x00, 1]);
        }
        message
    }

    /// Pairs the sequence skips are counted as lost, and the file still gets the rest. What comes
    /// before the header belongs to a stream this one replaced.
    #[test]
    fn a_skipped_sequence_counts_as_lost() {
        let mut end = vec![0xA2, 2];
        end.extend_from_slice(&7u32.to_le_bytes());
        let mut replaced = vec![0xA2, 1];
        replaced.extend_from_slice(&40u32.to_le_bytes());
        // Opening asks for mode 7, then drains until a read times out.
        let messages = [
            Some(vec![0x90, 0, 0]),
            None,
            Some(pairs(30, 2)),
            Some(replaced),
            Some(header()),
            Some(pairs(1, 3)),
            Some(pairs(6, 2)),
            Some(end),
        ];
        let mut aux = Aux::open(Playback(messages.into_iter().collect())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lost.csv");
        let writer = Writer::spawn(Sink::create(&path, true).unwrap());
        let bus = verge_probe::sim::SimBus::default();
        let recorded = stream(
            &bus,
            "the unit",
            &mut VergeControl::new(&mut aux),
            &writer,
            false,
            None,
            &Cancel::new(),
        )
        .unwrap();
        writer.finish().unwrap();
        assert_eq!((recorded.pairs, recorded.lost), (5, 2));
        assert_eq!(recorded.reason, EndReason::Stopped);
        let sequences: Vec<String> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .skip(1)
            .map(|line| line.split(',').next().unwrap().to_owned())
            .collect();
        assert_eq!(sequences, ["1", "2", "3", "6", "7"]);
    }

    /// The probe keeps room for the end when it drops pairs, so its last sequence counts pairs
    /// lost just before it.
    #[test]
    fn pairs_lost_before_the_end_are_counted() {
        let mut end = vec![0xA2, 2];
        end.extend_from_slice(&10u32.to_le_bytes());
        let messages = [
            Some(vec![0x90, 0, 0]),
            None,
            Some(header()),
            Some(pairs(1, 3)),
            Some(end),
        ];
        let mut aux = Aux::open(Playback(messages.into_iter().collect())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let writer = Writer::spawn(Sink::create(&dir.path().join("tail.csv"), true).unwrap());
        let recorded = stream(
            &verge_probe::sim::SimBus::default(),
            "the unit",
            &mut VergeControl::new(&mut aux),
            &writer,
            false,
            None,
            &Cancel::new(),
        )
        .unwrap();
        writer.finish().unwrap();
        assert_eq!((recorded.pairs, recorded.lost), (3, 7));
    }

    /// A probe that turns the start down, as one whose calibration failed would.
    struct RefuseStart(verge_probe::sim::SimAux);

    impl AuxLink for RefuseStart {
        fn write(&mut self, request: &[u8]) -> Result<(), verge_probe::dap::LinkError> {
            let mut request = request.to_vec();
            if request.starts_with(&[0x90, 5]) {
                request[2..6].fill(0);
            }
            self.0.write(&request)
        }

        fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, verge_probe::dap::LinkError> {
            self.0.read(timeout)
        }
    }

    fn pro_link() -> (verge_probe::sim::SimBus, verge_probe::UsbDevice) {
        let bus = verge_probe::sim::SimBus::scenario("ours-pro").unwrap();
        let device = verge_probe::detect(&bus).unwrap().remove(0).device;
        (bus, device)
    }

    fn options(out: &Path, csv: bool) -> Options {
        Options {
            out: out.to_owned(),
            rate: 1000,
            limit: Some(Duration::from_millis(300)),
            csv,
        }
    }

    #[test]
    fn a_refused_start_leaves_an_earlier_recording_alone() {
        let (bus, device) = pro_link();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("run.sr");
        std::fs::write(&out, b"an earlier recording").unwrap();
        let mut aux = Aux::open(RefuseStart(bus.open_aux(&device).unwrap())).unwrap();
        let mut pro = VergeControl::new(&mut aux);
        let refused = record(
            &bus,
            "the unit",
            &mut pro,
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
        let (bus, device) = pro_link();
        let mut aux = Aux::open(bus.open_aux(&device).unwrap()).unwrap();
        let mut pro = VergeControl::new(&mut aux);
        let options = Options {
            limit: Some(Duration::from_secs(2)),
            ..options(Path::new("/dev/full"), true)
        };
        let error = record(&bus, "the unit", &mut pro, &options, &Cancel::new())
            .err()
            .expect("the disk is full");
        assert!(format!("{error:#}").contains("No space left"), "{error:#}");
        let log: Vec<String> = bus
            .log()
            .into_iter()
            .filter_map(|line| line.split_once("AUX ").map(|(_, hex)| hex.to_owned()))
            .collect();
        assert_eq!(log.last().map(String::as_str), Some("9002"), "{log:?}");
    }

    #[test]
    fn rates_read_as_libsigrok_writes_them() {
        assert_eq!(samplerate(1), "1 Hz");
        assert_eq!(samplerate(1000), "1 kHz");
        assert_eq!(samplerate(1500), "1500 Hz");
        assert_eq!(samplerate(100_000), "100 kHz");
    }
}
