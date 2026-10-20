//! An update from start to finish: into the bootloader, flash, and back to the firmware.

use std::time::Duration;

use mboot::McuBoot;

use crate::bus::{Bus, BusError, UsbDevice};
use crate::dap::{self, DapLink, EntryError, LinkError};
use crate::detect::{self, Firmware, State, Unit};
use crate::ids::Role;
use crate::image::{Image, ImageError};
use crate::isp::{self, IspError};
use crate::paths::{self, Path};
use crate::progress::{Cancel, Progress, Step};

/// How long each wait lasts, in the bus's time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timing {
    /// How often a wait looks at the bus.
    pub poll: Duration,
    /// From the entry request to the bootloader enumerating. Stock takes about 2.4 s.
    pub bootloader_appears: Duration,
    /// For the bootloader to answer once it has enumerated.
    pub rom_answers: Duration,
    /// From the reset to the firmware enumerating. UM11126 allows five seconds to start.
    pub firmware_returns: Duration,
    /// Between the firmware enumerating and the tool asking it what it runs.
    pub settle: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            poll: Duration::from_millis(250),
            bootloader_appears: Duration::from_secs(15),
            rom_answers: Duration::from_secs(10),
            firmware_returns: Duration::from_secs(15),
            settle: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    StockEntry(#[from] EntryError),
    #[error("ours did not take the bootloader request ({0}). Use the ISP jumper")]
    OursEntry(LinkError),
    #[error(transparent)]
    Isp(#[from] IspError),
    #[error("{0}")]
    Refused(String),
    #[error("the unit did not come back as the bootloader within {0:?}. Use the ISP jumper")]
    NoBootloader(Duration),
    #[error("the bootloader enumerated but did not answer within {waited:?}: {last}")]
    RomSilent { waited: Duration, last: String },
    #[error(
        "the unit did not come back within {0:?} of the reset. If it stays away, fit the ISP jumper and flash it again"
    )]
    NoReturn(Duration),
    #[error("cancelled")]
    Cancelled,
}

/// How a flashed unit came back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Running a firmware, as this unit.
    Running(Box<Unit>),
    /// In ISP mode again: the jumper is still fitted. Unplug it, take the jumper off, plug it in.
    JumperStillFitted,
}

/// Picks the unit to act on: the one with this serial, or the only one attached.
pub fn select<'a>(units: &'a [Unit], serial: Option<&str>) -> Result<&'a Unit, UpdateError> {
    let found: Vec<&Unit> = match serial {
        Some(serial) => units
            .iter()
            .filter(|u| u.serial() == Some(serial))
            .collect(),
        None => units.iter().collect(),
    };
    match (found.as_slice(), serial) {
        ([unit], _) => Ok(unit),
        ([], Some(serial)) => Err(UpdateError::Refused(format!(
            "no attached MCU-Link has serial {serial}"
        ))),
        ([], None) => Err(UpdateError::Refused("no MCU-Link is attached".into())),
        (several, _) => {
            let serials: Vec<&str> = several.iter().map(|u| u.serial().unwrap_or("?")).collect();
            Err(UpdateError::Refused(format!(
                "{} MCU-Links are attached ({}); pick one by serial",
                several.len(),
                serials.join(", ")
            )))
        }
    }
}

fn wait_until<B: Bus, T>(
    bus: &B,
    timing: &Timing,
    limit: Duration,
    cancel: &Cancel,
    mut look: impl FnMut(&[UsbDevice]) -> Option<T>,
) -> Result<Option<T>, UpdateError> {
    let deadline = bus.now() + limit;
    loop {
        if cancel.is_cancelled() {
            return Err(UpdateError::Cancelled);
        }
        if let Some(found) = look(&bus.devices()?) {
            return Ok(Some(found));
        }
        let now = bus.now();
        if now >= deadline {
            return Ok(None);
        }
        // The last look falls on the deadline, so nothing arriving after it is accepted.
        bus.sleep(timing.poll.min(deadline - now));
    }
}

