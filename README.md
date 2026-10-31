# verge

A CLI and GUI to control the Verge firmware on MCU-Links, or update it. Verge is the firmware in
`mculink-verge`, and both tools are built on `verge-probe`.

To update, the tool lists attached MCU-Links with their firmware versions and board types, then
selects an update path for each:

- A unit running Verge restarts into its bootloader on request, without a jumper.
- A unit running stock V3.122 or later supports the same request that NXP's LinkServer uses.
- Anything else needs the ISP jumper. Fit it before plugging the unit in, because the bootloader
  reads the pin at startup.

Verge releases come from `mculink-verge` on GitHub. The tool verifies signatures and checksums
before flashing. Stock images come from your local LinkServer installation; we do not distribute
NXP's firmware.

## Boards

Stock reports its board in the USB product string. Verge reports it in its identification reply.
The tool recognises the standalone MCU-Link and MCU-Link Pro and lists each unit's board.

Verge images declare which boards they support (see "Board declarations"). An image without a
declaration supports only the standalone MCU-Link, matching builds made before declarations were
introduced. The tool checks both releases and local files against the unit's board. It rejects
unsupported boards and unknown board IDs, such as an MCU-Link MR. Stock images can be installed
on any board supported by stock.

A unit in ISP mode does not report its board. `verge isp flash` lists the image's supported boards
in its confirmation prompt, but cannot check the unit against them. For a known board other than
the standalone MCU-Link, `verge isp enter` requires `--stock`; follow it with
`verge isp flash --stock`. To install a Verge image on that board, use
`verge update --file IMAGE`. It checks board support before entering ISP mode.

When an update needs the ISP jumper or fails partway through, the tool gives a recovery command.
It names the board, if known, and identifies the image by release tag, file path or `--stock`.
`update` checks board support before giving jumper instructions. The library checks it again
before requesting bootloader entry.

A running unit that does not report its board requires an interactive confirmation.
`--yes` does not bypass this check; run `update` in a terminal.

## The SWD jumper

The SWD jumper hands the target's debug lines to another debugger. Stock and Verge both remove
their CMSIS-DAP interface while it is fitted. The tool then uses USB descriptors to identify the
firmware family, jumper state and board, where reported. It cannot identify the Verge build.

Stock still names its board in the product string. Verge names only the Pro there; its standalone
and on-board MCU-Link strings are identical. With the SWD jumper fitted, a standalone MCU-Link
running Verge therefore requires board confirmation before an update.

A unit running Verge can still update without the ISP jumper through a USB device request.
On Windows, remove the SWD jumper first: the request needs a claimed interface.
Stock's update request uses CMSIS-DAP, so stock V3.122 or later also needs the SWD jumper removed.
Older stock requires the ISP jumper. Batch updates skip units with the SWD jumper fitted.

## Building

From the repository's root:

```
cargo build --release
```

The CLI is `target/release/verge`; the GUI is `target/release/verge-gui`.

On Linux, building needs the libudev headers (`libudev-dev` on Debian and Ubuntu,
`systemd-devel` on Fedora). The HID library that talks to the bootloader uses them.

### Releases of verge

`.github/workflows/release.yml` builds verge and verge-gui when a tag `v<version>` is pushed, the
version matching the workspace version in `Cargo.toml`. Each release has one archive per platform:
x86_64 Linux (glibc 2.35 or newer), x86_64 Windows, and macOS for both Apple silicon and Intel in
one binary. Each archive holds both tools, `LICENSE`, and `THIRD-PARTY-LICENSES.html` with the
licence of every included crate. Each archive has a `.sha256` file and a minisign signature made
with the firmware release key. To verify an archive:

```
minisign -V -P <the public key> -m verge-v0.1.0-x86_64-linux.tar.gz
```

Signing uses a `release` environment with a required reviewer. Configure it as described in
mculink-verge's README under "Releases".

## Using it

