# Caster harbor pool: N slots on one port, one mount per caster.
# See docs/consolidated-architecture.md §2.5 and docs/liquidsoap.md §3.3 (D3/D4).
#
# Every slot binds its own input.harbor mount on the shared harbor port. The
# connection order is tracked in a FCFS registry (issue #30): the first
# connected caster stays live; later casters wait and never preempt. live.liq
# consumes `casters` to build the switch.

harbor_port = int_of_string(
  environment.get(default="8001", "ZUKKA_LIQUIDSOAP_HARBOR_PORT")
)
harbor_slots_count = int_of_string(
  environment.get(default="32", "ZUKKA_HARBOR_SLOTS")
)

# Issue #105: the Harbor auth check calls the scheduler (not the portal) and is
# guarded by the Liquidsoap token (§4.1). Both values are read unprefixed, like
# the scheduler reads them; the token must match the scheduler's LIQUIDSOAP_TOKEN.
scheduler_base_url = environment.get(default="", "SCHEDULER_BASE_URL")
liquidsoap_token = environment.get(default="", "LIQUIDSOAP_TOKEN")

def harbor_mount(i) =
  # list.init/list.map pass a zero-based index; slots are 1-based.
  # Canonical mount string carries the leading slash (issue #30 D-A): the same
  # value is used by the registry, the switch predicate and later live.set.
  "/live/slot-#{string.of_int(digits=2, i + 1)}"
end

harbor_mounts = list.init(harbor_slots_count, harbor_mount)

# Auth is a per-mount closure: input.harbor's auth callback only receives
# {address, user, password}, not the requested mount. The mount is captured
# here so the real check (issue #33) can attribute credentials to a slot.
#
# Fail-closed (issue #105): only HTTP 200 from the scheduler ⇒ true. A missing
# token/URL, a non-200 status, or a connection error all ⇒ false. `catch e do`
# is the 2.4.5 syntax (the doc form `catch _ ->` does not parse).
def harbor_auth(mount) =
  fun (r) -> begin
    if scheduler_base_url == "" or liquidsoap_token == "" then
      log("harbor auth rejected mount=#{mount} user=#{r.user}: missing SCHEDULER_BASE_URL/LIQUIDSOAP_TOKEN")
      false
    else
      try
        resp = http.post(
          scheduler_base_url ^ "/api/v1/internal/harbor/auth",
          data=json.stringify({mount=mount, username=r.user, password=r.password}),
          headers=[("Content-Type", "application/json"),
                   ("X-Liquidsoap-Token", liquidsoap_token)],
          timeout=0.5
        )
        resp.status_code == 200
      catch e do
        log("harbor auth error mount=#{mount} user=#{r.user}: #{e}")
        false
      end
    end
  end
end

# FCFS registry: mount names, oldest connection at the head.
live_order = ref([])

# Pure registry functions (no I/O) — unit-testable without a stream (US4).

# Append a mount that is not already present. Because the head is the oldest
# connection, a later join must go to the tail: FCFS never preempts.
def live_join(m, order) =
  if list.mem(m, order) then order else list.append(order, [m]) end
end

# Remove the first occurrence of a mount; no-op when absent.
def live_leave(m, order) =
  list.remove(m, order)
end

# Oldest connected mount, "" when nobody is connected.
def live_fcfs(order) =
  list.hd(default="", order)
end

# D-C: an override only wins while its mount is actually connected; otherwise
# fall back to FCFS (a stale override must not take the whole live layer down).
def live_select(order, override) =
  null.case(
    override,
    fun () -> live_fcfs(order),
    fun (o) -> if list.mem(o, order) then o else live_fcfs(order) end
  )
end

def make_harbor_caster(mount) =
  s = input.harbor(
    mount,
    port=harbor_port,
    auth=harbor_auth(mount),
    buffer=2.0,
    max=20.0,
    replay_metadata=true
  )
  # on_connect/on_disconnect are source methods in 2.4.5, not input.harbor
  # parameters (those are deprecated). Both update the FCFS registry.
  s.on_connect(synchronous=false, fun (_) -> begin
    live_order := live_join(mount, live_order())
    log("caster connected mount=#{mount}")
  end)
  s.on_disconnect(synchronous=false, fun () -> begin
    live_order := live_leave(mount, live_order())
    log("caster disconnected mount=#{mount}")
  end)
  (mount, s)
end

# [(mount, source)] in slot order; consumed by live.liq.
casters = list.map(make_harbor_caster, harbor_mounts)
