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

# --- Mixer / Layer commands (issue #145, docs/control-api.md §4.1/§4.2) ----
#
# Pure logic + validation live in lib/mixer.liq; these handlers only split the
# argument line and dispatch. Validation happens BEFORE any ref is written;
# error texts carry the stable codes (unknown_channel, bad_gain, bad_source,
# unknown_preset, unknown_layer).

def arity_error(usage) =
  "ERROR invalid_args: " ^ usage
end

def one_arg(arg, usage, on_ok) =
  parts = split_args(arg)
  if list.length(parts) != 1 then arity_error(usage)
  else on_ok(list.nth(parts, 0))
  end
end

def two_args(arg, usage, on_ok) =
  parts = split_args(arg)
  if list.length(parts) != 2 then arity_error(usage)
  else on_ok(list.nth(parts, 0), list.nth(parts, 1))
  end
end

server.register("mixer.set", description="Kanal-Fader setzen (dB oder linear)",
  usage="<ch> <gain>", fun (arg) ->
    two_args(arg, "mixer.set <ch> <gain>", cmd_set))

server.register("mixer.trim", description="Kanal-Trim (dB) setzen",
  usage="<ch> <gain_db>", fun (arg) ->
    two_args(arg, "mixer.trim <ch> <gain_db>", cmd_trim))

server.register("mixer.mute", description="Kanal muten/unmuten",
  usage="<ch> <true|false>", fun (arg) ->
    two_args(arg, "mixer.mute <ch> <true|false>", cmd_mute))

server.register("mixer.solo", description="Kanal auf Cue-Bus (Solo)",
  usage="<ch> <true|false>", fun (arg) ->
    two_args(arg, "mixer.solo <ch> <true|false>", cmd_solo))

server.register("mixer.exclusive", description="Nur diesen Kanal hoerbar",
  usage="<ch>", fun (arg) ->
    one_arg(arg, "mixer.exclusive <ch>", cmd_exclusive))

server.register("mixer.patch", description="Kanal-Quelle patchen",
  usage="<ch> <source>", fun (arg) ->
    two_args(arg, "mixer.patch <ch> <source>", cmd_patch))

server.register("mixer.master", description="Master-Fader (dB) setzen",
  usage="<gain_db>", fun (arg) ->
    one_arg(arg, "mixer.master <gain_db>", cmd_master))

server.register("mixer.meters", description="Kanal-Meter als JSON (§4.5)",
  usage="", fun (arg) -> begin
    ignore(arg)
    cmd_meters()
  end)

server.register("layer.preset", description="Mixer-Preset anwenden",
  usage="<default|live|relay-only>", fun (arg) ->
    one_arg(arg, "layer.preset <default|live|relay-only>", cmd_preset))

server.register("layer.force", description="Automatik-Override setzen",
  usage="<layer|none>", fun (arg) ->
    one_arg(arg, "layer.force <layer|none>", cmd_force))

server.register("layer.enable", description="Ebene ein-/ausschalten",
  usage="<layer> <true|false>", fun (arg) ->
    two_args(arg, "layer.enable <layer> <true|false>", cmd_enable))

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