```
verge                       # list what is attached, and how each unit can be updated
verge update                # install the latest Verge release on the only unit
verge --serial CJ3E7A91C2 update --release v0.3.0
verge update --stock        # put NXP's firmware back
verge isp enter             # send a unit into ISP mode without the jumper, for `verge isp flash`
verge isp flash --file mculink_cjtag.elf
verge isp reset             # start the image a unit in ISP mode holds, without flashing it
verge isp info              # report the ROM bootloader version
verge batch                 # update each unit as it is plugged in, until Ctrl-C
verge release               # the latest release, and how each unit compares
verge inspect image.s19     # what an image puts into flash; touches no USB device
verge cjtag --oscan1-only   # offer a host two-pin OScan1 alone, until the unit restarts
verge dio --high all        # drive all four of a Pro's DIO pins high
verge power on              # switch on a Pro's target supply
verge energy watch          # a Pro's target current and energy, live
verge energy record --out run.sr --for 10s   # every pair of a capture, for PulseView
verge read --chip CC1312R --protocol cjtag b32 0 64   # target memory, as probe-rs read prints it
```

A command acts on one unit. `--serial` names it, before or after the command. Without it, the tool
takes the only attached unit the command can act on. When several could be meant, it asks at a
terminal and lists them; elsewhere it stops and asks for `--serial`. A Pro command such as `energy`
looks only at Pros running Verge, so a plain board beside a Pro needs no serial. `list` and
`release` show only the unit `--serial` names.

`update` asks before downloading a release and shows the repository, tag and size. It asks again
before flashing. Pass `--yes` to skip both prompts. Without `--yes`, a download requires an
interactive terminal. `verge isp flash` uses the same prompts. Cached releases need no download
confirmation (see "Releases").

`cjtag` lists the cJTAG scan formats a unit running Verge offers. The host selects a format
supported by both the probe and target. `--oscan1-only` limits the offer to OScan1 for capture
analysis. `--formats` selects formats by name or number, such as `--formats oscan5` or
`--formats 9,13`. Use it to compare formats; OScan1 always remains available. `--all` or a restart
removes the limit.

`dio` reads and drives the four DIO pins on an MCU-Link Pro's J19 header (pins 2 to 5), on a unit
running Verge. With no options it reports each pin's state. `--high` and `--low` make pins
outputs at that level, and `--input` makes them inputs. Name pins as `0` to `3`, `dio0` to `dio3`,
or `all`. DIO0, DIO1 and DIO2 share a direction; DIO3 is independent.
`--high 0` alone is rejected while DIO1 and DIO2 are inputs. The pins keep their
state between debug sessions, so one can hold a target's boot mode through a reset, and they go
back to inputs when the unit restarts. The Pro's LED6 (SIO) is lit while any pin is driven.

`power` reads or switches an MCU-Link Pro's target supply, on a unit running Verge. The supply
reaches the target through J6, at 3.3 V, or at 1.8 V with J5 fitted, and gives up to 400 mA. It is
off whenever the Pro starts. The tool reports the state the firmware set, not a measured voltage.

`energy` measures the current that flows from J9 pin 1 to pin 3 of an MCU-Link Pro running Verge,
and the voltage on the target's side. `energy start` starts a capture at `--rate` voltage and
current pairs a second, 1,000 by default. The capture runs in the probe until `energy stop`, so a
debugger can use the probe meanwhile. `power` and `energy` keep working while a debugger has the
probe open: verge reaches them through Verge Aux, a second USB interface that debuggers leave alone.
A Verge image older than that interface answers only once the debugger lets go. `energy latest`
shows the newest pair, and `energy total` the energy since the start. The total counts only the time
the probe converts: it leaves out the moments it holds conversions, as when it times a new debug
connection at a fast clock. A stopped capture keeps its total until the next start or a restart.
`energy calibration` lists the points each current range was calibrated at. A start can calibrate
first, which disconnects J9 from the target for about 0.2 s. A target powered through J9 loses its
supply for that time.

