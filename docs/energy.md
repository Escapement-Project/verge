# Energy capture

An MCU-Link Pro running Verge measures the current a target draws and the voltage it sees. It keeps
a running total of the energy. verge reads the measurements live, records every one to a file, and
shows the total. The desktop app does the same with graphs: see [The desktop app](desktop-app.md).

## What the probe measures

The probe measures the current that flows from J9 pin 1 to pin 3, whatever supplies it, and the
voltage on the target's side. `verge power` switches the Pro's own target supply ([The
Pro](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/pro.md#target-supply)).

The probe reads voltage and current together, as one sample. A capture takes samples at a steady
rate, 1 to 100,000 a second, and adds each sample's energy to the total. Each sample is read on one
of two current ranges ([below](#the-two-current-ranges)).

The probe's calibration is checked in both of the Pro's current settings, 350 mA and 50 mA, which
J16 to J18 select. Alongside the calibrated voltage and current, the probe keeps each sample's
raw ADC codes.

A capture runs in the probe, not in verge. It goes on from `start` until `stop`, the next start or a
restart, so a debugger can use the probe meanwhile. verge reaches the capture through Verge Aux, a
second USB interface that debuggers leave alone. So `power` and the `energy` commands keep working
while a debugger has the probe open.

## Starting and stopping

```text
verge energy start --rate 10k   # capture 10,000 samples a second
verge energy latest             # the newest sample
verge energy total              # the energy since the start
verge energy stop               # stop it, and show the total
```

`start` takes 25,000 samples a second unless `--rate` says otherwise, as `10000` or `10k`. A start
clears the total, and replaces a capture that runs: it says so, with that capture's total. It can
calibrate first, which disconnects J9 from the target for about 0.2 s: a target powered through J9
loses its supply for that time. `verge power off` says when a capture runs on without the supply,
and `verge restart` when it ends a capture.

The total counts only the time the probe converts. It leaves out the moments the probe holds its
conversions, as when it times a new debug connection at a fast clock. A stopped capture keeps its
total until the next start or a restart.

`verge energy calibration` lists the points each current range was calibrated at.

## How accurate it is

- The probe's 3.0 V reference sets the scale of every reading: within 0.5%, and 100 ppm more for
  each °C.
- NXP gives the 350 mA setting's high range 5% and the other ranges 1%. The probe calibrates at
  3.3 V, so in that high range a 1.8 V target reads about 3% high. In the 50 mA setting it reads
  0.35% high.
- In the 350 mA setting, the high range reads currents under 2 mA 3 to 4% high on the Pro
  measured. An `auto` capture reads them on the low range.
- The high range's resistance can change from one start to the next, and J17's contact is part of
  it. The probe measures it as it starts, so restart the probe after moving J16 to J18.
- Rarely, the probe calibrates the 350 mA setting's high range high as it starts up: once in more
  than twenty starts on the Pro measured, by 19%. `verge energy calibration` shows it: every
  high-range code up by the same factor, with no jumper moved. A restart calibrates again.

## The two current ranges

The probe reads current on a low range and a high range. `--range` sets how a capture uses them. It
applies to a capture that `start`, `watch` or `record` starts.

`auto`, the default, switches between the ranges as the current moves. The low range reads currents
down to tens of nA, which a sleeping target needs. But each switch disturbs the reading. When the
current falls, the probe moves to the low range within about 2 ms, and the low range's sense
resistor lowers the target's supply. For about a millisecond the target's own decoupling then feeds
part of its load, so the reading dips below what the target draws. When the current rises again, the
decoupling recharges in a few microseconds, and the total misses most of that. A target can climb
back to the switch up on the low range alone, again and again. The probe then waits longer before
each switch down, up to about a second, so it reads such a target mostly on the high range.

`high` holds the high range for the whole capture. Nothing switches, so the reading follows what the
target draws. But the high range reads small currents unreliably: under about 0.65 mA in the 350 mA
setting, as the Pro measured reads it, and under about 90 µA in the 50 mA setting, scaled to its
sense resistor and not measured.

Either way, a sample read on the high range under that limit is marked under range. An `auto`
capture marks the ones it reads after the current falls, before it switches down.

Use `high` to see the shape of a load that stays above the limit. Use `auto` for a target that
sleeps under it, which only the low range reads reliably.

`watch` and `record` show a capture that is already running with the range it started with, and warn
if you give them `--range`.

## Watching live

```
verge energy watch
```

`watch` shows a capture live. The screen shows the newest sample, the total and the mean power,
over two graphs: current and power. Its title says when the capture holds the high range, and the
total's line counts the samples marked under range since the watch began.

The current graph shows every sample, as the lowest and highest in each column. The power graph
plots each interval's mean from the total, which counts every sample.

`watch` starts a capture if none runs, and stops only a capture it started. Over a capture that a
fault stopped, it shows that capture's total, then starts a new one, which clears it. Q, Esc or
Ctrl-C leaves; `--for` leaves after a set time. `--every` sets how often it reads the probe, 100 ms
by default, and `--window` how far back the graphs reach, 30 s by default. Where the output is not a
terminal, it prints a line for each read instead.

## Recording to a file

```text
verge energy record --out run.tickwave --for 10s
```

`record` saves every sample of a capture. Like `watch`, it starts a capture if none runs, and stops
only a capture it started. It records until Ctrl-C, or for as long as `--for` says. It reads the
samples through Verge Aux, so it works while a debugger holds the probe. As it starts, it says where
it records and at what rate. On a terminal, a line then counts the samples and the energy as they
come. Its own lines go to stderr.

The file's name picks the format:

- `.tickwave` writes a tickwave recording, which tickwave opens.
- `.sr` writes a sigrok session, which PulseView opens.
- `.csv` writes CSV.

A name with none of these writes a tickwave recording, or what `--csv` or `--tickwave` asks for.
verge refuses a flag that the name contradicts, such as `--csv` with `run.sr`.

`--dio` records the edges of the Pro's DIO pins too: `--dio 0,3`, `dio0` to `dio3`, or `all`.

The low range lowers the target's supply by up to about 90 mV, so a target can draw a little
differently on each range.

### A tickwave recording

A tickwave recording holds the voltage, the current and the power, each sample's current range and
its under-range mark, each DIO pin recorded, and the markers. verge closes a block of the file at
least every second, so a crash loses at most that. Once recording ends, it rewrites the file with
full-size blocks.

### A sigrok session

A sigrok session has two analog channels: `V` in volts and `mA` in milliamps. A session file stores
no units, so sigrok shows both as volts; the channel names give the units.

The probe measures nothing for a moment while it reads the target's supply, or while it measures how
quickly the target answers a debugger. There the last sample repeats and a logic channel, `GAP`, is
high, so the time axis stays true. A sample taken as the current range switched carries the current
the probe's total gives it, worked out from the samples either side, so the recording adds up to the
total. Samples marked under range are high on a logic channel of their own, `UNDER`.

A sigrok session shows each DIO pin recorded as a logic channel at the sample rate, with a pulse
shorter than a sample stretched to one sample. Where the probe lost DIO edges, the pins' levels are
unknown, and a logic channel `DIO_GAP` is high.

### CSV

CSV has one line per sample: its sequence number, its time in ns since the capture started,
millivolts, nanoamps, and the current range it was read on: `low`, `high`, or `under` for a sample
marked under range. Then come the levels of the DIO pins recorded, and `dio_gap`. Each edge of
those pins is a line of its own, in time order, with its time and the levels after it. Where the
probe lost DIO edges, `dio_gap` is 1 and the levels are empty; a line at each end of the loss marks
where it starts and where the levels are known again.

`--out - --csv` writes the CSV to stdout, for a pipe. verge's own lines go to stderr, so stdout
holds the CSV and nothing else.

### Markers

A debug session can mark moments in a capture ([Host
interface](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/host-interface.md#markers-0x94)
has the command). Markers keep their own times. CSV gives each marker a row of its own, with its
number and label, in time order among the samples and edges. A sigrok session carries them in a
`markers.csv` inside it. That file also gives each marker's `session_time_ns`: its time from the
session's first sample, negative for a marker before it. PulseView does not draw the markers as an
annotation track. Resaving with sigrok drops the extra file, so keep the original session to keep
its markers.

Labels keep UTF-8 text. Backslashes, control characters and other bytes are escaped. `marker_lost`
reports markers the probe lost; an empty time means the probe reported a loss without a marker to
place it. A report without a time applies to the whole capture, including an export of part of it.

CSV writes samples and edges as they arrive. When recording ends, verge adds the markers by writing
a new copy beside the file and swapping it in, so it needs to be able to write in the file's
directory. A recording with no markers and no marker loss has no marker columns. CSV to a pipe or a
terminal has none either: verge warns at the first marker and leaves the markers out.

## For tool authors

The capture's commands, and the energy stream Verge Aux carries, are in [Host
interface](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/host-interface.md):
vendor command `0x90` and the energy stream. Holding the high range is `0x90` mode 14.
