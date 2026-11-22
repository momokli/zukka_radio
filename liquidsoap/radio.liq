# radio zukka v2 — Liquidsoap main script
#
# No priority chain: the six program channels (live, show, queue, rotation,
# relay, fun) are summed by a mixer graph (lib/mixer.liq), not a `fallback`
# (issue #145). The scheduler drives playback via HTTP; Liquidsoap only pulls.
#
# The scheduler decides WHICH source should play and applies it through mixer
# presets (docs/product-v2-cockpit.md §4); Liquidsoap no longer picks a winner.

settings.log.level := 3

# Include order matters: fun_jingle.liq provides the pure jingle helpers,
# config.liq reads the scheduler URL/token and the jingle path next, harbor.liq
# creates the slots and the FCFS registry, live.liq consumes `casters` to build
# `live`, playback.liq provides the pure parse helpers, layers.liq builds
# show/queue and the fun layer, mixer.liq provides the mixer helpers/strips and
# master.liq instantiates the graph (mixer before master).
%include "lib/fun_jingle.liq"
%include "lib/config.liq"
%include "lib/harbor.liq"
%include "lib/live.liq"
%include "lib/playback.liq"
%include "lib/layers.liq"
%include "lib/mixer.liq"
%include "lib/master.liq"
%include "lib/events.liq"
%include "lib/tape.liq"
%include "lib/outputs.liq"
%include "lib/control.liq"
