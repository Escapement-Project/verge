//! The CMSIS-DAP commands this tool sends, over any [`DapLink`].
//!
//! One command per packet; a reply's first byte echoes the command id, except that a firmware
//! answers a command it does not know with `ID_DAP_Invalid` (`0xFF`).

use crate::version::StockVersion;

pub const DAP_INFO: u8 = 0x00;
pub const DAP_DISCONNECT: u8 = 0x03;
pub const ID_DAP_INVALID: u8 = 0xFF;
pub const DAP_OK: u8 = 0x00;

/// Ours answers this with its identity; stock does not know it.
pub const VENDOR_IDENTIFY: u8 = 0x82;
pub const IDENTIFY_MAGIC: &[u8; 5] = b"CJTAG";

/// Stock's jumper-free way into the bootloader, as LinkServer's `probe update` sends it.
pub const STOCK_BOOTLOADER_REQUEST: [u8; 2] = [0x81, 0x01];
pub const STOCK_BOOTLOADER_ACCEPTED: [u8; 3] = [0x81, 0x00, 0x01];

/// Ours reboots on a vendor request on endpoint 0, answered by CPU 0 even while a DAP command
/// runs. `wValue` 1 asks for the bootloader, 0 for a plain reboot (`src/reboot_usb.h`).
pub const REBOOT_REQUEST_TYPE: u8 = 0x40;
pub const REBOOT_REQUEST: u8 = 0x03;
pub const REBOOT_TO_BOOTLOADER: u16 = 1;

/// `DAP_Info` fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Info {
    Vendor = 0x01,
    Product = 0x02,
    Serial = 0x03,
    Protocol = 0x04,
    FirmwareVersion = 0x09,
    PacketCount = 0xFE,
    PacketSize = 0xFF,
}

/// A transport failure, below the level of any one command.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("the probe did not answer in time")]
    Timeout,
    #[error("the probe left the bus")]
    Disconnected,
    #[error("USB error: {0}")]
    Usb(String),
}

/// A probe's CMSIS-DAP v2 bulk pair, plus endpoint 0 for vendor requests.
pub trait DapLink {
    /// Sends one command and reads its reply.
    fn exchange(&mut self, request: &[u8]) -> Result<Vec<u8>, LinkError>;

    /// Reads and discards replies an earlier host left queued, returning how many.
    ///
    /// Nothing in CMSIS-DAP resynchronises a bulk endpoint: an unread reply is handed to whatever
    /// asks next, and every answer after it is off by one.
    fn drain(&mut self) -> Result<usize, LinkError>;

    /// A vendor request on endpoint 0 with no data stage.
    fn control_out(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
    ) -> Result<(), LinkError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DapError {
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error("asked for command 0x{asked:02x} and the reply was {got}: the probe is out of step")]
    OutOfStep { asked: u8, got: String },
    #[error("the reply to command 0x{command:02x} was {len} bytes, too short to parse")]
    Short { command: u8, len: usize },
}

/// Sends a command and checks that the reply echoes it.
pub fn command(link: &mut dyn DapLink, request: &[u8]) -> Result<Vec<u8>, DapError> {
    let reply = link.exchange(request)?;
    match reply.first() {
        Some(&id) if id == request[0] => Ok(reply),
        other => Err(DapError::OutOfStep {
            asked: request[0],
            got: other.map_or_else(|| "empty".to_owned(), |id| format!("0x{id:02x}")),
        }),
    }
}

/// The value bytes of a `DAP_Info` field.
pub fn info(link: &mut dyn DapLink, field: Info) -> Result<Vec<u8>, DapError> {
    let reply = command(link, &[DAP_INFO, field as u8])?;
    let len = usize::from(*reply.get(1).ok_or(DapError::Short {
        command: DAP_INFO,
        len: reply.len(),
    })?);
    reply
        .get(2..2 + len)
        .map(<[u8]>::to_vec)
        .ok_or(DapError::Short {
            command: DAP_INFO,
            len: reply.len(),
        })
}

/// A string field, without its terminating NUL. An empty field is `None`.
pub fn info_string(link: &mut dyn DapLink, field: Info) -> Result<Option<String>, DapError> {
    let bytes = info(link, field)?;
    let text = bytes.split(|&b| b == 0).next().unwrap_or_default();
    Ok((!text.is_empty()).then(|| String::from_utf8_lossy(text).into_owned()))
}

/// What `DAP_Info` says about a probe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub protocol: Option<String>,
    pub firmware_version: Option<String>,
    pub packet_count: Option<u8>,
    pub packet_size: Option<u16>,
}

pub fn read_identity(link: &mut dyn DapLink) -> Result<Identity, DapError> {
    let packet_count = info(link, Info::PacketCount)?.first().copied();
    let packet_size = info(link, Info::PacketSize)?
        .get(..2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]));
    Ok(Identity {
        vendor: info_string(link, Info::Vendor)?,
        product: info_string(link, Info::Product)?,
        serial: info_string(link, Info::Serial)?,
        protocol: info_string(link, Info::Protocol)?,
        firmware_version: info_string(link, Info::FirmwareVersion)?,
        packet_count,
        packet_size,
    })
}

