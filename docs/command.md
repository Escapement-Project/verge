# The verge command

Every command, with its options. Most sections lead to the doc that explains the command at length.
`verge help COMMAND`, or `--help` after any command, prints the same from the tool itself.

## Which probe a command acts on

A command acts on one probe. `--serial` names it by its serial number, before or after the command.
Without it, verge takes the only attached probe the command can act on. When several could be meant,
it lists them and asks at a terminal; elsewhere it stops and asks for `--serial`.

A Pro command, such as `energy`, looks only at Pros running Verge, so a standalone MCU-Link beside a
Pro needs no serial. `list` and `release` show only the probe `--serial` names.

## Options every command takes

| option | what it does |
| --- | --- |
| `--serial SERIAL` | the probe to act on, as above |
| `-v`, `--verbose` | say more about what is happening; twice for USB-level detail |
| `--repo OWNER/NAME` | the GitHub repository Verge releases come from; `VERGE_REPO` too. The default is `Escapement-Project/mculink-verge` |
| `--github-token TOKEN` | a GitHub token, for a private repository; `GITHUB_TOKEN` too |

## `list`

```
verge
verge list
```

Lists every attached MCU-Link: its USB port and serial number, the firmware it runs, its board, and
how it can update. The default when you give no command. `-v` adds what the firmware reports about
itself.

## `update`

```
verge update [--release [TAG] | --file FILE | --stock [DIR]] [--yes] [--force]
```

Installs a firmware on a probe, the best way its state allows. A probe already in ISP mode is
flashed as it is.

| option | what it does |
| --- | --- |
| `--release [TAG]` | Verge from a GitHub release: the latest, or the one tagged TAG. The default |
| `--file FILE` | an ELF or S-record file, such as a Verge build of your own |
| `--stock [DIR]` | NXP's firmware from your LinkServer install: the newest found, or the newest in DIR. verge never downloads it |
| `-y`, `--yes` | flash without asking first |
| `--force` | install a release even when the probe runs the same build or a newer one |

[Updating and recovery](updating.md).

## `batch`

```
verge batch [--release [TAG] | --file FILE | --stock [DIR]] [--take KINDS] [options]
```

Updates every probe of a chosen kind as it is plugged in, one at a time, until stopped. Each
finished probe blinks so you can tell which to unplug. Ctrl-C stops taking new probes; the one under
way finishes. Every probe gets a line in the record file.

