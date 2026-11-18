//! `verge read`: target memory through a probe, printed as probe-rs's `read` prints it.

use std::io::{self, Write};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use escapement_verge_probe::probe_rs::MemoryInterface;
use escapement_verge_probe::probe_rs::probe::cmsisdap::ScanFormat;
use escapement_verge_probe::probe_rs::probe::list::Lister;
use escapement_verge_probe::probe_rs::probe::{DebugProbeInfo, ProbeTransport, WireProtocol};

#[derive(Args)]
pub struct ReadArgs {
    /// The target chip, as probe-rs names it.
    #[arg(long)]
    chip: String,
    /// The wire to the target.
    #[arg(long, value_enum, ignore_case = true, default_value_t = Wire::Swd)]
    protocol: Wire,
    /// The wire clock in kHz. The probe's default when left out.
    #[arg(long, value_name = "KHZ")]
    speed: Option<u32>,
    /// The cJTAG scan format to connect in. OScan1 when left out. Nothing checks that the target
    /// has the format: ask only for one it has.
    #[arg(long, value_enum, ignore_case = true, value_name = "FORMAT")]
    cjtag_format: Option<CjtagFormat>,
    /// Print the attach and read times on stderr.
    #[arg(long)]
    time: bool,
    /// The width of each value.
    #[arg(value_enum, ignore_case = true)]
    width: Width,
    /// The first address: decimal, 0x hexadecimal, 0o octal or 0b binary.
    #[arg(value_parser = parse_int::parse::<u64>)]
    address: u64,
    /// How many values to read.
    words: usize,
}

/// At a terminal with several probes attached, the serial of the one the user picks. It asks
/// probe-rs, which lists every probe it can drive, not only MCU-Links.
fn pick_probe() -> Result<Option<String>> {
    if !crate::question::interactive() {
        return Ok(None);
    }
    let probes = Lister::new().list_all();
    if probes.len() < 2 {
        return Ok(None);
    }
    let items: Vec<String> = probes
        .iter()
        .map(|probe| {
            let serial = serial_number(probe).unwrap_or("-");
            format!("{serial:<14} {}", probe.identifier)
        })
        .collect();
    let index = crate::question::choose("Which probe?", &items)?;
    let probe = &probes[index];
    let serial = serial_number(probe).map(str::to_owned).with_context(|| {
        format!(
            "{} reports no serial, so it cannot be picked: unplug the other probes",
            probe.identifier
        )
    })?;
    Ok(Some(serial))
}

