# The desktop app

verge-gui gives the verge command's update workflows a window: it updates probes, one at a time or
in a batch. On a Pro it also switches the target supply, drives the DIO pins and records energy
captures. On Linux and Windows it comes in the same archive as verge
([Getting started](getting-started.md)); on macOS, build it from source
([Building](../README.md#building)).

## Updating a probe

Attached probes are listed on the left. Select one to see its firmware, how it updates, and the
image choices: Release, Stock or File. Stock is NXP's firmware, from your LinkServer install. Browse
for a file with File's dialog, or drop it on the window. The app cannot take dropped files under
Wayland.

Read the board and jumper advice, choose the update or install action it shows, and confirm. The app
asks before it flashes, and the same question covers any download. During the update it shows each
step and the bytes written. Cancel stops it at the next safe point, and closing the window waits for
the update to finish. When it is done, check the result, and the log along the bottom: Show opens
it.

The app checks a local file against the probe's board as soon as you select it. A release's boards
are checked after it downloads, before the probe is touched. If the probe needs the ISP jumper, the
app names its board and the image to flash in ISP mode. For a probe already in ISP mode, or one
whose board is unknown, the confirmation lists the file's boards. [Updating and
recovery](updating.md) explains the cases.

## Updating many probes

Batch, at the top, opens the batch view. It sets up the same batch as `verge batch`: the image, the
kinds of probe, the board, whether to take the probes attached now, a count to stop after, and the
record file. The record goes in your data directory unless you choose another place:
`~/.local/share/verge` on Linux, `~/Library/Application Support/verge` on macOS and
`%APPDATA%\verge` on Windows.

Once armed, the view shows the tally, the update under way, the queue and the finished probes. Stop
takes no new probes. After Stop, the button "Cancel the unit under way" stops the active update at its next safe point. While the image is still loading, Stop or closing the window ends the batch at
once; no probe has been flashed yet.

## The Pro's bench

Bench reaches a Pro's target supply, DIO pins and energy capture through Verge Aux, a second USB
interface that debuggers leave alone ([The
Pro](https://github.com/Escapement-Project/mculink-verge/blob/HEAD/docs/pro.md#verge-aux)). So a
debugger can keep CMSIS-DAP open meanwhile.

The supply switches at once. Turning it off while a capture runs asks first. The panel shows the
supply as commanded and the voltage as measured; they are separate readings.

DIO shows each pin's direction and level. DIO0 to DIO2 change direction together, and DIO3 on its
own. Switching to output asks first: check that the target is not driving those pins. Inputs have
pull-downs, and outputs read back their own drive. DIO needs a current Verge build, which carries
the DIO pins on Verge Aux, and the J15 jumper off when the probe started.

## Recording energy

Start recording starts a capture at 1 to 100,000 pairs a second, and records it. Auto range and High
range choose how the capture uses the current ranges, as `--range` does for `verge energy record`
([Energy capture](energy.md#the-two-current-ranges) says what each costs). Starting clears the
probe's energy total, and its calibration can cut the target's power for about 0.2 s.

If another program already runs a capture, Record running capture records it at its own rate. Stop
recording stops a capture the window started, and leaves another program's running.

Starting, recording and watching need a Verge build with capture identities, which let a program
stop the capture it started without stopping a capture another program has started since. Each keeps
the identity of the capture it started or joined, so cleaning up leaves a newer capture running. If
the reply to a start goes missing, the capture can be left running. Record running capture, or
`verge energy stop`, sorts that out.

### The graph

The graph shows current, voltage or power. Live follows the newest pairs. Inspect holds the view
while the recording carries on.

Drag or scroll the graph to move along time, and ctrl-scroll or pinch to zoom. The window's marker
in the overview below the graph drags too. The vertical axis fits what is visible until you move it:
drag or scroll up or down, or drag along the axis to zoom it. Fit Y, or a double click, fits it
again.

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
current, mean voltage and power, charge and energy. It shows how much of each was not measured; time
without measurements adds nothing to charge or energy. The readings come from the recorded pairs, so
zooming does not change them.

### The DIO pins

Recordings include all four DIO pins, in lanes below the trace on the same time axis. Zoomed out, a
lane keeps short pulses visible. Edges the probe lost leave breaks, and a count of the losses.

### Markers

Markers show as labelled lines on the graph, and in a list that pages. Picking one centres the view
on it. The panel reports markers the probe lost.

### Keeping, opening and exporting

Each recording goes to disk as it arrives, for as long as the disk has room. Recordings stay in a
`captures` folder in the data directory until you discard them. Kept on disk reopens one, including
a recording cut short by a crash. Save as copies the open capture somewhere else, and Open reads one
back.

Export writes the capture, or the selection, as CSV or as a sigrok session, with voltage, current
and a `GAP` channel. Exports include the DIO levels, and a `DIO_GAP` channel (`dio_gap` in CSV) for
time whose levels are unknown. Both formats keep markers as `verge energy record` does ([Energy
capture](energy.md#markers)). An export keeps whole pairs at its ends, with their own times, so it
can start a little before the selection. A CSV export of a capture saved before the range column
existed leaves the range empty.
