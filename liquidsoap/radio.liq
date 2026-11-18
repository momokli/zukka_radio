# radio zukka v2 — Liquidsoap main script
#
# Priority chain (see docs/consolidated-architecture.md §2.4):
#   live (100) > show (80) > queue (60) > rotation (40) > relay (20) > fun (1)
#
# The scheduler drives playback via HTTP; Liquidsoap only pulls.

settings.log.level := 3

%include "lib/harbor.liq"
%include "lib/layers.liq"
%include "lib/master.liq"
%include "lib/outputs.liq"
%include "lib/control.liq"
