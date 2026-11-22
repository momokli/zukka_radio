# Mixer master (issue #145).
#
# The former `fallback` priority chain is gone: the six program channels are
# mixed by lib/mixer.liq and that mix is the ONLY audio structure. `live` comes
# from lib/live.liq (issue #30/#31), the other layers from lib/layers.liq; the
# `fun` channel blends the jingle over the rotation bed via `smooth_add`.
#
# Program bus:  mix(strips) -> master fader -> compress -> clip
# Cue bus:      solo-gated strips (binding only, never in the program)
#
# The prior priority order (docs/consolidated-architecture.md §2.4) is no longer
# encoded here — priority is a source-level decision in the scheduler, applied
# through mixer presets (docs/product-v2-cockpit.md §4).

# Channel sources in registry order (docs/control-api.md §5.5). The `show`
# channel gets the forward-looking end hook (10 s before a track ends); `fun`
# is the rotation bed with the jingle faded on top (US7).
show_src = attach_end_hook(show, 10.0)
fun_src = smooth_add(normal=rotation, special=fallback_jingle)

mixer_channels := build_mixer([
  ("live", live),
  ("show", show_src),
  ("queue", queue),
  ("rotation", rotation),
  ("relay", relay),
  ("fun", fun_src),
])

# Cue bus export for a later monitor tap; no program effect in #145.
cue_bus = build_cue(mixer_channels())
ignore(cue_bus)

# No `fallback`: `mix` sums the strips, `compress`/`clip` bound the master.
# The master fader (`mixer.master`) wraps the limited chain.
radio = amplify(
  fun () -> master_gain(),
  clip(compress(mix(list.map(fun (c) -> c.src, mixer_channels()))))
)