fn same_port(a: &UsbDevice, b: &UsbDevice) -> bool {
    match (&a.location, &b.location) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// Asks a running firmware to restart into its bootloader, and waits for the bootloader.
pub fn enter_bootloader<B: Bus>(
    bus: &B,
    unit: &Unit,
    timing: &Timing,
    progress: &mut dyn Progress,
    cancel: &Cancel,
) -> Result<UsbDevice, UpdateError> {
    let all = detect::detect(bus)?;
    let offer = paths::offer(unit, &all);
    let path = offer
        .paths
        .iter()
        .copied()
        .find(|path| matches!(path, Path::OursJumperFree | Path::StockJumperFree));
    let Some(path) = path else {
        let mut reason = "this unit has no jumper-free path into its bootloader".to_owned();
        for note in &offer.notes {
            reason.push_str(". ");
            reason.push_str(note.trim_end_matches('.'));
        }
        return Err(UpdateError::Refused(reason));
    };

    progress.start(Step::EnterBootloader, 0);
    {
        let mut link = bus.open_dap(&unit.device)?;
        link.drain()
            .map_err(dap::DapError::from)
            .map_err(EntryError::from)?;
        match (path, &unit.state) {
            (Path::OursJumperFree, _) => {
                dap::request_ours_bootloader(&mut link).map_err(UpdateError::OursEntry)?;
            }
            (
                _,
                State::Probe {
                    firmware: Firmware::Stock { version },
                    ..
                },
            ) => {
                if let Some(odd) = dap::request_stock_bootloader(&mut link, *version)? {
                    progress.note(&format!(
                        "Stock accepted the request, and DAP_Disconnect then answered {odd}. Waiting for the bootloader anyway."
                    ));
                }
            }
            _ => unreachable!("offer gives the stock path only to stock"),
        }
    }

    progress.start(Step::WaitForBootloader, 0);
    let found = wait_until(bus, timing, timing.bootloader_appears, cancel, |devices| {
        devices
            .iter()
            .find(|d| d.role() == Some(Role::Bootloader) && same_port(d, &unit.device))
            .cloned()
    })?;
    found.ok_or(UpdateError::NoBootloader(timing.bootloader_appears))
}

/// The one unit in ISP mode, or why there is not exactly one.
pub fn only_bootloader(devices: &[UsbDevice]) -> Result<UsbDevice, UpdateError> {
    let roms: Vec<&UsbDevice> = devices
        .iter()
        .filter(|d| d.role() == Some(Role::Bootloader))
        .collect();
    match roms.as_slice() {
        [rom] => Ok((*rom).clone()),
        [] if devices.iter().any(|d| d.role() == Some(Role::MiniBootloader)) => Err(UpdateError::Refused(
            "the unit in ISP mode is an MCU-Link mini, a different board; this firmware is for the full MCU-Link".into(),
        )),
        [] => Err(UpdateError::Refused(
            "no unit is in ISP mode. Fit the ISP jumper, then plug the unit in: the bootloader samples the pin at reset, so a jumper fitted afterwards does nothing".into(),
        )),
        several => Err(UpdateError::Refused(format!(
            "{} units are in ISP mode and every LPC55S69 bootloader looks the same. Unplug all but one",
            several.len()
        ))),
    }
}

/// Flashes the one unit in ISP mode, verifies it, and resets it. Returns the bootloader's device.
pub fn flash_bootloader<B: Bus>(
    bus: &B,
    image: &Image,
    timing: &Timing,
    progress: &mut dyn Progress,
    cancel: &Cancel,
) -> Result<UsbDevice, UpdateError> {
    image.check_for_mculink()?;
    let rom = only_bootloader(&bus.devices()?)?;

    progress.start(Step::Connect, 0);
    let deadline = bus.now() + timing.rom_answers;
    let (mut boot, info) = loop {
        if cancel.is_cancelled() {
            return Err(UpdateError::Cancelled);
        }
        let last = match bus.open_isp(&rom) {
            Ok(protocol) => {
                let mut boot = McuBoot::new(protocol);
                match isp::rom_info(&mut boot) {
                    Ok(info) => break (boot, info),
                    Err(error) => error.to_string(),
                }
            }
            Err(error) => error.to_string(),
        };
        if bus.now() >= deadline {
            return Err(UpdateError::RomSilent {
                waited: timing.rom_answers,
                last,
            });
        }
        bus.sleep(timing.poll * 2);
    };
    tracing::info!("bootloader {} answers", info.version);

    isp::program(&mut boot, image, &info, progress, cancel)?;
    progress.start(Step::Reset, 0);
    isp::reset(&mut boot);
    Ok(rom)
}

/// Waits for a reset unit to leave the bus and come back, and says what it came back as.
pub fn wait_for_return<B: Bus>(
    bus: &B,
    rom: &UsbDevice,
    timing: &Timing,
    progress: &mut dyn Progress,
    cancel: &Cancel,
) -> Result<Outcome, UpdateError> {
    progress.start(Step::WaitForFirmware, 0);
    let start = bus.now();
    let left = wait_until(bus, timing, timing.firmware_returns, cancel, |devices| {
        (!devices.iter().any(|d| d.instance == rom.instance)).then_some(devices.to_vec())
    })?;
    let Some(others) = left else {
        return Err(UpdateError::NoReturn(timing.firmware_returns));
    };
    let remaining = timing.firmware_returns.saturating_sub(bus.now() - start);
    let back = wait_until(bus, timing, remaining, cancel, |devices| {
        devices
            .iter()
            .find(|d| same_port(d, rom) && !others.iter().any(|o| o.instance == d.instance))
            .cloned()
    })?;
    let Some(device) = back else {
        return Err(UpdateError::NoReturn(timing.firmware_returns));
    };
    match device.role() {
        Some(Role::Bootloader) => Ok(Outcome::JumperStillFitted),
        _ => {
            bus.sleep(timing.settle);
            let units = detect::detect(bus)?;
            let unit = units
                .into_iter()
                .find(|u| u.device.instance == device.instance)
                .ok_or(UpdateError::NoReturn(timing.firmware_returns))?;
            Ok(Outcome::Running(Box::new(unit)))
        }
    }
}

/// The jumper-free update: into the bootloader, flash, reset, and back.
pub fn update<B: Bus>(
    bus: &B,
    unit: &Unit,
    image: &Image,
    timing: &Timing,
    progress: &mut dyn Progress,
    cancel: &Cancel,
) -> Result<Outcome, UpdateError> {
    image.check_for_mculink()?;
    progress.plan(&[
        Step::EnterBootloader,
        Step::WaitForBootloader,
        Step::Connect,
        Step::Erase,
        Step::Write,
        Step::Verify,
        Step::Reset,
        Step::WaitForFirmware,
    ]);
    enter_bootloader(bus, unit, timing, progress, cancel)?;
    let rom = flash_bootloader(bus, image, timing, progress, cancel)?;
    wait_for_return(bus, &rom, timing, progress, cancel)
}

/// Installs `image` on `unit` by the best path its state allows: the jumper-free update, or a
/// flash when it is in ISP mode already. Refuses, saying why, when neither is open to it.
pub fn install<B: Bus>(
    bus: &B,
    unit: &Unit,
    image: &Image,
    timing: &Timing,
    progress: &mut dyn Progress,
    cancel: &Cancel,
) -> Result<Outcome, UpdateError> {
    let units = detect::detect(bus)?;
    let offer = paths::offer(unit, &units);
    match offer.best() {
        Some(Path::FlashNow) => flash_now(bus, image, timing, progress, cancel),
        Some(Path::OursJumperFree | Path::StockJumperFree) => {
            update(bus, unit, image, timing, progress, cancel)
        }
        Some(Path::Jumper) => Err(UpdateError::Refused(format!(
            "this unit has no jumper-free update. {} To use the jumper: unplug the unit, fit the ISP jumper, plug it back in, and flash it",
            offer.notes.join(" ")
        ))),
        None => Err(UpdateError::Refused(format!(
            "this unit cannot be updated as it is. {}",
            offer.notes.join(" ")
        ))),
    }
}

/// Flashes the unit that is in ISP mode already, as after fitting the jumper.
pub fn flash_now<B: Bus>(
    bus: &B,
    image: &Image,
    timing: &Timing,
    progress: &mut dyn Progress,
    cancel: &Cancel,
) -> Result<Outcome, UpdateError> {
    progress.plan(&[
        Step::Connect,
        Step::Erase,
        Step::Write,
        Step::Verify,
        Step::Reset,
        Step::WaitForFirmware,
    ]);
    let rom = flash_bootloader(bus, image, timing, progress, cancel)?;
    wait_for_return(bus, &rom, timing, progress, cancel)
}
