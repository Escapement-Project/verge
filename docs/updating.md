# Updating and recovery

verge updates a probe whatever it runs: Verge, NXP's firmware, or anything else. It picks the
simplest way the probe allows, and when that needs your hands it says what to do.

## How a probe updates

verge writes every image through the probe's ROM bootloader, in ISP mode. A probe gets there one of
four ways:

- A probe running Verge restarts into its bootloader when verge asks. No jumper.
- A probe running NXP's firmware from V3.122 on does too, through the same request NXP's LinkServer
  uses.
- A probe running daplink-rs, a Rust CMSIS-DAP firmware with an MCU-Link build, restarts into its
  bootloader on its own command.
- Anything else needs the ISP jumper. Fit it before you plug the probe in: the bootloader reads the
  pin as it starts.

daplink-rs uses Arm's DAPLink USB ID, which other DAPLink probes share. verge recognises
daplink-rs's MCU-Link build by its USB strings and leaves every other device with that ID alone.

verge does not recognise a probe running the J-Link firmware that LinkServer ships for the
MCU-Link. A probe running NXP's V2.x firmware, which speaks HID only, verge cannot ask what it
runs: it lists the probe as probably stock, and a batch leaves it out.

`verge` on its own lists each attached probe with the way verge would update it.

## Choosing the image

```
verge update                    # the latest Verge release
verge update --release v0.3.0   # a release by its tag
verge update --file IMAGE       # an ELF or S-record file, such as your own build
verge update --stock            # NXP's firmware, from your LinkServer install
```

`update` refuses a release the probe already runs, or one older than it. `--force` installs it
anyway.

