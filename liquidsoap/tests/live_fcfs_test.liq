# FCFS registry test (issue #30, US4).
#
# Pure registry logic — no harbor ports are bound, because harbor.liq is run
# with ZUKKA_HARBOR_SLOTS=0, so `casters` is empty and nothing listens.
#
# Run:
#   ZUKKA_HARBOR_SLOTS=0 liquidsoap tests/live_fcfs_test.liq
# Exits 0 when every case passes, 1 otherwise.

%include "../lib/harbor.liq"

failures = ref(0)

def check(label, actual, expected) =
  if actual == expected then
    print("PASS #{label}")
  else
    print("FAIL #{label}: expected=#{expected} actual=#{actual}")
    failures := failures() + 1
  end
end

# Canonical mount strings (issue #30 D-A).
m1 = "/live/slot-01"
m2 = "/live/slot-02"
m99 = "/live/slot-99"

# Env path: ZUKKA_HARBOR_SLOTS=0 leaves the pool empty (nothing binds a port).
check("case0 no slots -> no casters", list.length(casters), 0)

# Case 1: empty registry -> "".
check("case1 empty -> ''", live_select([], null), "")

# Case 2: slot-02 connects first, then slot-01 -> FCFS stays slot-02.
ord2 = live_join(m1, live_join(m2, []))
check("case2 first connected wins", live_fcfs(ord2), m2)

# Case 3: a duplicate live_join adds nothing.
ord3 = live_join(m2, live_join(m2, []))
check("case3 duplicate ignored", list.length(ord3), 1)

# Case 4: leaving the live mount promotes the waiting caster to the head.
check("case4 leave promotes next", live_fcfs(live_leave(m2, ord2)), m1)

# Case 5: an override on a connected mount wins.
check("case5 override connected wins", live_select(ord2, m1), m1)

# Case 6: an override on a disconnected mount falls back to FCFS.
check("case6 stale override -> fcfs", live_select(ord2, m99), m2)

# Case 7: leaving a mount that is not present is a no-op.
ord7 = live_leave(m99, ord2)
check("case7 leave absent keeps head", live_fcfs(ord7), m2)
check("case7 leave absent keeps length", list.length(ord7), 2)

if failures() == 0 then
  print("ALL PASS")
  shutdown()
else
  print("FAILURES=#{failures()}")
  shutdown(code=1)
end
