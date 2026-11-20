# Caster harbor pool: N slots on one port, one mount per caster.
# See docs/consolidated-architecture.md §2.5 and docs/liquidsoap.md §3.3 (D3/D4).
#
# Every slot binds its own input.harbor mount on the shared harbor port.
# master.liq clocks all slots via fallback(...), so multiple casters can be
# connected in parallel while only the selected one is audible.

harbor_port = int_of_string(
  environment.get(default="8001", "ZUKKA_LIQUIDSOAP_HARBOR_PORT")
)
harbor_slots_count = int_of_string(
  environment.get(default="32", "ZUKKA_HARBOR_SLOTS")
)

def harbor_mount(i) =
  # list.init/list.map pass a zero-based index; slots are 1-based.
  "live/slot-#{string.of_int(digits=2, i + 1)}"
end

harbor_mounts = list.init(harbor_slots_count, harbor_mount)

# Auth is a per-mount closure: input.harbor's auth callback only receives
# {address, user, password}, not the requested mount. The mount is captured
# here so the real check (issue #33) can attribute credentials to a slot.
#
# TODO(#33): replace the fail-open placeholder below with the portal call
#   POST /internal/harbor/auth (contract in docs/liquidsoap.md §3.3).
# The marker is intentional: without it this would be a silent fail-open.
def harbor_auth(mount) =
  fun (r) -> begin
    log("harbor auth placeholder accepted mount=#{mount} user=#{r.user} (TODO #33)")
    true
  end
end

def make_harbor_slot(mount) =
  input.harbor(
    mount,
    port=harbor_port,
    auth=harbor_auth(mount),
    buffer=2.0,
    max=20.0,
    replay_metadata=true
  )
end

harbor_slots = list.map(make_harbor_slot, harbor_mounts)
