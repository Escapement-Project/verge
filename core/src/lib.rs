//! Detection, update paths, the ROM bootloader flash sequence, firmware images and release lookup
//! for an MCU-Link running NXP's CMSIS-DAP firmware or this repository's. No user interface.
//!
//! Everything that touches USB goes through [`bus::Bus`]: [`usb::UsbBus`] is the real one and
//! [`sim::SimBus`] a simulated bench, which the tests and `--simulate` use.

pub mod bus;
pub mod dap;
pub mod detect;
pub mod ids;
pub mod image;
pub mod isp;
pub mod paths;
pub mod progress;
pub mod release;
pub mod sim;
pub mod source;
pub mod stock;
pub mod update;
pub mod usb;
pub mod version;

pub use bus::{Bus, UsbDevice};
pub use detect::{Firmware, State, Unit, detect};
pub use image::Image;
pub use paths::{Offer, Path, offer};
pub use progress::{Cancel, Progress, Step};
pub use source::{Loaded, Source};
pub use update::{Outcome, Timing, UpdateError};
