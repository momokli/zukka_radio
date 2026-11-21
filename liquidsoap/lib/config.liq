# Configuration seam for the scheduler-driven pull layers (issue #106).
#
# Show and queue are `request.dynamic` sources that pull their next item from
# the scheduler (`GET /api/v1/playback/next?layer=show|queue`). Both the base
# URL and the token come from the environment, exactly like the icecast source
# password (#109): there is no default. When either is unset the layers stay
# disabled (fail-closed) instead of silently trying to pull.
#
# This file must be included first: layers.liq and control.liq read the
# bindings below.

scheduler_base_url = environment.get(default="", "SCHEDULER_BASE_URL")
liquidsoap_token = environment.get(default="", "LIQUIDSOAP_TOKEN")

# Fun layer (issue #121): the priority-1 jingle path is operator-configurable.
# Unset/empty keeps the historical default `/rotation/fun.mp3` (fail-safe, no
# behavior change for existing deployments).
fun_jingle_raw = environment.get(default="", "ZUKKA_FUN_JINGLE")
fun_jingle_path = fun_jingle_resolve(fun_jingle_raw)

if fun_jingle_raw == "" then
  log(
    level=4,
    "fun-jingle: ZUKKA_FUN_JINGLE unset/empty -> default #{fun_jingle_default_path} (issue #121)"
  )
end

layers_configured = scheduler_base_url != "" and liquidsoap_token != ""

if not layers_configured then
  log(
    level=3,
    "layers: SCHEDULER_BASE_URL/LIQUIDSOAP_TOKEN unset -> show/queue disabled (fail-closed, issue #106)"
  )
end
