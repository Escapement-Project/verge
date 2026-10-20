# verge

A tool to update the firmware on an MCU-Link. It works with NXP's stock CMSIS-DAP firmware and
with the firmware from the `mculink-verge` repository, which it calls ours.

It finds every attached MCU-Link and says what it runs, with the version. Then it picks the best
way to update each one:

- A unit running ours restarts into its bootloader when asked, so it needs no jumper.
- A unit running stock V3.122 or later does the same, on the request NXP's LinkServer sends.
- Anything else needs the ISP jumper. Fit it before plugging the unit in, because the bootloader
  only looks at the pin when the unit starts.

Ours comes from that repository's GitHub releases. Each release is signed, and the tool checks
the signature and the checksums before it flashes anything. Stock comes from your own LinkServer
install, since NXP's image is not ours to hand out.

Ours is built for the plain MCU-Link only. Stock names its board in its USB product string, and
ours names it in its identify reply. The tool refuses to put ours, a release or a file, on a unit
that says it is another board, such as an MCU-Link Pro. Stock's own image can go on any of them.
A unit in ISP mode cannot say which board it is, so `flash` cannot check it.

## Building

In this directory:

```
cargo build --release
```

The command line tool is `target/release/verge`, and the window is `target/release/verge-gui`.

On Linux, building needs the libudev headers (`libudev-dev` on Debian and Ubuntu,
`systemd-devel` on Fedora). The HID library that talks to the bootloader uses them.

## Using it

```
verge                       # list what is attached, and how each unit can be updated
verge update                # install the latest release of ours on the only unit
verge update --serial CJ3E7A91C2 --release v0.3.0
verge update --stock        # put NXP's firmware back
verge flash --file mculink_cjtag.elf
verge batch                 # update each unit as it is plugged in, until Ctrl-C
verge release               # the latest release, and how each unit compares
verge inspect image.s19     # what an image puts into flash; touches no USB device
```

`update` asks before it flashes. Pass `--yes` to skip the question, for example in a script.

`update` refuses to install a release the unit already runs, or one older than it. Pass
`--force` to install it anyway.

`flash` is for a unit you put into ISP mode with the jumper. With the jumper still on, the unit
comes back in ISP mode after flashing, and the tool tells you so. Unplug it, take the jumper off,
and plug it back in.

Every LPC55S69 bootloader looks the same over USB, so the tool flashes only while exactly one is
attached. It refuses while a second unit or an MCU-Link mini is in ISP mode, whichever unit you
asked for. It will not start a jumper-free update while anything sits in ISP mode either.

For the same reason, the tool cannot tell an MCU-Link in ISP mode from any other LPC55S69 board
in ISP mode. Before it flashes a unit that was in ISP mode already, its question says so.

The bootloader can take a few seconds to answer, and the bus can change meanwhile. Once it
answers, the tool looks again, and flashes only if it is still the one bootloader attached and
the one it checked. On Linux it also opens that very device, by its path.

After a flash the unit restarts, and the tool finds it again on the same USB port. Where the
system reports no port, the tool takes the unit back only if it is the one new device. If two
appear, it stops and says so.

Ctrl-C stops at the next safe point. A unit stopped halfway through a write stays in ISP mode,
and `verge flash` finishes the job.

### Batch updates

`verge batch` is for a delivery of probes. Choose the image and the units it takes, answer one
question, then plug units in one after another or several on a hub. Each unit the batch takes is
updated without another question.

```
# The latest release, onto stock and older builds of ours
verge batch
# Only stock older than V3.172
verge batch --take stock --stock-below 3.172
# A build of your own, onto any unit, stopping after 20
verge batch --file mculink_cjtag.elf --ours-any --count 20
# Units that arrive in ISP mode, with the jumper fitted
verge batch --take isp --yes
```

