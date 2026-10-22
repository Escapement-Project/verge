# verge

A tool to update the firmware on an MCU-Link. It works with NXP's stock CMSIS-DAP firmware and
with the firmware from the `mculink-verge` repository, which it calls ours.

It finds every attached MCU-Link and says what it runs, with the version and the board. Then it
picks the best way to update each one:

- A unit running ours restarts into its bootloader when asked, so it needs no jumper.
- A unit running stock V3.122 or later does the same, on the request NXP's LinkServer sends.
- Anything else needs the ISP jumper. Fit it before plugging the unit in, because the bootloader
  only looks at the pin when the unit starts.

Ours comes from that repository's GitHub releases. Each release is signed, and the tool checks
the signature and the checksums before it flashes anything. Stock comes from your own LinkServer
install, since NXP's image is not ours to hand out.

## Boards

Stock names its board in its USB product string, and ours names it in its identify reply. The
tool knows two boards by name: the full MCU-Link and the MCU-Link Pro. `verge` lists each unit's
board.

An image of ours says which boards it runs on, in a declaration inside the image (see "Board
declarations" below). An image with no declaration runs on the full MCU-Link only, as every image
built before declarations did. The tool puts an image of ours, a release or a file, only on a
board its declaration names. It refuses it on any other board, and on a board it knows no number
for, such as an MCU-Link MR. Stock's own image can go on any board stock runs on.

A unit in ISP mode cannot say which board it is, so `verge isp flash` cannot check it. Its question says
which boards the image is built for instead. For the same reason, `verge isp enter` sends a unit
it knows is another board than the full MCU-Link into ISP mode only with `--stock`, for
`verge isp flash --stock` to follow. An image of ours built for that board goes on with `verge
update --file IMAGE`, which checks that the image declares the board before it flashes anything.

Wherever the tool sends you to `verge isp flash` for a unit whose board it knows, it names the board
and the image to flash: the release by its tag, the file by its path, or `--stock`. That covers
the jumper advice for a unit with no jumper-free update, and what to do after an update fails
halfway. `update` checks the image against the unit's board before it gives the jumper advice, so
a board the image is not built for is refused there, as it is on the jumper-free path. The core
library checks the board again before it asks a unit for its bootloader.

A running unit whose firmware does not say which board it is can still be updated, after a
question that says so. `--yes` does not answer that question: run `update` at a terminal.

## The SWD jumper

The SWD jumper hands the target's debug lines to another debugger. Stock and ours both drop
their CMSIS-DAP interface while it is fitted, and that interface is how the tool asks a unit what
it runs. So the tool reads such a unit from what USB shows without opening it: whose firmware it
is, that the jumper is fitted, and the board where that says it. It cannot tell which build of
ours the unit runs.

Stock names its board in its product string as always. Ours names only the Pro there, because an
on-board MCU-Link running ours gives the same string as the full MCU-Link. So a full MCU-Link
running ours, with its SWD jumper fitted, is a unit whose board the tool does not know, and
`update` asks before it puts ours on it.

A unit running ours still updates without the ISP jumper. The tool sends the restart request to
the USB device itself rather than to an interface. Windows sends such a request only through an
interface the tool has claimed, so there you take the SWD jumper off first. Stock's update request
needs the CMSIS-DAP interface. So a unit on stock V3.122 or later needs its SWD jumper off to
update without the ISP jumper, and older stock needs the ISP jumper either way. A batch skips a
unit with its SWD jumper fitted.

## Building

From the repository's root:

```
cargo build --release
```

The command line tool is `target/release/verge`, and the window is `target/release/verge-gui`.

On Linux, building needs the libudev headers (`libudev-dev` on Debian and Ubuntu,
`systemd-devel` on Fedora). The HID library that talks to the bootloader uses them.

### Releases of verge

`.github/workflows/release.yml` builds verge and verge-gui when a tag `v<version>` is pushed, the
version being the workspace's in `Cargo.toml`. Each release carries one archive a platform:
x86_64 Linux (glibc 2.35 or newer), x86_64 Windows, and macOS for both Apple silicon and Intel in
one binary. Each archive holds both tools, `LICENSE`, and `THIRD-PARTY-LICENSES.html` with the
licence of every crate they are built from. Beside it are a `.sha256` file and a minisign
signature, made with the same key as the firmware's releases. To check one:

```
minisign -V -P <the public key> -m verge-v0.1.0-x86_64-linux.tar.gz
```

The workflow signs as the firmware's does, in a `release` environment with a required reviewer,
and needs the same settings in this repository: mculink-verge's README, "Releases", lists them.

## Using it

```
verge                       # list what is attached, and how each unit can be updated
verge update                # install the latest release of ours on the only unit
verge update --serial CJ3E7A91C2 --release v0.3.0
verge update --stock        # put NXP's firmware back
verge isp enter             # send a unit into ISP mode without the jumper, for `verge isp flash`
verge isp flash --file mculink_cjtag.elf
verge isp reset             # start the image a unit in ISP mode holds, without flashing it
verge isp info              # what the bootloader of a unit in ISP mode says about itself
verge batch                 # update each unit as it is plugged in, until Ctrl-C
verge release               # the latest release, and how each unit compares
verge inspect image.s19     # what an image puts into flash; touches no USB device
verge cjtag --oscan1-only   # offer a host two-pin OScan1 alone, until the unit restarts
verge dio --high all        # drive all four of a Pro's DIO pins high
```

`update` asks before it downloads a release, and names the repository, the tag and the size.
It asks again before it flashes. Pass `--yes` to skip both questions, for example in a script.
Without `--yes`, and without a terminal to answer at, nothing is downloaded. `verge isp flash`
asks the same two questions. A release downloaded once is kept, and needs no download question the next
time (see "Releases").

`cjtag` shows the two-pin (cJTAG) scan formats a unit running ours offers a host. The host picks the
one it prefers of those the target has too. `--oscan1-only` limits it to OScan1, the format to look at on an
analyser. `--formats` picks a set by name or number, `--formats oscan5` or `--formats 9,13`, so you
can hold a host to one format to test it or compare it with another; OScan1 always stays. `--all`
lifts the limit, and so does a restart.

`dio` reads and drives the four DIO pins on an MCU-Link Pro's J19 header (pins 2 to 5), on a unit
running ours. With no options it says what each pin is doing. `--high` and `--low` make pins
outputs at that level, and `--input` makes them inputs. Name pins as `0` to `3`, `dio0` to `dio3`,
or `all`. DIO0, DIO1 and DIO2 change direction together, and DIO3 on its own, so a change turns a
whole group round: `--high 0` alone is refused while DIO1 and DIO2 are inputs. The pins keep their
state between debug sessions, so one can hold a target's boot mode through a reset, and they go
back to inputs when the unit restarts. The Pro's LED6 (SIO) is lit while any pin is driven.

`update` refuses to install a release the unit already runs, or one older than it. Pass
`--force` to install it anyway.

## ISP mode

`verge isp` works with a unit's ROM bootloader. `enter` sends a unit into it without the jumper,
`flash` writes an image, `reset` starts the image the unit already holds, and `info` asks the
bootloader its version.

Every LPC55S69 bootloader looks the same over USB. So `flash`, `reset` and `info` act on the only
unit in ISP mode, and refuse while there are several. With several attached, pass `--port` with
the port `verge` lists. That works on Linux, which ties each bootloader to its USB port. On
Windows and macOS the tool cannot tell which bootloader it opened, so it still refuses and asks
you to unplug all but one. `flash` also refuses while an MCU-Link mini is in ISP mode, with or
without `--port`. On Linux `enter` works beside another unit in ISP mode and names the port to
flash; `update` and a batch still wait until that unit is flashed or unplugged.

`flash` is for a unit you put into ISP mode with the jumper or with `enter`. A unit that comes back
in ISP mode after a flash still has its ISP pin held low. Either its ISP jumper is fitted, or a
probe on its debug connector holds the pin. An MCU-Link's target connector carries an ISP line on
pin 7, which reaches the ISP pin of an NXP board on the other end. Take the jumper off or unplug
the probe, then run `verge isp reset`. After `reset` alone there is a third cause: the unit's flash
holds no image the ROM starts.

For the same reason, the tool cannot tell an MCU-Link in ISP mode from any other LPC55S69 board
in ISP mode. Before it flashes a unit that was in ISP mode already, its question says so.

The bootloader can take a few seconds to answer, and the bus can change meanwhile. Once it
answers, the tool looks again, and flashes only if the bootloader it would pick now is still the
one it checked. On Linux it also opens that very device, by its path.

After a flash the unit restarts, and the tool finds it again on the same USB port. Where the
system reports no port, the tool takes the unit back only if it is the one new device. If two
appear, it stops and says so.

Sometimes the bootloader does not restart after the flash. On Linux and macOS the tool sees that
it never left the bus, and says so: the image is flashed and verified, and the unit starts it once
it is unplugged and plugged back in. On Windows a bootloader that restarts very quickly, with its
jumper fitted, looks the same, so there the tool names both causes: unplug the unit, take the
jumper off if it is fitted, and plug it back in. Either way the unit is not counted as updated.

Ctrl-C stops at the next safe point. A unit stopped halfway through a write stays in ISP mode,
and `verge isp flash` with the same image, which the tool names, finishes the job. A Ctrl-C that
comes before the unit is asked for its bootloader leaves it as it was.

### Batch updates

`verge batch` is for a delivery of probes. Choose the image and the units it takes, answer one
question, then plug units in one after another or several on a hub. Each unit the batch takes is
updated without another question. Where the release has to be downloaded first, the one question
says so.

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
version; `--ours-any` takes every build. A unit already running the image counts as current and
is left as it is.

Stock and a file carry no version of ours to compare with. So with either, a batch that takes
ours older than the image does not arm. It says so before it looks for the image, and says to
pass `--ours-any`, or to leave ours out, as with `--take stock`.

The batch takes only units plugged in after it is armed. `--include-attached` takes the ones
attached already too. A unit just plugged in gets a second to settle before the batch asks it
what it runs, as during an update. If it cannot be opened, the batch tries again for a few
polls more before it skips it.

`--board` picks the board: `mcu-link`, the default, or `mcu-link-pro`. The batch takes only units
that say they are that board. An image of ours has to declare the board, or the batch does not
arm; stock's own image arms for either. A unit in ISP mode cannot say which board it is, so
`--take isp` takes any, and the question that arms the batch says so.

Units go through the bootloader one at a time. One that arrives while another is being updated
waits its turn, in the order they arrived. The batch pauses while anything in ISP mode is not the
next unit it means to flash: an MCU-Link mini, a second unit, or a unit the batch does not take.
It says why, and carries on once that unit is unplugged.

A unit is taken once. Updated, failed, skipped or back in ISP mode with its jumper fitted, it is
left alone until it is unplugged. A failure does not stop the batch, but a unit left in ISP mode
pauses it: unplug that unit, or flash it again with `verge isp flash` and the batch's image, which
the pause names with the unit's board. So does a unit whose bootloader did not restart; plugged
back in, it starts the new image.

A unit that did not come back after its update has left the bus, so the batch cannot wait for it.
If it comes back on the same port while the batch runs, running the image, it counts as current.
Running what it ran before, under the serial it had, it is taken once more, when its turn comes.
If that update fails too, the batch leaves the unit alone until the batch ends. Another unit in
the same port has another serial, and is a unit of its own. A unit in ISP mode has no serial, so
there the port alone tells.

A finished unit blinks its LEDs until you unplug it, and the batch names its USB port. It keeps
blinking while the next unit is updated. The LEDs blink only while the batch runs, so the unit
that ends it is named without blinking. Each unit gets a line once the batch has settled it, and
the batch ends with a tally:

```
1-1      QK7RM2VTX8D41 -> CJ3E7A91C2  stock V3.172 -> ours, build v0.3.0  UPDATED in 4.7 s
1-1      blinking: unplug it when you like
1-3      CJ3E7AC4F5  ours, build v0.3.0  CURRENT
1-3      blinking: unplug it when you like
1-4      QK7RM2VTX8D45  stock V3.108  SKIPPED: needs the ISP jumper. Stock V3.108 has no jumper-free update. NXP added it in V3.122.
Batch done: 1 updated, 1 current, 1 skipped, 0 failed, 0 waiting.
```

The tally counts units, each once, by how it ended, including those unplugged since. A unit that
failed and came back running the image counts as current, and one updated and plugged in again
counts as updated. The record file keeps a line for every event, so it can have two lines for one
unit. A unit whose bootloader did not restart counts as failed until it comes back, since it needs
you to plug it in again. The batch knows a unit again by any serial it reported, or, after a
failed update, by the port it left when it comes back running the image. A file carries no
version to tell its image by, so in a batch with a file, a unit back on that port running ours
is the one that left only if the file went on it (its bootloader did not restart, or it did not
come back) and it did not run ours before; if it is not current, it is updated again.

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

`result` is `updated`, `jumper-fitted`, `not-restarted`, `current`, `skipped`, `failed` or
`waiting`. `reason` says why for `not-restarted` and the last three.

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

The window asks before it flashes. Where the release has to be downloaded first, the same
question says so, and so does the batch view's. While a unit is being written, the window shows
each step and the bytes so far, and it will not close until the update finishes. CANCEL stops it
at the next safe point instead. The log along the bottom shows what the tool did; SHOW opens it.

A file is checked against the unit's board as soon as it is picked. A release says which boards
it is for only once it is downloaded, so on a board it does not declare, the update stops there,
before the unit is touched. For a unit that needs the jumper, the view names its board and the
image to flash once it is in ISP mode. The question before flashing a unit in ISP mode, or one
that does not say its board, names the boards a picked file is built for.

BATCH, at the top, opens the batch view. It sets up the same batch as `verge batch`: the image,
the kinds of unit, the board, whether to take the units attached now, a count to stop after, and
the record file. The record goes in your data directory unless you name another:
`~/.local/share/verge` on Linux, `~/Library/Application Support/verge` on macOS and
`%APPDATA%\verge` on Windows. Once the batch is armed, the view shows the tally, the unit under
way, the units waiting and a line for each unit settled. STOP stops taking new units. CANCEL THE
UNIT UNDER WAY then stops that one at its next safe point. STOP, or closing the window, while the
image is still loading ends the batch at once, since nothing has been flashed.

### Releases

The tool looks for releases in `Escapement-Project/mculink-verge`. Point it elsewhere with `--repo OWNER/NAME` or `VERGE_REPO`. Public
releases need no token. For a private repository, set `GITHUB_TOKEN`.

A lookup or download that stalls fails with a message. The tool allows 15 seconds to connect,
30 for GitHub to answer, and 60 for a download to finish.

A release, once downloaded, is kept in your cache directory: `~/.cache/verge` on Linux,
`~/Library/Caches/verge` on macOS and `%LOCALAPPDATA%\verge` on Windows. So the `verge isp flash`
that `verge update` sends you to does not download the image again. A kept release is checked
every time it is used, as a fresh download is: its signature, its checksum file, and GitHub's
digest of the image where GitHub reports one. One that fails is deleted. The tool asks before it
downloads the release again, as it does for a release it cannot read from the cache. Deleting the
cache is always safe.

Looking a release up still needs GitHub, even for one that is kept. GitHub says which release is
the latest, and its digest of the image is what a kept copy is checked against. When GitHub
cannot be reached, the tool says so, rather than fall back to a release it kept.

A release has to carry three assets:

- exactly one `.s19` asset, the image as `objcopy -O srec` writes it;
- a checksum file named after it with `.sha256` added, in `sha256sum`'s format;
- a minisign signature named after it with `.minisig` added. Its trusted comment has to contain
  `file:` followed by the image's name, and `tag:` followed by the release's tag.

For example `mculink-verge-v0.3.0.s19`, `mculink-verge-v0.3.0.s19.sha256` and
`mculink-verge-v0.3.0.s19.minisig`. The tag is the version, and it is what a unit built from that
tag reports, so the tool can tell whether a unit is behind. The boards an image runs on are in
its declaration, inside the signed image.

The tool checks the signature against the public key built into it. It refuses a release with no
signature, one signed with another key, and one changed after it was signed, and says which. It
takes only the pre-hashed signatures minisign makes by default, and refuses a legacy one. The
trusted comment is signed too. Naming the file there stops a signed image being published under
another name. Naming the tag stops an old image being published again as a newer release under
its own name. The tool also checks the image against the checksum file and against the digest
GitHub keeps for every asset.

The key is `RELEASE_KEY` in `core/src/release.rs`, the base64 line of minisign's `.pub` file. It
is not set yet, so this build installs no release. A file or stock still works.

The firmware repository publishes the releases. Its README, under "Releases", describes the
workflow that builds, signs and attaches the three assets, how the key pair is made and kept, and
how to make the assets by hand.

### Board declarations

An image of ours declares the boards it runs on in sixteen bytes of its flash, found by their
magic. The firmware puts them just after CPU 0's vector table, but the tool looks at every
4-byte-aligned address of the image rather than rely on that. Fields are little-endian:

| offset | size | field |
| --- | --- | --- |
| 0 | 8 | the ASCII bytes `VERGEIMG`, with no terminator |
| 8 | 1 | format, 1 |
| 9 | 1 | the record's length in bytes, 16 at format 1 |
| 10 | 2 | zero |
| 12 | 4 | boards: bit n set means the image runs on board n |

Boards are numbered as ours numbers them in its identify reply: 0 the full MCU-Link, 1 the
MCU-Link Pro, 2 an on-board MCU-Link. A bit for a board the tool does not know is ignored. Fields
are only ever appended, and the tool reads each one only from the format that added it on. It
refuses as malformed an image with two declarations, a format of 0, a length under 16, or a
declaration that runs past the end of its part of the image. `verge inspect` shows an image's
declaration.

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
`gui` the window, built with egui. The window does its USB work on a thread of its own, and a
batch blinks finished units on a thread of its own too.

It leans on established crates:

- `rblhost`, NXP's own Rust client for the ROM bootloader's protocol. LinkServer ships the same
  version as its `rblhost` tool. Its USB transport opens the first device with the bootloader's
  ID, so the tool gives it a transport of its own over `hidapi` that opens the device it chose.
- `nusb` for USB, the same crate probe-rs uses.
- `srec` and `object` to read S-records and ELF files.
- `ureq`, `serde_json` and `sha2` for releases, `minisign-verify` for their signatures, `clap` for
  the command line, and `tracing` with `tracing-indicatif` for logs and progress.
- `dirs` for the cache and data directories.
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
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

`.github/workflows/check.yml` runs these on every push and pull request, with a check that a
release build leaves the simulator out and the Windows check below.

No test opens a USB device. They run against a simulated bench (`core/src/sim/`): probes that
answer with the identities recorded from the real units, a bootloader that keeps flash's rules,
and a fake GitHub. Time on the simulated bench passes only when the tool waits. A test of a
fifteen-second timeout takes no real time, and units can be plugged in and out on a schedule.

The simulator, and the `minisign` crate it signs its releases with, are built only with the
`simulate` feature. The tests turn it on themselves, so `cargo test` needs nothing more. A
release build leaves it out, along with the hidden `--simulate` option.

The fake GitHub signs its releases with a key pair that the `minisign` crate makes when the bench
starts. The tool run against the bench trusts that key in place of `RELEASE_KEY`, and no secret
key is stored anywhere. One more test checks a signature that the minisign command line made,
taken from `minisign-verify`'s documentation.

The command line runs against the same bench with `--simulate`, which is handy while working on
the tool. A simulated run keeps no releases, since the fake GitHub's key is new every run, so it
asks before every download:

```
cargo run -p verge --features simulate -- --simulate bench
cargo run -p verge --features simulate -- --simulate stock update --yes
cargo run -p verge --features simulate -- --simulate arrivals batch --yes --count 3
```

The scenarios are `bench`, `ours`, `stock`, `stock-old`, `stock-hid`, `isp`, `isp-jumper`,
`two-isp`, `mini`, `isp-mini`, `busy`, `arrivals`, `pro`, `pro-old`, `fails-writes`,
`unnamed-board`, `swd-jumper`, `pro-swd-jumper` and `stock-swd-jumper`. In `arrivals`, five units
are plugged in over the first eight seconds. In `pro`, the one unit is an MCU-Link Pro; in
`pro-old`, a Pro on stock V3.108, which needs the jumper. In `fails-writes`, the bootloader refuses
every write. In `unnamed-board`, the unit's product string names no board. In `swd-jumper` and
`pro-swd-jumper`, a full MCU-Link and a Pro run ours with the SWD jumper fitted; in
`stock-swd-jumper`, a full MCU-Link runs stock with it. A simulated unit boots only the
simulator's own images. A real image flashed there, such as stock from your LinkServer install,
leaves the unit silent.

The window's tests drive it headless through `egui_kittest`, against the same bench. One more
renders every state to PNG, which needs a GPU:

```
VERGE_GUI_SHOTS=/tmp/shots cargo test -p verge-gui pictures -- --ignored
```

`cargo run -p verge-gui --features simulate -- --simulate bench` opens the window on the
simulated bench, labelled as such.

Two tests read real images and are skipped by default:

```
VERGE_FIRMWARE_ELF=<firmware checkout>/build/mculink_cjtag.elf \
VERGE_STOCK_S19=<LinkServer>/MCU-LINK/probe_firmware/MCU-LINK_CMSIS-DAP_V3_172.s19 \
    cargo test -p verge-core --test image -- --ignored
```

They hold the image reader to `arm-none-eabi-objcopy`'s reading of the same files.

The Windows build is checked by compiling it, not by running it:

```
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

## Not done yet

- Variant releases. A release carries one image, so a build of ours with a variant suffix, such
  as `+armwire`, cannot be published beside the plain build.
- The J-Link firmware that LinkServer ships for the MCU-Link. The tool does not recognise a unit
  running it.
- Stock V2.x, which speaks HID only. Its USB product string has not been recorded from a real
  unit. The simulator's is a guess, and so is the version the tool reads from it.

## Licence

Apache-2.0, in `LICENSE`. The fonts in `gui/fonts/` are under the SIL Open Font License, with
their licence texts beside them.