`energy watch` shows a capture live, and starts one if none runs. Over a capture that a fault
stopped, it shows that capture's total and then starts a new one, which clears it. It stops only a
capture it started. The screen shows the newest pair, the total and the mean power, over two graphs.
Through Verge Aux, the current graph shows every pair, as the lowest and highest in each column. A
Verge image older than Verge Aux gives only the newest pair at each read, so the graph can miss
short peaks. The power graph plots each interval's mean from the total, which counts every pair. Q,
Esc or Ctrl-C leaves, and `--for` leaves after a set time. Where the output is not a terminal, it
prints a line for each read instead.

`energy record --out FILE` saves every pair of a capture. It reads them through Verge Aux, so it
works while a debugger holds the probe. Like `watch`, it starts a capture if none runs and stops only
a capture it started. It records until Ctrl-C, or for as long as `--for` says. The file is a sigrok
session, which PulseView opens. It has two analog channels: V in volts and mA in milliamps. A session
file stores no units, so sigrok shows both as volts, and the channel names give the units. The
probe measures nothing for a moment while it reads the target's supply or times a debugger's reads.
There the last pair repeats and a logic channel, GAP, is high, so the time axis stays true. A pair
taken as the current range switched carries the next pair's current, as in the probe's total, so the
recording adds up to that total. `--csv` writes one line per pair instead: its sequence number, its
time in ns since the capture started, millivolts and nanoamps. A Verge image older than Verge Aux
cannot record.

Markers from a debug session keep their own times. CSV gives each marker a row with its number
and label, in time order among pairs and edges. A sigrok session includes `markers.csv`; PulseView
does not draw it as an annotation track. Resaving with sigrok drops that extra file, so keep the
original session to retain markers. Labels keep UTF-8 text. Backslashes, control characters
and other bytes are escaped. `marker_lost` reports missing markers; an empty time means the probe
reported loss without a marker to locate it. Those unlocated reports apply to the whole capture,
including a selection export. CSV writes pairs and edges as they arrive, then merges markers when
recording ends. Before that merge, an interrupted recording keeps the pairs and edges written
so far. The final CSV rewrite is not atomic. The sigrok
sidecar also gives each marker's `session_time_ns`, relative to the exported session's first
sample. It can be negative when a marker precedes that sample.

`read` reads target memory through a probe over SWD, JTAG or two-pin cJTAG, and prints the table
probe-rs's `read` prints: each line is an address and eight values, four for `b64`. It picks among
every probe probe-rs can drive, not only MCU-Links. Set the clock in kHz with `--speed`. `--time`
prints the attach and read times on stderr. Like probe-rs's `read`, it resumes any halted core when
it finishes.

`update` refuses to install a release the unit already runs, or one older than it. Pass
`--force` to install it anyway.

## ISP mode

`verge isp` works with a unit's ROM bootloader. `enter` requests bootloader entry without the
jumper, `flash` writes an image, `reset` starts the installed image, and `info` reports the
bootloader version.

LPC55S69 bootloaders share a USB identity. By default, `flash`, `reset` and `info` require exactly
one unit in ISP mode. On Linux, use `--port` with a port listed by `verge` to select one of several.
Windows and macOS cannot select a bootloader by port; unplug all but one.
`flash` also refuses while an MCU-Link mini is in ISP mode, even with `--port`.
On Linux, `enter` works with another unit in ISP mode and names the port to flash. `update` and
batch updates wait until that unit is flashed or unplugged.

Use `flash` after entering ISP mode with the jumper or `enter`. If the unit returns in ISP mode
after flashing, its ISP pin is still held low. Check the ISP jumper and any probe attached to its
debug connector. An MCU-Link target connector carries ISP on pin 7, connected to the NXP board's
ISP pin. Remove the jumper or disconnect the probe, then run `verge isp reset`.
If `reset` returns a unit to ISP mode without a preceding flash, it may also lack a bootable image.

The tool cannot distinguish an MCU-Link in ISP mode from another LPC55S69 board. Its confirmation
prompt states this before flashing a unit already in ISP mode.

The bootloader can take a few seconds to respond. The tool checks the USB bus again before
flashing and proceeds only if it would still select the same bootloader. On Linux it also opens
that device by its path.