/// Ours, as its identify reply describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ours {
    /// The reply's format version: 2 carries flags and the build string.
    pub format: u8,
    pub flags: u8,
    /// `git describe` of the image; empty before format 2.
    pub build: String,
}

impl Ours {
    pub const FLAG_OSCAN1: u8 = 1 << 0;
    pub const FLAG_OSCAN1_ACTIVE: u8 = 1 << 1;
    pub const FLAG_TARGET_SENSE: u8 = 1 << 2;
    pub const FLAG_SWD_CODEGEN: u8 = 1 << 3;
    pub const FLAG_OSCAN1_ENTER: u8 = 1 << 4;
    pub const FLAG_JTAG_CODEGEN: u8 = 1 << 5;

    /// The capability flags by name, for display.
    #[must_use]
    pub fn flag_names(&self) -> Vec<&'static str> {
        [
            (Self::FLAG_OSCAN1, "OScan1"),
            (Self::FLAG_OSCAN1_ACTIVE, "OScan1 active"),
            (Self::FLAG_TARGET_SENSE, "target sense"),
            (Self::FLAG_SWD_CODEGEN, "generated SWD"),
            (Self::FLAG_OSCAN1_ENTER, "host-driven OScan1 entry"),
            (Self::FLAG_JTAG_CODEGEN, "generated JTAG"),
        ]
        .into_iter()
        .filter(|(bit, _)| self.flags & bit != 0)
        .map(|(_, name)| name)
        .collect()
    }
}

/// Reads a reply to [`VENDOR_IDENTIFY`]: `Some` for ours, `None` for anything else.
///
/// Stock answers `ID_DAP_Invalid` or a bare echo; any reply without the magic counts as stock.
pub fn parse_identify(reply: &[u8]) -> Result<Option<Ours>, DapError> {
    match reply.first() {
        Some(&ID_DAP_INVALID) => return Ok(None),
        Some(&VENDOR_IDENTIFY) => {}
        other => {
            return Err(DapError::OutOfStep {
                asked: VENDOR_IDENTIFY,
                got: other.map_or_else(|| "empty".to_owned(), |id| format!("0x{id:02x}")),
            });
        }
    }
    let body = &reply[1..];
    if body.get(..IDENTIFY_MAGIC.len()) != Some(IDENTIFY_MAGIC.as_slice()) {
        return Ok(None);
    }
    let short = || DapError::Short {
        command: VENDOR_IDENTIFY,
        len: reply.len(),
    };
    let format = *body.get(5).ok_or_else(short)?;
    if format < 2 {
        return Ok(Some(Ours {
            format,
            flags: 0,
            build: String::new(),
        }));
    }
    let flags = *body.get(6).ok_or_else(short)?;
    let len = usize::from(*body.get(7).ok_or_else(short)?);
    let build = body.get(8..8 + len).ok_or_else(short)?;
    Ok(Some(Ours {
        format,
        flags,
        build: String::from_utf8_lossy(build).into_owned(),
    }))
}

pub fn identify(link: &mut dyn DapLink) -> Result<Option<Ours>, DapError> {
    parse_identify(&link.exchange(&[VENDOR_IDENTIFY])?)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntryError {
    #[error("stock V{0} predates the jumper-free update (V3.122 and later); use the ISP jumper")]
    StockTooOld(StockVersion),
    #[error(
        "stock did not say which version it is, so the tool will not send it the update request"
    )]
    StockVersionUnknown,
    #[error("stock answered the update request with {0}, not 81 00 01; use the ISP jumper")]
    StockRefused(String),
    #[error(transparent)]
    Dap(#[from] DapError),
}

/// Asks stock to enter its bootloader: `81 01`, which must answer `81 00 01`, then
/// `DAP_Disconnect`. The unit leaves the bus about 2.1 s later.
///
/// Returns the reply to `DAP_Disconnect` when it was not `03 00`, so the caller can say so; by
/// then the request has been accepted and the unit may still leave the bus.
pub fn request_stock_bootloader(
    link: &mut dyn DapLink,
    version: Option<StockVersion>,
) -> Result<Option<String>, EntryError> {
    match version {
        None => return Err(EntryError::StockVersionUnknown),
        Some(version) if !version.has_jumper_free_update() => {
            return Err(EntryError::StockTooOld(version));
        }
        Some(_) => {}
    }
    let reply = link
        .exchange(&STOCK_BOOTLOADER_REQUEST)
        .map_err(DapError::from)?;
    if reply.get(..3) != Some(STOCK_BOOTLOADER_ACCEPTED.as_slice()) {
        return Err(EntryError::StockRefused(hex::encode(
            &reply[..reply.len().min(8)],
        )));
    }
    let disconnect = link.exchange(&[DAP_DISCONNECT]);
    Ok(match disconnect {
        Ok(reply) if reply.get(..2) == Some([DAP_DISCONNECT, DAP_OK].as_slice()) => None,
        Ok(reply) => Some(hex::encode(&reply[..reply.len().min(8)])),
        Err(error) => Some(error.to_string()),
    })
}

