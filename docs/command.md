# The verge command

Every command, and the doc that explains it. `verge help COMMAND`, or `--help` after any command,
lists its options.

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

## Commands

| command | what it does | more |
| --- | --- | --- |
| `list` | lists every attached MCU-Link: its USB port and serial number, its firmware and board, and how it can update. The default when you give no command | [See what is attached](getting-started.md#see-what-is-attached) |
| `update` | installs a firmware on a probe, the best way its state allows | [Updating and recovery](updating.md) |
| `batch` | updates probes one after another as you plug them in | [Updating many probes](updating.md#updating-many-probes) |
| `isp` | works with a probe's ROM bootloader: `enter`, `flash`, `reset` and `info` | [ISP mode](updating.md#isp-mode) |
| `release` | shows a Verge release, and how each attached probe compares to it | [below](#release) |
| `inspect` | says what an image file puts into flash | [below](#inspect) |
| `read` | reads target memory over SWD, JTAG or cJTAG | [below](#read) |
| `dio` | drives and reads the DIO pins of an MCU-Link Pro running Verge | [The Pro](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/pro.md#dio-pins) |
| `power` | reads or switches the target supply of an MCU-Link Pro running Verge | [The Pro](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/pro.md#target-supply) |
| `energy` | measures a target's voltage, current and energy through an MCU-Link Pro running Verge | [Energy capture](energy.md) |

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
