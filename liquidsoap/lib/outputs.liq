# Outputs: Icecast stream + tape recording.
# Input is fixed at 192k (see docs/consolidated-architecture.md §3.7).

output.icecast(
  %mp3(bitrate=192),
  host="icecast",
  port=8000,
  password="hackme",  # TODO: sourced from the generated config in M4
  mount="stream.mp3",
  name="radio zukka",
  mksafe(radio)
)

# TODO(M1): start/stop per broadcast via control tape.start / tape.stop
# output.file(%mp3(bitrate=192), "/recordings/current.mp3", radio)
