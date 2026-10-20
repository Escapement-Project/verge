# verge-update

A tool to update the firmware on an MCU-Link. It works with NXP's stock CMSIS-DAP firmware and
with the firmware in this repository, which it calls ours.

It finds every attached MCU-Link and says what it runs, with the version. Then it picks the best
way to update each one:

- A unit running ours restarts into its bootloader when asked, so it needs no jumper.
- A unit running stock V3.122 or later does the same, on the request NXP's LinkServer sends.
- Anything else needs the ISP jumper. Fit it before plugging the unit in, because the bootloader
  only looks at the pin when the unit starts.

Ours comes from this repository's GitHub releases. The tool checks each image's checksum before
it flashes anything. Stock comes from your own LinkServer install, since NXP's image is not ours
to hand out.

## Building

```
cd updater
cargo build --release
```

The command line tool is `target/release/verge-update`, and the window is
`target/release/verge-update-gui`.

On Linux, building needs the libudev headers (`libudev-dev` on Debian and Ubuntu,
`systemd-devel` on Fedora). The HID library that talks to the bootloader uses them.

## Using it

```
verge-update                       # list what is attached, and how each unit can be updated
verge-update update                # install the latest release of ours on the only unit
verge-update update --serial CJ3E7A91C2 --release v0.3.0
verge-update update --stock        # put NXP's firmware back
verge-update flash --file mculink_cjtag.elf
verge-update release               # the latest release, and how each unit compares
verge-update inspect image.s19     # what an image puts into flash; touches no USB device
```

`update` asks before it flashes. Pass `--yes` to skip the question, for example in a script.

`update` refuses to install a release the unit already runs, or one older than it. Pass
`--force` to install it anyway.

`flash` is for a unit you put into ISP mode with the jumper. With the jumper still on, the unit
comes back in ISP mode after flashing, and the tool tells you so. Unplug it, take the jumper off,
and plug it back in.

Only one unit may be in ISP mode at a time. Every LPC55S69 bootloader looks the same over USB, so
with two the tool cannot tell which one it would be flashing. It refuses, and it will not start a
jumper-free update while another unit sits in ISP mode either.

Ctrl-C stops at the next safe point. A unit stopped halfway through a write stays in ISP mode,
and `verge-update flash` finishes the job.

### The window

`verge-update-gui` does the same in a window. The attached units are on the left. Pick one, and
the right side shows what it runs, the way it can be updated, and the image to install: the latest
release, the stock image from LinkServer, or a file. A file can also be dropped on the window,
except under Wayland, which does not pass dropped files on; use FILE and its dialog there.

The window asks before it flashes. While a unit is being written, it shows each step and the
bytes so far, and it will not close until the update finishes. CANCEL stops it at the next safe
point instead. The log along the bottom shows what the tool did; SHOW opens it.

### Releases

The tool looks for releases in `CordlessCoder/mculink-cjtag-fw`, a placeholder until the
repository is public. Point it elsewhere with `--repo OWNER/NAME` or `VERGE_UPDATE_REPO`. Public
releases need no token. For a private repository, set `GITHUB_TOKEN`.

A release has to carry:

- exactly one `.s19` asset, the image as `objcopy -O srec` writes it;
- a checksum file named after it with `.sha256` added, in `sha256sum`'s format.

For example `mculink-cjtag-fw-v0.3.0.s19` and `mculink-cjtag-fw-v0.3.0.s19.sha256`. The tool
also checks the image against the digest GitHub keeps for every asset. The tag is the version,
and it is what a unit built from that tag reports, so the tool can tell whether a unit is behind.

To make the two assets:

```
arm-none-eabi-objcopy -O srec build/mculink_cjtag.elf mculink-cjtag-fw-v0.3.0.s19
sha256sum mculink-cjtag-fw-v0.3.0.s19 > mculink-cjtag-fw-v0.3.0.s19.sha256
```

### Stock images

`--stock` takes the newest `MCU-LINK_CMSIS-DAP_V*.s19` it finds in LinkServer's install, under
`/opt` or `/usr/local` on Linux, `/Applications` on macOS and `C:\NXP` on Windows. Name the
directory yourself with `--stock DIR`, or give any file with `--file`.

### USB permissions on Linux

Your user needs to open two devices: the probe's CMSIS-DAP interface and the bootloader's HID
interface. These udev rules allow it for the logged-in user:

```
# /etc/udev/rules.d/69-mculink-update.rules
# An MCU-Link running a CMSIS-DAP firmware, stock or ours
SUBSYSTEM=="usb", ATTRS{idVendor}=="1fc9", ATTRS{idProduct}=="0143", TAG+="uaccess"
# The LPC55S69 ROM bootloader, ISP mode
KERNEL=="hidraw*", ATTRS{idVendor}=="1fc9", ATTRS{idProduct}=="0021", TAG+="uaccess"
```

Then run `sudo udevadm control --reload` and plug the unit in again. probe-rs's udev rules cover
the first line already.

## How it is built

`core` is a library with no user interface: detection, update paths, the bootloader flash
sequence, image reading and release lookup. `cli` is the command line over it, and `gui` the
window, built with egui. The window does its USB work on a thread of its own.

It leans on established crates:

- `rblhost`, NXP's own Rust client for the ROM bootloader's protocol. LinkServer ships the same
  version as its `rblhost` tool.
- `nusb` for USB, the same crate probe-rs uses.
- `srec` and `object` to read S-records and ELF files.
- `ureq`, `serde_json` and `sha2` for releases, `clap` for the command line, and `tracing` with
  `tracing-indicatif` for logs and progress.
- `eframe` and `egui` for the window, `rfd` for its file dialog, and `egui_kittest` to test it.

The window's fonts are IBM Plex and Kode Mono, under the SIL Open Font License; `gui/fonts/`
has them with their licences and where each came from. The window's LICENCES button shows the
licence texts too.

The tool flashes whole 512-byte pages and verifies every page it writes. That matters on this
part: erased flash has no valid ECC, so a read past the image fails and drops the bootloader off
the bus.

## Testing

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

No test opens a USB device. They run against a simulated bench (`core/src/sim/`): probes that
answer with the identities recorded from the real units, a bootloader that keeps flash's rules,
and a fake GitHub. Time on the simulated bench passes only when the tool waits, so a test of a
fifteen-second timeout takes no real time.

The command line runs against the same bench with the hidden `--simulate` option, which is handy
while working on the tool:

```
cargo run -p updater-cli -- --simulate bench
cargo run -p updater-cli -- --simulate stock update --yes
```

The scenarios are `bench`, `ours`, `stock`, `stock-old`, `stock-hid`, `isp`, `isp-jumper`,
`two-isp`, `mini` and `busy`. A simulated unit boots only the simulator's own images, so a real
image flashed there leaves the unit silent.

The window's tests drive it headless through `egui_kittest`, against the same bench. One more
renders every state to PNG, which needs a GPU:

```
UPDATER_GUI_SHOTS=/tmp/shots cargo test -p updater-gui pictures -- --ignored
```

`verge-update-gui --simulate bench` opens the window on the simulated bench, labelled as such.

Two tests read real images and are skipped by default:

```
UPDATER_FIRMWARE_ELF=../build/mculink_cjtag.elf \
UPDATER_STOCK_S19=/opt/linkserver/MCU-LINK/probe_firmware/MCU-LINK_CMSIS-DAP_V3_172.s19 \
    cargo test -p updater-core --test image -- --ignored
```

They hold the image reader to `arm-none-eabi-objcopy`'s reading of the same files.
