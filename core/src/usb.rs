//! The real bus: `nusb` for enumeration and the CMSIS-DAP v2 interface, and NXP's McuBoot client
//! over `hidapi` for the ROM bootloader, which is a HID device.

use std::time::{Duration, Instant};

use mboot::protocols::ProtocolOpen;
use mboot::protocols::usb::USBProtocol;
use nusb::descriptors::TransferType;
use nusb::transfer::{
    Buffer, Bulk, ControlOut, ControlType, Direction, In, Out, Recipient, TransferError,
};
use nusb::{DeviceInfo, ErrorKind, MaybeFuture};

use crate::bus::{Bus, BusError, UsbDevice};
use crate::dap::{DapLink, LinkError};
use crate::ids;

const USB_CLASS_HID: u8 = 0x03;
const USB_CLASS_VENDOR: u8 = 0xFF;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_TIMEOUT: Duration = Duration::from_millis(50);
/// Erasing and writing a 4 KB chunk is one bootloader command; this bounds each reply.
const ROM_TIMEOUT: Duration = Duration::from_secs(10);

/// USB as the operating system presents it.
pub struct UsbBus {
    started: Instant,
}

impl Default for UsbBus {
    fn default() -> UsbBus {
        UsbBus {
            started: Instant::now(),
        }
    }
}

fn is_cmsis_dap(text: &str) -> bool {
    text.to_ascii_lowercase().contains("cmsis-dap")
}

fn describe(info: &DeviceInfo) -> UsbDevice {
    let product = info.product_string().map(str::to_owned);
    let dap_v2 = info.interfaces().any(|interface| {
        interface.class() == USB_CLASS_VENDOR
            && match interface.interface_string() {
                Some(name) => is_cmsis_dap(name),
                None => product.as_deref().is_some_and(is_cmsis_dap),
            }
    });
    let location = (!info.port_chain().is_empty()).then(|| {
        let ports: Vec<String> = info.port_chain().iter().map(u8::to_string).collect();
        format!("{}-{}", info.bus_id(), ports.join("."))
    });
    UsbDevice {
        vid: info.vendor_id(),
        pid: info.product_id(),
        location,
        instance: format!("{:?}", info.id()),
        serial: info.serial_number().map(str::to_owned),
        manufacturer: info.manufacturer_string().map(str::to_owned),
        product,
        dap_v2,
        hid: info
            .interfaces()
            .any(|interface| interface.class() == USB_CLASS_HID),
    }
}

impl UsbBus {
    fn find(&self, device: &UsbDevice) -> Result<DeviceInfo, BusError> {
        nusb::list_devices()
            .wait()
            .map_err(|error| BusError::Enumerate(error.to_string()))?
            .find(|info| format!("{:?}", info.id()) == device.instance)
            .ok_or_else(|| BusError::Gone {
                device: device.to_string(),
            })
    }
}

impl Bus for UsbBus {
    type Dap = NusbDap;
    type Isp = USBProtocol;

    fn devices(&self) -> Result<Vec<UsbDevice>, BusError> {
        Ok(nusb::list_devices()
            .wait()
            .map_err(|error| BusError::Enumerate(error.to_string()))?
            .filter(|info| ids::role(info.vendor_id(), info.product_id()).is_some())
            .map(|info| describe(&info))
            .collect())
    }

