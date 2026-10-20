//! What every attached MCU-Link is running.

use std::fmt;

use crate::bus::{Bus, BusError, UsbDevice};
use crate::dap::{self, DapError, Identity, Ours};
use crate::ids::Role;
use crate::version::{Build, StockVersion};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Firmware {
    Ours(Ours),
    Stock { version: Option<StockVersion> },
}

/// What USB strings alone suggest about a probe the tool could not query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Guess {
    Ours,
    Stock(Option<StockVersion>),
    Unknown,
}

impl Guess {
    #[must_use]
    pub fn from_strings(device: &UsbDevice) -> Guess {
        let product = device.product.as_deref().unwrap_or_default();
        let serial = device.serial.as_deref().unwrap_or_default();
        if serial.starts_with("CJ") || product.contains("Verge") {
            Guess::Ours
        } else if product.to_ascii_uppercase().contains("MCU-LINK") {
            Guess::Stock(StockVersion::from_product_string(product))
        } else {
            Guess::Unknown
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Running a CMSIS-DAP firmware the tool could query.
    Probe {
        identity: Identity,
        firmware: Firmware,
    },
    /// Running a firmware the tool could not query.
    Unreadable { reason: String, guess: Guess },
    /// In ISP mode: the LPC55S69 ROM bootloader.
    Bootloader,
    /// An MCU-Link mini in ISP mode.
    MiniBootloader,
}

/// Which board a unit is, as far as what it reports says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hardware {
    /// The plain MCU-Link, the only board ours is built for.
    McuLink,
    /// Another board with an LPC55S69 MCU-Link on it, named for a person to read.
    Other(String),
    /// Nothing says which: a unit in ISP mode, or a firmware that does not report it.
    Unknown,
}

impl Hardware {
    /// The board in stock's USB product string, which LinkServer's Readme gives as
    /// `MCU-LINK [Pro|MR|on-board|<board>] (r<RRR>) CMSIS-DAP V<V>.<BBB>`. The plain board has
    /// nothing between `MCU-LINK` and `(r`.
    #[must_use]
    pub fn from_stock_product(product: &str) -> Hardware {
        let Some((board, _)) = product
            .strip_prefix("MCU-LINK ")
            .and_then(|rest| rest.split_once("(r"))
        else {
            return Hardware::Unknown;
        };
        match board.trim() {
            "" => Hardware::McuLink,
            "Pro" => Hardware::Other("an MCU-Link Pro".into()),
            "MR" => Hardware::Other("an MCU-Link MR".into()),
            "on-board" => Hardware::Other("an on-board MCU-Link".into()),
            name => Hardware::Other(format!("the MCU-Link on a {name}")),
        }
    }

    /// The board ours reports in its identify reply. Images before format 3 were built only for
    /// the plain board.
    #[must_use]
    pub fn from_ours(ours: &Ours) -> Hardware {
        match ours.board {
            None | Some(Ours::BOARD_MCU_LINK) => Hardware::McuLink,
            Some(Ours::BOARD_MCU_LINK_PRO) => Hardware::Other("an MCU-Link Pro".into()),
            Some(Ours::BOARD_ON_BOARD) => Hardware::Other("an on-board MCU-Link".into()),
            Some(other) => Hardware::Other(format!("board {other}, which this tool does not know")),
        }
    }
}

/// One attached MCU-Link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub device: UsbDevice,
    pub state: State,
}

impl Unit {
    /// The serial a user would pick this unit by: what `DAP_Info` says, else the USB string.
    #[must_use]
    pub fn serial(&self) -> Option<&str> {
        match &self.state {
            State::Probe { identity, .. } => identity.serial.as_deref(),
            _ => None,
        }
        .or(self.device.serial.as_deref())
    }

