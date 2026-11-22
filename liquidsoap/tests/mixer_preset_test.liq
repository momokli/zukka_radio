# Mixer pure-logic test (issue #145, US8).
#
# Hermetic: only the pure helpers from lib/mixer.liq are exercised. No sources,
# no harbor ports, no HTTP, no output. Run exactly like the other tests:
#
#   ZUKKA_HARBOR_SLOTS=0 liquidsoap tests/mixer_preset_test.liq
#
# Exits 0 when every case passes, 1 otherwise.

%include "../lib/mixer.liq"

failures = ref(0)

def check(label, actual, expected) =
  if actual == expected then
    print("PASS #{label}")
  else
    print("FAIL #{label}: expected=#{expected} actual=#{actual}")
    failures := failures() + 1
  end
end

# Case 1: channel registry (stable ID order, docs/control-api.md §5.5).
check("case1 ids", channel_ids, ["live", "show", "queue", "rotation", "relay", "fun"])
check("case1 known live", channel_known("live"), true)
check("case1 known fun", channel_known("fun"), true)
check("case1 unknown", channel_known("bogus"), false)
check("case1 empty unknown", channel_known(""), false)

# Case 2: preset table (pure, value-exact).
check("case2 preset live", preset_gains("live"),
  [("live", 1.0), ("show", 0.0), ("queue", 0.0), ("rotation", 0.0), ("relay", 0.0), ("fun", 0.0)])
check("case2 preset relay-only", preset_gains("relay-only"),
  [("live", 0.0), ("show", 0.0), ("queue", 0.0), ("rotation", 0.0), ("relay", 1.0), ("fun", 0.0)])
check("case2 preset default", preset_gains("default"),
  [("live", 0.0), ("show", 0.0), ("queue", 0.0), ("rotation", 1.0), ("relay", 0.0), ("fun", 0.0)])
check("case2 preset unknown empty", preset_gains("bogus"), [])
check("case2 preset known live", preset_known("live"), true)
check("case2 preset known bogus", preset_known("bogus"), false)

# Case 3: gain parsing. Bare [0,1] = linear, anything else = dB, a `dB` suffix
# forces dB, malformed = -1.0 (=> bad_gain).
check("case3 linear 0.5", parse_gain("0.5"), 0.5)
check("case3 linear 1.0", parse_gain("1.0"), 1.0)
check("case3 linear 0.0 silent", parse_gain("0.0"), 0.0)
check("case3 dB -6.0", parse_gain("-6.0"), lin_of_dB(-6.0))
check("case3 suffix 0.5dB", parse_gain("0.5dB"), lin_of_dB(0.5))
check("case3 suffix 0dB", parse_gain("0dB"), lin_of_dB(0.0))
check("case3 bad abc", parse_gain("abc"), -1.0)
check("case3 bad empty", parse_gain(""), -1.0)
check("case3 bad 1.2.3", parse_gain("1.2.3"), -1.0)
check("case3 db -6.0", parse_db("-6.0"), lin_of_dB(-6.0))
check("case3 db bad", parse_db("x"), -1.0)

# Case 4: fader folds mute in; the stored gain survives (mute is reversible).
check("case4 muted -> 0", fader_value(true, 0.7), 0.0)
check("case4 unmuted -> gain", fader_value(false, 0.7), 0.7)

# Case 5: exclusive mute map (only `ch` stays open).
check("case5 exclusive live", exclusive_mutes(["live", "show", "queue"], "live"),
  [("live", false), ("show", true), ("queue", true)])

# Case 6: source descriptor validation.
check("case6 file", source_valid("file:/tracks/x.mp3"), true)
check("case6 https", source_valid("https://example/x"), true)
check("case6 harbor", source_valid("harbor:/live/slot-03"), true)
check("case6 harbor bad slot", source_valid("harbor:/live/slot-x"), false)
check("case6 relay", source_valid("relay:https://x"), true)
check("case6 bogus", source_valid("bogus"), false)

# Case 7: meters JSON, byte-exact vs the same stringifier (§4.5 shape).
check("case7 empty", meters_json([]), json.stringify([]))
rows1 = [("live", (null : float?), (null : float?), (null : float?), false)]
expected1 = [{id="live", rms_l=(null : float?), rms_r=(null : float?), peak=(null : float?), limiter_active=false}]
check("case7 silent row", meters_json(rows1), json.stringify(expected1))
rows2 = [
  ("live", (-18.2 : float?), (-18.0 : float?), (-3.1 : float?), false),
  ("show", (null : float?), (null : float?), (null : float?), false)
]
expected2 = [
  {id="live", rms_l=(-18.2 : float?), rms_r=(-18.0 : float?), peak=(-3.1 : float?), limiter_active=false},
  {id="show", rms_l=(null : float?), rms_r=(null : float?), peak=(null : float?), limiter_active=false}
]
check("case7 two rows", meters_json(rows2), json.stringify(expected2))

# Case 8: linear -> dB with `-inf` as null.
check("case8 silence null",
  null.case(lin_to_db_opt(0.0), fun () -> "none", fun (_) -> "some"), "none")
check("case8 unity",
  null.case(lin_to_db_opt(1.0), fun () -> -999.0, fun (v) -> v), 0.0)
check("case8 half",
  null.case(lin_to_db_opt(0.5), fun () -> -999.0, fun (v) -> v), dB_of_lin(0.5))

# Case 9: validation happens BEFORE any ref write (the registry is empty here,
# so the code path/order is what the assertions pin down).
check("case9 set unknown channel", cmd_set("bogus", "0.5"), "ERROR unknown_channel: bogus")
check("case9 set bad gain", cmd_set("live", "abc"), "ERROR bad_gain: abc")
check("case9 trim bad gain", cmd_trim("live", "abc"), "ERROR bad_gain: abc")
check("case9 mute unknown channel", cmd_mute("bogus", "true"), "ERROR unknown_channel: bogus")
check("case9 mute bad bool", cmd_mute("live", "maybe"), "ERROR invalid_args: maybe")
check("case9 solo bad bool", cmd_solo("live", "maybe"), "ERROR invalid_args: maybe")
check("case9 preset unknown", cmd_preset("bogus"), "ERROR unknown_preset: bogus")
check("case9 force unknown", cmd_force("bogus"), "ERROR unknown_layer: bogus")
check("case9 force none ok", cmd_force("none"), "OK")
check("case9 patch unknown channel", cmd_patch("bogus", "file:/x"), "ERROR unknown_channel: bogus")
check("case9 patch bad source", cmd_patch("live", "bogus"), "ERROR bad_source: bogus")
check("case9 master bad", cmd_master("x"), "ERROR bad_gain: x")

# Case 10: argument splitting.
check("case10 split", split_args("  live   0.5 "), ["live", "0.5"])
check("case10 split empty", split_args("   "), [])

if failures() == 0 then
  print("ALL PASS")
  shutdown()
else
  print("FAILURES=#{failures()}")
  shutdown(code=1)
end
