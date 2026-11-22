# Mixer graph for the v2 cockpit (issue #145).
#
# Replaces the former `fallback` priority chain (lib/master.liq) with a real
# mixer. The six program channels `live, show, queue, rotation, relay, fun`
# become strips:
#
#   source -> amplify(trim) -> amplify(fader) -> peak.stereo -> rms.stereo
#
# The fader folds mute in (muted => 0.0) and every stage reads a ref, so
# lib/control.liq can drive it at runtime (mixer.set/trim/mute/solo/exclusive).
# The program bus is `mix(strips)` -> master fader -> compress -> clip
# (lib/master.liq); solo feeds a separate cue bus that never touches the
# program (docs/product-v2-cockpit.md §2).
#
# I/O-free at include time: only pure helpers and the `build_mixer` factory are
# defined here. lib/master.liq instantiates the graph, which keeps the pure
# logic hermetically testable (tests/mixer_preset_test.liq).

# --- stable channel registry (docs/control-api.md §5.5) --------------------

# Fixed order; JSON state, meters and presets all follow it.
channel_ids = ["live", "show", "queue", "rotation", "relay", "fun"]

def channel_known(ch) =
  list.mem(ch, channel_ids)
end

# --- pure fader / mute logic (US3) -----------------------------------------

# One gain semantics per channel: mute folds into the fader, so the fader value
# is 0.0 while muted and the stored gain survives (mute stays reversible).
def fader_value(muted, gain) =
  if muted then 0.0 else gain end
end

# `mixer.exclusive <ch>`: unmute exactly `ch`, mute every other channel. Returns
# the mute map `[(id, bool)]` in channel order (pure, testable).
def exclusive_mutes(chs, ch) =
  list.map(fun (cid) -> (cid, cid != ch), chs)
end

# --- gain parsing (mixer.set / mixer.trim / mixer.master, US5) -------------

# Bare decimal number (optional sign, optional fraction).
num_re = "^[+-]?[0-9]+(\\.[0-9]+)?$"

def is_number(t) =
  string.match(pattern=num_re, t)
end

# `mixer.set` gain -> linear factor. A bare float in `[0.0, 1.0]` is a linear
# factor, anything else is dB; a `dB`/`DB` suffix always forces dB. `-1.0` is
# the error sentinel (linear gains are never negative, so `bad_gain`).
def parse_gain(token) =
  t = string.trim(token)
  if string.match(pattern="[dD][bB]$", t) then
    body = string.replace(pattern="[dD][bB]$", fun (_) -> "", t)
    if is_number(body) then lin_of_dB(string.to_float(body)) else -1.0 end
  elsif is_number(t) then
    v = string.to_float(t)
    if v >= 0.0 and v <= 1.0 then v else lin_of_dB(v) end
  else
    -1.0
  end
end

# `mixer.trim` / `mixer.master` dB argument -> linear factor (`-1.0` on error).
def parse_db(token) =
  t = string.trim(token)
  if is_number(t) then lin_of_dB(string.to_float(t)) else -1.0 end
end

# Boolean argument -> `bool?` (`null` = malformed).
def parse_bool(raw) =
  if raw == "true" then
    true
  elsif raw == "false" then
    false
  else
    null
  end
end

# --- presets (layer.preset, US4) -------------------------------------------

preset_names = ["default", "live", "relay-only"]

def preset_known(name) =
  list.mem(name, preset_names)
end

# Pure preset table: `[(channel_id, linear_gain)]` in channel order.
#   live       -> only the live channel open
#   relay-only -> only the relay channel open
#   default    -> documented start distribution: music path (rotation) open
def preset_gains(name) =
  if name == "live" then
    [("live", 1.0), ("show", 0.0), ("queue", 0.0), ("rotation", 0.0), ("relay", 0.0), ("fun", 0.0)]
  elsif name == "relay-only" then
    [("live", 0.0), ("show", 0.0), ("queue", 0.0), ("rotation", 0.0), ("relay", 1.0), ("fun", 0.0)]
  elsif name == "default" then
    [("live", 0.0), ("show", 0.0), ("queue", 0.0), ("rotation", 1.0), ("relay", 0.0), ("fun", 0.0)]
  else
    []
  end