verge asks before it downloads a release, showing the repository, the tag and the size, and again
before it flashes. `--yes` answers both. Without `--yes`, a download needs a terminal to ask at. A
release verge has cached needs no download question; [Releases](#releases) says where it keeps them.

For `--stock`, verge takes the newest `MCU-LINK_CMSIS-DAP_V*.s19` in LinkServer's install: under
`/opt` or `/usr/local` on Linux, `/Applications` on macOS and `C:\NXP` on Windows. `--stock DIR`
looks in a directory you name.

## Which image goes on which board

NXP's firmware names its board in the USB product string. Verge names it in its identification
reply. daplink-rs does not report it. verge recognises the standalone MCU-Link and the MCU-Link Pro,
and lists each probe's board.

A Verge image says which boards it runs on, in a [board
declaration](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/README.md#the-board-declaration)
inside the image. An image without one runs only on the standalone MCU-Link. verge checks releases
and local files against the probe's board. It refuses a board the image does not declare, and a
board it does not know, such as an MCU-Link MR. NXP's firmware goes on any board NXP's firmware
supports.

A probe in ISP mode cannot say which board it is. `verge isp flash` lists the image's boards when it
asks you to confirm, but cannot check the probe against them. So `verge isp enter` sends a known
board other than the standalone MCU-Link to ISP mode only with `--stock`, for NXP's firmware. `verge isp flash --stock` then flashes it. To put Verge on such a board, use
`verge update --file IMAGE`, which checks the board before the probe enters ISP mode.

A running probe that does not report its board needs you to confirm the board at a terminal. `--yes`
does not skip that question.

## The SWD jumper

The SWD jumper hands the target's debug lines to another debugger. NXP's firmware and Verge both
drop their CMSIS-DAP interface while it is fitted. verge then reads the USB descriptors to tell the
firmware family, the jumper and the board, where the firmware reports it. It cannot tell which Verge
build is running.

NXP's firmware still names its board in the product string. Verge names only the Pro there; its
standalone and on-board MCU-Link strings are the same. So a standalone MCU-Link running Verge, with
the SWD jumper fitted, needs you to confirm its board before an update.

Verge still updates without the ISP jumper, through a USB request to the probe. On Windows, take the
SWD jumper off first: there, the request needs a claimed interface. NXP's update request goes over
CMSIS-DAP, so NXP's firmware from V3.122 needs the SWD jumper off too. Older NXP firmware needs the
ISP jumper either way. A batch skips probes with the SWD jumper fitted.

## When an update needs your hands

For an image that does not start, the ISP jumper is the way back. Fit it before you plug the probe in, run `verge isp flash`, then take the jumper off and run `verge isp reset`.

When an update needs the ISP jumper, or stops partway, verge prints the command that finishes it.
The command names the board, where verge knows it, and the image: a release tag, a file, or
`--stock`. `update` checks the board before it gives jumper instructions.

### ISP mode

`verge isp` works with the bootloader directly:

```
verge isp enter      # send a running probe into ISP mode, without the jumper
verge isp flash      # write an image to the probe in ISP mode
verge isp reset      # start the image the probe holds, without writing anything
verge isp info       # report the bootloader's version
```

`flash` takes the same image options as `update`, `--release`, `--file` and `--stock`, and asks the
same questions.

Every LPC55S69 bootloader has the same USB identity, so verge cannot tell an MCU-Link in ISP mode
from another LPC55S69 board. It says so before it flashes a probe that was already in ISP mode. By
default, `flash`, `reset` and `info` need exactly one probe in ISP mode. On Linux, `--port` picks
one of several by the USB port `verge` lists. Windows and macOS cannot pick a bootloader by its
port, so unplug all but one there. `flash` also refuses while an MCU-Link mini is in ISP mode, even
with `--port`. On Linux, `enter` works with another probe already in ISP mode, and names the port to
flash. `update` and batches wait until that probe is flashed or unplugged.

Use `flash` once the probe is in ISP mode, by the jumper or by `enter`. The bootloader can take a
few seconds to answer.

After flashing, verge finds the restarted probe on the same USB port. If the system reports no port,
exactly one new device has to appear; if two do, verge stops with an error.

### If the probe comes back in ISP mode

If the probe comes back in ISP mode after a flash, something still holds its ISP pin low. Check the
ISP jumper, and any probe attached to its debug connector: an MCU-Link's target connector carries
ISP on pin 7, wired to the NXP board's ISP pin. Take the jumper off or disconnect the other probe,
then run `verge isp reset`.

If `reset` brings a probe back to ISP mode without a flash before it, the probe may hold no image that starts.

### If the bootloader does not restart

If the bootloader does not restart after a flash, Linux and macOS report that it never left the bus.
The image is written and checked; unplug the probe and plug it in again to start it. Windows cannot
tell this from a quick restart with the ISP jumper fitted: unplug the probe, take the jumper off if
it is fitted, and plug it in again. Either way, verge does not count the probe as updated.

### Stopping

Ctrl-C stops at the next safe point. Before the probe enters its bootloader, it is left as it was.
During the write, it is left in ISP mode: run `verge isp flash` again with the same image, as the
recovery command says.

## Updating many probes

`verge batch` updates probes one after another as you plug them in. You choose the image and the
kinds of probe once, answer one question, then plug probes in one at a time or several on a hub.
Each one that qualifies updates without another question. The question includes any release
download.

```
# The latest release, onto NXP's firmware and older Verge builds
verge batch
# Only NXP's firmware older than V3.172
verge batch --take stock --stock-below 3.172
# A build of your own, onto any probe, stopping after 20
verge batch --file mculink_cjtag.elf --verge-any --count 20
# Probes that arrive in ISP mode, with the jumper fitted
verge batch --take isp --yes
```

### What a batch takes

The image is a release, NXP's firmware or a file, as for `update`. verge loads it and checks its
release signature once, before the batch is armed. Ctrl-C while it loads stops before any probe is
touched.

`--take` picks the kinds of probe: `stock` (NXP's firmware), `verge` and `isp`. The default is
`stock,verge`. A probe running Verge qualifies if its build is older than the image, or has no
version. `--verge-any` takes every build. A probe already running the image counts as current and is
left alone.

NXP's firmware and local files have no release version to compare with. With either, a batch that
takes older Verge builds needs `--verge-any`; otherwise leave Verge out, with `--take stock` for
instance. verge checks this before it looks for the image.

`--board` picks `mcu-link`, the default, or `mcu-link-pro`. The batch takes probes that report that
board. A Verge image has to declare the board before the batch arms; NXP's firmware allows either.
Probes in ISP mode do not report their board, so `--take isp` takes any of them, as its question
says.

### Plugging probes in

The batch takes probes plugged in after it is armed. `--include-attached` takes the ones already
attached too. As with a single update, the batch waits a second before it identifies a newly plugged
probe. If opening it fails, the batch tries again for a few polls before it skips it.

Updates run one at a time, in the order the probes arrived. The batch pauses while another device is
in ISP mode: an MCU-Link mini, a second probe, or a probe it left out. It says why, and carries on
once that device is unplugged.

The batch handles each probe once per connection, whatever happened: updated, failed, skipped or
left in ISP mode. A failure does not stop it. A probe left in ISP mode pauses it until you unplug
the probe, or flash it again with `verge isp flash` and the batch's image; the pause names the board
and the image. A bootloader that did not restart pauses it too: plug the probe in again to start its
new image.

If a probe does not come back after its update, the batch carries on. When it comes back running the
image, it counts as current. When it comes back with its old firmware, the batch tries it once more.
A second failure leaves it out until the batch ends.

A finished probe blinks its LEDs until you unplug it or the batch ends, and the batch lists its USB
port. The last one is listed without blinking, because the batch has ended. Each probe gets a result
line, and a tally ends the batch:

```
1-1      QK7RM2VTX8D41 -> CJ3E7A91C2  stock V3.172 -> Verge, build v0.3.0  UPDATED in 4.7 s
1-1      blinking: unplug it when you like
1-3      CJ3E7AC4F5  Verge, build v0.3.0  CURRENT
1-3      blinking: unplug it when you like
1-4      QK7RM2VTX8D45  stock V3.108  SKIPPED: needs the ISP jumper. Stock V3.108 has no jumper-free update. NXP added it in V3.122.
Batch done: 1 updated, 1 current, 1 skipped, 0 failed, 0 waiting.
```

The tally counts each probe once, unplugged ones included.

The batch looks for unplugged probes every quarter of a second. Windows keeps a reconnected probe's
identity, so unplugging and plugging back between two looks counts as one connection.

### Stopping a batch

Ctrl-C stops taking new probes and lets the update under way finish. A second Ctrl-C stops that
update at its next safe point; a third leaves at once. `--count N` stops after N successful updates;
N has to be at least 1. The batch exits with an error if any probe failed. Probes still waiting when
it stops get a record line too.

### The record

The batch writes one JSON object per line, to the file `--record` names, or by default to
`verge-batch-<time>.jsonl` in the current directory. It adds to a file that exists. If writing or
flushing the record fails, the update under way finishes, the batch stops, and the error is
reported. A record line:

```
{"time":"2026-10-20T14:34:58Z","port":"1-1","serial_before":"QK7RM2VTX8D41","firmware_before":"stock V3.172","serial_after":"CJ3E7A91C2","firmware_after":"Verge, build v0.3.0","result":"updated","reason":null,"seconds":4.6795}
```

`result` is one of `updated`, `jumper-fitted`, `not-restarted`, `current`, `skipped`, `failed` and
`waiting`. `reason` explains `not-restarted`, `skipped`, `failed` and `waiting`. The record keeps
every event, so one probe can have several lines; the tally counts it once.

## Releases

verge takes Verge releases from `Escapement-Project/mculink-verge` on GitHub. `--repo OWNER/NAME`,
or `VERGE_REPO`, names another repository. Public releases need no token; for a private repository,
set `GITHUB_TOKEN`.

verge allows 15 seconds to connect, 30 seconds for GitHub to answer and 60 seconds to download a
file. A timeout says which of them ran out.

verge's release key is not set yet, so verge refuses every release for now. Local files and
NXP's firmware still install.

### The cache

verge keeps downloaded releases in `~/.cache/verge` on Linux, `~/Library/Caches/verge` on macOS and
`%LOCALAPPDATA%\verge` on Windows. A recovery command that uses `verge isp flash` can reuse a
download. Each use checks the signature, the checksum and GitHub's digest of the file, where GitHub
gives one. A failed check deletes the cached release, and an unreadable one needs downloading again;
both ask before the download. You can delete the cache at any time.

Looking a release up needs GitHub even when the image is cached: GitHub says which release is the
latest, and gives the digest that checks the cache. If GitHub cannot be reached, the lookup fails.

### What a release carries

A release has to carry three files:

- exactly one `.s19` file, the image as `objcopy -O srec` writes it;
- a checksum file with the same name and `.sha256` added, in `sha256sum`'s format;
- a minisign signature with the same name and `.minisig` added. Its trusted comment contains `file:`
  and the image's name, and `tag:` and the release's tag.

For example, `mculink-verge-v0.3.0.s19`, `mculink-verge-v0.3.0.s19.sha256` and
`mculink-verge-v0.3.0.s19.minisig`. The tag is the version, and a probe built from that tag reports
it, so verge can compare what is installed. The boards an image runs on are in its declaration,
inside the signed image ([Building the
firmware](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/README.md#the-board-declaration)),
and `verge inspect IMAGE` shows it.

verge checks the signature against the public key built into it. It reports a missing signature, a
signature from another key, and an image changed after signing. It takes minisign's default
pre-hashed signatures and refuses legacy ones. The trusted comment is signed too: its file name
stops a signed file being renamed, and its tag stops an old image being published again as a newer
release. verge also checks the checksum file and GitHub's digest.

The key is `RELEASE_KEY` in verge-probe's `src/release.rs`: the base64 line of minisign's `.pub`
file. mculink-verge's README, under
[Releases](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/README.md#releases), says
how the firmware's releases are built, signed and published, how to make the key pair, and how to
make the files by hand.

