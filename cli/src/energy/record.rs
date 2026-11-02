//! `verge energy record`: every pair of a capture, from the Pro's stream on Verge Aux, to a sigrok
//! session or a CSV file, with the edges of the DIO pins it asks for.

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::output::outputln;
use anyhow::{Context, Result, anyhow, bail};
use verge_probe::aux::{Aux, AuxLink};
use verge_probe::record::Sink;
use verge_probe::stream::{Converter, Edge, EndReason, Markers, Message, Sample, Total};
use verge_probe::{Bus, Cancel, CommandError, EnergyCaptureSample, EnergyError, VergeControl};

pub struct Options {
    pub out: PathBuf,
    pub rate: u32,
    pub limit: Option<Duration>,
    pub csv: bool,
    /// The DIO pins whose edges to record, bit `k` for DIO`k`.
    pub dio: u8,
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
    /// The DIO pins the probe reported, their edges, and how often it lost some.
    pub dio: u8,
    pub edges: u64,
    pub edges_lost: u64,
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
            outputln!(
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
        pro.stream_energy_capture_identified(options.dio)
            .with_context(|| format!("streaming {name}'s capture"))?;
    } else {
        pro.start_energy_stream_identified(options.rate, options.dio)
            .with_context(|| format!("starting {name}'s capture; completion may be uncertain"))?;
    }
    let started = !running;
    let sink = match Sink::create(&options.out, options.csv) {
        Ok(sink) => sink,
        Err(error) => {
            leave(name, pro, started);
            return Err(error.into());
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
    if !started && let Err(error) = super::bind_attached_capture(pro) {
        tracing::warn!("could not identify {name}'s attached stream: {error}");
        return;
    }
    let left = if started {
        pro.stop_tracked_energy_capture()
    } else {
        pro.end_tracked_energy_stream()
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
    let began = bus.now();
    let deadline = limit.map(|limit| began + limit);
    let mut converter: Option<Converter> = None;
    let mut next_sequence = None;
    let mut lost = 0u64;
    let mut asked_at = None;
    // A pairs message's samples wait for the next one: every edge up to them has come by then.
    let mut held: Vec<Sample> = Vec::new();
    let (mut dio, mut edges, mut edges_lost) = (0u8, 0u64, 0u64);
    loop {
        if converter.is_none() && bus.now().saturating_sub(began) >= END_WAIT {
            bail!("{name} did not provide its capture header");
        }
        if asked_at.is_none()
            && converter.is_some()
            && (cancel.is_cancelled() || deadline.is_some_and(|deadline| bus.now() >= deadline))
        {
            if started {
                pro.stop_tracked_energy_capture()
                    .with_context(|| format!("stopping {name}'s capture"))?;
            } else {
                pro.end_tracked_energy_stream()
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
                pro.accept_energy_header(&header)?;
                writer.send(Output::Begin {
                    rate: header.rate,
                    pins: header.dio_mask,
                    levels: header.dio_levels,
                })?;
                dio = header.dio_mask;
                next_sequence = Some(header.first_sequence);
                converter = Some(Converter::new(&header));
            }
            // Pairs and an end before the header are a stream this one replaced.
            Message::Pairs(pairs) => {
                let Some(converter) = converter.as_mut() else {
                    continue;
                };
                if let Some(next) = next_sequence {
                    let missing = skipped(next, pairs.first_sequence);
                    lost += missing;
                    if missing > 0
                        && let Some(pair) = pairs.pairs.first()
                    {
                        converter.discontinuity(pair.low);
                    }
                }
                next_sequence = Some(pairs.first_sequence.wrapping_add(pairs.pairs.len() as u32));
                converter.add_message(&pairs);
                writer.send(Output::Pairs(std::mem::replace(
                    &mut held,
                    converter.take_samples(),
                )))?;
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
                held.extend(converter.take_samples());
                writer.send(Output::Pairs(held))?;
                let total = converter.total();
                return Ok(Recorded {
                    pairs: total.pairs,
                    total,
                    lost,
                    reason: end.reason,
                    dio,
                    edges,
                    edges_lost,
                });
            }
            Message::Edges(message) => {
                if converter.is_none() {
                    continue;
                }
                edges += message.edges.len() as u64;
                edges_lost += u64::from(message.lost());
                writer.send(Output::Edges(message.edges))?;
            }
            Message::Markers(markers) => {
                if converter.is_some() {
                    writer.send(Output::Markers(markers))?;
                }
            }
        }
    }
}

/// What the read loop hands the writer. Every edge up to a batch of pairs comes before it.
enum Output {
    Begin { rate: u32, pins: u8, levels: u8 },
    Pairs(Vec<Sample>),
    Edges(Vec<Edge>),
    Markers(Markers),
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
            let mut dropped_markers = false;
            for output in receive {
                match output {
                    Output::Begin { rate, pins, levels } => {
                        sink.include_markers()?;
                        sink.begin(rate, pins, levels)?;
                    }
                    Output::Pairs(samples) => sink.samples(&samples)?,
                    Output::Edges(edges) => sink.edges(edges),
                    Output::Markers(markers) => {
                        if !sink.markers(&markers)? && !dropped_markers {
                            dropped_markers = true;
                            tracing::warn!(
                                "markers are not recorded: the output is not a regular file"
                            );
                        }
                    }
                }
            }
            Ok(sink.finish()?)
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

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Read;
    use std::path::Path;
    use verge_probe::record::samplerate;
    const CHUNK: usize = 1 << 20;

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
        sink.begin(1000, 0, 0).unwrap();
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

    fn edge(time_ns: u64, levels: u8) -> Edge {
        Edge { time_ns, levels }
    }

    /// Each pin reported is a logic channel after GAP, its level at each sample, the analog
    /// channels numbered on after them. A pulse between two samples shows in the second; a gap's
    /// repeated samples take the levels at their own times.
    #[test]
    fn dio_pins_are_logic_channels_at_each_sample() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dio.sr");
        let mut sink = Sink::create(&path, false).unwrap();
        sink.begin(1000, 0x09, 0x0F).unwrap();
        sink.edges(vec![
            // DIO0 falls just after the sample at 1 ms.
            edge(1_000_100, 0x08),
            // DIO3 pulses low and back between 2 and 3 ms, so reads low at 3.
            edge(2_000_300, 0x00),
            edge(2_000_400, 0x08),
            // DIO0 rises in the gap, at 3.5 ms.
            edge(3_500_000, 0x09),
        ]);
        sink.samples(&[
            sample(1, 0, 3300, 1),
            sample(2, 1, 3300, 1),
            sample(3, 2, 3300, 1),
            sample(4, 3, 3300, 1),
            sample(5, 6, 3300, 1),
        ])
        .unwrap();
        sink.finish().unwrap();

        let mut zip = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        // Bit 0 GAP, bit 1 DIO0, bit 2 DIO3.
        assert_eq!(read(&mut zip, "logic-1-1"), [6, 6, 4, 0, 7, 7, 6]);
        assert_eq!(floats(&read(&mut zip, "analog-1-4-1")).len(), 7);
        assert_eq!(floats(&read(&mut zip, "analog-1-5-1")).len(), 7);
        let metadata = String::from_utf8(read(&mut zip, "metadata")).unwrap();
        assert!(
            metadata.contains(
                "total probes=3\nsamplerate=1 kHz\ntotal analog=2\nprobe1=GAP\nprobe2=DIO0\n\
                 probe3=DIO3\nanalog4=V\nanalog5=mA\nunitsize=1\n"
            ),
            "{metadata}"
        );
    }

    /// CSV puts each edge on a line of its own, among the pairs by time, and each pair's levels
    /// after it; edges after the last pair come last.
    #[test]
    fn csv_lines_up_edges_and_pairs_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dio.csv");
        let mut sink = Sink::create(&path, true).unwrap();
        sink.begin(1000, 0x0A, 0x02).unwrap();
        sink.edges(vec![edge(1_000_000, 0x0A), edge(1_500_000, 0x08)]);
        sink.samples(&[sample(1, 0, 3300, 7), sample(2, 1, 3300, 8)])
            .unwrap();
        sink.edges(vec![edge(2_200_000, 0x00)]);
        sink.samples(&[sample(3, 2, 3300, 9)]).unwrap();
        sink.finish().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "sequence,time_ns,millivolts,nanoamps,dio1,dio3\n\
             1,0,3300,7,1,0\n\
             ,1000000,,,1,1\n\
             2,1000000,3300,8,1,1\n\
             ,1500000,,,0,1\n\
             3,2000000,3300,9,0,1\n\
             ,2200000,,,0,0\n"
        );
    }

