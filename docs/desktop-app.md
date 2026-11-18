# The desktop app

verge-gui gives the verge command's update workflows a window: it updates units, one at a time or
in a batch. It also drives the DIO pins of both boards and records them, and on a Pro it switches
the target supply and records energy captures. It comes in the same archive as verge, as Verge.app
on macOS ([Getting started](getting-started.md#install-verge)). About, at the top right, gives its
version and its licences.

## Updating a unit

The Units view lists the attached units on the left, each with its firmware, its board and its USB
port. Rescan looks again. Select a unit to see its firmware, how it updates, and the image choices:
Release, NXP's firmware or File. Release is the latest release; to install another, write its tag
under Tag and press Look up, as `verge update --release` takes one. NXP's firmware comes from your
LinkServer install. Browse for a file with File's dialog, or drop it on the window. The app cannot
take dropped files under Wayland.

Read the board and jumper advice, choose the update or install action it shows, and confirm. The app
asks before it flashes, and the same question covers any download. Escape answers Back. During the
update it shows each step and the bytes written. Cancel stops it at the next safe point, and closing
the window waits for the update to finish. When it is done, check the result, and the log along the
bottom: Show opens it, and Copy copies every line it keeps, each with its local time. If a step
fails, the result marks it, and says whether anything was flashed.

The app checks a local file against the unit's board as soon as you select it. A release's boards
are checked after it downloads, before the unit is touched. If the unit needs the ISP jumper, the
app names its board and the image to flash in ISP mode. For a unit already in ISP mode, or one
whose board is unknown, the confirmation lists the file's boards. [Updating and
recovery](updating.md) explains the cases.

A unit in ISP mode can also leave it without being flashed: take the ISP jumper off, then press
Reset out of ISP mode, as `verge isp reset` does. The unit starts the image it holds. An update
that leaves the unit in ISP mode because its jumper is still fitted offers the same reset.

The other way round, Enter ISP mode restarts a unit into ISP mode without the jumper, and leaves it
there for a flash, after asking, as `verge isp enter` does. It follows the same rules. In ISP mode
nothing tells a unit's board, so a unit other than a standalone MCU-Link enters it only with NXP's
firmware chosen, which goes on any board. A unit enters only by a jumper-free path, and a unit a
debugger holds does not enter.

Your user needs permission to open a unit; on Linux, the udev rules on [Getting
started](getting-started.md#usb-permissions-on-linux) give it. The app says so for a unit it may
not open, and on Linux when nothing is listed.

## Updating many units

Batch, at the top, opens the batch view. It sets up the same batch as `verge batch`: the image, the
kinds of unit, the board, whether to take the units attached now, a count to stop after, and the
record file. The record goes in your data directory unless you choose another place:
`~/.local/share/verge` on Linux, `~/Library/Application Support/verge` on macOS and
`%APPDATA%\verge` on Windows. If the form cannot arm, the reason shows under the field it is about.

Once armed, the view shows the tally, the update under way, the queue and the finished units. Stop
sits at the top, in the batch's own panel, and takes no new units. After Stop, "Cancel the unit
under way" stops the active update at its next safe point. While the image is still loading, Stop or
closing the window ends the batch at once; no unit has been flashed yet. If a unit fails and stays
in ISP mode, the batch waits until it is unplugged; to flash it again, stop the batch and flash it
under Units.

## Bench

Bench reaches the unit through Verge Aux, a second USB interface that debuggers leave alone ([The
Pro](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/pro.md#verge-aux)). So a
debugger can keep CMSIS-DAP open meanwhile. On a standalone MCU-Link, Bench has the DIO pins. On a
Pro, it also has the target supply and energy capture. A Pro without its energy measurement circuit
has the target supply and the DIO pins, and no energy capture.

The unit list names each unit's board and what it runs. A unit Bench can do nothing with is greyed
out, and says why; Update under Units goes to the Units view, where a unit not running Verge gets
it. If the session with the unit fails, or another program holds its Verge Aux, Reconnect opens it
again. Restart restarts the unit as unplugging it would, after asking, as `verge restart` does: the
DIO pins go back to inputs, a Pro's target supply turns off and a capture ends.

Bench asks its questions in its own panel, and holds the rest of the panel until you answer. Escape
answers Back.

The supply switches at once. Turning it off while a capture runs asks first. The panel shows the
supply as commanded and the voltage as measured; they are separate readings. Without the energy
circuit, there is no measured voltage. Beside the voltage is the probe's energy total, as `verge
energy total` reads it. Calibration opens the points the probe's latest calibration left for each
current range, as `verge energy calibration` shows them: each point's ADC code and the current it
stands for.

DIO shows each pin's direction and level. Switching to output asks first: check that the target is
not driving those pins. Inputs have pull-downs, and outputs read back their own drive. Pins that a
bench tool's command marker holds are not listed, and the panel names them; Restart gives them
back.

On a Pro, DIO0 to DIO2 change direction together, and DIO3 on its own. DIO needs the J15 jumper off
when the probe started.

On a standalone MCU-Link, the pins are J8's pads, and each changes direction on its own. They work
at 3.3 V only, so a pin driven high can damage a target that runs at a lower voltage ([Probes and
jumpers](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/probes.md#the-mcu-links-dio-pins)).

## Recording

Start recording starts a capture at 1 to 100,000 pairs a second, and records it. Auto range and High
range choose how the capture uses the current ranges, as `--range` does for `verge energy record`
([Energy capture](energy.md#the-two-current-ranges) says what each costs). Starting clears the
probe's energy total, and its calibration can cut the target's power for about 0.2 s, so it asks
first.

If another program already runs a capture, the panel shows its rate, and Record running capture
records it at that rate. Stop recording stops a capture the window started, and leaves another
program's running. If the probe's last capture failed, the panel says so, with what it measured
before it failed.

A program stops only the capture it started or joined, not one another program has started since:
each capture has an identity, and the window keeps the one it started or joined. If the reply to a
start goes missing, the capture can be left running. Record running capture, or `verge energy stop`,
sorts that out.

On a standalone MCU-Link, and on a Pro without its energy measurement circuit, Start recording
records the DIO pins alone, as `verge dio record` does.

### The graph

The graph shows current, voltage or power. While a recording runs, the graph follows its newest
pairs: 1s, 10s and 60s set how much of them it shows. A finished recording shorter than that opens
whole. The Live button at the right end of the time axis stops following, and starts it again.
Dragging along time, or selecting, also stops following; zooming keeps it.

Drag or scroll the graph to move along time, and ctrl-scroll or pinch to zoom. The window's marker
in the overview below the graph drags too; on a short window the overview gives the graph its
room. The vertical axis fits what is visible until you move it: drag or scroll up or down, or drag
along the axis to zoom it. Fit Y, or a double click, fits it again.

Zoomed out, each column of the graph spans the lowest to the highest reading in its time, so a pulse
shorter than a column keeps its height. Zoomed in, the graph marks each pair. Once it shows single
pairs, a pair marked under range draws pale, and a capture that held the high range says so under
the graph.

Where pairs were lost within a capture, the graph shades their time and draws the columns around it
pale. A pause, as while a debugger holds the conversions, only breaks the trace.

### Measuring

With Select on, dragging across the graph selects an interval. The panel on the left sets its ends
exactly, and zooms to it. Select to next marker selects from a marker to the one after it.

The panel measures the visible window and the selection: duration, pairs, minimum, mean and peak
current, mean voltage and power, charge and energy. It shows how much of each was not measured,
including any part of it outside the recording; time without measurements adds nothing to charge or
energy. The readings come from the recorded pairs, so zooming does not change them. A recording of
the DIO pins alone has a duration and each pin's edges.

### The DIO pins

Recordings include all four DIO pins where the unit offers them, in lanes below the trace on the
same time axis. With J15 fitted they hold none, and the panel says so before you start. Zoomed out,
a lane keeps short pulses visible. Edges the probe lost leave breaks, and a count of the losses.

### Markers

Markers show as labelled lines on the graph, and in a list that pages. Picking one centres the view
on it. The panel reports markers the probe lost, above the marker that follows the loss.

### Keeping, opening and exporting

Each recording goes to disk as it arrives, for as long as the disk has room, as a
[tickwave](https://github.com/Escapement-Project/tickwave) recording (`.tickwave`), which tickwave
opens too. Recordings stay in a `captures` folder in the data directory until you discard them. Kept
on disk reopens one, and repairs a recording a crash cut short. Once a recording stops, Save as
copies it somewhere else, Open reads a recording back, and Discard deletes the open recording from
disk, after asking.

Once a recording stops, Export writes it, or the selection, as CSV or as a sigrok session, with
voltage, current and a `GAP` channel. Exports include the DIO levels, and a `DIO_GAP` channel
(`dio_gap` in CSV) for time whose levels are unknown. Both formats keep markers as
`verge energy record` does ([Energy capture](energy.md#markers)). An export keeps whole pairs at its
ends, with their own times, so it can start a little before the selection. A recording of the DIO
pins alone has no pairs to export.

The app holds still when the desktop asks for reduced motion, and follows the setting when it
changes.