    /// The build ours runs, parsed.
    #[must_use]
    pub fn build(&self) -> Option<Build> {
        match &self.state {
            State::Probe {
                firmware: Firmware::Ours(ours),
                ..
            } => Some(Build::parse(&ours.build)),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_bootloader(&self) -> bool {
        self.state == State::Bootloader
    }

    /// Which board this is. Every LPC55S69 bootloader looks alike, so a unit in ISP mode is
    /// [`Hardware::Unknown`].
    #[must_use]
    pub fn hardware(&self) -> Hardware {
        match &self.state {
            State::Probe {
                firmware: Firmware::Ours(ours),
                ..
            } => Hardware::from_ours(ours),
            State::Probe { .. } | State::Unreadable { .. } => self
                .device
                .product
                .as_deref()
                .map_or(Hardware::Unknown, Hardware::from_stock_product),
            State::Bootloader => Hardware::Unknown,
            State::MiniBootloader => Hardware::Other("an MCU-Link mini".into()),
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            State::Probe {
                firmware: Firmware::Ours(ours),
                ..
            } if ours.build.is_empty() => write!(f, "ours, identify format {}", ours.format),
            State::Probe {
                firmware: Firmware::Ours(ours),
                ..
            } => write!(f, "ours, build {}", ours.build),
            State::Probe {
                firmware: Firmware::Stock { version: Some(v) },
                ..
            } => write!(f, "stock V{v}"),
            State::Probe {
                firmware: Firmware::Stock { version: None },
                ..
            } => write!(f, "stock, version not reported"),
            State::Unreadable { guess, .. } => match guess {
                Guess::Ours => write!(f, "probably ours, could not ask"),
                Guess::Stock(Some(v)) => write!(f, "probably stock V{v}, could not ask"),
                Guess::Stock(None) => write!(f, "probably stock, could not ask"),
                Guess::Unknown => write!(f, "a CMSIS-DAP firmware, could not ask"),
            },
            State::Bootloader => write!(f, "in ISP mode, waiting for an image"),
            State::MiniBootloader => write!(f, "an MCU-Link mini in ISP mode"),
        }
    }
}

/// Every attached MCU-Link and what it runs.
///
/// A probe is opened, asked `DAP_Info` and vendor command 0x82, and released. Both are read-only.
/// A bootloader is not opened.
pub fn detect<B: Bus>(bus: &B) -> Result<Vec<Unit>, BusError> {
    Ok(bus
        .devices()?
        .into_iter()
        .filter_map(|device| {
            let role = device.role()?;
            Some(query(bus, device, role))
        })
        .collect())
}

/// What one device runs, asked as [`detect`] asks every device. `None` for a device this tool
/// ignores.
pub fn query_device<B: Bus>(bus: &B, device: UsbDevice) -> Option<Unit> {
    let role = device.role()?;
    Some(query(bus, device, role))
}

fn query<B: Bus>(bus: &B, device: UsbDevice, role: Role) -> Unit {
    let state = match role {
        Role::Bootloader => State::Bootloader,
        Role::MiniBootloader => State::MiniBootloader,
        Role::Probe if !device.dap_v2 => State::Unreadable {
            reason: "it has no CMSIS-DAP v2 interface (stock V2.x speaks HID only)".into(),
            guess: Guess::from_strings(&device),
        },
        Role::Probe => match bus.open_dap(&device) {
            Ok(mut link) => match ask(&mut link, &device) {
                Ok(state) => state,
                Err(error) => State::Unreadable {
                    reason: error.to_string(),
                    guess: Guess::from_strings(&device),
                },
            },
            Err(BusError::Busy { .. }) => State::Unreadable {
                reason: "another program has it open".into(),
                guess: Guess::from_strings(&device),
            },
            Err(error) => State::Unreadable {
                reason: error.to_string(),
                guess: Guess::from_strings(&device),
            },
        },
    };
    Unit { device, state }
}

fn ask(link: &mut dyn dap::DapLink, device: &UsbDevice) -> Result<State, DapError> {
    let drained = link.drain()?;
    if drained > 0 {
        tracing::debug!("{device}: drained {drained} replies an earlier host left unread");
    }
    let identity = dap::read_identity(link)?;
    let firmware = match dap::identify(link)? {
        Some(ours) => Firmware::Ours(ours),
        None => Firmware::Stock {
            version: identity
                .firmware_version
                .as_deref()
                .and_then(StockVersion::parse)
                .or_else(|| {
                    device
                        .product
                        .as_deref()
                        .and_then(StockVersion::from_product_string)
                }),
        },
    };
    Ok(State::Probe { identity, firmware })
}
