# Operator existence gate (issue #145, US8).
#
# Machine-checkable proof that the operators the mixer graph uses exist in the
# pinned image (savonet/liquidsoap:v2.4.5): amplify, rms, peak, mix, smooth_add,
# crossfade, compress, clip — plus the `on_position` SOURCE METHOD (it is NOT a
# top-level operator; the full signature needs an explicit `synchronous` and a
# 2-arg handler). If any of these were missing the script would not parse.
#
#   ZUKKA_HARBOR_SLOTS=0 liquidsoap tests/operators_test.liq
#
# Exits 0 when every operator builds, 1 otherwise.

failures = ref(0)

def check(label, actual, expected) =
  if actual == expected then
    print("PASS #{label}")
  else
    print("FAIL #{label}: expected=#{expected} actual=#{actual}")
    failures := failures() + 1
  end
end

s = sine(440.0)

# Each operator from the mixer graph, built once.
a = amplify(fun () -> 0.5, s)
r = rms(duration=1.0, a)
p = peak(duration=1.0, a)
m = mix([a, s])
c = clip(compress(m))
cf = crossfade(s)
sa = smooth_add(normal=a, special=s)

# `on_position` is a source method, not a top-level operator.
r.on_position(
  position=fun () -> 10.0,
  remaining=true,
  synchronous=false,
  fun (pos, meta) -> begin
    ignore(pos)
    ignore(meta)
  end
)

check("amplify builds", a.id() != "", true)
check("rms builds", r.id() != "", true)
check("peak builds", p.id() != "", true)
check("mix builds", m.id() != "", true)
check("compress+clip builds", c.id() != "", true)
check("crossfade builds", cf.id() != "", true)
check("smooth_add builds", sa.id() != "", true)

if failures() == 0 then
  print("ALL PASS")
  shutdown()
else
  print("FAILURES=#{failures()}")
  shutdown(code=1)
end