end

# --- source descriptor validation (mixer.patch, US5) -----------------------

# Valid `mixer.patch` sources: `file:<path>`, `https:<url>`,
# `harbor:/live/slot-NN`, `relay:<url>` (docs/control-api.md §4.1).
def source_valid(spec) =
  string.match(pattern="^file:.+", spec) or
  string.match(pattern="^https:.+", spec) or
  string.match(pattern="^harbor:/live/slot-[0-9]+$", spec) or
  string.match(pattern="^relay:.+", spec)
end

# --- meters (mixer.meters, US6, docs/control-api.md §4.5) ------------------

# Linear RMS/peak (0.0..1.0) -> dBFS; `null` for silence (`-inf` on the wire —
# json.stringify cannot represent -inf).
def lin_to_db_opt(lin) =
  if lin <= 0.0 then null else dB_of_lin(lin) end
end

# Byte-exact JSON array from `(id, rms_l, rms_r, peak, limiter_active)` rows,
# field order = §4.5 / §5.5.
def meters_json(rows) =
  json.stringify(
    list.map(
      fun ((cid, l, r, p, la)) -> {id=cid, rms_l=l, rms_r=r, peak=p, limiter_active=la},
      rows
    )
  )
end

# --- runtime registry + graph factory (US1) --------------------------------

# Populated by lib/master.liq via `build_mixer`. A ref so lib/control.liq
# (included later) can read and mutate the strips.
mixer_channels = ref([])

# Master fader (linear), driven by `mixer.master`.
master_gain = ref(1.0)

# Last `layer.force` value: a channel id, or `null` = automation.
layer_forced = ref((null : string?))

# Generic per-layer availability (`layer.enable`).
layer_enabled = ref([
  ("live", true), ("show", true), ("queue", true),
  ("rotation", true), ("relay", true), ("fun", true)
])

def channel_by_id(ch) =
  m = list.filter(fun (c) -> c.id == ch, mixer_channels())
  if list.length(m) == 0 then null else list.hd(m) end
end

# One mixer strip: trim gain -> fader (mute folded in) -> peak -> rms. The rms
# output is the program input; `pre` (post-trim, pre-fader) feeds the cue bus.
def build_strip(ch, src) =
  gain = ref(1.0)
  trim = ref(1.0)
  mute = ref(false)
  solo = ref(false)
  patch = ref("")
  pre = amplify(fun () -> trim(), src)
  fader = amplify(fun () -> fader_value(mute(), gain()), pre)
  pk = peak.stereo(duration=1.0, fader)
  meter = rms.stereo(duration=1.0, pk)
  {
    id = ch, src = meter, pre = pre,
    gain = gain, trim = trim, mute = mute, solo = solo, patch = patch,
    meter = meter, peakm = pk
  }
end

# Build the strips in the given (registry) order.
def build_mixer(sources) =
  list.map(fun ((ch, src)) -> build_strip(ch, src), sources)
end

# Cue bus: every channel gated by its solo flag. Binding only in #145 — never
# mixed into the program.
def build_cue(strips) =
  mix(list.map(fun (c) -> amplify(fun () -> if c.solo() then 1.0 else 0.0 end, c.pre), strips))
end

# Forward-looking hook (US7). `on_position` is a SOURCE METHOD in 2.4.5 (not a
# top-level operator); the full signature needs an explicit `synchronous` and a
# 2-arg handler `(position, metadata)`.
def attach_end_hook(src, lead) =
  src.on_position(
    position=fun () -> lead,
    remaining=true,
    synchronous=false,
    fun (pos, meta) -> begin
      log(level=4, "mixer: track ending in #{pos}s meta=#{list.length(meta)}")
    end
  )
  src
end

# --- command helpers (US5) -------------------------------------------------

# Split a command argument line into non-empty tokens.
def split_args(arg) =
  list.filter(fun (s) -> s != "", string.split(separator=" ", string.trim(arg)))
end

def apply_to_channel(ch, on_found) =
  null.case(
    channel_by_id(ch),
    fun () -> "ERROR unknown_channel: " ^ ch,
    on_found
  )
end