fn serial_number(probe: &DebugProbeInfo) -> Option<&str> {
    let ProbeTransport::Usb(usb) = &probe.transport;
    usb.serial_number.as_deref()
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Wire {
    Swd,
    Jtag,
    Cjtag,
}

impl From<Wire> for WireProtocol {
    fn from(wire: Wire) -> Self {
        match wire {
            Wire::Swd => Self::Swd,
            Wire::Jtag => Self::Jtag,
            Wire::Cjtag => Self::CJtag,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum CjtagFormat {
    Oscan1,
    Oscan2,
    Oscan5,
    Oscan6,
}

impl From<CjtagFormat> for ScanFormat {
    fn from(format: CjtagFormat) -> Self {
        match format {
            CjtagFormat::Oscan1 => Self::OScan1,
            CjtagFormat::Oscan2 => Self::OScan2,
            CjtagFormat::Oscan5 => Self::OScan5,
            CjtagFormat::Oscan6 => Self::OScan6,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Width {
    B8,
    B16,
    B32,
    B64,
}

/// Refuses options that do not go together, before any probe is opened.
pub fn check(args: &ReadArgs) -> Result<()> {
    if args.cjtag_format.is_some() && args.protocol != Wire::Cjtag {
        anyhow::bail!("--cjtag-format needs --protocol cjtag");
    }
    Ok(())
}

pub fn read(args: &ReadArgs, serial: Option<&str>) -> Result<()> {
    check(args)?;
    let picked = match serial {
        Some(_) => None,
        None => pick_probe()?,
    };
    let started = Instant::now();
    let mut session = escapement_verge_probe::attach(
        serial.or(picked.as_deref()),
        &args.chip,
        args.protocol.into(),
        args.speed,
        args.cjtag_format.map(Into::into),
    )?;
    let attached = Instant::now();
    let data = read_memory(
        &mut session.core(0).context("opening core 0")?,
        args.width,
        args.address,
        args.words,
    )
    .with_context(|| format!("reading {} values from {:#x}", args.words, args.address))?;
    let read = Instant::now();
    // probe-rs's read leaves every core running, whatever state the attach found it in.
    for (index, _) in session.list_cores() {
        let mut core = session.core(index)?;
        if core.core_halted()? {
            core.run()?;
        }
    }

    let mut out = io::stdout().lock();
    write_hex_table(&mut out, args.address, args.width, &data)
        .map_err(crate::output::Error::from)?;
    out.flush().map_err(crate::output::Error::from)?;
    if args.time {
        writeln!(
            io::stderr().lock(),
            "attach {:.6} s, read {:.6} s, {} bytes",
            (attached - started).as_secs_f64(),
            (read - attached).as_secs_f64(),
            data.len()
        )?;
    }
    Ok(())
}

/// Reads `count` values of `width` at `address`, as little-endian bytes.
fn read_memory(
    core: &mut impl MemoryInterface<escapement_verge_probe::probe_rs::Error>,
    width: Width,
    address: u64,
    count: usize,
) -> Result<Vec<u8>> {
    Ok(match width {
        Width::B8 => {
            let mut values = vec![0; count];
            core.read_8(address, &mut values)?;
            values
        }
        Width::B16 => {
            let mut values = vec![0; count];
            core.read_16(address, &mut values)?;
            values.iter().flat_map(|v| v.to_le_bytes()).collect()
        }
        Width::B32 => {
            let mut values = vec![0; count];
            core.read_32(address, &mut values)?;
            values.iter().flat_map(|v| v.to_le_bytes()).collect()
        }
        Width::B64 => {
            let mut values = vec![0; count];
            core.read_64(address, &mut values)?;
            values.iter().flat_map(|v| v.to_le_bytes()).collect()
        }
    })
}

/// The table probe-rs's `read` prints by default, byte for byte, so its output can be compared
/// with a reference taken with probe-rs.
fn write_hex_table(
    out: &mut impl Write,
    address: u64,
    width: Width,
    data: &[u8],
) -> io::Result<()> {
    let (value_bytes, line_bytes) = match width {
        Width::B8 => (1, 8),
        Width::B16 => (2, 16),
        Width::B32 => (4, 32),
        Width::B64 => (8, 32),
    };
    for (line, chunk) in data.chunks(line_bytes).enumerate() {
        write!(out, "{:08x}: ", address + (line * line_bytes) as u64)?;
        for (index, value) in chunk.chunks(value_bytes).enumerate() {
            if index > 0 {
                write!(out, " ")?;
            }
            for byte in value.iter().rev() {
                write!(out, "{byte:02x}")?;
            }
        }
        writeln!(out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(address: u64, width: Width, data: &[u8]) -> String {
        let mut out = Vec::new();
        write_hex_table(&mut out, address, width, data).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_table_matches_probe_rs_byte_for_byte() {
        // A line probe-rs read printed for flash at 0 on a CC1312R1.
        let words = [
            0x2001_3a00u32,
            0x0001_fb29,
            0x0002_09a9,
            0x0002_09a9,
            0x0002_09a9,
            0x0002_09a9,
            0x0002_09a9,
            0x0002_09a9,
            0x0002_09a9,
        ];
        let data: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(
            table(0, Width::B32, &data),
            "00000000: 20013a00 0001fb29 000209a9 000209a9 000209a9 000209a9 000209a9 000209a9\n\
             00000020: 000209a9\n"
        );
    }

    #[test]
    fn each_width_fills_its_line_and_advances_the_address_by_it() {
        let data: Vec<u8> = (0..40).collect();
        assert_eq!(
            table(0x1000, Width::B8, &data[..10]),
            "00001000: 00 01 02 03 04 05 06 07\n00001008: 08 09\n"
        );
        assert_eq!(
            table(0x1000, Width::B16, &data[..18]),
            "00001000: 0100 0302 0504 0706 0908 0b0a 0d0c 0f0e\n00001010: 1110\n"
        );
        assert_eq!(
            table(0x1000, Width::B64, &data),
            "00001000: 0706050403020100 0f0e0d0c0b0a0908 1716151413121110 1f1e1d1c1b1a1918\n\
             00001020: 2726252423222120\n"
        );
        assert_eq!(table(0, Width::B32, &[]), "");
    }
}
