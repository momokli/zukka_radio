# Outputs: Icecast stream + HLS segments.
# Input is fixed at 192k (see docs/consolidated-architecture.md §3.7).
#
# `mksafe` is applied once: both outputs pull from the same infallible source,
# so a fallible `radio` chain cannot take either output down (issue #84).

safe_radio = mksafe(radio)

output.icecast(
  %mp3(bitrate=192),
  host="icecast",
  port=8000,
  password="hackme",  # TODO(M4): source from the generated config
  mount="stream.mp3",
  name="radio zukka",
  safe_radio
)

# HLS (D13): latency fallback for clients that cannot use Icecast. Segments
# land in the shared `hls` volume; Caddy serves them read-only at /hls/*.
# `persist_at` keeps the segment counter across restarts so clients do not get
# a broken playlist state.
output.file.hls(
  playlist = "index.m3u8",
  segment_duration = 4.0,
  segments = 6,
  persist_at = "/hls/.persist",
  "/hls",
  [("aac", %ffmpeg(format="mpegts", %audio(codec="aac", b="128k")))],
  safe_radio
)

# TODO(M1): start/stop per broadcast via control tape.start / tape.stop
# output.file(%mp3(bitrate=192), "/recordings/current.mp3", safe_radio)
