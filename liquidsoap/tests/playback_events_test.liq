# Playback events test (issue #118, US1).
#
# Hermetic: only the pure helpers from lib/playback.liq are exercised. No
# harbor ports, no HTTP, no scheduler — the HTTP edge lives in lib/events.liq
# and is intentionally not pulled in here. Run exactly like the parse test:
#
#   ZUKKA_HARBOR_SLOTS=0 liquidsoap tests/playback_events_test.liq
#
# Exits 0 when every case passes, 1 otherwise.

%include "../lib/playback.liq"

failures = ref(0)

def check(label, actual, expected) =
  if actual == expected then
    print("PASS #{label}")
  else
    print("FAIL #{label}: expected=#{expected} actual=#{actual}")
    failures := failures() + 1
  end
end

# Render a `string?` as a stable token: "-" for null, the raw value otherwise.
def opt(s) =
  null.case(s, fun () -> "-", fun (x) -> x)
end

# Render the `(ended_id, started_id, new_last)` triple as a comparable string.
def show3(t) =
  let (e, s, n) = t
  "#{opt(e)}|#{opt(s)}|#{opt(n)}"
end

# --- qid_of_metadata -------------------------------------------------------

check("qid missing key -> empty", qid_of_metadata([]), "")
check("qid other keys -> empty", qid_of_metadata([("title", "T")]), "")
check("qid empty value -> empty", qid_of_metadata([("liq_queue_item_id", "")]), "")
check("qid present", qid_of_metadata([("liq_queue_item_id", "q1")]), "q1")
check("qid among others",
  qid_of_metadata([("title", "T"), ("liq_queue_item_id", "q7"), ("artist", "A")]),
  "q7")

# --- body builders (byte-exact) --------------------------------------------

check("started body", playback_started_body("q1"), "{\"queue_item_id\":\"q1\"}")
check("ended body", playback_ended_body("q2"), "{\"queue_item_id\":\"q2\"}")

# --- decide_report: single transitions -------------------------------------

check("null -> q1", show3(decide_report(null, "q1")), "-|q1|q1")
check("q1 -> q1 (dup)", show3(decide_report("q1", "q1")), "-|-|q1")
check("q1 -> \"\"", show3(decide_report("q1", "")), "q1|-|-")
check("null -> \"\"", show3(decide_report(null, "")), "-|-|-")

# --- decide_report: sequences ----------------------------------------------

# q1 -> q2: ended(q1) + started(q2).
check("seq q1->q2", show3(decide_report("q1", "q2")), "q1|q2|q2")

# q1 -> q1 -> q1: only the first start, then nothing.
check("seq q1->q1->q1 (1)", show3(decide_report(null, "q1")), "-|q1|q1")
check("seq q1->q1->q1 (2)", show3(decide_report("q1", "q1")), "-|-|q1")
check("seq q1->q1->q1 (3)", show3(decide_report("q1", "q1")), "-|-|q1")

# q1 -> "" -> "": one ended, then nothing (loop of a non-queue layer).
check("seq q1->\"\" (1)", show3(decide_report("q1", "")), "q1|-|-")
check("seq q1->\"\"->\"\" (2)", show3(decide_report(null, "")), "-|-|-")

if failures() == 0 then
  print("ALL PASS")
  shutdown()
else
  print("FAILURES=#{failures()}")
  shutdown(code=1)
end