    fn open_dap(&self, device: &UsbDevice) -> Result<NusbDap, BusError> {
        let name = device.to_string();
        let open_error = |reason: String| BusError::Open {
            device: name.clone(),
            reason,
        };
        let info = self.find(device)?;
        let usb = info.open().wait().map_err(|error| match error.kind() {
            ErrorKind::Busy => BusError::Busy {
                device: name.clone(),
            },
            _ => open_error(error.to_string()),
        })?;
        let configuration = usb
            .active_configuration()
            .map_err(|error| open_error(error.to_string()))?;
        for interface in configuration.interfaces() {
            let number = interface.interface_number();
            let named_dap = info
                .interfaces()
                .find(|i| i.interface_number() == number)
                .is_some_and(|i| match i.interface_string() {
                    Some(name) => is_cmsis_dap(name),
                    None => device.product.as_deref().is_some_and(is_cmsis_dap),
                });
            for alt in interface.alt_settings() {
                if alt.class() != USB_CLASS_VENDOR || !named_dap {
                    continue;
                }
                let endpoints: Vec<_> = alt.endpoints().collect();
                let [out_ep, in_ep, ..] = endpoints.as_slice() else {
                    continue;
                };
                if out_ep.transfer_type() != TransferType::Bulk
                    || out_ep.direction() != Direction::Out
                    || in_ep.transfer_type() != TransferType::Bulk
                    || in_ep.direction() != Direction::In
                {
                    continue;
                }
                let (out_address, in_address) = (out_ep.address(), in_ep.address());
                let claimed =
                    usb.claim_interface(number)
                        .wait()
                        .map_err(|error| match error.kind() {
                            ErrorKind::Busy => BusError::Busy {
                                device: name.clone(),
                            },
                            _ => open_error(error.to_string()),
                        })?;
                let out = claimed
                    .endpoint::<Bulk, Out>(out_address)
                    .map_err(|error| open_error(error.to_string()))?;
                let input = claimed
                    .endpoint::<Bulk, In>(in_address)
                    .map_err(|error| open_error(error.to_string()))?;
                return Ok(NusbDap {
                    interface: claimed,
                    out,
                    input,
                });
            }
        }
        Err(open_error("it has no CMSIS-DAP v2 bulk interface".into()))
    }

    fn open_isp(&self, device: &UsbDevice) -> Result<USBProtocol, BusError> {
        USBProtocol::open_with_options(
            &format!("0x{:04x},0x{:04x}", device.vid, device.pid),
            0,
            ROM_TIMEOUT,
            Duration::from_millis(1),
        )
        .map_err(|error| BusError::Open {
            device: device.to_string(),
            reason: error.to_string(),
        })
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// A claimed CMSIS-DAP v2 interface. Dropping it releases the interface.
pub struct NusbDap {
    interface: nusb::Interface,
    out: nusb::Endpoint<Bulk, Out>,
    input: nusb::Endpoint<Bulk, In>,
}

fn link_error(error: TransferError) -> LinkError {
    match error {
        TransferError::Cancelled => LinkError::Timeout,
        TransferError::Disconnected => LinkError::Disconnected,
        other => LinkError::Usb(other.to_string()),
    }
}

impl NusbDap {
    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        // A reply as long as the packet ends without a short packet, so ask for exactly one packet.
        let len = self.input.max_packet_size();
        let completion = self.input.transfer_blocking(Buffer::new(len), timeout);
        completion
            .into_result()
            .map(Buffer::into_vec)
            .map_err(link_error)
    }
}

impl DapLink for NusbDap {
    fn exchange(&mut self, request: &[u8]) -> Result<Vec<u8>, LinkError> {
        self.out
            .transfer_blocking(Buffer::from(request), COMMAND_TIMEOUT)
            .into_result()
            .map_err(link_error)?;
        self.read(COMMAND_TIMEOUT)
    }

    fn drain(&mut self) -> Result<usize, LinkError> {
        let mut count = 0;
        loop {
            match self.read(DRAIN_TIMEOUT) {
                Ok(_) => count += 1,
                Err(LinkError::Timeout) => return Ok(count),
                Err(error) => return Err(error),
            }
        }
    }

    fn control_out(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
    ) -> Result<(), LinkError> {
        let control_type = match (request_type >> 5) & 0x3 {
            0 => ControlType::Standard,
            1 => ControlType::Class,
            2 => ControlType::Vendor,
            _ => {
                return Err(LinkError::Usb(format!(
                    "bmRequestType 0x{request_type:02x}"
                )));
            }
        };
        let recipient = match request_type & 0x1f {
            0 => Recipient::Device,
            1 => Recipient::Interface,
            2 => Recipient::Endpoint,
            _ => Recipient::Other,
        };
        if request_type & 0x80 != 0 {
            return Err(LinkError::Usb("an IN request on control_out".into()));
        }
        self.interface
            .control_out(
                ControlOut {
                    control_type,
                    recipient,
                    request,
                    value,
                    index,
                    data: &[],
                },
                COMMAND_TIMEOUT,
            )
            .wait()
            .map_err(link_error)
    }
}
