# Playback parse test (issue #106, US6).
#
# Hermetic: only the pure helpers from lib/playback.liq are exercised. No
# harbor ports, no HTTP, no scheduler. Run exactly like the fcfs test:
#
#   ZUKKA_HARBOR_SLOTS=0 liquidsoap tests/playback_parse_test.liq
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

# `null.case` avoids any `request` equality question.
def is_null(x) =
  null.case(x, fun () -> true, fun (_) -> false)
end

def uri_of(body) =
  build_request_uri(parse_playback_item(body))
end

# Scheduler wire form (PlaybackItemDto), full item with cue points + queue id.
full = '{"queue_item_id":"q1","kind":"track","title":"T","uri":"/media/a.mp3","duration_ms":1000,"cue_in_ms":100,"cue_out_ms":2000}'
# No cue fields at all.
nocue = '{"queue_item_id":null,"kind":"track","title":"T","uri":"/media/b.mp3","duration_ms":1000}'
# Cue in only, queue id null.
onlyin = '{"queue_item_id":null,"kind":"track","title":"T","uri":"/media/c.mp3","duration_ms":1,"cue_in_ms":5}'

# Case 1: all annotations present, in a stable order (cue_in, cue_out, qid).
check("case1 full annotate uri",
  uri_of(full),
  "annotate:liq_cue_in=100,liq_cue_out=2000,liq_queue_item_id=q1:/media/a.mp3")

# Case 2: DTO without cue fields -> no cue prefixes, bare uri.
check("case2 no cue -> bare uri", uri_of(nocue), "/media/b.mp3")

# Case 3: queue_item_id null -> no liq_queue_item_id.
check("case3 no queue id", uri_of(onlyin), "annotate:liq_cue_in=5:/media/c.mp3")

# Case 4: empty/whitespace/invalid/missing-uri bodies -> null.
check("case4 empty body", uri_of(""), null)
check("case4 whitespace body", uri_of("   "), null)
check("case4 invalid json", uri_of("{not json"), null)
check("case4 missing uri", uri_of('{"title":"x"}'), null)
check("case4 empty uri", uri_of('{"uri":""}'), null)

# Case 5: parse is null for the same inputs.
check("case5 parse empty", parse_playback_item(""), null)
check("case5 parse invalid", parse_playback_item("["), null)

# Case 6: fail-closed — unconfigured never yields a request.
check("case6 unconfigured -> null", is_null(request_for_body(full, false)), true)
check("case6 configured -> request", is_null(request_for_body(full, true)), false)
check("case6 configured empty body -> null", is_null(request_for_body("", true)), true)

if failures() == 0 then
  print("ALL PASS")
  shutdown()
else
  print("FAILURES=#{failures()}")
  shutdown(code=1)
end