After flashing, the tool finds the restarted unit on the same USB port. If the system reports no
port, exactly one new device must appear. If two appear, the tool stops with an error.

If the bootloader does not restart after flashing, Linux and macOS report that it never left the
bus. The image is written and verified; unplug and reconnect the unit to start it. Windows cannot
distinguish this from a fast restart with the ISP jumper fitted. Unplug the unit, remove the
jumper if fitted, then reconnect it. In either case the unit is not counted as updated.

Ctrl-C stops at the next safe point. An interrupted write leaves the unit in ISP mode.
Retry with `verge isp flash` and the same image, named in the recovery instructions.
Ctrl-C before bootloader entry leaves the unit unchanged.

### Batch updates

`verge batch` updates multiple probes. Choose an image and the unit types to include, confirm,
then connect probes one at a time or through a hub. Each eligible unit is updated without another
prompt. The confirmation includes any required release download.

```
# The latest release, onto stock and older Verge builds
verge batch
# Only stock older than V3.172
verge batch --take stock --stock-below 3.172
# A build of your own, onto any unit, stopping after 20
verge batch --file mculink_cjtag.elf --verge-any --count 20
# Units that arrive in ISP mode, with the jumper fitted
verge batch --take isp --yes
```

The image can be a release, stock or a file, as for `update`. It is loaded and its release
signature verified once, before the batch is armed. Ctrl-C during loading stops before any unit
is updated. `--take` selects unit types: `stock`, `verge` and `isp`; the default is `stock,verge`.
A unit running Verge is eligible if its build is older than the image or has no version.
`--verge-any` accepts every build. A unit already running the image counts as current and is left
unchanged.

Stock and local files have no release version for comparison. With either source, a batch that
selects older Verge builds requires `--verge-any`. Otherwise, exclude Verge units with an option
such as `--take stock`. The tool checks this before looking for the image.

The batch accepts units connected after it is armed. `--include-attached` also includes units
already connected. As with a single update, the batch waits one second before identifying a newly
connected unit. If opening it fails, the batch retries for several polls before skipping it.

`--board` selects `mcu-link` (the default) or `mcu-link-pro`. The batch accepts units reporting that
board. A Verge image must declare support for it before the batch can arm; stock images allow
either selection. Units in ISP mode do not report their board. `--take isp` accepts any of them,
as stated in the confirmation prompt.

Updates run one at a time, in arrival order. The batch pauses if another device is in ISP mode:
an MCU-Link mini, a second unit or an excluded unit. It reports the reason and resumes once that
device is disconnected.

A unit is processed once per connection, whether updated, failed, skipped or left in ISP mode.
A failure does not stop the batch. A unit left in ISP mode pauses it until disconnected or
flashed again with `verge isp flash` and the batch image. The pause message names the board and
image. A bootloader that did not restart also pauses the batch; reconnect the unit to start its
new image.

If a unit does not return after an update, the batch continues. If it later returns on the same
port running the image, it counts as current. If it returns with its original firmware and serial
number, the batch retries it once, in arrival order. A second failure leaves it excluded until
the batch ends. A different serial number identifies a separate unit on the same port.
Units in ISP mode have no serial number, so identification relies on the port.

Finished units blink their LEDs until disconnected or the batch ends. The batch lists their USB
ports. The final unit is listed without blinking because the batch has ended.
Each completed unit gets a result line, followed by a final tally:

```
1-1      QK7RM2VTX8D41 -> CJ3E7A91C2  stock V3.172 -> Verge, build v0.3.0  UPDATED in 4.7 s
1-1      blinking: unplug it when you like
1-3      CJ3E7AC4F5  Verge, build v0.3.0  CURRENT
1-3      blinking: unplug it when you like
1-4      QK7RM2VTX8D45  stock V3.108  SKIPPED: needs the ISP jumper. Stock V3.108 has no jumper-free update. NXP added it in V3.122.
Batch done: 1 updated, 1 current, 1 skipped, 0 failed, 0 waiting.
```

