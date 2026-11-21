# Fun-jingle default and pure helpers (issue #121, US2).
#
# Pure and I/O-free: no sources, no `server.register`, no file access, so this
# file is testable in isolation (`tests/fun_jingle_test.liq`, same pattern as
# `lib/tape.liq`). The actual priority-1 wiring (request loop + substitute
# tone) lives in `lib/layers.liq`.
#
# Included before `lib/config.liq`: config.liq resolves `ZUKKA_FUN_JINGLE`
# through `fun_jingle_resolve`.

fun_jingle_default_path = "/rotation/fun.mp3"

# Unset/empty `ZUKKA_FUN_JINGLE` keeps the historical default path; any set
# value wins, so the jingle stays operator-configurable.
def fun_jingle_resolve(raw_path) =
  if raw_path == "" then
    fun_jingle_default_path
  else
    raw_path
  end
end

# Startup selection for the priority-1 layer: loop the jingle when the file is
# present, otherwise a substitute source so the chain never ends in silence.
def fun_jingle_choice(available) =
  if available then
    "loop"
  else
    "substitute"
  end
end

# Log line for the missing-jingle case (emitted by layers.liq at level=3).
def fun_jingle_log(jingle_path) =
  "fun-jingle: #{jingle_path} missing -> substitute source, stream stays on air (issue #121)"
end