# Pure helpers for the scheduler pull layers (issue #106).
#
# Everything here is side-effect free: no `http`, no source access. That keeps
# the JSON → request mapping hermetically testable (tests/playback_parse_test.liq)
# while layers.liq only adds the network edge (http.get).

# Parse the `PlaybackItemDto` wire form (scheduler/src/dto.rs) into a small
# record. Returns `null` for an empty/whitespace body, invalid JSON, a missing
# `uri` or an empty `uri` — i.e. "nothing usable on this layer".
#
# The `let json.parse (… : …) = body` form (2.4.5) is the type-driven parser:
# the annotation both selects the fields we care about and validates their
# types; a mismatch raises error.json, which we turn into `null`.
def parse_playback_item(body) =
  try
    let json.parse (
      {uri, cue_in_ms, cue_out_ms, queue_item_id} : {
        uri: string,
        cue_in_ms: int?,
        cue_out_ms: int?,
        queue_item_id: string?
      }
    ) = body
    if uri == "" then
      null
    else
      {uri=uri, cue_in_ms=cue_in_ms, cue_out_ms=cue_out_ms, queue_item_id=queue_item_id}
    end
  catch _ do
    null
  end
end

# Build the request URI for a parsed item. Cue points and the queue item id
# become `annotate:` metadata (verified against 2.4.5: `request.create` resolves
# `liq_cue_in`/`liq_cue_out`/`liq_queue_item_id` from the annotate protocol).
# Only present (non-`null`) values are emitted; no bare `annotate:` wrapper when
# there is nothing to annotate.
def build_annotated_uri(x) =
  parts = ref([])
  parts := null.case(
    x.cue_in_ms,
    fun () -> parts(),
    # Liquidsoap 2.4.5 reads `liq_cue_in`/`liq_cue_out` in SECONDS, while the
    # scheduler DTO carries milliseconds (`*_ms`). Convert ms -> s here.
    fun (v) -> list.append(parts(), ["liq_cue_in=#{float(v) / 1000.}"])
  )
  parts := null.case(
    x.cue_out_ms,
    fun () -> parts(),
    fun (v) -> list.append(parts(), ["liq_cue_out=#{float(v) / 1000.}"])
  )
  parts := null.case(
    x.queue_item_id,
    fun () -> parts(),
    fun (v) -> list.append(parts(), ["liq_queue_item_id=#{v}"])
  )
  if list.length(parts()) == 0 then
    x.uri
  else
    "annotate:#{string.concat(separator=',', parts())}:#{x.uri}"
  end
end

# Nullable wrapper: `null` in → `null` out.
def build_request_uri(item) =
  null.case(item, fun () -> null, fun (x) -> build_annotated_uri(x))
end

# Body from the scheduler → request (or `null` when nothing is playable).
# Fail-closed: when the layers are not configured we never build a request.
def request_for_body(body, configured) =
  if not configured then
    null
  else
    null.case(
      parse_playback_item(body),
      fun () -> null,
      fun (x) -> request.create(build_annotated_uri(x))
    )
  end
end

# --- playback reporting helpers (issue #118) --------------------------------
#
# Pure helpers for the master `on_track` → scheduler report wiring. Still no
# I/O here: the HTTP edge lives in lib/events.liq. Keeping the decision logic
# pure makes the full `last → next` state machine hermetically testable
# (tests/playback_events_test.liq).

# Read `liq_queue_item_id` out of an `on_track` metadata list ([(string,string)]).
# `list.assoc` raises when the key is absent, so the whole lookup is wrapped:
# a missing key (show tapes, rotation, relay, live, …) yields "" — never a
# raise. An empty value is treated the same as a missing one (no report).
def qid_of_metadata(m) =
  try
    list.assoc("liq_queue_item_id", m)
  catch _ do
    ""
  end
end

# Exact JSON bodies for POST /api/v1/playback/{started,ended}. Both endpoints
# take the same `{"queue_item_id": …}` shape (scheduler/src/dto.rs), so only
# the URL differs. Built as literal strings to keep them byte-exact.
def playback_started_body(qid) =
  "{\"queue_item_id\":\"#{qid}\"}"
end

def playback_ended_body(qid) =
  "{\"queue_item_id\":\"#{qid}\"}"
end

# Pure dedupe/transition: given the last reported queue item id (`last`, a
# `string?`) and the queue item id of the track that just started (`qid`, ""
# when the master fired on_track without a queue annotation), return
# `(ended_id, started_id, new_last)` (design #2/#3):
#
#   qid != "" and qid != last → (last, qid, qid)   [last may be null → no ended]
#   qid != "" and qid == last → (null, null, last) [no duplicate event]
#   qid == "" and last != null → (last, null, null) [layer left the queue]
#   qid == "" and last == null → (null, null, null) [nothing to report]
#
# The `null.case` comparison keeps `last` typed as `string?` (a bare `==`
# would force it to `string`).
def decide_report(last, qid) =
  if qid != "" then
    if null.case(last, fun () -> false, fun (l) -> l == qid) then
      (null, null, last)
    else
      (last, qid, qid)
    end
  else
    if last == null then
      (null, null, null)
    else
      (last, null, null)
    end
  end
end