The tally counts each unit once, including disconnected units. A failed unit that returns running
the image counts as current. A successfully updated unit remains counted as updated after
reconnection. The record file keeps every event and can contain several lines for one unit.
A unit whose bootloader did not restart counts as failed until it returns.

The batch recognises returning units by any serial number they reported. After a failed update,
it can also recognise a unit running the image on its previous port. Local files have no release
version to identify. For a file update, port matching therefore requires a completed write, a
unit that did not restart or return, and a unit that was not previously running Verge.
If the returning unit is not current, the batch updates it again.

Ctrl-C stops accepting new units and lets the active update finish. A second Ctrl-C stops that
update at its next safe point; a third exits. `--count N` stops after N successful updates.
N must be positive. The batch exits with an error if any unit failed. Units still waiting when
the batch stops also get a record line.

The batch records one JSON object per line. `--record FILE` selects the file; the default is
`verge-batch-<time>.jsonl` in the current directory. verge appends to an existing file. If writing or
flushing the record fails, the active update finishes, the batch stops and the command reports
the error. A record looks like this:

```
{"time":"2026-10-20T14:34:58Z","port":"1-1","serial_before":"QK7RM2VTX8D41","firmware_before":"stock V3.172","serial_after":"CJ3E7A91C2","firmware_after":"Verge, build v0.3.0","result":"updated","reason":null,"seconds":4.6795}
```

`result` is `updated`, `jumper-fitted`, `not-restarted`, `current`, `skipped`, `failed` or
`waiting`. `reason` explains `not-restarted`, `skipped`, `failed` and `waiting` results.

The batch tracks units through bootloader entry by USB port. It pauses while a unit without a
reported port is attached.

The batch polls for disconnected units every quarter of a second. Windows retains a reconnected
unit's identity, so a disconnect and reconnect between polls counts as a continuous connection.

### GUI

`verge-gui` provides the same update workflows. Attached units are on the left. Select one to see
its firmware, update path and image choices: RELEASE, STOCK or FILE. Browse for a file with FILE's
dialog, or drop it on the window. This app cannot receive file drops under Wayland.

Read the board and jumper advice, choose the update or install action shown, and confirm. The
GUI shows download and flash progress. When it finishes, check the result and the log below.

The GUI asks before flashing and includes any required download in the same confirmation.
During an update it shows each step and the byte count. Closing waits for the update to finish.
CANCEL stops it at the next safe point. SHOW opens the log along the bottom.

Local files are checked against the board when selected. Release board support is checked after
download, before the unit is changed. If the unit needs the ISP jumper, the GUI names its board
and the image to flash in ISP mode. For a unit already in ISP mode or with an unknown board, the
confirmation lists the selected file's supported boards.

BATCH, at the top, opens the batch view. It sets up the same batch as `verge batch`: the image,
the kinds of unit, the board, whether to take the units attached now, a count to stop after, and
the record file. The default record location is your data directory:
`~/.local/share/verge` on Linux, `~/Library/Application Support/verge` on macOS and
`%APPDATA%\verge` on Windows. Once armed, the view shows the tally, active update, queue and
completed units. STOP stops accepting new units. CANCEL THE UNIT UNDER WAY stops the active
update at its next safe point. During image loading, STOP or closing the window ends the batch
immediately; no unit has been flashed.

BENCH reaches a Pro's target supply and energy measurement through Verge Aux, so a debugger can
keep CMSIS-DAP open meanwhile. The supply switches at once. Turning it off asks first while a
capture runs. The panel shows the supply as commanded and the voltage as measured, which are
separate readings. DIO shows each pin’s direction and level. DIO0–2 change direction together;
DIO3 changes alone. Switching to output asks first: check that the target is not driving the pins.
Inputs have pull-downs. Outputs read their own drive. DIO requires current Verge firmware with
DIO on Aux and J15 removed at startup.

START RECORDING starts a capture at 1 to 100,000 pairs/s and records it. Starting clears the probe's
energy total, and its calibration can interrupt target power for about 0.2 s. If another host
already runs a capture, RECORD RUNNING CAPTURE records it at its own rate. STOP RECORDING stops a
capture the window started, and leaves another host's running.

