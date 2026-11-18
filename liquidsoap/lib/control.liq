# Control API: scheduler → Liquidsoap.
# Commands (see docs/consolidated-architecture.md §4.4):
#   live.set, live.clear, live.status
#   layer.available, tape.start, tape.stop, status, health
#
# TODO(M4): server.harbor or an HTTP control endpoint on port 8010.

settings.server.harbor.bind_addr := "0.0.0.0"
settings.server.harbor.port := int_of_string(env("ZUKKA_LIQUIDSOAP_CONTROL_PORT", "8010"))
