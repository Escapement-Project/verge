# Energy capture

An MCU-Link Pro running Verge measures the current a target draws and the voltage it sees. It keeps
a running total of the energy. verge reads the measurements live, records every one to a file, and
shows the total. The desktop app does the same with graphs: see [The desktop app](desktop-app.md).

## What the probe measures

The probe measures the current that flows from J9 pin 1 to pin 3, whatever supplies it, and the
voltage on the target's side. `verge power` switches the Pro's own target supply ([The
Pro](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/pro.md#target-supply)).

The probe takes voltage and current together, as a pair. A capture takes pairs at a steady rate, 1
to 100,000 a second, and adds each pair's energy to the total. Each pair is read on one of two
current ranges ([below](#the-two-current-ranges)).

Energy capture needs the board's HW_VER_5 strap fitted
([Boards](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/BOARDS.md) lists the
straps). The probe's calibration is checked in both of the Pro's current settings, 350 mA and 50 mA,
which J16 to J18 select. Alongside the calibrated voltage and current, the probe keeps each pair's
raw ADC codes.

A capture runs in the probe, not in verge. It goes on from `start` until `stop`, the next start or a
restart, so a debugger can use the probe meanwhile. verge reaches the capture through Verge Aux, a
second USB interface that debuggers leave alone. So `power` and the `energy` commands keep working
while a debugger has the probe open.

## Starting and stopping

```
verge energy start --rate 10000   # start a capture at 10,000 pairs a second
verge energy latest               # the newest pair
verge energy total                # the energy since the start
verge energy stop                 # stop it, and show the total
```

`start` takes 1,000 pairs a second unless `--rate` says otherwise. A start clears the total. It can
calibrate first, which disconnects J9 from the target for about 0.2 s: a target powered through J9
loses its supply for that time.

The total counts only the time the probe converts. It leaves out the moments the probe holds its
conversions, as when it times a new debug connection at a fast clock. A stopped capture keeps its
total until the next start or a restart.

`verge energy calibration` lists the points each current range was calibrated at.

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
target draws. But the high range reads small currents unreliably. In the 350 mA setting that starts
under about 0.36 mA: a target asleep at 166 µA read anywhere from 137 to 363 µA. Scaled to the 50 mA
setting, the limit is about 46 µA, which has not been measured.

Either way, a pair read on the high range under that limit is marked under range. An `auto` capture
marks the ones it reads after the current falls, before it switches down.

Use `high` to see the shape of a load that stays above the limit. Use `auto` for a target that
sleeps under it, which only the low range reads reliably.

`watch` and `record` show a capture that is already running with the range it started with, and warn
if you give them `--range`.

## Watching live

```
verge energy watch
```

`watch` shows a capture live. The screen shows the newest pair, the total and the mean power, over
two graphs: current and power. Its title says when the capture holds the high range, and the total's
line counts the pairs marked under range since the watch began.

The current graph shows every pair, as the lowest and highest in each column. The power graph plots
each interval's mean from the total, which counts every pair.

`watch` starts a capture if none runs, and stops only a capture it started. Over a capture that a
fault stopped, it shows that capture's total, then starts a new one, which clears it. Q, Esc or
Ctrl-C leaves; `--for` leaves after a set time. `--every` sets how often it reads the probe, 100 ms
by default, and `--window` how far back the graphs reach, 30 s by default. Where the output is not a
terminal, it prints a line for each read instead.

## Recording to a file

```
verge energy record --out run.sr --for 10s
```

`record` saves every pair of a capture. Like `watch`, it starts a capture if none runs, and stops
only a capture it started. It records until Ctrl-C, or for as long as `--for` says. It reads the
pairs through Verge Aux, so it works while a debugger holds the probe, and needs a Verge build that
has Verge Aux.

`--dio` records the edges of the Pro's DIO pins too: `--dio 0,3`, `dio0` to `dio3`, or `all`.

The low range lowers the target's supply by up to about 90 mV, so a target can draw a little
differently on each range.

### A sigrok session

By default the file is a sigrok session, which PulseView opens. It has two analog channels: `V` in
volts and `mA` in milliamps. A session file stores no units, so sigrok shows both as volts; the
channel names give the units.

The probe measures nothing for a moment while it reads the target's supply, or times a debugger's
reads. There the last pair repeats and a logic channel, `GAP`, is high, so the time axis stays true.
A pair taken as the current range switched carries the current the probe's total gives it, worked
out from the pairs either side, so the recording adds up to the total. Pairs marked under range are
high on a logic channel of their own, `UNDER`.

A sigrok session shows each DIO pin recorded as a logic channel at the pair rate, with a pulse
shorter than a pair stretched to one. Where the probe lost DIO edges, the pins' levels are unknown,
and a logic channel `DIO_GAP` is high.

### CSV

`--csv` writes one line per pair instead: its sequence number, its time in ns since the capture
started, millivolts, nanoamps, and the current range it was read on: `low`, `high`, or `under` for a
pair marked under range. Then come the levels of the DIO pins recorded, and `dio_gap`. Each edge of
those pins is a line of its own, in time order, with its time and the levels after it. Where the
probe lost DIO edges, `dio_gap` is 1 and the levels are empty; a line at each end of the loss marks
where it starts and where the levels are known again.

### Markers

A debug session can mark moments in a capture ([Host
interface](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/host-interface.md#markers-0x94)
has the command). Markers keep their own times. CSV gives each marker a row of its own, with its
number and label, in time order among the pairs and edges. A sigrok session carries them in a
`markers.csv` inside it. That file also gives each marker's `session_time_ns`: its time from the
session's first sample, negative for a marker before it. PulseView does not draw the markers as an
annotation track. Resaving with sigrok drops the extra file, so keep the original session to keep
its markers.

Labels keep UTF-8 text. Backslashes, control characters and other bytes are escaped. `marker_lost`
reports markers the probe lost; an empty time means the probe reported a loss without a marker to
place it. A report without a time applies to the whole capture, including an export of part of it.

CSV writes pairs and edges as they arrive. When recording ends, verge adds the markers by writing a
new copy beside the file and swapping it in, so it needs to be able to write in the file's
directory. A recording with no markers and no marker loss has no marker columns. CSV to a pipe or a
terminal has none either: verge warns at the first marker and leaves the markers out.

## For tool authors

The capture's commands, and the energy stream Verge Aux carries, are in [Host
interface](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/host-interface.md):
vendor command `0x90` and the energy stream. Holding the high range is `0x90` mode 14.