The image is a release, stock or a file, as for `update`. It is loaded once, when the batch is
armed, and a release's signature is checked then. Ctrl-C while it loads stops before anything
is armed. `--take` picks the kinds of unit: `stock`, `ours` and `isp`, with stock and ours the
default. A unit running ours is taken when its build is older than the image or carries no
version; `--ours-any` takes every build. Stock and a file carry no version of ours to compare
with, so with those a unit running ours is taken only with `--ours-any`. A unit already running
the image counts as current and is left as it is.

The batch takes only units plugged in after it is armed. `--include-attached` takes the ones
attached already too. A unit just plugged in gets a second to settle before the batch asks it
what it runs, as during an update. If it cannot be opened, the batch tries again for a few
polls more before it skips it.

The board is the full MCU-Link, and the batch takes only units that say they are one. A unit in
ISP mode cannot say, so `--take isp` takes any, and the question that arms the batch says so.

Units go through the bootloader one at a time. One that arrives while another is being updated
waits its turn, in the order they arrived. The batch pauses while anything in ISP mode is not the
next unit it means to flash: an MCU-Link mini, a second unit, or a unit the batch does not take.
It says why, and carries on once that unit is unplugged.

A unit is taken once. Updated, failed, skipped or back in ISP mode with its jumper fitted, it is
left alone until it is unplugged. A failure does not stop the batch, but a unit left in ISP mode
pauses it: unplug that unit, or flash it with `verge flash`.

A finished unit blinks its LEDs until you unplug it, and the batch names its USB port. The LEDs
blink only while the batch runs, so the unit that ends it is named without blinking. Each unit
gets a line once the batch has settled it, and the batch ends with a tally:

```
1-1      QK7RM2VTX8D41 -> CJ3E7A91C2  stock V3.172 -> ours, build v0.3.0  UPDATED in 4.7 s
1-1      blinking: unplug it when you like
1-3      CJ3E7AC4F5  ours, build v0.3.0  CURRENT
1-3      blinking: unplug it when you like
1-4      QK7RM2VTX8D45  stock V3.108  SKIPPED: needs the ISP jumper. Stock V3.108 has no jumper-free update. NXP added it in V3.122.
Batch done: 1 updated, 1 current, 1 skipped, 0 failed, 0 waiting.
```

Ctrl-C stops taking new units, and the unit under way finishes. A second Ctrl-C stops that unit
at its next safe point, and a third quits. `--count N` stops once N units are updated. The batch
exits with an error if any unit failed. Units still waiting for their turn when the batch stops
get a line in the record too.

The batch keeps a record, a file with one JSON object per line. `--record FILE` names it; it
defaults to `verge-batch-<time>.jsonl` in the current directory. A batch adds its lines to the
end of a file that exists already. A line looks like this:

```
{"time":"2026-10-20T14:34:58Z","port":"1-1","serial_before":"QK7RM2VTX8D41","firmware_before":"stock V3.172","serial_after":"CJ3E7A91C2","firmware_after":"ours, build v0.3.0","result":"updated","reason":null,"seconds":4.6795}
```

`result` is `updated`, `jumper-fitted`, `current`, `skipped`, `failed` or `waiting`. `reason`
says why for the last three.

The batch knows a unit by its USB port. A unit that reports no port cannot be followed through
its restart into the bootloader, so while one is attached the batch pauses and says so.

The batch notices a unit leave by looking while it is gone, every quarter of a second. On
Windows a unit that comes back keeps the identity it had, so one unplugged and plugged back in
between two looks counts as never having left.

### The window

`verge-gui` does the same in a window. The attached units are on the left. Pick one, and the
right side shows what it runs, the way it can be updated, and the image to install: the latest
release, the stock image from LinkServer, or a file. A file can also be dropped on the window,
except under Wayland, which does not pass dropped files on; use FILE and its dialog there.

The window asks before it flashes. While a unit is being written, it shows each step and the
bytes so far, and it will not close until the update finishes. CANCEL stops it at the next safe
point instead. The log along the bottom shows what the tool did; SHOW opens it.

