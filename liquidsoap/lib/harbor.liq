# Caster harbor pool: N slots on one port, one mount per caster.
# See docs/consolidated-architecture.md §4.4 / §5.4.

harbor_port = int_of_string(env("ZUKKA_LIQUIDSOAP_HARBOR_PORT", "8001"))
harbor_slots_count = int_of_string(env("ZUKKA_HARBOR_SLOTS", "32"))

def make_harbor_slot(i) =
  input.harbor(
    "live/slot-#{i}",
    port=harbor_port,
    buffer=2.0,
    auth=fun (args) -> true  # TODO(M4): ask scheduler /internal/harbor/auth
  )
end

harbor_slots = list.init(harbor_slots_count, make_harbor_slot)