def cmd_set(ch, token) =
  if not channel_known(ch) then
    "ERROR unknown_channel: " ^ ch
  elsif parse_gain(token) < 0.0 then
    "ERROR bad_gain: " ^ token
  else
    apply_to_channel(ch, fun (c) -> begin c.gain := parse_gain(token); "OK" end)
  end
end

def cmd_trim(ch, token) =
  if not channel_known(ch) then
    "ERROR unknown_channel: " ^ ch
  elsif parse_db(token) < 0.0 then
    "ERROR bad_gain: " ^ token
  else
    apply_to_channel(ch, fun (c) -> begin c.trim := parse_db(token); "OK" end)
  end
end

def cmd_mute(ch, raw) =
  if not channel_known(ch) then
    "ERROR unknown_channel: " ^ ch
  else
    null.case(
      parse_bool(raw),
      fun () -> "ERROR invalid_args: " ^ raw,
      fun (v) -> apply_to_channel(ch, fun (c) -> begin c.mute := v; "OK" end)
    )
  end
end

def cmd_solo(ch, raw) =
  if not channel_known(ch) then
    "ERROR unknown_channel: " ^ ch
  else
    null.case(
      parse_bool(raw),
      fun () -> "ERROR invalid_args: " ^ raw,
      fun (v) -> apply_to_channel(ch, fun (c) -> begin c.solo := v; "OK" end)
    )
  end
end

def cmd_exclusive(ch) =
  if not channel_known(ch) then
    "ERROR unknown_channel: " ^ ch
  else
    apply_to_channel(ch, fun (_) -> begin
      list.iter(
        fun ((cid, muted)) -> null.case(
          channel_by_id(cid),
          fun () -> (),
          fun (c) -> c.mute := muted
        ),
        exclusive_mutes(channel_ids, ch)
      )
      "OK"
    end)
  end
end

def cmd_master(token) =
  if parse_db(token) < 0.0 then
    "ERROR bad_gain: " ^ token
  else begin
    master_gain := parse_db(token)
    "OK"
  end
  end
end

# `mixer.patch` validates and records the descriptor, then mutes the channel
# (docs/control-api.md §4.1). Actual re-routing needs runtime source creation,
# which Liquidsoap 2.4.5 does not support (patchable slots are a follow-up).
def cmd_patch(ch, spec) =
  if not channel_known(ch) then
    "ERROR unknown_channel: " ^ ch
  elsif not source_valid(spec) then
    "ERROR bad_source: " ^ spec
  else
    apply_to_channel(ch, fun (c) -> begin c.patch := spec; c.mute := true; "OK" end)
  end
end

def cmd_preset(name) =
  if not preset_known(name) then
    "ERROR unknown_preset: " ^ name
  else begin
    list.iter(
      fun ((cid, g)) -> null.case(
        channel_by_id(cid),
        fun () -> (),
        fun (c) -> begin c.gain := g; c.mute := g <= 0.0 end
      ),
      preset_gains(name)
    )
    "OK"
  end
  end
end

def cmd_force(layer) =
  if layer == "none" then begin
    layer_forced := null
    "OK"
  end elsif channel_known(layer) then begin
    layer_forced := layer
    "OK"
  end else
    "ERROR unknown_layer: " ^ layer
  end
end

def cmd_enable(ch, raw) =
  if not channel_known(ch) then
    "ERROR unknown_layer: " ^ ch
  else
    null.case(
      parse_bool(raw),
      fun () -> "ERROR invalid_args: " ^ raw,
      fun (v) -> begin
        layer_enabled := list.map(
          fun ((k, e)) -> if k == ch then (k, v) else (k, e) end,
          layer_enabled()
        )
        "OK"
      end
    )
  end
end

# Live meter rows, read from each strip's rms/peak meters (channel order).
def meter_rows() =
  list.map(
    fun (c) -> begin
      let (l, r) = c.meter.rms()
      let (pl, pr) = c.peakm.peak()
      (c.id, lin_to_db_opt(l), lin_to_db_opt(r), lin_to_db_opt(max(pl, pr)), false)
    end,
    mixer_channels()
  )
end

def cmd_meters() =
  meters_json(meter_rows())
end
