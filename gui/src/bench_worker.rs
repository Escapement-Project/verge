//! Probe controls and continuous acquisition; called only by the USB worker.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use verge_probe::aux::Aux;
use verge_probe::capture::Recorder;
use verge_probe::pro;
use verge_probe::stream::{EndReason, Message};
use verge_probe::{
    Bus, CommandError, DioState, EnergyCaptureSample, EnergyError, Refused, Unit, VergeControl,
};

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
    Dio { change: DioChange, confirmed: bool },
    Supply { on: bool, confirmed: bool },
    Start { rate: u32, path: PathBuf },
    Stop,
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
            Self::Feature(reason) => f.write_str(reason),
            Self::Aux => {
                f.write_str("Verge Aux is unavailable. Close the program holding it and reconnect.")
            }
            Self::Disconnected => f.write_str("Reconnect to an available Pro to use DIO."),
        }
    }
}
impl DioUnavailable {
    pub fn connection(error: &anyhow::Error) -> Self {
        match error.downcast_ref::<pro::ProError>() {
            Some(pro::ProError::Open {
                interface: "Verge Aux",
                source: verge_probe::bus::BusError::Busy { .. },
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
    pub supply: bool,
    pub capturing: bool,
    pub recording: bool,
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
}

pub struct Session<L: verge_probe::aux::AuxLink> {
    aux: Aux<L>,
    status: Status,
    recorder: Option<Recorder>,
    owned: bool,
    capture_id: Option<verge_probe::CaptureId>,
    stopping: Option<Instant>,
    synced: bool,
    poll_at: Instant,
    stream_mask: u8,
    stream_levels: u8,
}

impl<L: verge_probe::aux::AuxLink> Session<L> {
    pub fn open<B: Bus<Aux = L>>(bus: &B, unit: &Unit) -> Result<Self> {
        let aux = pro::open_stream(bus, unit)?;
        let mut session = Self {
            aux,
            status: Status {
                instance: unit.device.instance.clone(),
                connected: true,
                ..Status::default()
            },
            recorder: None,
            owned: false,
            capture_id: None,
            stopping: None,
            synced: false,
            poll_at: Instant::now(),
            stream_mask: 0,
            stream_levels: 0,
        };
        session.status.dio_reason = pro::check_offered(
            unit.serial().unwrap_or("the unit"),
            &mut VergeControl::new(&mut session.aux),
            pro::Feature::Dio,
        )
        .err()
        .map(|e| DioUnavailable::Feature(e.to_string()));
        session.poll()?;
        Ok(session)
    }
    pub fn status(&self) -> Status {
        self.status.clone()
    }
    pub fn recording(&self) -> bool {
        self.recorder.is_some()
    }
    fn poll(&mut self) -> Result<()> {
        let mut control = VergeControl::new(&mut self.aux);
        self.status.supply = control.read_target_power()?;
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
        if self.status.dio_reason.is_none() {
            let mut state = control.access_dio(0, 0, 0)?;
            if self.recorder.is_some() {
                state.read =
                    (state.read & !self.stream_mask) | (self.stream_levels & self.stream_mask);
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
                let mut control = VergeControl::new(&mut self.aux);
                let state = control.access_dio(0, 0, 0)?;
                if state.split(change.mask, change.outputs).is_some() {
                    self.status.dio = Some(state);
                    self.status.error =
                        Some("DIO0–2 must change direction together; DIO3 changes alone.".into());
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
                self.status.supply = VergeControl::new(&mut self.aux).set_target_power(on)?;
            }
            Command::Start { rate, path } => {
                if self.recording() {
                    bail!("a recording is already running");
                }
                self.poll()?;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let recorder = Recorder::create(&path)?;
                let mut control = VergeControl::new(&mut self.aux);
                let mask = if self.status.dio_reason.is_none() {
                    0x0f
                } else {
                    0
                };
                let started = if self.status.capturing {
                    control
                        .stream_energy_capture_identified(mask)
                        .map(|()| None)
                } else {
                    control
                        .start_energy_stream_identified(rate, mask)
                        .map(|start| start.capture_id)
                };
                if let Err(error) = started {
                    // Nothing was recorded: leave no empty capture among those kept.
                    drop(recorder);
                    if let Err(removing) = verge_probe::capture::remove(&path) {
                        tracing::warn!("removing {}: {removing}", path.display());
                    }
                    return Err(error.into());
                }
                self.capture_id = started.expect("start/attach succeeded");
                self.owned = !self.status.capturing;
                self.recorder = Some(recorder);
                self.status.path = Some(path);
                self.status.recording = true;
                self.synced = false;
                self.stream_mask = 0;
                self.status.edge_losses = 0;
                self.status.rate = None;
                self.status.capturing = true;
            }
            Command::Stop => self.ask_stop()?,
            Command::CancelSupply => self.status.confirm_off = false,
            Command::Connect(_) | Command::Disconnect => {}
        }
        Ok(())
    }
    fn update_levels(&mut self) {
        if let Some(state) = &mut self.status.dio {
            state.read = (state.read & !self.stream_mask) | (self.stream_levels & self.stream_mask);
        }
    }
    fn ask_stop(&mut self) -> Result<()> {
        if !self.recording() || self.stopping.is_some() {
            return Ok(());
        }
        self.tick()?;
        if !self.recording() {
            return Ok(());
        }
        let header_until = Instant::now() + Duration::from_secs(2);
        while self.capture_id.is_none() && self.recording() && Instant::now() < header_until {
            self.tick()?;
        }
        if !self.recording() {
            return Ok(());
        }
        let id = self
            .capture_id
            .ok_or_else(|| anyhow::anyhow!("capture identity has not arrived"))?;
        let mut control = VergeControl::new(&mut self.aux);
        let stopped = if self.owned {
            control.stop_energy_capture_if(id)
        } else {
            control.end_energy_stream_if(id)
        };
        match stopped {
            Ok(()) | Err(CommandError::Firmware(EnergyError::DmaError)) => {}
            Err(CommandError::Firmware(EnergyError::StaleCapture)) => self.owned = false,
            Err(error) => return Err(error.into()),
        }
        self.stopping = Some(Instant::now());
        Ok(())
    }
    /// Drain in short slices so controls stay responsive even at low sample rates.
    pub fn tick(&mut self) -> Result<bool> {
        let mut changed = false;
        if let Some(recorder) = self.recorder.as_mut() {
            recorder.check()?;
            for _ in 0..64 {
                let Some(message) = self.aux.message(Duration::from_millis(5))? else {
                    break;
                };
                if let Message::Header(header) = &message {
                    let id = header
                        .capture_id
                        .ok_or_else(|| anyhow::anyhow!("stream header has no capture identity"))?;
                    if self.capture_id.is_some_and(|expected| expected != id) {
                        bail!("stream header names another capture");
                    }
                    self.capture_id = Some(id);
                    self.synced = true;
                    self.status.rate = Some(header.rate);
                    self.stream_mask = header.dio_mask;
                    self.stream_levels = header.dio_levels;
                    self.update_levels();
                    changed = true;
                }
                if let Message::Edges(edges) = &message {
                    if edges.lost() {
                        self.status.edge_losses += 1;
                    }
                    if let Some(edge) = edges.edges.last() {
                        self.stream_levels = edge.levels;
                    }
                    self.update_levels();
                    changed = true;
                }
                if !self.synced {
                    continue;
                }
                let end = if let Message::End(end) = &message {
                    Some(end.reason)
                } else {
                    None
                };
                if end.is_some() {
                    self.owned = false;
                }
                self.recorder.as_mut().expect("recorder").send(message)?;
                if let Some(reason) = end {
                    self.owned = false;
                    self.capture_id = None;
                    self.stopping = None;
                    self.status.recording = false;
                    self.recorder.take().expect("recorder").finish()?;
                    if matches!(reason, EndReason::Failed) {
                        self.status.error =
                            Some("Capture failed: ADC overrun or DMA error.".into());
                    }
                    self.poll()?;
                    changed = true;
                    break;
                }
            }
            if self
                .stopping
                .is_some_and(|at| at.elapsed() > Duration::from_secs(2))
            {
                bail!("probe did not end its stream when asked");
            }
        }
        if Instant::now() >= self.poll_at {
            self.poll()?;
            changed = true;
        }
        Ok(changed)
    }
    pub fn fail(&mut self, error: anyhow::Error) {
        // A restart may already be queued behind the pair whose disk write failed.
        let pending_attach =
            self.recorder.is_some() && !self.synced && self.capture_id.is_none() && !self.owned;
        let drain_until = Instant::now()
            + if pending_attach {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(50)
            };
        while Instant::now() < drain_until {
            match self.aux.message(Duration::from_millis(1)) {
                Ok(Some(Message::Header(header)))
                    if self.recorder.is_some()
                        && !self.synced
                        && self.capture_id.is_none()
                        && !self.owned =>
                {
                    self.capture_id = header.capture_id;
                }
                Ok(Some(Message::End(_))) => {
                    self.owned = false;
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) if pending_attach && self.capture_id.is_none() => {}
                _ => break,
            }
        }
        let owned = self.owned;
        self.owned = false;
        self.stopping = None;
        self.status.recording = false;
        // The probe first: a disk that failed may also be slow to let the writer finish.
        let mut control = VergeControl::new(&mut self.aux);
        let cleanup = match self.capture_id.take() {
            Some(id) if owned => control.stop_energy_capture_if(id),
            Some(id) => control.end_energy_stream_if(id),
            None => Ok(()),
        };
        let written = self.recorder.take().map(Recorder::finish).transpose();
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
        if self.recorder.is_none() {
            return Ok(());
        }
        self.ask_stop()?;
        while self.recorder.is_some() {
            self.tick().context("ending the recording")?;
        }
        self.status.recording = false;
        Ok(())
    }
}

impl<L: verge_probe::aux::AuxLink> Drop for Session<L> {
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
    use verge_probe::{detect, sim::SimBus};
    fn session(name: &str) -> (SimBus, Session<verge_probe::sim::SimAux>) {
        let bus = SimBus::scenario(name).unwrap().paced(0.2);
        let unit = detect(&bus).unwrap().remove(0);
        let session = Session::open(&bus, &unit).unwrap();
        (bus, session)
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
        VergeControl::new(&mut s.aux)
            .start_energy_capture(1000)
            .unwrap();
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
        let path = dir.path().join("capture.vergecapture");
        assert!(s.command(Command::Start {
            rate: 1000,
            path: path.clone(),
        }));
        assert!(!s.owned);
        for _ in 0..3 {
            s.tick().unwrap();
        }
        assert!(s.command(Command::Stop));
        while s.recording() {
            s.tick().unwrap();
        }
        assert!(s.status().capturing);
        let archive = verge_probe::capture::Archive::open(&path).unwrap();
        assert!(archive.extent().unwrap().duration_ns() > 0);
    }
    #[test]
    fn own_capture_stops_and_debugger_does_not_block_aux() {
        let (_bus, mut s) = session("ours-pro-debugger");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.vergecapture");
        assert!(s.command(Command::Start {
            rate: 100_000,
            path: path.clone(),
        }));
        assert!(s.owned);
        for _ in 0..3 {
            s.tick().unwrap();
        }
        s.close().unwrap();
        assert!(!s.status().capturing);
        let archive = verge_probe::capture::Archive::open(&path).unwrap();
        assert_eq!(archive.header().unwrap().rate, 100_000);
    }
    #[test]
    fn restart_ends_recording_without_stopping_replacement_capture() {
        let (_bus, mut s) = session("ours-pro");
        let dir = tempfile::tempdir().unwrap();
        assert!(s.command(Command::Start {
            rate: 1000,
            path: dir.path().join("capture.vergecapture"),
        }));
        for _ in 0..2 {
            s.tick().unwrap();
        }
        VergeControl::new(&mut s.aux)
            .start_energy_capture(2000)
            .unwrap();
        while s.recording() {
            s.tick().unwrap();
        }
        assert!(!s.owned);
        assert!(s.status().capturing);
    }
    #[test]
    fn delayed_attach_header_is_bound_before_stop_or_failure_cleanup() {
        struct DelayedHeader {
            link: verge_probe::sim::SimAux,
            bus: SimBus,
            pending: Option<(Duration, Vec<u8>)>,
        }
        impl verge_probe::aux::AuxLink for DelayedHeader {
            fn write(&mut self, request: &[u8]) -> Result<(), verge_probe::dap::LinkError> {
                self.link.write(request)
            }
            fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, verge_probe::dap::LinkError> {
                if let Some((ready, _)) = &self.pending {
                    if self.bus.now() >= *ready {
                        return Ok(self.pending.take().unwrap().1);
                    }
                    self.bus.sleep(timeout);
                    return Err(verge_probe::dap::LinkError::Timeout);
                }
                let message = self.link.read(timeout)?;
                if message.first() == Some(&0xa1) {
                    self.pending = Some((self.bus.now() + Duration::from_millis(100), message));
                    self.bus.sleep(timeout);
                    return Err(verge_probe::dap::LinkError::Timeout);
                }
                Ok(message)
            }
        }
        for failure in [false, true] {
            let (bus, original) = session("ours-pro-energy");
            let status = original.status();
            drop(original);
            let device = detect(&bus).unwrap().remove(0).device;
            let mut s = Session {
                aux: Aux::open(DelayedHeader {
                    link: bus.open_aux(&device).unwrap(),
                    bus: bus.clone(),
                    pending: None,
                })
                .unwrap(),
                status,
                recorder: None,
                owned: false,
                capture_id: None,
                stopping: None,
                synced: false,
                poll_at: Instant::now(),
                stream_mask: 0,
                stream_levels: 0,
            };
            let dir = tempfile::tempdir().unwrap();
            assert!(s.command(Command::Start {
                rate: 1000,
                path: dir.path().join("capture.vergecapture")
            }));
            assert!(s.capture_id.is_none());
            if failure {
                s.fail(anyhow::anyhow!("disk failure"));
            } else {
                s.close().unwrap();
            }
            let log = bus.log();
            assert!(log.iter().any(|line| line.contains("AUX 900d")), "{log:?}");
            assert!(!log.iter().any(|line| line.contains("AUX 900b")), "{log:?}");
            let mut control = VergeControl::new(&mut s.aux);
            assert!(control.energy_capture_status().unwrap().running);
        }
    }

    #[test]
    fn attach_failure_before_first_tick_ends_only_its_stream() {
        let (_bus, mut s) = session("ours-pro-energy");
        let dir = tempfile::tempdir().unwrap();
        assert!(s.command(Command::Start {
            rate: 1000,
            path: dir.path().join("capture.vergecapture")
        }));
        assert!(s.capture_id.is_none());
        s.fail(anyhow::anyhow!("disk failure before first tick"));
        let mut control = VergeControl::new(&mut s.aux);
        assert!(control.energy_capture_status().unwrap().running);
        assert!(matches!(
            s.aux.message(Duration::from_millis(50)).unwrap(),
            Some(Message::End(_))
        ));
        assert!(s.aux.message(Duration::from_millis(50)).unwrap().is_none());
    }

    #[test]
    fn a_refused_start_leaves_no_capture_behind() {
        let (_bus, mut s) = session("ours-pro");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.vergecapture");
        assert!(s.command(Command::Start {
            rate: 0,
            path: path.clone(),
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
    use std::sync::atomic::AtomicBool;
    use verge_probe::{
        capture::{Archive, Interval},
        detect::detect_named,
        release::{GitHub, Repo, Ureq},
        usb::UsbBus,
    };

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
            if let Err(error) = verge_probe::pro::with_pro(
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
        use verge_probe::{DapTransport, Marked};
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
            let path = directory.join(format!("markers-{rate}.vergecapture"));
            assert!(session.command(Command::Start {
                rate,
                path: path.clone()
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
            let archive = Archive::open(&path).unwrap();
            let cancel = AtomicBool::new(false);
            let extent = archive.extent().unwrap();
            let markers = archive.markers(extent, None, 20, &cancel).unwrap();
            assert_eq!((sent, markers.total, markers.loss_events), (6, 6, 0));
            assert_eq!(
                markers
                    .entries
                    .iter()
                    .map(|entry| entry.marker.number)
                    .collect::<Vec<_>>(),
                (40..46).collect::<Vec<_>>()
            );
            let view = archive.view(extent, 800, &cancel).unwrap();
            assert!(view.statistics.samples >= 2 * u64::from(rate));
            assert_eq!(view.lost_pairs, 0);
            assert_eq!(
                archive.digital(extent, 800, &cancel).unwrap().loss_events,
                0
            );
            for csv in [true, false] {
                let output =
                    directory.join(format!("markers-{rate}.{}", if csv { "csv" } else { "sr" }));
                archive.export(&output, csv, extent, &cancel).unwrap();
                if csv {
                    let text = std::fs::read_to_string(output).unwrap();
                    for n in 0..6 {
                        assert!(text.contains(&format!("debug step,{n}")));
                    }
                }
            }
            eprintln!(
                "markers at {rate} pairs/s: {} pairs, {} lost; {} markers, {} loss reports",
                view.statistics.samples, view.lost_pairs, markers.total, markers.loss_events
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
            Err(verge_probe::bus::BusError::Busy { .. }) => None,
            Err(error) => panic!("opening CMSIS-DAP: {error}"),
        };
        let _restore = Restore(units[0].clone());
        let selected = Some(units[0].device.instance.clone());
        let repo: Repo = verge_probe::release::DEFAULT_REPO.parse().unwrap();
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
            let path = directory.join(format!("gui-{rate}.vergecapture"));
            h.state_mut().worker.send(Request::Bench(Command::Start {
                rate,
                path: path.clone(),
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
            let mut selection_verified = false;
            while Instant::now() < until_time {
                h.step();
                h.render().unwrap();
                frames += 1;
                if rate == 100_000
                    && old_selection.is_none()
                    && until_time.saturating_duration_since(Instant::now()) < Duration::from_secs(6)
                {
                    let archive = Archive::open(&path).unwrap();
                    let extent = archive.extent().unwrap();
                    let selected = Interval::new(extent.start_ns, extent.start_ns + 1_000_000_000);
                    let expected = archive
                        .statistics(selected, &AtomicBool::new(false))
                        .unwrap();
                    h.state_mut().bench.inspect(selected, Some(selected));
                    old_selection = Some((selected, expected));
                }
                assert!(h.state().bench.recording(), "{:?}", h.state().bench.status);
                if let Some((interval, expected)) = &old_selection
                    && let Some((shown, actual)) = h.state().bench.selection_statistics()
                    && shown == *interval
                {
                    assert_eq!(actual, expected);
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
            let archive = Archive::open(&path).unwrap();
            let extent = archive.extent().unwrap();
            let view = archive.view(extent, 800, &AtomicBool::new(false)).unwrap();
            assert!(view.ended);
            assert_eq!(view.lost_pairs, 0, "{rate} pairs/s lost samples");
            assert!(view.statistics.samples > u64::from(rate));
            assert!(view.statistics.mean_voltage_v().unwrap_or_default() > 1.0);
            assert!(view.statistics.mean_current_a().unwrap_or_default() > 0.0);
            assert_eq!(archive.header().unwrap().dio_mask, 15);
            let digital = archive
                .digital(extent, 800, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(digital.loss_events, 0);
            assert!(digital.bins.iter().any(|bin| bin.high & 8 != 0));
            assert!(digital.bins.iter().any(|bin| bin.low & 8 != 0));
            let csv_path = directory.join(format!("gui-{rate}.csv"));
            archive
                .export(&csv_path, true, extent, &AtomicBool::new(false))
                .unwrap();
            let edges = {
                use std::io::BufRead;
                let mut levels = archive.header().unwrap().dio_levels & 8 != 0;
                let mut edges = Vec::new();
                for line in std::io::BufReader::new(std::fs::File::open(csv_path).unwrap()).lines()
                {
                    let line = line.unwrap();
                    if !line.starts_with(',') {
                        continue;
                    }
                    let fields: Vec<_> = line.split(',').collect();
                    let high = fields[7] == "1";
                    if high != levels {
                        edges.push(fields[1].parse::<u64>().unwrap());
                        levels = high;
                    }
                }
                edges
            };
            assert!(edges.len() as f64 > extent.duration_ns() as f64 / 1e9 * 50.0);
            assert!(
                edges.windows(2).all(|w| w[1] - w[0] < 20_000_000),
                "a missing DIO3 transition"
            );
            eprintln!(
                "DIO: {} transitions at {rate} pairs/s, {} loss reports; first intervals {:?} ns",
                edges.len(),
                digital.loss_events,
                edges
                    .windows(2)
                    .take(4)
                    .map(|w| w[1] - w[0])
                    .collect::<Vec<_>>()
            );
            if let Some((selected, expected)) = old_selection {
                assert_eq!(
                    archive
                        .statistics(selected, &AtomicBool::new(false))
                        .unwrap(),
                    expected
                );
            }
            let selected = Interval::new(extent.start_ns, extent.start_ns + 1_000_000_000);
            let stats = archive
                .statistics(selected, &AtomicBool::new(false))
                .unwrap();
            assert!(stats.samples > 0);
            archive
                .export(
                    &directory.join(format!("gui-{rate}.sr")),
                    false,
                    extent,
                    &AtomicBool::new(false),
                )
                .unwrap();
            eprintln!(
                "rate {rate}: {} pairs, {} lost, {} rendered frames, mean {:.6} V / {:.6} A, {:.9} J; old selection {} pairs",
                view.statistics.samples,
                view.lost_pairs,
                frames,
                view.statistics.mean_voltage_v().unwrap_or_default(),
                view.statistics.mean_current_a().unwrap_or_default(),
                view.statistics.energy_j(),
                stats.samples
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
    use std::sync::atomic::AtomicBool;
    use verge_probe::capture::{Archive, Interval};
    use verge_probe::sim::SimBus;

    #[test]
    fn debugger_markers_survive_gui_recording_and_both_exports() {
        use verge_probe::{Bus, DapTransport, Marked};
        let bus = SimBus::scenario("ours-pro").unwrap().paced(0.1);
        let unit = verge_probe::detect(&bus).unwrap().remove(0);
        let mut session = Session::open(&bus, &unit).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("markers.vergecapture");
        assert!(session.command(Command::Start {
            rate: 1000,
            path: path.clone()
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
        let archive = Archive::open(&path).unwrap();
        let cancel = AtomicBool::new(false);
        let extent = archive.extent().unwrap();
        let page = archive.markers(extent, None, 10, &cancel).unwrap();
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.marker.number)
                .collect::<Vec<_>>(),
            [42, 43]
        );
        for csv in [true, false] {
            let out = dir
                .path()
                .join(if csv { "markers.csv" } else { "markers.sr" });
            archive.export(&out, csv, extent, &cancel).unwrap();
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
        let unit = verge_probe::detect(&bus).unwrap().remove(0);
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
            let unit = verge_probe::detect(&bus).unwrap().remove(0);
            let mut session = Session::open(&bus, &unit).unwrap();
            session.command(Command::Supply {
                on: true,
                confirmed: false,
            });
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("dio.vergecapture");
            assert!(session.command(Command::Start {
                rate: 1000,
                path: path.clone()
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
            let archive = Archive::open(&path).unwrap();
            assert_eq!(archive.header().unwrap().dio_mask, 15);
            let view = archive
                .digital(archive.extent().unwrap(), 500, &AtomicBool::new(false))
                .unwrap();
            assert!(view.bins.iter().any(|bin| bin.high & 8 != 0));
            assert!(view.bins.iter().any(|bin| bin.low & 8 != 0));
            assert_eq!(
                view.loss_events,
                u64::from(scenario == "ours-pro-edge-loss")
            );
            let selected = Interval::new(
                archive.extent().unwrap().start_ns,
                archive.extent().unwrap().end_ns,
            );
            archive
                .export(
                    &dir.path().join("dio.sr"),
                    false,
                    selected,
                    &AtomicBool::new(false),
                )
                .unwrap();
        }
    }
}
