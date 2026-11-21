# Priority chain: live > show > queue > rotation > relay > fun.
# `live` is defined in lib/live.liq (issue #30): the FCFS-selected caster.
# `show`/`queue` are the scheduler pull layers (issue #106); they are unavailable
# while the scheduler URL/token are unset, so the chain falls straight through.
#
# Weights (docs/consolidated-architecture.md §2.4): 100 live > 80 show >
# 60 queue > 40 rotation > 20 relay > 1 fun — encoded here by the list order
# of a `track_sensitive=false` fallback.

radio = fallback(
  track_sensitive=false,
  [live, show, queue, rotation, relay, fallback_jingle]
)
