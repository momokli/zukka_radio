# Priority chain: live > show > queue > rotation > relay > fun.
# `live` is defined in lib/live.liq (issue #30): the FCFS-selected caster.

radio = fallback(
  track_sensitive=false,
  [live, rotation, relay, fallback_jingle]
)
