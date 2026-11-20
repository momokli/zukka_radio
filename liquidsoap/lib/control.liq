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