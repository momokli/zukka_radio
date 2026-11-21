# Outputs: Icecast stream + HLS segments.
# Input is fixed at 192k (see docs/consolidated-architecture.md §3.7).
#
# `mksafe` is applied once: both outputs pull from the same infallible source,
# so a fallible `radio` chain cannot take either output down (issue #84).

safe_radio = mksafe(radio)

# Icecast source password comes from the environment (same value the icecast
# container renders into its config, issue #109). No default: an unset/empty
# value is a misconfiguration; fail fast instead of streaming with a bogus
# password.
icecast_source_password = environment.get(
  default="",
  "ZUKKA_ICECAST_SOURCE_PASSWORD"
)

if icecast_source_password == "" then
  log.critical(
    "ZUKKA_ICECAST_SOURCE_PASSWORD is unset or empty; refusing to start (issue #109)"
  )
  shutdown()
end

output.icecast(
  %mp3(bitrate=192),
  host="icecast",
  port=8000,
  password=icecast_source_password,
  mount="stream.mp3",
  name="radio zukka",
  safe_radio
)

# HLS (D13): latency fallback for clients that cannot use Icecast. Segments
# land in the shared `hls` volume; Caddy serves them read-only at /hls/*.
# `persist_at` keeps the segment counter across restarts so clients do not get
# a broken playlist state.
#
# The stream name (first tuple element) determines the playlist filename:
# `index` -> index.m3u8 + index_<n>.ts. The `playlist` argument is ignored for
# the list form, so the name must be `index` to match the Caddy route and the
# smoke check.
output.file.hls(
  playlist = "index.m3u8",
  segment_duration = 4.0,
  segments = 6,
  persist_at = "/hls/.persist",
  "/hls",
  [("index", %ffmpeg(format="mpegts", %audio(codec="aac", b="128k")))],
  safe_radio
)

# TODO(M1): start/stop per broadcast via control tape.start / tape.stop
# output.file(%mp3(bitrate=192), "/recordings/current.mp3", safe_radio)
