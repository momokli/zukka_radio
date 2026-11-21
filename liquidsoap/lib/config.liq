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

layers_configured = scheduler_base_url != "" and liquidsoap_token != ""

if not layers_configured then
  log(
    level=3,
    "layers: SCHEDULER_BASE_URL/LIQUIDSOAP_TOKEN unset -> show/queue disabled (fail-closed, issue #106)"
  )
end