    /// Past a chunk's worth, the samples go on in the next chunk file of each channel.
    #[test]
    fn a_long_recording_is_written_in_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.sr");
        let mut sink = Sink::create(&path, false).unwrap();
        sink.begin(100_000, 0, 0).unwrap();
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
        fn write(&mut self, request: &[u8]) -> Result<(), verge_probe::dap::LinkError> {
            if request == [0x82] {
                let mut info = b"\x82CJTAG\x06\x00\x00\x01".to_vec();
                info.extend_from_slice(&0u32.to_le_bytes());
                info.extend_from_slice(&257u32.to_le_bytes());
                self.0.push_front(Some(info));
            } else if request == [0x90, 10, 0] {
                self.0.push_front(Some(vec![0x90, 0, 0]));
            }
            Ok(())
        }

        fn read(&mut self, _: Duration) -> Result<Vec<u8>, verge_probe::dap::LinkError> {
            self.0
                .pop_front()
                .flatten()
                .ok_or(verge_probe::dap::LinkError::Timeout)
        }
    }

    fn playback_control(aux: &mut Aux<Playback>) -> VergeControl<'_, Aux<Playback>> {
        let mut control = VergeControl::new(aux);
        control.stream_energy_capture_identified(0).unwrap();
        control
    }

    fn header() -> Vec<u8> {
        let mut header = vec![0xA1, 2];
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
        header.extend_from_slice(&[0; 16]);
        header.extend_from_slice(&1u64.to_le_bytes());
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
            &mut playback_control(&mut aux),
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

    #[test]
    fn late_stream_markers_are_exported_in_time_order() {
        let mut marker = vec![0xa4, 1, 1];
        marker.extend_from_slice(&500_000u64.to_le_bytes());
        marker.extend_from_slice(&42u32.to_le_bytes());
        marker.push(8);
        marker.extend_from_slice(b"step,one");
        let mut end = vec![0xa2, 2];
        end.extend_from_slice(&6u32.to_le_bytes());
        let messages = [
            Some(vec![0x90, 0, 0]),
            None,
            Some(header()),
            Some(pairs(1, 3)),
            Some(pairs(4, 3)),
            Some(marker),
            Some(end),
        ];
        let mut aux = Aux::open(Playback(messages.into_iter().collect())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("markers.csv");
        let writer = Writer::spawn(Sink::create(&path, true).unwrap());
        stream(
            &verge_probe::sim::SimBus::default(),
            "unit",
            &mut playback_control(&mut aux),
            &writer,
            false,
            None,
            &Cancel::new(),
        )
        .unwrap();
        writer.finish().unwrap();
        let csv = std::fs::read_to_string(path).unwrap();
        assert!(csv.contains("marker_number,marker_label,marker_lost"));
        assert!(csv.contains(",500000,,,42,\"step,one\",1"), "{csv}");
        let times: Vec<u64> = csv
            .lines()
            .skip(1)
            .map(|row| row.split(',').nth(1).unwrap().parse().unwrap())
            .collect();
        assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{csv}");
    }

    fn header_with_dio(pins: u8, levels: u8) -> Vec<u8> {
        let mut header = header();
        header[64] = pins;
        header[65] = levels;
        header[66] = 11;
        header
    }

    fn edges(lost: bool, edges: &[(u64, u8)]) -> Vec<u8> {
        verge_probe::stream::Edges::new(
            u8::from(lost),
            edges
                .iter()
                .map(|&(time_ns, levels)| edge(time_ns, levels))
                .collect(),
        )
        .to_bytes()
    }

    /// A pairs message's samples wait for the next message, so an edge inside them that comes
    /// after them still lands among them; edges count, and so do messages that lost some.
    #[test]
    fn edges_after_their_pairs_land_among_them() {
        let mut end = vec![0xA2, 2];
        end.extend_from_slice(&5u32.to_le_bytes());
        let messages = [
            Some(vec![0x90, 0, 0]),
            None,
            Some(header_with_dio(0x01, 0x00)),
            Some(pairs(1, 3)),
            Some(edges(false, &[(1_500_000, 0x01)])),
            Some(pairs(4, 2)),
            Some(edges(true, &[(3_200_000, 0x00), (4_000_000, 0x01)])),
            Some(end),
        ];
        let mut aux = Aux::open(Playback(messages.into_iter().collect())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("held.csv");
        let writer = Writer::spawn(Sink::create(&path, true).unwrap());
        let recorded = stream(
            &verge_probe::sim::SimBus::default(),
            "the unit",
            &mut playback_control(&mut aux),
            &writer,
            false,
            None,
            &Cancel::new(),
        )
        .unwrap();
        writer.finish().unwrap();
        assert_eq!(
            (recorded.dio, recorded.edges, recorded.edges_lost),
            (0x01, 3, 1)
        );
        let lines: Vec<String> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                let fields: Vec<&str> = line.split(',').collect();
                format!("{}@{}={}", fields[0], fields[1], fields[4])
            })
            .collect();
        assert_eq!(
            lines,
            [
                "sequence@time_ns=dio0",
                "1@0=0",
                "2@1000000=0",
                "@1500000=1",
                "3@2000000=1",
                "4@3000000=1",
                "@3200000=0",
                "@4000000=1",
                "5@4000000=1",
            ]
        );
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
            &mut playback_control(&mut aux),
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
            if request.starts_with(&[0x90, 9]) {
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
            dio: 0,
        }
    }

    #[test]
    fn missing_header_times_out_even_while_pairs_keep_arriving() {
        struct Headerless {
            inner: verge_probe::sim::SimAux,
            bus: verge_probe::sim::SimBus,
        }
        impl AuxLink for Headerless {
            fn write(&mut self, request: &[u8]) -> Result<(), verge_probe::dap::LinkError> {
                self.inner.write(request)
            }
            fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, verge_probe::dap::LinkError> {
                self.bus.sleep(Duration::from_millis(100));
                let message = self.inner.read(timeout)?;
                Ok(if message.first() == Some(&0xa1) {
                    pairs(1, 1)
                } else {
                    message
                })
            }
        }
        let (bus, device) = pro_link();
        let mut aux = Aux::open(Headerless {
            inner: bus.open_aux(&device).unwrap(),
            bus: bus.clone(),
        })
        .unwrap();
        let mut pro = VergeControl::new(&mut aux);
        let dir = tempfile::tempdir().unwrap();
        let result = record(
            &bus,
            "unit",
            &mut pro,
            &Options {
                limit: Some(Duration::ZERO),
                ..options(&dir.path().join("headerless.csv"), true)
            },
            &Cancel::new(),
        )
        .err()
        .unwrap();
        assert!(result.to_string().contains("capture header"), "{result:#}");
        assert!(bus.now() < Duration::from_secs(4));
        assert!(bus.log().iter().any(|line| line.contains("AUX 900b")));
    }

    #[test]
    fn zero_duration_and_file_failure_end_an_attached_stream() {
        for file_failure in [false, true] {
            let bus = verge_probe::sim::SimBus::scenario("ours-pro-energy").unwrap();
            let device = verge_probe::detect(&bus).unwrap().remove(0).device;
            let dir = tempfile::tempdir().unwrap();
            let out = if file_failure {
                dir.path().to_owned()
            } else {
                dir.path().join("empty.csv")
            };
            let mut aux = Aux::open(bus.open_aux(&device).unwrap()).unwrap();
            let mut pro = VergeControl::new(&mut aux);
            let result = record(
                &bus,
                "unit",
                &mut pro,
                &Options {
                    limit: Some(Duration::ZERO),
                    ..options(&out, true)
                },
                &Cancel::new(),
            );
            assert_eq!(result.is_err(), file_failure);
            assert!(matches!(
                pro.latest_energy_sample().unwrap(),
                EnergyCaptureSample::Sample(_)
            ));
            let log = bus.log();
            assert!(log.iter().any(|line| line.contains("AUX 900d")), "{log:?}");
            assert!(
                !log.iter()
                    .any(|line| line.contains("AUX 900b") || line.ends_with("AUX 9002")),
                "{log:?}"
            );
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
        assert!(
            log.last()
                .is_some_and(|hex| hex.starts_with("900b") && hex.len() == 52),
            "{log:?}"
        );
    }

    #[test]
    fn rates_read_as_libsigrok_writes_them() {
        assert_eq!(samplerate(1), "1 Hz");
        assert_eq!(samplerate(1000), "1 kHz");
        assert_eq!(samplerate(1500), "1500 Hz");
        assert_eq!(samplerate(100_000), "100 kHz");
    }
}