The graph shows current, voltage or power. LIVE follows the newest pairs; INSPECT holds the view
while recording carries on. Drag or scroll the graph to move along time, and ctrl-scroll or pinch
to zoom; the window's marker in the overview under it drags too. The vertical axis fits what is
visible until you move it: drag or scroll up or down, or drag along the axis to zoom it. FIT Y, or a
double click, fits it again. Zoomed out, each column of the graph spans the lowest to the highest
reading in its time, so a pulse shorter than a column keeps its height. Zoomed in, the graph marks
each pair. With SELECT on, dragging across the graph selects an interval; the left
panel sets its ends exactly and zooms to it. The panel measures the visible window and the
selection: duration, pairs, minimum, mean and peak current, mean voltage and power, charge and
energy. It shows how much of each was not measured. Time without measurements adds nothing to
charge or energy. The readings come from the recorded pairs, so zooming does not change them.

Each recording goes to disk as it arrives, for as long as the disk has room. Recordings stay in a
`captures` folder in that data directory until discarded. KEPT ON DISK reopens one, including one
cut short by a crash. SAVE AS copies the open capture; OPEN reads one back. EXPORT writes the
capture, or the selection, as CSV or as a sigrok session with voltage, current and a GAP channel.
Recordings include all four DIO pins. Their lanes below the trace share its time axis. Zoomed
out, a lane keeps short pulses visible. Missing edges leave breaks and an edge-loss report count.
Exports include DIO levels and a DIO_GAP channel (dio_gap in CSV) for time whose levels are unknown.
An export keeps whole pairs at its edges, with their own times, so it can start a little before
the selection.

Markers appear as labelled lines on the graph and in a paged list. Picking a marker centres the
view on it. SELECT TO NEXT MARKER selects the span to the following marker for measurement.
The panel reports marker loss. Both export formats keep markers as `energy record` does.

### Releases

The tool looks for releases in `Escapement-Project/mculink-verge`. Select another repository with
`--repo OWNER/NAME` or `VERGE_REPO`. Public releases need no token. For a private repository, set
`GITHUB_TOKEN`.

The tool allows 15 seconds to connect, 30 seconds for GitHub to respond and 60 seconds to download
an asset. A timeout reports which operation failed.

Downloaded releases are cached in `~/.cache/verge` on Linux, `~/Library/Caches/verge` on macOS
and `%LOCALAPPDATA%\verge` on Windows. A recovery command using `verge isp flash` can reuse the
download. Each use verifies the signature, checksum and GitHub asset digest, where available.
A failed check deletes the cached release. An unreadable cache entry also requires another
download. Both cases require download confirmation. You can delete the cache at any time.

Release lookup requires GitHub even when the image is cached. GitHub supplies the latest release
and the asset digest used to verify the cache. If GitHub is unreachable, lookup fails.

A release has to carry three assets:

- exactly one `.s19` asset, the image as `objcopy -O srec` writes it;
- a checksum file named after it with `.sha256` added, in `sha256sum`'s format;
- a minisign signature named after it with `.minisig` added. Its trusted comment has to contain
  `file:` followed by the image's name, and `tag:` followed by the release's tag.

For example `mculink-verge-v0.3.0.s19`, `mculink-verge-v0.3.0.s19.sha256` and
`mculink-verge-v0.3.0.s19.minisig`. The tag is the version, and it is what a unit built from that
tag reports, so the tool can compare installed versions. The boards an image supports are in
its declaration, inside the signed image.

The tool verifies signatures against its built-in public key. It reports missing signatures,
signatures from another key and images changed after signing. It accepts minisign's default
pre-hashed signatures and rejects legacy signatures. The trusted comment is also signed.
Its filename prevents renaming a signed asset; its tag prevents republishing an old image as a
newer release. The tool also verifies the checksum file and GitHub's asset digest.

The key is `RELEASE_KEY` in `verge-probe/src/release.rs`, the base64 line of minisign's `.pub`
file. It is not set yet, so this build cannot install releases. Local files and stock images
still work.