BATCH, at the top, opens the batch view. It sets up the same batch as `verge batch`: the image,
the kinds of unit, whether to take the units attached now, a count to stop after, and the record
file. The record goes in the directory the window was started from unless you name another.
Once the batch is armed, the view shows the tally, the unit under way, the units waiting and a
line for each unit settled. STOP stops taking new units. CANCEL THE UNIT UNDER WAY then stops
that one at its next safe point. STOP, or closing the window, while the image is still loading
ends the batch at once, since nothing has been flashed.

### Releases

The tool looks for releases in `Escapement-Project/mculink-verge`, a placeholder until the
repository is public. Point it elsewhere with `--repo OWNER/NAME` or `VERGE_REPO`. Public
releases need no token. For a private repository, set `GITHUB_TOKEN`.

A lookup or download that stalls fails with a message. The tool allows 15 seconds to connect,
30 for GitHub to answer, and 60 for a download to finish.

A release has to carry three assets:

- exactly one `.s19` asset, the image as `objcopy -O srec` writes it;
- a checksum file named after it with `.sha256` added, in `sha256sum`'s format;
- a minisign signature named after it with `.minisig` added. Its trusted comment has to contain
  `file:` followed by the image's name, and `tag:` followed by the release's tag.

For example `mculink-verge-v0.3.0.s19`, `mculink-verge-v0.3.0.s19.sha256` and
`mculink-verge-v0.3.0.s19.minisig`. The tag is the version, and it is what a unit built from that
tag reports, so the tool can tell whether a unit is behind.

The tool checks the signature against the public key built into it. It refuses a release with no
signature, one signed with another key, and one changed after it was signed, and says which. The
trusted comment is signed too. Naming the file there stops a signed image being published under
another name. Naming the tag stops an old image being published again as a newer release under
its own name. The tool also checks the image against the checksum file and against the digest
GitHub keeps for every asset.

The key is `RELEASE_KEY` in `core/src/release.rs`, the base64 line of minisign's `.pub` file. It
is not set yet, so this build installs no release. A file or stock still works. To make the pair:

```
minisign -G -W -p verge.pub -s verge.key
```

`-W` leaves the secret key without a password, which the release workflow needs. Keep
`verge.key` out of the repository; it goes into the workflow's environment, below. The second
line of `verge.pub` goes into `RELEASE_KEY`.

#### The release workflow

The firmware repository's `.github/workflows/release.yml` publishes a release when a tag starting
with `v` is pushed. It has three jobs, so that the secret key never sits on a machine that ran
code the build fetched.

- `build` runs in an Arch Linux container, which has the same `arm-none-eabi-gcc` the firmware
  is built with. It installs that toolchain, CMake, Ninja, Python and uv from Arch's packages.
  It fetches TinyUSB's NXP dependencies, builds the firmware, runs `ninja -C build check`, and
  writes the image and its checksum. It has no secret.
- `sign` installs minisign from Ubuntu's packages and signs the image. It runs in the `release`
  environment, so it waits for a reviewer's approval and is the only job that sees the key.
- `publish` attaches the three files to the tag's release, creating the release if there is
  none. It has no secret either.

