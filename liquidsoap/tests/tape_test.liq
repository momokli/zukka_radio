# Tape helper test (issue #108, US1).
#
# Hermetic: only the pure helpers from lib/tape.liq are exercised. No harbor
# ports, no HTTP, no recorder. Run exactly like the other tests:
#
#   ZUKKA_HARBOR_SLOTS=0 liquidsoap tests/tape_test.liq
#
# Exits 0 when every case passes, 1 otherwise.

%include "../lib/tape.liq"

failures = ref(0)

def check(label, actual, expected) =
  if actual == expected then
    print("PASS #{label}")
  else
    print("FAIL #{label}: expected=#{expected} actual=#{actual}")
    failures := failures() + 1
  end
end

good = "0192f0a1-0001-7000-8000-000000000003"

# Case 1: filename build + traversal-safe id validation.
check("case1 filename", tape_filename(good), "/recordings/" ^ good ^ ".mp3")
check("case1 valid uuidv7", tape_id_valid(good), true)
check("case1 empty invalid", tape_id_valid(""), false)
check("case1 short invalid", tape_id_valid("0192f0a1"), false)
check("case1 uppercase invalid", tape_id_valid("0192F0A1-0001-7000-8000-000000000003"), false)
check("case1 slash traversal invalid", tape_id_valid("../etc/passwd"), false)
check("case1 dotdot invalid", tape_id_valid(".."), false)
check("case1 space invalid", tape_id_valid("0192f0a1 0001-7000-8000-000000000003"), false)
check("case1 embedded slash invalid", tape_id_valid("0192f0a1-0001-7000-8000-00000000000/"), false)

# Case 2: guard matrix. Invalid id wins over the active flag.
check("case2 startable", tape_start_guard(good, false), "")
check("case2 invalid -> usage", tape_start_guard("nope", false), "error: usage: tape.start <tape_id>")
check("case2 active -> already", tape_start_guard(good, true), "error: already recording")
check("case2 invalid+active -> usage", tape_start_guard("", true), "error: usage: tape.start <tape_id>")

# Case 3: status JSON. Build the expectation with the same stringifier so the
# formatting is identical. Inactive -> empty tape_id/path; active -> the path.
check("case3 status inactive (fresh)", tape_status_json(false),
  json.stringify({active = false, tape_id = "", path = ""}))

tape_current := good
check("case3 status active", tape_status_json(true),
  json.stringify({active = true, tape_id = good, path = "/recordings/" ^ good ^ ".mp3"}))
check("case3 path getter", tape_path(), "/recordings/" ^ good ^ ".mp3")

if failures() == 0 then
  print("ALL PASS")
  shutdown()
else
  print("FAILURES=#{failures()}")
  shutdown(code=1)
end
