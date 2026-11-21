# Playback-state reporting to the scheduler (issue #118).
#
# The scheduler has `POST /api/v1/playback/{started,ended,error}` (token
# `X-Liquidsoap-Token`) and a playback state machine, but until now nothing
# called it: `/playback/current` stayed empty and `next_pending_item` kept
# returning the same track. This file wires the master `on_track` to those
# endpoints.
#
# Attachment point is the MASTER (`radio`, design #1), not each
# `request.dynamic`: the master `on_track` fires exactly once per track that is
# actually on air and carries the metadata of the layer currently playing.
# `radio` must already exist, so this file is included AFTER lib/master.liq.
#
# Fail-open (design #5): `on_track(synchronous=false, …)` runs on its own
# thread and never blocks the audio path; every push ignores its result and
# catches all errors; when the scheduler URL/token are unset we do not even
# attempt a request. A dead scheduler can therefore never stop the stream.
#
# Documented gap (design #6): `error` (decode failures) is NOT reported. In
# Liquidsoap 2.4.5 there is no source-level `on_error` hook (`on_end` is
# deprecated), so decode errors are not observable at the source layer. The
# `/error` endpoint and its body shape stay as-is; the trigger is deferred
# rather than inventing an event. See progress-feat-118-playback-report.md #6.

# Last queue item id we reported as started; `null` = nothing on air. A ref so
# the handler can carry state across `on_track` calls.
last_reported_qid = ref((null : string?))

# POST one playback event, best-effort. No-op while the layers are
# unconfigured; a transport error is logged and swallowed (never raises into
# the source). The response is intentionally ignored.
def emit_playback(kind, body) =
  if scheduler_base_url == "" or liquidsoap_token == "" then
    ()
  else
    try
      ignore(
        http.post(
          "#{scheduler_base_url}/api/v1/playback/#{kind}",
          data=body,
          headers=[
            ("Content-Type", "application/json"),
            ("X-Liquidsoap-Token", liquidsoap_token)
          ],
          timeout=2.0
        )
      )
      ()
    catch _ do
      log(level=3, "playback: report #{kind} failed (ignored, fail-open)")
    end
  end
end

# Master `on_track` handler: derive the transition from the pure helpers and
# emit at most one `ended` and one `started` per call.
def report_track(m) =
  qid = qid_of_metadata(m)
  let (ended_id, started_id, new_last) = decide_report(last_reported_qid(), qid)
  null.case(
    ended_id,
    fun () -> (),
    fun (q) -> emit_playback("ended", playback_ended_body(q))
  )
  null.case(
    started_id,
    fun () -> (),
    fun (q) -> emit_playback("started", playback_started_body(q))
  )
  last_reported_qid := new_last
end

radio.on_track(synchronous=false, report_track)