The firmware repository publishes the releases. Its README, under "Releases", describes the
workflow that builds, signs and attaches the three assets, how to generate and store the key pair,
and how to make the assets by hand.

### Board declarations

A Verge image declares its supported boards in a 16-byte record identified by a magic value.
The firmware places it after CPU 0's vector table. The tool scans every 4-byte-aligned address
in the image. Fields are little-endian:

| offset | size | field |
| --- | --- | --- |
| 0 | 8 | the ASCII bytes `VERGEIMG`, with no terminator |
| 8 | 1 | format, 1 |
| 9 | 1 | the record's length in bytes, 16 at format 1 |
| 10 | 2 | zero |
| 12 | 4 | boards: bit n set means the image runs on board n |

Board IDs match Verge's identification reply: 0 for the standalone MCU-Link, 1 for the MCU-Link Pro
and 2 for an on-board MCU-Link. Unknown board bits are ignored. New fields are appended and read
only in formats that define them. The tool rejects duplicate declarations, format 0, a length
below 16 or a declaration extending beyond its image chunk. `verge inspect` displays the record.

### Stock images

`--stock` takes the newest `MCU-LINK_CMSIS-DAP_V*.s19` it finds in LinkServer's install, under
`/opt` or `/usr/local` on Linux, `/Applications` on macOS and `C:\NXP` on Windows. Name the
directory yourself with `--stock DIR`, or give any file with `--file`.

### USB permissions on Linux

Your user needs to open two devices: the probe's CMSIS-DAP interface and the bootloader's HID
interface. These udev rules allow it for the logged-in user:

```
# /etc/udev/rules.d/69-mculink-update.rules
# An MCU-Link running a CMSIS-DAP firmware, stock or Verge
SUBSYSTEM=="usb", ATTRS{idVendor}=="1fc9", ATTRS{idProduct}=="0143", TAG+="uaccess"
# The LPC55S69 ROM bootloader, ISP mode
KERNEL=="hidraw*", ATTRS{idVendor}=="1fc9", ATTRS{idProduct}=="0021", TAG+="uaccess"
```

Then run `sudo udevadm control --reload` and plug the unit in again. probe-rs's udev rules cover
the first line already.

## How it is built

`verge-probe` owns discovery, update paths, bootloader operations, image parsing, releases and
batch updates. It also exposes Verge's debug and bridge features through probe-rs. This repository
contains the `cli` and `gui` frontends. The GUI uses egui and runs USB work on a worker thread.
A separate thread blinks completed units during batch updates.

The library and frontends use:

- `rblhost`, NXP's own Rust client for the ROM bootloader's protocol. LinkServer ships the same
  version as its `rblhost` tool. Its default USB transport opens the first matching bootloader.
  `verge-probe` supplies a `hidapi` transport to open the selected device.
- `nusb` for USB, the same crate probe-rs uses.
- `srec` and `object` to read S-records and ELF files.
- `ureq`, `serde_json` and `sha2` for releases, `minisign-verify` for their signatures, `clap` for
  the command line, and `tracing` with `tracing-indicatif` for logs and progress.
- `dirs` for the cache and data directories.
- `eframe` and `egui` for the GUI, `rfd` for its file dialog, and `egui_kittest` for GUI tests.

The GUI uses IBM Plex and Kode Mono under the SIL Open Font License. `gui/fonts/` records their
sources and licence texts. The LICENCES button also displays the texts.

The tool writes and verifies whole 512-byte pages. Erased flash on the LPC55S69 has no valid ECC;
reading past the image can disconnect the bootloader from USB.

## Testing