The `release` environment (the repository's Settings, Environments) needs:

- at least one required reviewer, who approves each signing;
- the environment secret `MINISIGN_SECRET_KEY`, holding the whole of `verge.key`. Keep it out of
  the repository's own secrets, where any job could ask for it. Without it the workflow fails
  rather than publish an unsigned image;
- optionally, the variable `MINISIGN_PUBLIC_KEY`, holding the second line of `verge.pub`. When it
  is set, the workflow checks the signature before it leaves the signing job;
- ideally, deployments limited to tags matching `v*`, so that no other ref can reach the key.

The workflow asks for permission to write the repository's contents in `publish` only. It has
never run, and cannot until the firmware repository is published.

To make the assets by hand instead:

```
arm-none-eabi-objcopy -O srec build/mculink_cjtag.elf mculink-verge-v0.3.0.s19
sha256sum mculink-verge-v0.3.0.s19 > mculink-verge-v0.3.0.s19.sha256
minisign -S -s verge.key -m mculink-verge-v0.3.0.s19 -t "file:mculink-verge-v0.3.0.s19 tag:v0.3.0"
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
sequence, image reading, release lookup and the batch. `cli` is the command line over it, and
`gui` the window, built with egui. The window does its USB work on a thread of its own.

It leans on established crates:

- `rblhost`, NXP's own Rust client for the ROM bootloader's protocol. LinkServer ships the same
  version as its `rblhost` tool. Its USB transport opens the first device with the bootloader's
  ID, so the tool gives it a transport of its own over `hidapi` that opens the device it chose.
- `nusb` for USB, the same crate probe-rs uses.
- `srec` and `object` to read S-records and ELF files.
- `ureq`, `serde_json` and `sha2` for releases, `minisign-verify` for their signatures, `clap` for
  the command line, and `tracing` with `tracing-indicatif` for logs and progress.
- `eframe` and `egui` for the window, `rfd` for its file dialog, and `egui_kittest` to test it.

The window's fonts are IBM Plex and Kode Mono, under the SIL Open Font License; `gui/fonts/` has
them with their licences and where each came from. The window's LICENCES button shows the licence
texts too.

The tool flashes whole 512-byte pages and verifies every page it writes. That matters on this
part: erased flash has no valid ECC, so a read past the image fails and drops the bootloader off
the bus.

## Testing

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

`.github/workflows/check.yml` runs all three on every push and pull request, and the Windows
check below. GitHub runs workflows only from a repository's root, so it runs once this directory
is a repository of its own.

No test opens a USB device. They run against a simulated bench (`core/src/sim/`): probes that
answer with the identities recorded from the real units, a bootloader that keeps flash's rules,
and a fake GitHub. Time on the simulated bench passes only when the tool waits. A test of a
fifteen-second timeout takes no real time, and units can be plugged in and out on a schedule.

The fake GitHub signs its releases with a key pair that the `minisign` crate makes when the bench
starts. The tool run against the bench trusts that key in place of `RELEASE_KEY`, and no secret
key is stored anywhere. One more test checks a signature that the minisign command line made,
taken from `minisign-verify`'s documentation.

The command line runs against the same bench with the hidden `--simulate` option, which is handy
while working on the tool:

```
cargo run -p verge -- --simulate bench
cargo run -p verge -- --simulate stock update --yes
cargo run -p verge -- --simulate arrivals batch --yes --count 3
```

The scenarios are `bench`, `ours`, `stock`, `stock-old`, `stock-hid`, `isp`, `isp-jumper`,
`two-isp`, `mini`, `isp-mini`, `busy`, `arrivals` and `pro`. In `arrivals`, five units are plugged
in over the first eight seconds. In `pro`, the one unit is an MCU-Link Pro. A simulated unit
boots only the simulator's own images. A real image flashed there, such as stock from your
LinkServer install, leaves the unit silent.

The window's tests drive it headless through `egui_kittest`, against the same bench. One more
renders every state to PNG, which needs a GPU:

```
VERGE_GUI_SHOTS=/tmp/shots cargo test -p verge-gui pictures -- --ignored
```

`verge-gui --simulate bench` opens the window on the simulated bench, labelled as such.

Two tests read real images and are skipped by default:

```
VERGE_FIRMWARE_ELF=<firmware checkout>/build/mculink_cjtag.elf \
VERGE_STOCK_S19=/opt/linkserver/MCU-LINK/probe_firmware/MCU-LINK_CMSIS-DAP_V3_172.s19 \
    cargo test -p verge-core --test image -- --ignored
```

They hold the image reader to `arm-none-eabi-objcopy`'s reading of the same files.

The Windows build is checked by compiling it, not by running it:

```
cargo check --workspace --all-targets --target x86_64-pc-windows-gnu
```

## Not done yet

- Variant releases. A release carries one image, so a build of ours with a variant suffix, such
  as `+armwire`, cannot be published beside the plain build.
- The J-Link firmware that LinkServer ships for the MCU-Link. The tool does not recognise a unit
  running it.
- Stock V2.x, which speaks HID only. Its USB product string has not been recorded from a real
  unit. The simulator's is a guess, and so is the version the tool reads from it.
