# Tape recording state + pure helpers (issue #108, US1).
#
# Pure, I/O-free logic: no output, no `server.register`, so it is testable in
# isolation (see `tests/tape_test.liq`). The recorder output itself lives in
# `lib/outputs.liq` (US2): it taps `safe_radio` (PCM, before the encoders) and
# uses `tape_path` as its `{string}` filename getter.
#
# This file must be included before `lib/outputs.liq`.

# Currently selected tape id (empty = no recording). `output.file`'s filename
# getter re-reads it on every (re-)open, so one file is produced per broadcast.
tape_current = ref("")

# Path of one recorded tape (issue #108, consolidated §3.6): MP3 192k.
def tape_filename(tape_id) =
  "/recordings/" ^ tape_id ^ ".mp3"
end

# The filename getter handed to `output.file`; evaluated at open time.
def tape_path() =
  tape_filename(tape_current())
end

# True only for a canonical 36-char UUIDv7-looking id (`[0-9a-f-]`): this is
# the path-traversal guard — no `/`, no `..`, no whitespace can pass.
def tape_id_valid(tape_id) =
  string.match(
    pattern="^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
    tape_id
  )
end

# Guard for `tape.start`: `""` means startable, otherwise the exact error string
# the command returns (invalid id first, then an already-running recorder).
def tape_start_guard(tape_id, active) =
  if not tape_id_valid(tape_id) then
    "error: usage: tape.start <tape_id>"
  elsif active then
    "error: already recording"
  else
    ""
  end
end

# Status payload (`tape.status`): `{active, tape_id, path}`.
def tape_status_json(active) =
  json.stringify({
    active = active,
    tape_id = tape_current(),
    path = if tape_current() == "" then "" else tape_path() end
  })
end
