# Fun-jingle helper test (issue #121, US2/US5).
#
# Hermetic: only the pure helpers from `lib/fun_jingle.liq` are exercised. No
# file access, no harbor ports, no HTTP. Run exactly like the other tests:
#
#   ZUKKA_HARBOR_SLOTS=0 liquidsoap tests/fun_jingle_test.liq
#
# Exits 0 when every case passes, 1 otherwise.

%include "../lib/fun_jingle.liq"

failures = ref(0)

def check(label, actual, expected) =
  if actual == expected then
    print("PASS #{label}")
  else
    print("FAIL #{label}: expected=#{expected} actual=#{actual}")
    failures := failures() + 1
  end
end

# Case 1: env resolution. Unset/empty -> historical default; set value wins.
check("case1 empty -> default", fun_jingle_resolve(""), "/rotation/fun.mp3")
check("case1 default path", fun_jingle_default_path, "/rotation/fun.mp3")
check("case1 set wins", fun_jingle_resolve("/mnt/jingles/fun.mp3"), "/mnt/jingles/fun.mp3")

# Case 2: loop-vs-substitute selection.
check("case2 present -> loop", fun_jingle_choice(true), "loop")
check("case2 missing -> substitute", fun_jingle_choice(false), "substitute")

# Case 3: the level=3 log line names the configured path and the substitute.
check(
  "case3 log text",
  fun_jingle_log("/rotation/fun.mp3"),
  "fun-jingle: /rotation/fun.mp3 missing -> substitute source, stream stays on air (issue #121)"
)
check("case3 log is path-specific", fun_jingle_log("/a/fun.mp3") != fun_jingle_log("/b/fun.mp3"), true)

if failures() == 0 then
  print("ALL PASS")
  shutdown()
else
  print("FAILURES=#{failures()}")
  shutdown(code=1)
end