```
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

`.github/workflows/check.yml` runs these on every push and pull request, with a check that a
release build leaves the simulator out and the Windows check below.

Tests use `verge-probe`'s simulator without opening USB devices. It models recorded probe
identities, bootloader flash behaviour and GitHub responses. Simulated time advances when the
tool waits, so timeout tests run without real delays. Units can connect and disconnect on a
schedule.

The simulator, and the `minisign` crate it signs its releases with, are built only with the
`simulate` feature. The tests turn it on themselves, so `cargo test` needs nothing more. A
release build leaves it out, along with the hidden `--simulate` option.

The simulated GitHub signs releases with a key pair generated by the `minisign` crate at startup.
The tool trusts that key in place of `RELEASE_KEY`. No secret key is stored. A separate test
verifies a signature made by the minisign CLI, taken from `minisign-verify`'s documentation.

Use `--simulate` to run the CLI against the same simulator. Simulated releases are not cached
because each run generates a new signing key. Every download therefore needs confirmation:

```
cargo run -p verge --features simulate -- --simulate bench
cargo run -p verge --features simulate -- --simulate stock update --yes
cargo run -p verge --features simulate -- --simulate arrivals batch --yes --count 3
```

The scenarios are `bench`, `ours`, `stock`, `stock-old`, `stock-hid`, `isp`, `isp-jumper`,
`two-isp`, `ours-beside-isp`, `isp-ours`, `two-isp-ours`, `mini`, `isp-mini`, `busy`, `arrivals`,
`pro`, `ours-pro`, `ours-pro-debugger`, `ours-pro-aux-taken`, `ours-pro-energy`,
`ours-pro-energy-failed`, `ours-pro-held`, `ours-plain`, `pro-old`, `fails-writes`, `unnamed-board`,
`swd-jumper`, `pro-swd-jumper` and `stock-swd-jumper`. In `arrivals`, five units are plugged in over
the first eight seconds. In `pro`, the one unit is an MCU-Link Pro; in `pro-old`, a Pro on stock
V3.108, which needs the jumper. In `ours-pro-energy`, a Pro running Verge has its target supply on
and has captured for 10 s; its target sleeps at 20 µA and wakes at 5 mA for 10 ms in every 100 ms.
In `ours-pro-energy-failed`, that capture has stopped with an acquisition fault. In
`ours-pro-debugger`, another program holds a Pro's CMSIS-DAP interface, as a debugger would; in
`ours-pro-aux-taken`, another program holds its Verge Aux. In `fails-writes`, the bootloader refuses
every write. In `unnamed-board`, the unit's product string names no board. In `swd-jumper` and
`pro-swd-jumper`, a standalone MCU-Link and a Pro run Verge with the SWD jumper fitted; in
`stock-swd-jumper`, a standalone MCU-Link runs stock with it. A simulated unit boots only the
simulator's own images. A real image flashed there, such as stock from your LinkServer install,
leaves the unit silent.

GUI tests run headlessly through `egui_kittest` against the same simulator. A separate test
renders each state to PNG and requires a GPU:

```
VERGE_GUI_SHOTS=/tmp/shots cargo test -p verge-gui pictures -- --ignored
```

`cargo run -p verge-gui --features simulate -- --simulate bench` opens the window on the
simulated bench, labelled as such.

Two tests read real images and are skipped by default:

```
VERGE_FIRMWARE_ELF=<firmware checkout>/build/mculink_cjtag.elf \
VERGE_STOCK_S19=<LinkServer>/MCU-LINK/probe_firmware/MCU-LINK_CMSIS-DAP_V3_172.s19 \
    cargo test --manifest-path ../verge-probe/Cargo.toml --test image -- --ignored
```

They compare the image reader's output with `arm-none-eabi-objcopy` for the same files.

The Windows build is checked by compiling it, not by running it:

```
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

## Not done yet

- Variant releases. A release carries one image, so a Verge build with a variant suffix, such
  as `+armwire`, cannot be published beside the plain build.
- The J-Link firmware that LinkServer ships for the MCU-Link. The tool does not recognise a unit
  running it.
- Stock V2.x, which speaks HID only. Its USB product string has not been recorded from a real
  unit. The simulator's is a guess, and so is the version the tool reads from it.

## Licence

Apache-2.0, in `LICENSE`. The fonts in `gui/fonts/` are under the SIL Open Font License, with
their licence texts beside them.
