#!/usr/bin/env bash
# E2E test for issue #118: master on_track -> scheduler playback reporting.
#
# Boots a stub scheduler (python3 http.server) that hands out two short silent
# MP3s as `PlaybackItemDto`s and logs the `POST /api/v1/playback/{started,ended}`
# bodies + `X-Liquidsoap-Token`. A minimal Liquidsoap script (no harbor, no
# icecast) includes the real lib/config.liq + lib/playback.liq + lib/events.liq,
# pulls the items via request.dynamic and consumes the stream, so the genuine
# `radio.on_track` -> `report_track` -> `emit_playback` path runs.
#
# Asserts:
#   1. exactly  started(q1), ended(q1), started(q2) reach the scheduler,
#      token + body byte-exact;
#   2. fail-open: with a dead report endpoint the stream still runs to
#      completion and exits 0 (a dead scheduler can never stop the stream).
#
# Requires: docker (with savonet/liquidsoap:v2.4.5), python3, ffmpeg, and
# Linux host networking (docker --network host).
#
#   ./scripts/zukka-playback-e2e-test.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIQ_DIR="$REPO_ROOT/liquidsoap"
IMAGE="savonet/liquidsoap:v2.4.5"
PORT="${E2E_PORT:-18099}"
TOKEN="e2e-token"

for tool in docker python3 ffmpeg; do
  command -v "$tool" >/dev/null 2>&1 || { echo "SKIP: missing $tool"; exit 0; }
done
docker image inspect "$IMAGE" >/dev/null 2>&1 || {
  echo "pulling $IMAGE ..."; docker pull "$IMAGE" >/dev/null; }

WORK="$(mktemp -d)"
cleanup() { kill "${STUB_PID:-}" 2>/dev/null || true; rm -rf "$WORK"; }
trap cleanup EXIT

mkdir -p "$WORK/media"
# mktemp yields a 700 dir; the in-container user must be able to read the
# mounted workdir, otherwise Liquidsoap falls back to parsing the path string
# as a script ("Undefined variable <mountpoint>").
chmod 755 "$WORK" "$WORK/media"
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "anullsrc=r=44100:cl=mono" \
  -t 2 -codec:a libmp3lame -q:a 9 "$WORK/media/q1.mp3"
cp "$WORK/media/q1.mp3" "$WORK/media/q2.mp3"

cat >"$WORK/stub.py" <<'PY'
import http.server, json, os, sys, urllib.parse
BASE = os.path.dirname(os.path.abspath(__file__))
PORT = int(sys.argv[1]); LOG = os.path.join(BASE, "events.log")
ITEMS = ["q1", "q2"]; nxt = {"i": 0}
def dto(q):
    return {"queue_item_id": q, "kind": "track", "track_id": None, "tape_id": None,
            "title": "silence-"+q, "artist": "e2e", "duration_ms": 2000,
            "uri": "http://127.0.0.1:%d/media/%s.mp3" % (PORT, q),
            "cue_in_ms": None, "cue_out_ms": None}
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _log(self, s):
        open(LOG, "a").write(s + "\n")
    def do_GET(self):
        p = urllib.parse.urlparse(self.path).path
        if p == "/api/v1/playback/next":
            i = nxt["i"]
            if i < len(ITEMS):
                nxt["i"] = i + 1
                b = json.dumps(dto(ITEMS[i])).encode()
                self.send_response(200); self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
            else:
                self.send_response(204); self.end_headers()
            return
        if p.startswith("/media/") and p.endswith(".mp3") and os.path.isfile(os.path.join(BASE, p.lstrip("/"))):
            d = open(os.path.join(BASE, p.lstrip("/")), "rb").read()
            self.send_response(200); self.send_header("Content-Type", "audio/mpeg")
            self.send_header("Content-Length", str(len(d))); self.end_headers(); self.wfile.write(d)
            return
        self.send_response(404); self.end_headers()
    def do_POST(self):
        if os.environ.get("STUB_FAIL_POST") == "1":
            self.connection.close(); return
        n = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(n).decode("utf-8", "replace") if n else ""
        kind = urllib.parse.urlparse(self.path).path.rsplit("/", 1)[-1]
        self._log("POST %s token=%s body=%s" % (kind, self.headers.get("X-Liquidsoap-Token", ""), body))
        self.send_response(200); self.send_header("Content-Length", "2"); self.end_headers(); self.wfile.write(b"{}")
if __name__ == "__main__":
    if os.path.exists(LOG): os.remove(LOG)
    http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
PY

cat >"$WORK/radio_test.liq" <<'LIQ'
settings.log.level := 3
%include "/etc/liquidsoap/lib/config.liq"
%include "/etc/liquidsoap/lib/playback.liq"
queue_available = ref(true)
def next_for(layer) =
  if not layers_configured then null else
    try
      body = http.get(headers=[("X-Liquidsoap-Token", liquidsoap_token)], timeout=5.0,
        "#{scheduler_base_url}/api/v1/playback/next?layer=#{layer}")
      request_for_body(body, layers_configured)
    catch _ do null end
  end
end
queue = request.dynamic(available=queue_available, prefetch=1, fun () -> next_for("queue"))
radio = queue
%include "/etc/liquidsoap/lib/events.liq"
output.dummy(fallible=true, radio)
thread.run(delay=8.0, fun () -> begin shutdown() end)
LIQ

run_ls() {
  timeout 40 docker run --rm --network host \
    -e ZUKKA_HARBOR_SLOTS=0 -e SCHEDULER_BASE_URL="http://127.0.0.1:$PORT" \
    -e LIQUIDSOAP_TOKEN="$TOKEN" \
    -v "$LIQ_DIR:/etc/liquidsoap:ro" -v "$WORK:/work:ro" \
    "$IMAGE" liquidsoap /work/radio_test.liq >"$WORK/ls.out" 2>&1
}

echo "== phase 1: happy path =="
python3 "$WORK/stub.py" "$PORT" & STUB_PID=$!
sleep 2
rc=0; run_ls || rc=$?
kill "$STUB_PID" 2>/dev/null || true; wait "$STUB_PID" 2>/dev/null || true
EXPECT=$(printf 'POST started token=%s body={"queue_item_id":"q1"}\nPOST ended token=%s body={"queue_item_id":"q1"}\nPOST started token=%s body={"queue_item_id":"q2"}' "$TOKEN" "$TOKEN" "$TOKEN")
GOT="$(grep '^POST ' "$WORK/events.log" 2>/dev/null || true)"
if [ "$rc" -ne 0 ] || [ "$GOT" != "$EXPECT" ]; then
  echo "FAIL phase1 (rc=$rc)"; echo "--- got ---"; echo "$GOT"; echo "--- want ---"; echo "$EXPECT"
  tail -20 "$WORK/ls.out"; exit 1
fi
echo "PASS phase1: started(q1), ended(q1), started(q2); rc=$rc"

echo "== phase 2: fail-open (dead report endpoint) =="
STUB_FAIL_POST=1 python3 "$WORK/stub.py" "$PORT" & STUB_PID=$!
sleep 2
rc=0; run_ls || rc=$?
kill "$STUB_PID" 2>/dev/null || true; wait "$STUB_PID" 2>/dev/null || true
if [ "$rc" -ne 0 ]; then
  echo "FAIL phase2: stream did not survive dead scheduler (rc=$rc)"; tail -20 "$WORK/ls.out"; exit 1
fi
echo "PASS phase2: stream survived dead scheduler, rc=$rc"
echo "ALL PASS"
