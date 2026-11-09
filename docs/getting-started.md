# Getting started

Verge is open CMSIS-DAP firmware for NXP's MCU-Link and MCU-Link Pro debug probes. It runs on the
standalone boards. It does not run on an MCU-Link built into an evaluation board.

You install it with verge, a command-line tool. verge also updates Verge, puts NXP's firmware back,
and drives the Pro's extras. Its desktop app, verge-gui, does the same in a window.

## What you need

- A standalone MCU-Link or MCU-Link Pro, and a USB cable.
- verge, for Linux, Windows or macOS.
- NXP's LinkServer, installed, if you want to put NXP's firmware back later. verge takes NXP's image
  from it. The tool never downloads NXP's firmware, and the project does not distribute it.

## Install verge

Each verge release has one archive per system:

- Linux on x86_64, with glibc 2.35 or newer;
- Windows on x86_64;
- macOS, one program for both Apple silicon and Intel.

An archive holds both programs, `verge` and `verge-gui`, with `LICENSE` and
`THIRD-PARTY-LICENSES.html`, the licence of every crate inside. Beside each archive are a `.sha256`
checksum and a minisign signature, made with the same key that signs the firmware. To check an
archive before you unpack it:

```
minisign -V -P <the public key> -m verge-v0.1.0-x86_64-linux.tar.gz
```

To build verge from source instead, see [Building](../README.md#building) in its README.

### USB permissions on Linux

On Linux, your user has to be allowed to open two devices: the probe's CMSIS-DAP interface, and the
bootloader's HID interface that an update talks to. These udev rules allow it for whoever is logged
in:

```
# /etc/udev/rules.d/69-mculink-update.rules
# An MCU-Link running a CMSIS-DAP firmware, NXP's or Verge
SUBSYSTEM=="usb", ATTRS{idVendor}=="1fc9", ATTRS{idProduct}=="0143", TAG+="uaccess"
# daplink-rs, under Arm's DAPLink ID
SUBSYSTEM=="usb", ATTRS{idVendor}=="0d28", ATTRS{idProduct}=="0204", TAG+="uaccess"
# The LPC55S69 ROM bootloader, ISP mode
KERNEL=="hidraw*", ATTRS{idVendor}=="1fc9", ATTRS{idProduct}=="0021", TAG+="uaccess"
```

Save them as that file, run `sudo udevadm control --reload`, and plug the probe in again. If you
already use probe-rs, its udev rules cover the first line.

## See what is attached

```
verge
```

On its own, `verge` lists every MCU-Link attached: its USB port and serial number, the firmware it
runs, its board, and how verge can update it. `-v` adds what the firmware says about itself. Nothing
changes on the probe.

## Put Verge on the probe

```
verge update
```

`update` installs the latest Verge release on the probe. It checks the release's signature and
checksum before it writes anything. It asks before it downloads, showing the repository, the tag and
the size, and asks again before it flashes. `--yes` answers both questions for you.

verge's release key is not set yet, so verge refuses every release for now. Until it is, install a
build of your own with `verge update --file`, as [Building the
firmware](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/README.md) describes.

A probe running NXP's firmware from V3.122 on updates without a jumper. So does a probe already
running Verge. Older NXP firmware needs the ISP jumper, fitted before you plug the probe in. verge
tells you which case you have, and [Updating and recovery](updating.md) covers each one.

With more than one probe attached, name the one you mean with its serial number:

```
verge --serial CJ3E7A91C2 update
```

At a terminal, verge asks which one instead.

## Check it

Run `verge` again. The probe now lists as running Verge, with its build, such as `Verge, build
v0.3.0`.

Any CMSIS-DAP host can use the probe. Verge keeps the USB vendor and product IDs NXP's firmware
uses, `0x1FC9:0x0143`, so a tool that knows the MCU-Link finds it. [Probes and
jumpers](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/probes.md#how-the-probe-shows-up)
says how it shows up in a listing. [Wires and
clocks](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/wires.md) says which
wires and clocks it offers.

## Put NXP's firmware back

```
verge update --stock
```

verge takes the newest `MCU-LINK_CMSIS-DAP_V*.s19` it finds in your LinkServer install: under `/opt`
or `/usr/local` on Linux, `/Applications` on macOS and `C:\NXP` on Windows. `--stock DIR` looks in a
directory you name instead, and `--file` takes any image file.

## Where next

- [The verge command](command.md): every command and its options.
- [The desktop app](desktop-app.md): updates and the Pro's bench, in a window.
- [Probes and
  jumpers](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/probes.md): what each
  probe has, and what each jumper turns off.
