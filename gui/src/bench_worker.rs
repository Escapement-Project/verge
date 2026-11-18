//! Probe controls and continuous acquisition; called only by the USB worker.

use anyhow::{Context, Result, bail};
use escapement_verge_probe::stream::{EndReason, EnergyStream, Message, Reader, ReaderError};
use escapement_verge_probe::tickwave::{Recorder, Snapshots};
use escapement_verge_probe::{
    BoardKind, Bus, CalibrationRange, CommandError, DioState, EnergyCalibration,
    EnergyCaptureSample, EnergyError, EnergyTotal, Refused, Unit, VergeControl,
};
use escapement_verge_probe::{EnergyRanges, pro};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct DioChange {
    pub mask: u8,
    pub outputs: u8,
    pub levels: u8,
}

#[derive(Clone, Debug)]
pub enum Command {
    Connect(Box<Unit>),
    Disconnect,
    CancelSupply,
    CancelDio,
    Dio {
        change: DioChange,
        confirmed: bool,
    },
    Supply {
        on: bool,
        confirmed: bool,
    },
    Start {
        rate: u32,
        ranges: EnergyRanges,
        path: PathBuf,
    },
    /// Records the DIO pins alone, on a board without energy capture.
    StartDio {
        path: PathBuf,
    },
    Stop,
}

/// What the window says when the probe ends a capture on an acquisition fault, in the CLI's words.
pub const CAPTURE_FAILED: &str =
    "The capture failed: the ADC overran, its DMA failed or the probe fell behind reading it.";