/// Asks ours to enter the ROM bootloader through its endpoint 0 request.
pub fn request_ours_bootloader(link: &mut dyn DapLink) -> Result<(), LinkError> {
    link.control_out(REBOOT_REQUEST_TYPE, REBOOT_REQUEST, REBOOT_TO_BOOTLOADER, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers from a fixed table, and records what it was asked.
    struct Table {
        replies: Vec<(Vec<u8>, Vec<u8>)>,
        asked: Vec<Vec<u8>>,
    }

    impl DapLink for Table {
        fn exchange(&mut self, request: &[u8]) -> Result<Vec<u8>, LinkError> {
            self.asked.push(request.to_vec());
            self.replies
                .iter()
                .find(|(req, _)| req == request)
                .map(|(_, reply)| reply.clone())
                .ok_or(LinkError::Timeout)
        }
        fn drain(&mut self) -> Result<usize, LinkError> {
            Ok(0)
        }
        fn control_out(&mut self, _: u8, _: u8, _: u16, _: u16) -> Result<(), LinkError> {
            Ok(())
        }
    }

    fn table(replies: &[(&[u8], &[u8])]) -> Table {
        Table {
            replies: replies
                .iter()
                .map(|(a, b)| (a.to_vec(), b.to_vec()))
                .collect(),
            asked: Vec::new(),
        }
    }

    #[test]
    fn identify_ours() {
        let mut reply = vec![0x82];
        reply.extend_from_slice(b"CJTAG");
        reply.extend_from_slice(&[2, 61, 7]);
        reply.extend_from_slice(b"14d05d4");
        let ours = parse_identify(&reply).unwrap().unwrap();
        assert_eq!(ours.format, 2);
        assert_eq!(ours.flags, 61);
        assert_eq!(ours.build, "14d05d4");
        assert_eq!(
            ours.flag_names(),
            [
                "OScan1",
                "target sense",
                "generated SWD",
                "host-driven OScan1 entry",
                "generated JTAG"
            ]
        );
    }

    #[test]
    fn identify_stock() {
        assert_eq!(parse_identify(&[0xFF]).unwrap(), None);
        assert_eq!(parse_identify(&[0x82]).unwrap(), None);
        assert_eq!(
            parse_identify(&[0x82, b'C', b'J', b'T', b'A', b'X']).unwrap(),
            None
        );
        assert!(matches!(
            parse_identify(&[0x00, 0x01]),
            Err(DapError::OutOfStep { asked: 0x82, .. })
        ));
    }

    #[test]
    fn identify_truncated_build_is_an_error() {
        let mut reply = vec![0x82];
        reply.extend_from_slice(b"CJTAG");
        reply.extend_from_slice(&[2, 61, 9]);
        reply.extend_from_slice(b"14d05d4");
        assert!(matches!(
            parse_identify(&reply),
            Err(DapError::Short { command: 0x82, .. })
        ));
    }

    #[test]
    fn info_strings_drop_the_terminator() {
        let mut link = table(&[(&[0x00, 0x03], b"\x00\x0bCJ3E7A91C2\x00")]);
        assert_eq!(
            info_string(&mut link, Info::Serial).unwrap().as_deref(),
            Some("CJ3E7A91C2")
        );
        let mut link = table(&[(&[0x00, 0x04], &[0x00, 0x00])]);
        assert_eq!(info_string(&mut link, Info::Protocol).unwrap(), None);
    }

    #[test]
    fn stock_entry_refuses_old_and_unknown_versions_without_sending() {
        let mut link = table(&[]);
        let old = StockVersion {
            major: 3,
            minor: 108,
        };
        assert_eq!(
            request_stock_bootloader(&mut link, Some(old)),
            Err(EntryError::StockTooOld(old))
        );
        assert_eq!(
            request_stock_bootloader(&mut link, None),
            Err(EntryError::StockVersionUnknown)
        );
        assert!(link.asked.is_empty());
    }

    #[test]
    fn stock_entry_sends_the_request_then_disconnect() {
        let mut link = table(&[
            (&[0x81, 0x01], &[0x81, 0x00, 0x01]),
            (&[0x03], &[0x03, 0x00]),
        ]);
        let version = StockVersion::parse("3.172");
        assert_eq!(request_stock_bootloader(&mut link, version), Ok(None));
        assert_eq!(link.asked, [vec![0x81, 0x01], vec![0x03]]);
    }

    #[test]
    fn stock_entry_refuses_any_other_reply() {
        let version = StockVersion::parse("3.172");
        for reply in [&[0x81, 0x00, 0x00][..], &[0x81, 0x01], &[0xFF], &[0x81]] {
            let mut link = table(&[(&[0x81, 0x01], reply), (&[0x03], &[0x03, 0x00])]);
            assert!(
                matches!(
                    request_stock_bootloader(&mut link, version),
                    Err(EntryError::StockRefused(_))
                ),
                "accepted {reply:02x?}"
            );
            assert_eq!(
                link.asked,
                [vec![0x81, 0x01]],
                "sent more after {reply:02x?}"
            );
        }
    }
}
