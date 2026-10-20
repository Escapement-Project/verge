//! What the tool needs from USB, as a trait the real backend and the simulator both implement.

use std::fmt;
use std::time::Duration;

use mboot::protocols::Protocol;

use crate::dap::DapLink;
use crate::ids::{self, Role};

/// A device as enumeration describes it, before anything opens it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbDevice {
    pub vid: u16,
    pub pid: u16,
    /// The physical port, stable while the unit stays plugged into it, `None` where the platform
    /// does not say.
    pub location: Option<String>,
    /// This enumeration of the device. It changes when the device leaves the bus and comes back,
    /// even on the same port.
    pub instance: String,
    pub serial: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    /// Offers a vendor-class interface whose string contains "CMSIS-DAP": the v2 bulk pair.
    pub dap_v2: bool,
    /// Offers a HID interface, which is all stock V2.x and the ROM bootloader have.
    pub hid: bool,
}

impl UsbDevice {
    #[must_use]
    pub fn role(&self) -> Option<Role> {
        ids::role(self.vid, self.pid)
    }
}

impl fmt::Display for UsbDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}:{:04x}", self.vid, self.pid)?;
        if let Some(location) = &self.location {
            write!(f, " at {location}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BusError {
    #[error("could not list USB devices: {0}")]
    Enumerate(String),
    #[error("{device} is in use by another program")]
    Busy { device: String },
    #[error("{device} is no longer attached")]
    Gone { device: String },
    #[error("could not open {device}: {reason}")]
    Open { device: String, reason: String },
}

/// USB as the tool uses it.
///
/// Time is part of the bus so that the simulator can run an update's waits in simulated time;
/// every wait in the tool goes through [`Bus::sleep`].
pub trait Bus {
    type Dap: DapLink;
    type Isp: Protocol;

    /// Every device with an ID in [`ids::KNOWN_IDS`].
    fn devices(&self) -> Result<Vec<UsbDevice>, BusError>;
    /// Claims a probe's CMSIS-DAP v2 interface.
    fn open_dap(&self, device: &UsbDevice) -> Result<Self::Dap, BusError>;
    /// Opens the ROM bootloader's HID interface.
    fn open_isp(&self, device: &UsbDevice) -> Result<Self::Isp, BusError>;
    /// Time since an arbitrary start.
    fn now(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}
