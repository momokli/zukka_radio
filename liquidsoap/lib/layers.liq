# Non-live layers.
# show and queue will become request.dynamic against the scheduler (M1).
# rotation and relay are static fallbacks.

rotation = playlist("/rotation/rotation.m3u", reload_mode="watch")

# SomaFM rejects Liquidsoap's default user agent (HTTP 403), so the relay
# presents a browser UA. Without it the relay layer is permanently silent.
relay = input.http(
  "https://ice1.somafm.com/groovesalad-128-mp3",
  user_agent="Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36"
)

fallback_jingle = single("/rotation/fun.mp3")
