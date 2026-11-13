# AGENTS.md

Agent rules for verge, verge-gui and their docs.

**Plotting costs as little as it can.** Energy captures get very large, and verge-gui runs on
whatever machine sits beside the bench. A view's work stays off the UI thread where it can, and what
is left fits well inside a frame with the process held to one core (`taskset -c 0`), the stand-in
for a less powerful machine. Latency from input or from new data to the screen is measured, stated
and kept down: it feels worse than it measures. A change to drawing or querying comes with its
frame-time and latency figures. tickwave's renderer, which verge-gui draws through, keeps the same
rule.
