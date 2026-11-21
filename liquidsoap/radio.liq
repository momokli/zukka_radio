# radio zukka v2 — Liquidsoap main script
#
# Priority chain (see docs/consolidated-architecture.md §2.4):
#   live (100) > show (80) > queue (60) > rotation (40) > relay (20) > fun (1)
#
# The scheduler drives playback via HTTP; Liquidsoap only pulls.

settings.log.level := 3

# Include order matters: config.liq reads the scheduler URL/token first,
# harbor.liq creates the slots and the FCFS registry, live.liq consumes
# `casters` to build `live`, playback.liq provides the pure parse helpers,
# layers.liq builds show/queue, master.liq wires the chain.
%include "lib/config.liq"
%include "lib/harbor.liq"
%include "lib/live.liq"
%include "lib/playback.liq"
%include "lib/layers.liq"
%include "lib/master.liq"
%include "lib/outputs.liq"
%include "lib/control.liq"
