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

# Priority-1 fun layer (issue #121): re-requesting the configured jingle as
# soon as it ends loops one file. `playlist(loop=true, <audio file>)` is not an
# option here: it parses the file as an M3U and fails on a bare mp3 (verified
# against v2.4.5). A missing path is retried instead of ending the source, and
# the low sine keeps the icecast source alive while making the fault audible
# (a silent stream is exactly the failure #121 reports).
fun_jingle_available = file.exists(fun_jingle_path)
fun_jingle_mode = fun_jingle_choice(fun_jingle_available)

if fun_jingle_mode == "substitute" then
  log(level=3, fun_jingle_log(fun_jingle_path))
end

fun_jingle_loop = request.dynamic(
  retry_delay=5.0,
  prefetch=1,
  fun () ->
    if fun_jingle_path == "" then
      null
    else
      request.create(fun_jingle_path)
    end
)

# Low amplitude: audible as a technical fault, not as a music bed.
fun_jingle_substitute = sine(220.0, amplitude=0.05)

fallback_jingle = fallback(
  track_sensitive=false,
  [fun_jingle_loop, fun_jingle_substitute]
)

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
