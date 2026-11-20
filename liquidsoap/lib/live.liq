# Live selection layer (issue #30/#31).
#
# Exactly one connected caster is on air: the FCFS winner from harbor.liq's
# registry. `switch` (not `fallback`) is the only mechanism that picks one
# source by connection order rather than by availability.
#
# TODO(#32): control.liq will set/clear this ref to force a specific caster
#   (live.set "/live/slot-03" / live.clear). #30 never writes it — it is only
#   the seam so #32 can attach without touching this file's structure.
live_override = ref(null)

def is_live(m) =
  live_select(live_order(), live_override()) == m
end

# Handoff fade (#31): short 0.5 s out / 0.5 s in instead of a full crossfade.
# After a caster disconnects, its harbor source is no longer reliably
# deliverable, so fading out over a long window would fade into nothing (D11).
def live_transition(old, new) =
  add([fade.out(duration=0.5, old), fade.in(duration=0.5, new)])
end

live = switch(
  track_sensitive=false,
  transitions=[live_transition], transition_length=0.5,
  list.map(fun ((m, s)) -> ((fun () -> is_live(m)), s), casters)
)
