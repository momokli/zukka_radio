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
  catch err do
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
    fun (v) -> list.append(parts(), ["liq_cue_in=#{v}"])
  )
  parts := null.case(
    x.cue_out_ms,
    fun () -> parts(),
    fun (v) -> list.append(parts(), ["liq_cue_out=#{v}"])
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
