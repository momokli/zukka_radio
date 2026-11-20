# Live selection layer (issue #30).
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

# TODO(#31): add transitions=[live_transition] and transition_length=0.5 for the
#   handoff fade. #30 is a hard cut on purpose so the selection stays reviewable.
live = switch(
  track_sensitive=false,
  list.map(fun ((m, s)) -> ((fun () -> is_live(m)), s), casters)
)