/// A direction group's name: its pin, or its first and last pins when they run in order.
pub fn group_name(mask: u8) -> String {
    let pins: Vec<u8> = (0..8).filter(|n| mask >> n & 1 != 0).collect();
    match pins.as_slice() {
        [pin] => format!("DIO{pin}"),
        [first, .., last] if usize::from(last - first) + 1 == pins.len() => {
            format!("DIO{first}–{last}")
        }
        _ => pins
            .iter()
            .map(|pin| format!("DIO{pin}"))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// Whether opening a unit failed for what it runs or is, which reconnecting cannot change.
pub fn refused(error: &anyhow::Error) -> Option<Refusal> {
    match error.downcast_ref::<pro::ProError>()? {
        pro::ProError::NotVerge { .. }
        | pro::ProError::Predates { .. }
        | pro::ProError::NoAux { .. }
        | pro::ProError::NoAuxDio { .. } => Some(Refusal::Update),
        pro::ProError::SwdJumper { .. }
        | pro::ProError::NotPro { .. }
        | pro::ProError::NoDio { .. }
        | pro::ProError::NotFitted { .. }
        | pro::ProError::DioDisabled { .. } => Some(Refusal::Board),
        // The unit may answer once another program lets go of it, or once it is back.
        _ => None,
    }
}

/// What changes a refusal, where reconnecting cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// An update: the unit runs something other than Verge, or a build too old.
    Update,
    /// A jumper or the board itself.
    Board,
}

#[derive(Clone, Debug)]
pub enum DioUnavailable {
    Feature(String),
    Aux,
    Disconnected,
}
impl std::fmt::Display for DioUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Feature(reason) => f.write_str(&crate::app::sentence(reason)),
            Self::Aux => {
                f.write_str("Verge Aux is unavailable. Close the program holding it and reconnect.")
            }
            Self::Disconnected => f.write_str("Reconnect to use DIO."),
        }
    }
}
impl DioUnavailable {
    pub fn connection(error: &anyhow::Error) -> Self {
        match error.downcast_ref::<pro::ProError>() {
            Some(pro::ProError::Open {
                interface: "Verge Aux",
                source: escapement_verge_probe::bus::BusError::Busy { .. },
                ..
            }) => Self::Aux,
            Some(error) => Self::Feature(error.to_string()),
            None => Self::Disconnected,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub instance: String,
    pub connected: bool,
    /// The unit switches a target supply, as any Pro does.
    pub target_power: bool,
    /// The unit measures energy, as a Pro does with its energy circuit fitted.
    pub energy: bool,
    /// Which board the unit is, which says where its DIO pins are.
    pub board: Option<BoardKind>,
    pub supply: bool,
    pub capturing: bool,
    pub recording: bool,
    /// The recording's stream has ended and its file is still being written and synced, which
    /// can take seconds on a busy disk. `recording` stays true until the file is whole.
    pub finishing: bool,
    pub millivolts: Option<u32>,
    pub nanoamps: Option<i32>,
    pub rate: Option<u32>,
    pub path: Option<PathBuf>,
    pub confirm_off: bool,
    pub error: Option<String>,
    pub dio: Option<DioState>,
    pub dio_reason: Option<DioUnavailable>,
    pub confirm_dio: Option<DioChange>,
    pub edge_losses: u64,
    /// Marker-loss reports in the recording under way.
    pub marker_losses: u64,
    /// The probe's energy total, where it keeps one.
    pub total: Option<EnergyTotal>,
    /// The rate of a capture another program started, which recording it keeps.
    pub running_rate: Option<u32>,
    /// The unit records its DIO pins alone: a board with the pins and no energy capture.
    pub dio_stream: bool,
    /// The connection was refused for what the unit runs or is, which reconnecting cannot change.
    pub refused: Option<Refusal>,
    /// The calibration points the probe's latest calibration left for each current range, as
    /// `verge energy calibration` reads them; `None` where the firmware gives none.
    pub calibration: Option<Vec<(CalibrationRange, EnergyCalibration)>>,
    /// The recording under way's latest snapshot, for the view to take.
    pub snapshots: Option<Snapshots>,
}

/// A session's recording file.
enum File {
    /// The unit's stream goes to it.
    Recording(Recorder),
    /// The stream has ended, and the writer is still finishing the file on its own thread.
    Finishing(Recorder),
}

pub struct Session<L: escapement_verge_probe::aux::AuxLink> {
    /// The unit's Verge Aux, and the stream on it while a recording runs.
    reader: Reader<L>,
    status: Status,
    file: Option<File>,
    poll_at: Instant,
    /// A capture started since the calibration points were read, which calibrates again.
    calibration_stale: bool,
}

impl<L: escapement_verge_probe::aux::AuxLink> Session<L> {
    pub fn open<B: Bus<Aux = L>>(bus: &B, unit: &Unit) -> Result<Self> {
        let pro_board = pro::validate(unit, pro::Feature::Energy).is_ok();
        let (mut aux, energy) = if pro_board {
            match pro::open_stream(bus, unit) {
                Ok(aux) => (aux, true),
                // A Pro without its energy circuit still switches its target supply, with J15
                // fitted or not.
                Err(pro::ProError::NotFitted { .. }) => {
                    (pro::open_aux(bus, unit, pro::Feature::Supply)?, false)
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            (pro::open_aux(bus, unit, pro::Feature::Dio)?, false)
        };
        let name = unit.serial().unwrap_or("the probe");
        let target_power =
            pro::check_offered(name, &mut VergeControl::new(&mut aux), pro::Feature::Supply)
                .is_ok();
        let mut session = Self {
            reader: Reader::new(aux),
            status: Status {
                instance: unit.device.instance.clone(),
                connected: true,
                target_power,
                energy,
                board: unit.verge().and_then(|ours| ours.board),
                dio_stream: !energy && unit.verge().is_some_and(|ours| ours.supports_dio_stream()),
                ..Status::default()
            },
            file: None,
            poll_at: Instant::now(),
            calibration_stale: true,
        };
        // Opening a standalone unit's Aux has checked DIO already.
        if pro_board {
            session.status.dio_reason =
                pro::check_offered(name, &mut session.reader.control(), pro::Feature::Dio)
                    .err()
                    .map(|e| DioUnavailable::Feature(e.to_string()));
        }
        session.poll()?;
        Ok(session)
    }
    pub fn status(&self) -> Status {
        self.status.clone()
    }
    /// The unit's stream is being read into a recording. `status.recording` stays true after it
    /// ends, until the file is whole.
    pub fn recording(&self) -> bool {
        matches!(self.file, Some(File::Recording(_)))
    }
    fn recorder(&mut self) -> Option<&mut Recorder> {
        match &mut self.file {
            Some(File::Recording(recorder)) => Some(recorder),
            _ => None,
        }
    }
    /// Waits for the recording's file to be whole, and ends the recording in the status.
    fn close_file(&mut self) -> Result<()> {
        let closed = match self.file.take() {
            Some(File::Recording(recorder) | File::Finishing(recorder)) => recorder.close(),
            None => Ok(()),
        };
        self.status.recording = false;
        self.status.finishing = false;
        self.status.snapshots = None;
        Ok(closed?)
    }
    fn poll(&mut self) -> Result<()> {
        let streaming = self.recording();
        let counts = self.reader.counts().clone();
        let mut control = self.reader.control();
        if self.status.target_power {
            self.status.supply = control.read_target_power()?;
        }
        if self.status.energy {
            let was_capturing = self.status.capturing;
            match control.latest_energy_sample() {
                Ok(EnergyCaptureSample::Sample(sample)) => {
                    self.status.capturing = true;
                    self.status.millivolts = sample.calibrated.map(|c| c.voltage_millivolts);
                    self.status.nanoamps = sample.calibrated.map(|c| c.current_nanoamps);
                }
                Ok(EnergyCaptureSample::NoSample) => self.status.capturing = true,
                Ok(EnergyCaptureSample::Stopped)
                | Err(CommandError::Firmware(EnergyError::DmaError)) => {
                    self.status.capturing = false;
                    self.status.millivolts = None;
                    self.status.nanoamps = None;
                }
                Err(error) => return Err(error.into()),
                _ => bail!("unsupported capture state"),
            }
            // A capture calibrates as it starts: read the points once its first calibrated pair
            // shows the calibration is done, or while nothing captures.
            self.calibration_stale |= self.status.capturing && !was_capturing;
            if self.calibration_stale
                && (!self.status.capturing || self.status.millivolts.is_some())
            {
                self.calibration_stale = false;
                self.status.calibration = [CalibrationRange::Low, CalibrationRange::High]
                    .into_iter()
                    .map(|range| Some((range, control.read_energy_calibration(range).ok()?)))
                    .collect();
            }
            // A firmware without the total, or without capture identities, leaves these unread.
            self.status.total = control.read_energy_accumulator().ok();
            if !self.status.capturing || streaming {
                self.status.running_rate = None;
            } else if self.status.running_rate.is_none() {
                self.status.running_rate = control
                    .energy_capture_status()
                    .ok()
                    .filter(|status| status.running)
                    .map(|status| status.actual_rate_pairs_per_second);
            }
        }
        if self.status.dio_reason.is_none() {
            let mut state = control.access_dio(0, 0, 0)?;
            if streaming {
                state.read = (state.read & !counts.pins) | (counts.levels & counts.pins);
            }
            self.status.dio = Some(state);
        }
        self.poll_at = Instant::now() + Duration::from_millis(500);
        Ok(())
    }
    /// Runs `command`, and says whether the session is still open. An error during a recording
    /// fails the recording, as one from `tick` does; any other stays in the status until the next
    /// command, so a later status cannot replace it before the window reads it.
    pub fn command(&mut self, command: Command) -> bool {
        match self.act(command) {
            Ok(()) => true,
            Err(error) if self.recording() => {
                self.fail(error);
                false
            }
            Err(error) => {
                self.status.error = Some(format!("{error:#}"));
                true
            }
        }
    }
    fn act(&mut self, command: Command) -> Result<()> {
        self.status.error = None;
        match command {
            Command::CancelDio => self.status.confirm_dio = None,
            Command::Dio { change, confirmed } => {
                if let Some(reason) = &self.status.dio_reason {
                    bail!("{reason}");
                }
                let mut control = self.reader.control();
                let state = control.access_dio(0, 0, 0)?;
                if let Some(group) = state.split(change.mask, change.outputs) {
                    self.status.dio = Some(state);
                    self.status.error = Some(format!(
                        "{} must change direction together.",
                        group_name(group)
                    ));
                    return Ok(());
                }
                if change.mask & change.outputs & !state.outputs != 0 && !confirmed {
                    self.status.confirm_dio = Some(change);
                    return Ok(());
                }
                self.status.confirm_dio = None;
                match control.access_dio(change.mask, change.outputs, change.levels) {
                    Ok(state) => self.status.dio = Some(state),
                    Err(CommandError::Firmware(Refused(state))) => {
                        self.status.dio = Some(state);
                        self.status.error = Some("The probe refused the DIO change.".into());
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Command::Supply { on, confirmed } => {
                self.poll()?;
                if !on && self.status.capturing && !confirmed {
                    self.status.confirm_off = true;
                    return Ok(());
                }
                self.status.confirm_off = false;
                self.status.supply = self.reader.control().set_target_power(on)?;
            }
            Command::Start { rate, ranges, path } => {
                if self.recording() {
                    bail!("a recording is already running");
                }
                if self.file.is_some() {
                    bail!("the last recording's file is still being written");
                }
                self.poll()?;
                let pins = if self.status.dio_reason.is_none() {
                    0x0f
                } else {
                    0
                };
                let stream = EnergyStream::new(rate).ranges(ranges).dio(pins);
                self.record_to(path, |reader| reader.start_energy(&stream).map(|_| ()))?;
                self.calibration_stale |= self.reader.owns_capture();
                self.status.capturing = true;
            }
            Command::StartDio { path } => {
                if self.recording() {
                    bail!("a recording is already running");
                }
                if self.file.is_some() {
                    bail!("the last recording's file is still being written");
                }
                if !self.status.dio_stream {
                    bail!("this probe records its DIO pins beside its energy capture");
                }
                self.record_to(path, |reader| reader.start_dio(0x0f))?;
            }
            Command::Stop => self.ask_stop()?,
            Command::CancelSupply => self.status.confirm_off = false,
            Command::Connect(_) | Command::Disconnect => {}
        }
        Ok(())
    }
    /// Makes the recording's file at `path`, then starts the stream into it with `start`. The
    /// probe is not touched when the file cannot be made, and a refused start removes the file.
    fn record_to(
        &mut self,
        path: PathBuf,
        start: impl FnOnce(&mut Reader<L>) -> Result<(), ReaderError>,
    ) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.reader.info()?;
        let recorder = Recorder::create(&path, self.reader.context())?;
        if let Err(error) = start(&mut self.reader) {
            // Nothing was recorded: leave no empty capture among those kept.
            drop(recorder);
            if let Err(removing) = std::fs::remove_file(&path)
                && removing.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!("removing {}: {removing}", path.display());
            }
            return Err(error.into());
        }
        self.status.snapshots = Some(recorder.snapshots());
        self.file = Some(File::Recording(recorder));
        self.status.path = Some(path);
        self.status.recording = true;
        self.status.edge_losses = 0;
        self.status.marker_losses = 0;
        self.status.rate = None;
        Ok(())
    }
    /// Shows the stream's levels of the pins it reports, which it has newer than a poll.
    fn update_levels(&mut self) {
        let counts = self.reader.counts();
        if let Some(state) = &mut self.status.dio {
            state.read = (state.read & !counts.pins) | (counts.levels & counts.pins);
        }
    }
    fn ask_stop(&mut self) -> Result<()> {
        if !self.recording() || self.reader.ending() {
            return Ok(());
        }
        self.tick()?;
        if !self.recording() {
            return Ok(());
        }
        Ok(self.reader.end()?)
    }
    /// Drain in short slices so controls stay responsive even at low sample rates.
    pub fn tick(&mut self) -> Result<bool> {
        let mut changed = false;
        if let Some(File::Finishing(recorder)) = &self.file
            && recorder.is_finished()
        {
            self.close_file()
                .context("finishing the recording's file")?;
            changed = true;
        }
        if let Some(recorder) = self.recorder() {
            recorder.check()?;
            for _ in 0..64 {
                let Some(message) = self.reader.read(Duration::from_millis(5))? else {
                    break;
                };
                match &message {
                    Message::Header(header) => {
                        self.status.rate = Some(header.whole_rate());
                        changed = true;
                    }
                    Message::DioHeader(_) | Message::DioLost(_) | Message::Edges(_) => {
                        changed = true;
                    }
                    Message::Markers(markers) => changed |= markers.lost(),
                    _ => {}
                }
                let counts = self.reader.counts();
                self.status.edge_losses = counts.edge_losses;
                self.status.marker_losses = counts.marker_losses;
                self.update_levels();
                let end = if let Message::End(end) = &message {
                    Some(end.reason)
                } else {
                    None
                };
                self.recorder().expect("recording").send(message)?;
                if let Some(reason) = end {
                    // The window opens the file once `recording` turns false, so that waits for
                    // the writer, on its own thread, while this one goes on serving the unit.
                    if let Some(File::Recording(mut recorder)) = self.file.take() {
                        recorder.end();
                        self.file = Some(File::Finishing(recorder));
                    }
                    self.status.finishing = true;
                    if matches!(reason, EndReason::Failed) {
                        self.status.error = Some(CAPTURE_FAILED.into());
                    }
                    self.poll()?;
                    changed = true;
                    break;
                }
            }
        }
        if Instant::now() >= self.poll_at {
            self.poll()?;
            changed = true;
        }
        Ok(changed)
    }
    pub fn fail(&mut self, error: anyhow::Error) {
        // The probe first: a disk that failed may also be slow to let the writer finish.
        let cleanup = self.reader.abandon();
        let written = self.close_file();
        self.status.connected = false;
        self.status.error = Some(format!(
            "{error:#}{}{}",
            written
                .err()
                .map_or(String::new(), |e| format!("; file: {e:#}")),
            cleanup
                .err()
                .map_or(String::new(), |e| format!("; cleanup: {e}"))
        ));
    }
    pub fn close(&mut self) -> Result<()> {
        if self.recording() {
            self.ask_stop()?;
            while self.recording() {
                self.tick().context("ending the recording")?;
            }
        }
        // Dropping the session waits for the file to be whole.
        self.close_file().context("finishing the recording's file")
    }
}

impl<L: escapement_verge_probe::aux::AuxLink> Drop for Session<L> {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            tracing::error!("closing bench: {error:#}");
            self.fail(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use escapement_verge_probe::{detect, sim::SimBus};
    fn session(name: &str) -> (SimBus, Session<escapement_verge_probe::sim::SimAux>) {
        let bus = SimBus::scenario(name).unwrap().paced(0.2);
        let unit = detect(&bus).unwrap().remove(0);
        let session = Session::open(&bus, &unit).unwrap();
        (bus, session)
    }
    /// A unit refused for its board or a jumper gets no update offer, and one refused for what it
    /// runs gets one: reconnecting changes neither.
    #[test]
    fn a_refusal_names_what_changes_it() {
        let open = |name: &str| {
            let bus = SimBus::scenario(name).unwrap();
            let unit = detect(&bus).unwrap().remove(0);
            refused(&Session::open(&bus, &unit).err().expect("refused"))
        };
        assert_eq!(open("swd-jumper"), Some(Refusal::Board));
        assert_eq!(open("ours"), Some(Refusal::Update));
    }
    #[test]
    fn supply_requires_confirmation_only_during_capture() {
        let (_bus, mut s) = session("ours-pro");
        assert!(s.command(Command::Supply {
            on: true,
            confirmed: false,
        }));
        assert!(s.command(Command::Supply {
            on: false,
            confirmed: false,
        }));
        assert!(!s.status().supply);
        assert!(!s.status().confirm_off);
        assert!(s.command(Command::Supply {
            on: true,
            confirmed: false,
        }));
        s.reader.control().start_energy_capture(1000).unwrap();
        assert!(s.command(Command::Supply {
            on: false,
            confirmed: false,
        }));
        assert!(s.status().supply);
        assert!(s.status().confirm_off);
        assert!(s.command(Command::Supply {
            on: false,
            confirmed: true,
        }));
        assert!(!s.status().supply);
    }
    #[test]
    fn recording_a_running_capture_leaves_it_running() {
        let (_bus, mut s) = session("ours-pro-energy");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.tickwave");
        assert!(s.command(Command::Start {
            rate: 1000,
            path: path.clone(),
            ranges: Default::default(),
        }));
        assert!(!s.reader.owns_capture());
        for _ in 0..3 {
            s.tick().unwrap();
        }
        assert!(s.command(Command::Stop));
        while s.status().recording {
            s.tick().unwrap();
        }
        assert!(s.status().capturing);
        let extent = read::open(&path).extent().unwrap();
        assert!(extent.end > extent.start);
    }
    /// The panel's range choice reaches the capture it starts: the recording's header says whether
    /// the capture held the high range, and a held one has no pair read on the low range.
    #[test]
    fn a_start_uses_the_range_asked_for() {
        for ranges in [EnergyRanges::Auto, EnergyRanges::High] {
            let (_bus, mut s) = session("ours-pro-debugger");
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("capture.tickwave");
            assert!(s.command(Command::Start {
                rate: 1000,
                path: path.clone(),
                ranges,
            }));
            for _ in 0..3 {
                s.tick().unwrap();
            }
            s.close().unwrap();
            let high = ranges == EnergyRanges::High;
            assert_eq!(read::header(&read::open(&path)).high_range_only, high);
            // The CSV export's range column: low, high or under.
            let text = std::fs::read_to_string(read::export(&path, true)).unwrap();
            let rows: Vec<&str> = text
                .lines()
                .filter(|l| !l.starts_with([',', 's']))
                .collect();
            let low = rows
                .iter()
                .filter(|l| l.split(',').nth(4) == Some("low"))
                .count();
            assert!(!rows.is_empty());
            assert_eq!(low == 0, high, "{ranges:?}: {low} of {} low", rows.len());
        }
    }
    #[test]
    fn own_capture_stops_and_debugger_does_not_block_aux() {
        let (_bus, mut s) = session("ours-pro-debugger");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.tickwave");
        assert!(s.command(Command::Start {
            rate: 100_000,
            path: path.clone(),
            ranges: Default::default(),
        }));
        assert!(s.reader.owns_capture());
        for _ in 0..3 {
            s.tick().unwrap();
        }
        s.close().unwrap();
        assert!(!s.status().capturing);
        assert_eq!(read::header(&read::open(&path)).whole_rate(), 100_000);
    }
    /// The stream's end leaves the session serving the unit while the writer finishes the file:
    /// the status says so, a new recording is refused, and `recording` turns false only once the
    /// file is whole.
    #[test]
    fn a_stopped_recording_is_finished_off_the_usb_thread() {
        let (_bus, mut s) = session("ours-plain");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dio.tickwave");
        assert!(s.command(Command::StartDio { path: path.clone() }));
        for _ in 0..5 {
            s.tick().unwrap();
        }
        assert!(s.command(Command::Stop));
        // The tick that takes the stream's end hands the file to its writer and returns.
        while !s.status().finishing {
            assert!(s.status().recording, "ended without a finishing state");
            s.tick().unwrap();
        }
        assert!(s.status().recording);
        assert!(s.command(Command::StartDio {
            path: dir.path().join("next.tickwave"),
        }));
        assert!(
            s.status()
                .error
                .is_some_and(|e| e.contains("still being written"))
        );
        while s.status().recording {
            s.tick().unwrap();
        }
        assert!(!s.status().finishing);
        assert!(escapement_verge_probe::tickwave::finished(&path).unwrap());
    }
    /// A session closed or failed while its writer finishes the file waits for the file and
    /// leaves nothing of the recording in the status.
    #[test]
    fn a_session_ended_while_its_file_finishes_waits_for_the_file() {
        for failed in [false, true] {
            let (_bus, mut s) = session("ours-plain");
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("dio.tickwave");
            assert!(s.command(Command::StartDio { path: path.clone() }));
            assert!(s.command(Command::Stop));
            while !s.status().finishing {
                s.tick().unwrap();
            }
            if failed {
                s.fail(anyhow::anyhow!("the unit went away"));
                assert!(
                    s.status()
                        .error
                        .is_some_and(|e| e.starts_with("the unit went away"))
                );
            } else {
                s.close().unwrap();
            }
            assert!(s.file.is_none(), "failed: {failed}");
            let status = s.status();
            assert!(!status.recording && !status.finishing && status.snapshots.is_none());
            assert!(escapement_verge_probe::tickwave::finished(&path).unwrap());
        }
    }
    #[test]
    fn restart_ends_recording_without_stopping_replacement_capture() {
        let (_bus, mut s) = session("ours-pro");
        let dir = tempfile::tempdir().unwrap();
        assert!(s.command(Command::Start {
            rate: 1000,
            path: dir.path().join("capture.tickwave"),
            ranges: Default::default(),
        }));
        for _ in 0..2 {
            s.tick().unwrap();
        }
        s.reader.control().start_energy_capture(2000).unwrap();
        while s.status().recording {
            s.tick().unwrap();
        }
        assert!(!s.reader.owns_capture());
        assert!(s.status().capturing);
    }
    #[test]
    fn delayed_attach_header_is_bound_before_stop_or_failure_cleanup() {
        struct DelayedHeader {
            link: escapement_verge_probe::sim::SimAux,
            bus: SimBus,
            pending: Option<(Duration, Vec<u8>)>,
        }
        impl escapement_verge_probe::aux::AuxLink for DelayedHeader {
            fn write(
                &mut self,
                request: &[u8],
            ) -> Result<(), escapement_verge_probe::dap::LinkError> {
                self.link.write(request)
            }
            fn read(
                &mut self,
                timeout: Duration,
            ) -> Result<Vec<u8>, escapement_verge_probe::dap::LinkError> {
                if let Some((ready, _)) = &self.pending {
                    if self.bus.now() >= *ready {
                        return Ok(self.pending.take().unwrap().1);
                    }
                    self.bus.sleep(timeout);
                    return Err(escapement_verge_probe::dap::LinkError::Timeout);
                }
                let message = self.link.read(timeout)?;
                if message.first() == Some(&0xa1) {
                    self.pending = Some((self.bus.now() + Duration::from_millis(100), message));
                    self.bus.sleep(timeout);
                    return Err(escapement_verge_probe::dap::LinkError::Timeout);
                }
                Ok(message)
            }
            fn now(&self) -> Duration {
                self.bus.now()
            }
        }
        for failure in [false, true] {
            let (bus, original) = session("ours-pro-energy");
            let status = original.status();
            drop(original);
            let device = detect(&bus).unwrap().remove(0).device;
            let mut s = Session {
                reader: Reader::new(
                    escapement_verge_probe::aux::Aux::open(DelayedHeader {
                        link: bus.open_aux(&device).unwrap(),
                        bus: bus.clone(),
                        pending: None,
                    })
                    .unwrap(),
                ),
                status,
                file: None,
                poll_at: Instant::now(),
                calibration_stale: true,
            };
            let dir = tempfile::tempdir().unwrap();
            assert!(s.command(Command::Start {
                rate: 1000,
                path: dir.path().join("capture.tickwave"),
                ranges: Default::default(),
            }));
            assert!(s.reader.capture_id().is_none());
            if failure {
                s.fail(anyhow::anyhow!("disk failure"));
            } else {
                s.close().unwrap();
            }
            let log = bus.log();
            assert!(log.iter().any(|line| line.contains("AUX 900d")), "{log:?}");
            assert!(!log.iter().any(|line| line.contains("AUX 900b")), "{log:?}");
            assert!(s.reader.control().energy_capture_status().unwrap().running);
        }
    }

    #[test]
    fn attach_failure_before_first_tick_ends_only_its_stream() {
        let (_bus, mut s) = session("ours-pro-energy");
        let dir = tempfile::tempdir().unwrap();
        assert!(s.command(Command::Start {
            rate: 1000,
            path: dir.path().join("capture.tickwave"),
            ranges: Default::default(),
        }));
        assert!(s.reader.capture_id().is_none());
        s.fail(anyhow::anyhow!("disk failure before first tick"));
        let mut control = s.reader.control();
        assert!(control.energy_capture_status().unwrap().running);
        // What the stream sent before the probe took the end request comes ahead of the end.
        let mut before = Vec::new();
        loop {
            match control
                .transport()
                .message(Duration::from_millis(50))
                .unwrap()
            {
                Some(Message::End(_)) => break,
                Some(
                    data @ (Message::Pairs(_)
                    | Message::Edges(_)
                    | Message::DioLost(_)
                    | Message::Markers(_)),
                ) if before.len() < 100 => before.push(data),
                other => panic!("{other:?} after {before:?}"),
            }
        }
        assert!(
            control
                .transport()
                .message(Duration::from_millis(50))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_refused_start_leaves_no_capture_behind() {
        let (_bus, mut s) = session("ours-pro");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.tickwave");
        assert!(s.command(Command::Start {
            rate: 0,
            path: path.clone(),
            ranges: Default::default(),
        }));
        assert!(s.status().error.is_some());
        assert!(!s.recording());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    #[test]
    fn refuses_other_boards_stock_isp_and_held_aux() {
        for name in ["ours", "stock", "isp", "ours-pro-aux-taken"] {
            {
                let bus = SimBus::scenario(name).expect("known refusal scenario");
                let units = detect(&bus).unwrap();
                assert!(Session::open(&bus, &units[0]).is_err(), "{name}");
            }
        }
    }
}

#[cfg(test)]
mod hardware {
    use super::*;
    use crate::bench::Bench;
    use crate::worker::{Backend, Event, Request, Worker};
    use egui_kittest::Harness;
    use escapement_verge_probe::{
        detect::detect_named,
        release::{GitHub, Repo, Ureq},
        usb::UsbBus,
    };
    use std::sync::atomic::AtomicBool;

    struct Rig {
        bench: Bench,
        worker: Worker,
        units: Vec<Unit>,
        selected: Option<String>,
    }
    impl Drop for Rig {
        fn drop(&mut self) {
            self.worker.send(Request::Bench(Command::Stop));
            self.worker.send(Request::Bench(Command::Supply {
                on: true,
                confirmed: true,
            }));
            self.worker.send(Request::Bench(Command::Disconnect));
        }
    }
    struct Restore(Unit);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Err(error) = escapement_verge_probe::pro::with_pro(
                &UsbBus::default(),
                &self.0,
                pro::Feature::Supply,
                |_, p| -> anyhow::Result<()> {
                    p.set_target_power(true)?;
                    Ok(())
                },
            ) {
                eprintln!("target supply restore failed: {error:#}");
            }
        }
    }
    fn until(h: &mut Harness<'_, Rig>, predicate: impl Fn(&Rig) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            h.step();
            if predicate(h.state()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("bench state timeout: {:?}", h.state().bench.status);
    }

    #[test]
    #[ignore = "uses USB; VERGE_BENCH_SERIAL and VERGE_BENCH_OUT required"]
    fn bench_markers() {
        use escapement_verge_probe::{DapTransport, Marked};
        let serial = std::env::var("VERGE_BENCH_SERIAL").expect("explicit Pro serial");
        let directory =
            PathBuf::from(std::env::var("VERGE_BENCH_OUT").expect("capture output directory"));
        std::fs::create_dir_all(&directory).unwrap();
        let bus = UsbBus::default();
        let unit = detect_named(&bus, Some(&serial)).unwrap().remove(0);
        let mut debugger = bus.open_dap(&unit.device).expect("CMSIS-DAP for markers");
        for rate in [1000, 100_000] {
            let mut session = Session::open(&bus, &unit).unwrap();
            assert!(
                !session.status().capturing,
                "leave an existing capture to its owner"
            );
            assert!(session.status().supply, "target supply must already be on");
            assert_eq!(session.status().dio.as_ref().unwrap().outputs & 8, 0);
            let path = directory.join(format!("markers-{rate}.tickwave"));
            assert!(session.command(Command::Start {
                rate,
                path: path.clone(),
                ranges: Default::default(),
            }));
            assert!(session.status().error.is_none(), "{:?}", session.status());
            let mut sent = 0;
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(3) {
                session.tick().unwrap();
                if sent < 6 && start.elapsed() >= Duration::from_millis(200 + 300 * u64::from(sent))
                {
                    let mut transport = DapTransport(&mut debugger);
                    let mut control = VergeControl::new(&mut transport);
                    assert_eq!(
                        control
                            .mark(40 + sent, format!("debug step,{sent}").as_bytes())
                            .unwrap(),
                        Marked::Sent
                    );
                    sent += 1;
                }
            }
            session.close().unwrap();
            let recording = read::open(&path);
            let numbers = read::marker_numbers(&recording);
            let losses = &recording.sources()[0].extra["marker_losses"];
            assert_eq!((sent, numbers.len(), losses.as_str()), (6, 6, "0"));
            assert_eq!(numbers, (40..46).collect::<Vec<_>>());
            let i = recording.channel("I").unwrap();
            let pairs = read::whole(&recording).channels[i]
                .as_ref()
                .unwrap()
                .samples;
            assert!(pairs >= 2 * u64::from(rate));
            assert_eq!(read::lost(&recording), 0);
            assert_eq!(read::dio_losses(&recording), 0);
            for csv in [true, false] {
                let output = read::export(&path, csv);
                if csv {
                    let text = std::fs::read_to_string(output).unwrap();
                    for n in 0..6 {
                        assert!(text.contains(&format!("debug step,{n}")));
                    }
                }
            }
            eprintln!(
                "markers at {rate} pairs/s: {pairs} pairs, {} lost; {} markers, {losses} loss reports",
                read::lost(&recording),
                numbers.len()
            );
        }
    }
    /// Opt-in; the caller must hold the shared bench and authorize target power cycling.
    #[test]
    #[ignore = "uses USB and target power; VERGE_BENCH_SERIAL and VERGE_BENCH_OUT required"]
    fn bench_hardware() {
        let serial = std::env::var("VERGE_BENCH_SERIAL").expect("explicit Pro serial");
        let directory =
            PathBuf::from(std::env::var("VERGE_BENCH_OUT").expect("capture output directory"));
        std::fs::create_dir_all(&directory).unwrap();
        let units = detect_named(&UsbBus::default(), Some(&serial)).unwrap();
        assert_eq!(units.len(), 1);
        pro::validate(&units[0], pro::Feature::Energy).unwrap();
        let _debugger = match UsbBus::default().open_dap(&units[0].device) {
            Ok(link) => Some(link),
            Err(escapement_verge_probe::bus::BusError::Busy { .. }) => None,
            Err(error) => panic!("opening CMSIS-DAP: {error}"),
        };
        let _restore = Restore(units[0].clone());
        let selected = Some(units[0].device.instance.clone());
        let repo: Repo = escapement_verge_probe::release::DEFAULT_REPO
            .parse()
            .unwrap();
        let worker = Worker::spawn(
            UsbBus::default(),
            GitHub::new(Ureq::default(), repo, None),
            || {},
        );
        let rig = Rig {
            bench: Bench::new(&directory),
            worker,
            units,
            selected,
        };
        let mut h = Harness::builder()
            .with_size(egui::vec2(1080.0, 700.0))
            .with_context_setup(crate::style::install)
            .wgpu()
            .build_ui_state(
                |ui, r: &mut Rig| {
                    while let Some(event) = r.worker.try_recv() {
                        if let Event::Bench(status) = event {
                            r.bench.receive(status);
                        }
                    }
                    r.bench.show(ui, &r.units, &mut r.selected, &mut r.worker);
                },
                rig,
            );
        until(&mut h, |r| {
            r.bench.status.as_ref().is_some_and(|s| s.connected)
        });
        let dio = h
            .state()
            .bench
            .status
            .as_ref()
            .unwrap()
            .dio
            .as_ref()
            .expect("DIO through Aux");
        assert_eq!(dio.outputs & 8, 0, "DIO3 must remain an input");
        assert!(
            !h.state().bench.status.as_ref().unwrap().capturing,
            "leave any existing capture to its owner"
        );
        h.state_mut().worker.send(Request::Bench(Command::Supply {
            on: false,
            confirmed: false,
        }));
        until(&mut h, |r| !r.bench.status.as_ref().unwrap().supply);
        assert!(!h.state().bench.status.as_ref().unwrap().confirm_off);
        h.state_mut().worker.send(Request::Bench(Command::Supply {
            on: true,
            confirmed: false,
        }));
        until(&mut h, |r| r.bench.status.as_ref().unwrap().supply);
        std::thread::sleep(Duration::from_millis(100));
        let (mut polled_high, mut polled_low) = (false, false);
        for _ in 0..100 {
            h.state_mut().worker.send(Request::Bench(Command::Dio {
                change: DioChange {
                    mask: 0,
                    outputs: 0,
                    levels: 0,
                },
                confirmed: false,
            }));
            h.step();
            let dio = h
                .state()
                .bench
                .status
                .as_ref()
                .unwrap()
                .dio
                .as_ref()
                .unwrap();
            assert_eq!(dio.outputs & 8, 0);
            polled_high |= dio.read & 8 != 0;
            polled_low |= dio.read & 8 == 0;
            std::thread::sleep(Duration::from_millis(3));
        }
        assert!(polled_high && polled_low, "DIO3 must toggle when polled");
        for (rate, seconds) in [(1000, 3), (100_000, 12)] {
            let path = directory.join(format!("gui-{rate}.tickwave"));
            h.state_mut().worker.send(Request::Bench(Command::Start {
                rate,
                path: path.clone(),
                ranges: Default::default(),
            }));
            until(&mut h, |r| r.bench.recording());
            if rate == 1000 {
                h.state_mut().worker.send(Request::Bench(Command::Supply {
                    on: false,
                    confirmed: false,
                }));
                until(&mut h, |r| r.bench.status.as_ref().unwrap().confirm_off);
                assert!(h.state().bench.status.as_ref().unwrap().supply);
                h.state_mut().worker.send(Request::Bench(Command::Supply {
                    on: false,
                    confirmed: true,
                }));
                until(&mut h, |r| !r.bench.status.as_ref().unwrap().supply);
                h.state_mut().worker.send(Request::Bench(Command::Supply {
                    on: true,
                    confirmed: false,
                }));
                until(&mut h, |r| r.bench.status.as_ref().unwrap().supply);
            }
            let until_time = Instant::now() + Duration::from_secs(seconds);
            let mut frames = 0;
            let mut old_selection = None;
            let mut seen = None;
            let mut selection_verified = false;
            while Instant::now() < until_time {
                h.step();
                h.render().unwrap();
                frames += 1;
                if rate == 100_000
                    && old_selection.is_none()
                    && until_time.saturating_duration_since(Instant::now()) < Duration::from_secs(6)
                    && let Some(extent) = h.state().bench.answer().and_then(|a| a.extent.clone())
                {
                    let second = h.state().bench.ticks_per_second().unwrap().round() as i64;
                    let selected = extent.start..extent.start + second;
                    h.state_mut().bench.select(selected.clone());
                    old_selection = Some(selected);
                }
                assert!(h.state().bench.recording(), "{:?}", h.state().bench.status);
                // A selection behind the recording's end keeps its readings while it grows.
                if let Some(selected) = &old_selection
                    && let Some(actual) = h.state().bench.selection_statistics()
                    && actual.interval == *selected
                {
                    match &seen {
                        Some(first) => assert_eq!(&actual, first),
                        None => seen = Some(actual),
                    }
                    selection_verified = true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if rate == 100_000 {
                assert!(
                    selection_verified,
                    "old selection did not update during acquisition"
                );
            }
            h.state_mut().worker.send(Request::Bench(Command::Stop));
            until(&mut h, |r| !r.bench.recording());
            assert!(
                h.state().bench.status.as_ref().unwrap().error.is_none(),
                "{:?}",
                h.state().bench.status
            );
            assert!(escapement_verge_probe::tickwave::finished(&path).unwrap());
            let recording = read::open(&path);
            let extent = recording.extent().unwrap();
            let whole = read::whole(&recording);
            let channel = |name: &str| {
                whole.channels[recording.channel(name).unwrap()]
                    .as_ref()
                    .unwrap()
            };
            let (current, voltage) = (channel("I"), channel("V"));
            assert_eq!(read::lost(&recording), 0, "{rate} pairs/s lost samples");
            assert!(current.samples > u64::from(rate));
            assert!(voltage.mean().unwrap_or_default() > 1.0);
            assert!(current.mean().unwrap_or_default() > 0.0);
            let header = read::header(&recording);
            assert_eq!(header.dio_mask, 15);
            assert_eq!(read::dio_losses(&recording), 0);
            let csv_path = read::export(&path, true);
            let edges = {
                use std::io::BufRead;
                let mut levels = header.dio_levels & 8 != 0;
                let mut edges = Vec::new();
                for line in std::io::BufReader::new(std::fs::File::open(csv_path).unwrap()).lines()
                {
                    let line = line.unwrap();
                    if !line.starts_with(',') {
                        continue;
                    }
                    let fields: Vec<_> = line.split(',').collect();
                    // sequence, time_ns, millivolts, nanoamps, range, then dio0 to dio3.
                    let high = fields[8] == "1";
                    if high != levels {
                        edges.push(fields[1].parse::<u64>().unwrap());
                        levels = high;
                    }
                }
                edges
            };
            let seconds =
                (extent.end - extent.start) as f64 / h.state().bench.ticks_per_second().unwrap();
            assert!(edges.len() as f64 > seconds * 50.0);
            assert!(
                edges.windows(2).all(|w| w[1] - w[0] < 20_000_000),
                "a missing DIO3 transition"
            );
            eprintln!(
                "DIO: {} transitions at {rate} pairs/s; first intervals {:?} ns",
                edges.len(),
                edges
                    .windows(2)
                    .take(4)
                    .map(|w| w[1] - w[0])
                    .collect::<Vec<_>>()
            );
            if let (Some(selected), Some(seen)) = (old_selection, &seen) {
                assert_eq!(
                    &recording
                        .statistics(selected, &AtomicBool::new(false))
                        .unwrap(),
                    seen
                );
            }
            read::export(&path, false);
            let power = channel("P");
            eprintln!(
                "rate {rate}: {} pairs, {} lost, {} rendered frames, mean {:.6} V / {:.6} A, {:.9} J",
                current.samples,
                read::lost(&recording),
                frames,
                voltage.mean().unwrap_or_default(),
                current.mean().unwrap_or_default(),
                power.integral(recording.timebase()),
            );
            h.render()
                .unwrap()
                .save(directory.join(format!("gui-{rate}.png")))
                .unwrap();
        }
    }
}

#[cfg(test)]
mod dio_tests {
    use super::*;
    use escapement_verge_probe::sim::SimBus;

    #[test]
    fn debugger_markers_survive_gui_recording_and_both_exports() {
        use escapement_verge_probe::{Bus, DapTransport, Marked};
        let bus = SimBus::scenario("ours-pro").unwrap().paced(0.1);
        let unit = escapement_verge_probe::detect(&bus).unwrap().remove(0);
        let mut session = Session::open(&bus, &unit).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("markers.tickwave");
        assert!(session.command(Command::Start {
            rate: 1000,
            path: path.clone(),
            ranges: Default::default(),
        }));
        for _ in 0..5 {
            session.tick().unwrap();
        }
        {
            let mut link = bus.open_dap(&unit.device).unwrap();
            let mut transport = DapTransport(&mut link);
            let mut control = VergeControl::new(&mut transport);
            assert_eq!(control.mark(42, b"debug step").unwrap(), Marked::Sent);
            assert_eq!(control.mark(43, b"resume").unwrap(), Marked::Sent);
        }
        for _ in 0..10 {
            session.tick().unwrap();
        }
        session.close().unwrap();
        assert_eq!(read::marker_numbers(&read::open(&path)), [42, 43]);
        for csv in [true, false] {
            let out = read::export(&path, csv);
            if csv {
                assert!(std::fs::read_to_string(out).unwrap().contains("debug step"));
            } else {
                assert!(std::fs::metadata(out).unwrap().len() > 0);
            }
        }
    }

    #[test]
    fn dio_polling_group_confirmation_and_split_refusal() {
        let bus = SimBus::scenario("ours-pro-held").unwrap();
        let unit = escapement_verge_probe::detect(&bus).unwrap().remove(0);
        let mut session = Session::open(&bus, &unit).unwrap();
        assert_eq!(session.status.dio.as_ref().unwrap().read, 8);
        let change = DioChange {
            mask: 7,
            outputs: 7,
            levels: 5,
        };
        session.command(Command::Dio {
            change,
            confirmed: false,
        });
        assert!(session.status.confirm_dio.is_some());
        assert_eq!(session.status.dio.as_ref().unwrap().outputs, 0);
        session.command(Command::CancelDio);
        assert!(session.status.confirm_dio.is_none());
        session.command(Command::Dio {
            change,
            confirmed: false,
        });
        session.command(Command::Dio {
            change,
            confirmed: true,
        });
        let state = session.status.dio.as_ref().unwrap();
        assert_eq!((state.outputs, state.read), (7, 13));
        session.command(Command::Dio {
            change: DioChange {
                mask: 1,
                outputs: 0,
                levels: 0,
            },
            confirmed: true,
        });
        assert!(session.status.error.as_ref().unwrap().contains("together"));
        assert_eq!(session.status.dio.as_ref().unwrap().outputs, 7);
        session.command(Command::Dio {
            change: DioChange {
                mask: 7,
                outputs: 0,
                levels: 0,
            },
            confirmed: false,
        });
        assert_eq!(session.status.dio.as_ref().unwrap().outputs, 0);
        assert!(session.status.confirm_dio.is_none());
    }

    #[test]
    fn dio_recording_uses_all_pins_and_keeps_edges_and_loss() {
        for scenario in ["ours-pro", "ours-pro-edge-loss"] {
            let bus = SimBus::scenario(scenario).unwrap().paced(0.1);
            let unit = escapement_verge_probe::detect(&bus).unwrap().remove(0);
            let mut session = Session::open(&bus, &unit).unwrap();
            session.command(Command::Supply {
                on: true,
                confirmed: false,
            });
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("dio.tickwave");
            assert!(session.command(Command::Start {
                rate: 1000,
                path: path.clone(),
                ranges: Default::default(),
            }));
            let mut saw_high = false;
            let mut saw_low = false;
            for _ in 0..40 {
                session.tick().unwrap();
                if let Some(state) = &session.status.dio {
                    saw_high |= state.read & 8 != 0;
                    saw_low |= state.read & 8 == 0;
                }
            }
            assert!(
                saw_high && saw_low,
                "levels must come from edges between polls"
            );
            session.close().unwrap();
            let recording = read::open(&path);
            assert_eq!(read::header(&recording).dio_mask, 15);
            // DIO3, the CSV export's ninth column, is high and low in turn.
            let text = std::fs::read_to_string(read::export(&path, true)).unwrap();
            let dio3: Vec<&str> = text
                .lines()
                .skip(1)
                .filter_map(|l| l.split(',').nth(8))
                .collect();
            assert!(dio3.contains(&"1") && dio3.contains(&"0"));
            assert_eq!(
                read::dio_losses(&recording),
                usize::from(scenario == "ours-pro-edge-loss")
            );
            read::export(&path, false);
        }
    }
}

/// What tests read back from a recording the worker wrote, through tickwave's view.
#[cfg(test)]
pub(crate) mod read {
    use escapement_tickwave_egui::view::{Cache, Recording, Statistics};
    use escapement_verge_probe::stream::Header;
    use std::path::Path;
    use std::sync::{Arc, atomic::AtomicBool};

    pub fn open(path: &Path) -> Recording {
        Recording::open(
            Arc::new(std::fs::File::open(path).unwrap()),
            Cache::new(64 << 20),
        )
        .unwrap()
    }
    pub fn header(r: &Recording) -> Header {
        escapement_verge_probe::tickwave::header_from(&r.sources()[0].extra).unwrap()
    }
    /// Each channel's statistics over the whole recording.
    pub fn whole(r: &Recording) -> Statistics {
        r.statistics(r.extent().unwrap(), &AtomicBool::new(false))
            .unwrap()
    }
    /// The markers' numbers, in order.
    pub fn marker_numbers(r: &Recording) -> Vec<u32> {
        r.try_markers(r.extent().unwrap(), None, 100)
            .unwrap()
            .entries
            .iter()
            .map(|m| {
                m.annotation.values.as_ref().unwrap()["number"]
                    .parse()
                    .unwrap()
            })
            .collect()
    }
    /// Pairs lost, by the current's channel.
    pub fn lost(r: &Recording) -> u64 {
        r.lost(r.channel("I").unwrap())
    }
    /// DIO edge-loss reports: the gaps each one leaves in every DIO channel.
    pub fn dio_losses(r: &Recording) -> usize {
        r.gaps(r.channel("DIO3").unwrap())
    }
    /// Writes the export of the whole recording beside it, and gives the CSV's text.
    pub fn export(path: &Path, csv: bool) -> std::path::PathBuf {
        let out = path.with_extension(if csv { "csv" } else { "sr" });
        crate::bench::export(path, &out, csv, None).unwrap();
        out
    }
}
