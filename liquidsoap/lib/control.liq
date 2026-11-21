# Control API: scheduler → Liquidsoap (issue #32).
#
# Commands (see docs/consolidated-architecture.md §4.4):
#   live.set, live.clear, live.status
#
# Transport, verified against the pinned Liquidsoap 2.4.5 image:
# `server.harbor` starts the command server (telnet over HTTP). Registered
# commands are invoked with a **POST** whose body is the command line:
#
#   POST http://liquidsoap:8010/control
#   body: live.set /live/slot-03
#
# The response body is the command's return value. Command names are
# registered without a namespace so `live.set` stays `live.set`.
#
# The port is only reachable on the internal `back` network (D14) and is never
# published to the host.

control_port = int_of_string(
  environment.get(default="8010", "ZUKKA_LIQUIDSOAP_CONTROL_PORT")
)

server.harbor(port=control_port, uri="/control")

# `live_order` and `live_override` come from harbor.liq/live.liq (issue #30).
# `live_select` is the same predicate the `switch` uses, so `current` always
# matches what is actually on air.
def live_status_json() =
  json.stringify({
    live_order = live_order(),
    override = live_override(),
    current = live_select(live_order(), live_override())
  })
end

server.register("live.set", description="Live-Mount erzwingen",
  usage="<mount>", fun (arg) -> begin
    mount = string.trim(arg)
    live_override := mount
    log("control live.set override=#{mount}")
    live_status_json()
  end)

server.register("live.clear", description="Live-Override aufheben",
  usage="", fun (arg) -> begin
    ignore(arg)
    live_override := null
    log("control live.clear")
    live_status_json()
  end)

server.register("live.status", description="Live-Auswahl als JSON",
  usage="", fun (arg) -> begin
    ignore(arg)
    live_status_json()
  end)

# Layer availability (issue #106): toggle the show/queue pull layers at runtime
# without a restart. `layer.available show false` makes the chain fall straight
# to queue/rotation even if the scheduler endpoint would hand out an item.
# The refs live in layers.liq; `layers_configured` in config.liq.

def layer_status_json() =
  json.stringify({
    show_available = show_available(),
    queue_available = queue_available(),
    layers_configured = layers_configured
  })
end

server.register("layer.available", description="Show/Queue-Ebene ein-/ausschalten",
  usage="<show|queue> <true|false>", fun (arg) -> begin
    parts = list.filter(
      fun (s) -> s != "",
      string.split(separator=" ", string.trim(arg))
    )
    if list.length(parts) != 2 then
      "error: usage: layer.available <show|queue> <true|false>"
    else
      layer = list.nth(parts, 0)
      raw = list.nth(parts, 1)
      if layer != "show" and layer != "queue" then
        "error: unknown layer '#{layer}' (expected show|queue)"
      elsif raw != "true" and raw != "false" then
        "error: value must be true or false"
      else
        value = raw == "true"
        if layer == "show" then show_available := value else queue_available := value end
        log("control layer.available layer=#{layer} value=#{value}")
        layer_status_json()
      end
    end
  end)

server.register("layer.status", description="Show/Queue-Verfuegbarkeit als JSON",
  usage="", fun (arg) -> begin
    ignore(arg)
    layer_status_json()
  end)

# Tape recording (issue #108, consolidated §4.4): open/close the per-broadcast
# recorder defined in outputs.liq. `tape_status_json`/`tape_start_guard` come
# from tape.liq; the guard returns "" when a start may proceed.
server.register("tape.start", description="Tape-Aufnahme starten",
  usage="<tape_id>", fun (arg) -> begin
    tape_id = string.trim(arg)
    guard = tape_start_guard(tape_id, recorder.is_started())
    if guard == "" then begin
      tape_current := tape_id
      recorder.start()
      log("control tape.start tape_id=#{tape_id}")
      tape_status_json(true)
    end else
      guard
    end
  end)

server.register("tape.stop", description="Tape-Aufnahme stoppen",
  usage="", fun (arg) -> begin
    ignore(arg)
    recorder.stop()
    log("control tape.stop")
    tape_status_json(false)
  end)

server.register("tape.status", description="Tape-Status als JSON",
  usage="", fun (arg) -> begin
    ignore(arg)
    tape_status_json(recorder.is_started())
  end)