| option | what it does |
| --- | --- |
| `--release`, `--file`, `--stock` | the image, as for `update` |
| `--take KINDS` | which probes to take: `stock` (NXP's firmware), `verge`, `isp` (probes that arrive in ISP mode). The default is `stock,verge` |
| `--stock-below VERSION` | take only NXP's firmware older than VERSION, such as 3.172. Implies `--take stock` |
| `--verge-any` | take Verge of any build, not only builds older than the image or without a version. Implies `--take verge` |
| `--include-attached` | take probes attached already, not only those plugged in from now on |
| `--board BOARD` | `mcu-link`, the default, or `mcu-link-pro`. A Verge image has to declare the Pro |
| `--count N` | stop once N probes are updated |
| `--record FILE` | the record file, one JSON line per probe. The default is `verge-batch-<time>.jsonl` in the current directory |
| `-y`, `--yes` | arm without asking first |

[Updating many probes](updating.md#updating-many-probes).

## `isp`

Works with a probe's ROM bootloader, ISP mode.

```
verge isp enter [--stock]
verge isp flash [--release [TAG] | --file FILE | --stock [DIR]] [--port PORT] [--yes]
verge isp reset [--port PORT]
verge isp info [--port PORT]
```

| command | what it does |
| --- | --- |
| `enter` | restarts a probe into its ROM bootloader without the jumper, and leaves it there. In ISP mode a probe cannot say which board it is, so a probe known to be another board than the standalone MCU-Link is sent there only with `--stock`: NXP's firmware goes on any board |
| `flash` | flashes the probe in ISP mode, as after fitting the jumper. With the jumper still fitted, the probe comes back in ISP mode: take the jumper off and run `verge isp reset` |
| `reset` | resets a probe out of ISP mode without flashing it, so it starts the image it holds |
| `info` | asks the bootloader what it is. Changes nothing |

`--port PORT` picks the probe in ISP mode at that USB port, as `verge list` names it, when there are
several. It works only where the system ties a bootloader to its port, as Linux does.

[ISP mode](updating.md#isp-mode).

## `release`

```
verge release [TAG]
```

Shows a Verge release, the latest unless you give a tag, and how each attached probe compares to it.

## `inspect`

```
verge inspect FILE
```

Reads an ELF or S-record file and says what it puts into flash, including the boards a Verge image
declares. Touches no USB device. [Board declarations](updating.md#board-declarations).

## `read`

```
verge read --chip CHIP [--protocol swd|jtag|cjtag] [--speed KHZ] [--time] WIDTH ADDRESS WORDS
```

Reads target memory through a probe over SWD, JTAG or cJTAG, as probe-rs's `read` does. It prints
probe-rs's hex table on stdout and nothing else: each line is an address and eight values, four for
`b64`. It picks among every probe probe-rs can drive, not only MCU-Links. Like probe-rs's `read`, it
resumes any halted core when it finishes.

| argument or option | what it does |
| --- | --- |
| `WIDTH` | the width of each value: `b8`, `b16`, `b32` or `b64` |
| `ADDRESS` | the first address: decimal, `0x` hexadecimal, `0o` octal or `0b` binary |
| `WORDS` | how many values to read |
| `--chip CHIP` | the target chip, as probe-rs names it |
| `--protocol` | the wire: `swd`, the default, `jtag` or `cjtag`. cJTAG works on a probe running Verge |
| `--speed KHZ` | the wire clock in kHz; the probe's default when left out |
| `--time` | print the attach and read times on stderr |

```
verge read --chip CC1312R --protocol cjtag b32 0 64
```

## `cjtag`

```
verge cjtag [--oscan1-only | --all | --formats FORMATS]
```

Shows or limits the cJTAG scan formats a probe running Verge offers a host. A host uses the format
it prefers of those both the probe and the target have. A limit lasts until the probe restarts.

| option | what it does |
| --- | --- |
| `--oscan1-only` | offer OScan1 alone, which is what to look at on an analyser |
| `--formats FORMATS` | offer these formats, by name or number: `--formats oscan1,oscan5` or `9,13`. OScan1 stays offered whatever the list says, and a format the image does not have is left out |
| `--all` | offer every format the image has again |

[Wires and
clocks](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/wires.md#cjtag).

## `dio`

```
verge dio [--high PINS] [--low PINS] [--input PINS]
```

Drives and reads the four DIO pins on an MCU-Link Pro's J19 header, on a probe running Verge. With
no options it reads them.

| option | what it does |
| --- | --- |
| `--high PINS` | drive these pins high: `0` to `3`, as `--high 0,1,2`, or `dio0` to `dio3`, or `all` |
| `--low PINS` | drive these pins low |
| `--input PINS` | make these pins inputs again |

DIO0, DIO1 and DIO2 change direction together, and DIO3 on its own, so a change has to turn a whole
group round: `--high 0` alone is refused while DIO1 and DIO2 are inputs. The pins keep their state
between debug sessions, and go back to inputs when the probe restarts. [The
Pro](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/pro.md#dio-pins).

## `power`

```
verge power [on|off]
```

Reads or switches the target supply of an MCU-Link Pro running Verge. With no argument it reads it.
The state shown is the one the firmware set, not a measured voltage. [The
Pro](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/pro.md#target-supply).

## `energy`

Measures a target's voltage, current and energy through an MCU-Link Pro running Verge.

```
verge energy start [--rate N] [--range auto|high]
verge energy latest
verge energy total
verge energy stop
verge energy calibration
verge energy watch [--rate N] [--every TIME] [--window TIME] [--for TIME] [--range auto|high]
verge energy record --out FILE [--rate N] [--for TIME] [--csv] [--dio PINS] [--range auto|high]
```

| command | what it does |
| --- | --- |
| `start` | starts a capture. It runs in the probe until `stop`, the next start or a restart |
| `latest` | shows the newest voltage and current pair |
| `total` | shows the energy since the last start |
| `stop` | stops the capture and shows its total |
| `calibration` | shows the points each current range was calibrated at |
| `watch` | shows the capture live: the newest pair, the total, and graphs of current and power |
| `record` | records every pair of a capture to a file |

| option | what it does |
| --- | --- |
| `--rate N` | voltage and current pairs a second, 1 to 100,000, for a capture the command starts. The default is 1,000 |
| `--range` | how a capture the command starts uses the current ranges: `auto`, the default, or `high` |
| `--every TIME` | how often `watch` reads the probe, as `100ms` or `1s`. The default is 100 ms |
| `--window TIME` | how far back `watch`'s graphs reach. The default is 30 s |
| `--for TIME` | leave, or stop recording, after this long |
| `--out FILE` | the file `record` writes |
| `--csv` | write CSV instead of a sigrok session |
| `--dio PINS` | record the edges of these DIO pins too |

[Energy capture](energy.md).
