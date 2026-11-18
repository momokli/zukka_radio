# Priority chain: live > show > queue > rotation > relay > fun.

live = fallback(track_sensitive=false, harbor_slots)

radio = fallback(
  track_sensitive=false,
  [live, rotation, relay, fallback_jingle]
)
