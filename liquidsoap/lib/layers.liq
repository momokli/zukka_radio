# Non-live layers.
# show and queue pull their next item from the scheduler as `request.dynamic`
# sources (issue #106); rotation and relay are static fallbacks.

rotation = playlist("/rotation/rotation.m3u", reload_mode="watch")

# SomaFM rejects Liquidsoap's default user agent (HTTP 403), so the relay
# presents a browser UA. Without it the relay layer is permanently silent.
relay = input.http(
  "https://ice1.somafm.com/groovesalad-128-mp3",
  user_agent="Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36"
)

fallback_jingle = single("/rotation/fun.mp3")

# --- scheduler pull layers (issue #106) ------------------------------------

# Availability refs: `true` means "this layer may hand out requests". Both start
# disabled when the scheduler URL/token are missing (fail-closed, see
# config.liq); control.liq can toggle them at runtime via `layer.available`.
show_available = ref(layers_configured)
queue_available = ref(layers_configured)

# Pull the next item for one layer. Any transport/parse problem collapses to
# `null` (= nothing on this layer), which makes the fallback drop one step
# lower. No request is even attempted while `!layers_configured`.
def next_for(layer) =
  if not layers_configured then
    null
  else
    try
      # 204 (nothing queued) yields an empty body; `request_for_body` maps it to
      # `null` just like a parse failure. Connection errors raise and are caught.
      body = http.get(
        headers=[("X-Liquidsoap-Token", liquidsoap_token)],
        timeout=5.0,
        "#{scheduler_base_url}/api/v1/playback/next?layer=#{layer}"
      )
      request_for_body(body, layers_configured)
    catch _ do
      null
    end
  end
end

# One request.dynamic per layer (D7), prefetch=1: a deeper prefetch would
# pre-fetch items ahead of a live handoff and mis-order the queue.
show = request.dynamic(
  available=show_available,
  prefetch=1,
  fun () -> next_for("show")
)

queue = request.dynamic(
  available=queue_available,
  prefetch=1,
  fun () -> next_for("queue")
)
