#!/usr/bin/env python3
"""Contract guard for the Cockpit v2 spec bundle (Issue #152).

Durably re-verifies the machine-checkable parts of the multi-channel contract:
  1. every fixtures/*.json validates against state.schema.json;
  2. RED-before-GREEN: fixtures/wide.json FAILS a temp copy of the *old*
     6-value ChannelKind enum and PASSES the current 8-value schema;
  3. channels[] ordering follows the normative group order (§5.5);
  4. binary meter frame math: total = 4 + N*12 and N <= 255 (u8 cap);
  5. all spec JSON files parse; no dead fixtures/*.json refs in README/ui-brief.

Run:  python3 spec/validate-fixtures.py        (jsonschema must be importable;
      e.g. /tmp/jsvenv/bin/python spec/validate-fixtures.py)
Exit: 0 = all checks pass, 1 = a check failed.
"""
import json
import re
import sys
from pathlib import Path

SPEC = Path(__file__).resolve().parent
ROOT = SPEC.parent
FIXTURES = SPEC / "fixtures"
ALL_FIXTURES = ["idle.json", "live.json", "relay-crossfade.json", "wide.json"]
PROGRAM = ["live", "show", "queue", "rotation", "relay", "fun"]

fails = []
total = 0
def check(name, ok, detail=""):
    global total
    total += 1
    print(("PASS " if ok else "FAIL ") + name + ((" — " + detail) if detail else ""))
    if not ok:
        fails.append(name)

try:
    from jsonschema import Draft202012Validator
except ImportError as e:  # pragma: no cover
    print("FAIL import jsonschema: " + str(e))
    sys.exit(1)

schema = json.loads((SPEC / "state.schema.json").read_text())
Draft202012Validator.check_schema(schema)
validator = Draft202012Validator(schema)

# --- 1. all fixtures valid -------------------------------------------------
channels = {}
for f in ALL_FIXTURES:
    doc = json.loads((FIXTURES / f).read_text())
    channels[f] = doc["channels"]
    errs = list(validator.iter_errors(doc))
    check("fixture %s valid" % f, not errs,
          "channels=%d" % len(doc["channels"]) if not errs
          else "; ".join(e.message for e in errs[:3]))

# --- 2. RED-before-GREEN (old 6-value enum) --------------------------------
old = json.loads(json.dumps(schema))  # temp copy, real schema untouched
old["$defs"]["ChannelKind"]["enum"] = PROGRAM[:]  # 6 values, no harbor/adhoc
old_v = Draft202012Validator(old)
wide = json.loads((FIXTURES / "wide.json").read_text())
red_errs = list(old_v.iter_errors(wide))
check("RED wide.json vs old 6-enum fails", bool(red_errs),
      "enum error(s)=%d" % len(red_errs))
green_errs = list(validator.iter_errors(wide))
check("GREEN wide.json vs new 8-enum passes", not green_errs,
      "errors=%d" % len(green_errs))

# --- 3. normative channels[] order (§5.5) ----------------------------------
def group(cid):
    if cid in PROGRAM:
        return (0, PROGRAM.index(cid))
    if re.fullmatch(r"harbor\.slot-\d{2,}", cid):
        return (1, int(cid.rsplit("-", 1)[1]))
    if re.fullmatch(r"relay\.slot-\d{2,}", cid):
        return (2, int(cid.rsplit("-", 1)[1]))
    if re.fullmatch(r"adhoc-\d+", cid):
        return (3, int(cid.split("-", 1)[1]))
    return (9, cid)

for f in ALL_FIXTURES:
    ids = [c["id"] for c in channels[f]]
    keys = [group(i) for i in ids]
    check("order %s" % f, keys == sorted(keys), ",".join(ids))

# --- 4. binary meter frame math (docs/control-api.md §6.2) ------------------
n = len(channels["wide.json"])
check("meter total 4+N*12 for N=%d" % n, 4 + n * 12 == 4 + 156, "= %d bytes" % (4 + n * 12))
check("meter N <= 255 (u8 cap)", n <= 255, "N=%d" % n)

# --- 5. JSON validity + no dead fixture refs --------------------------------
for p in ["state.schema.json", "commands.json", "design-tokens.json"] + \
        ["fixtures/" + f for f in ALL_FIXTURES]:
    try:
        json.loads((SPEC / p).read_text())
        check("json parse %s" % p, True)
    except Exception as e:
        check("json parse %s" % p, False, str(e))

for md in ["README.md", "ui-brief.md"]:
    text = (SPEC / md).read_text()
    refs = re.findall(r"fixtures/([A-Za-z0-9_.-]+\.json)", text)
    dead = [r for r in set(refs) if not (FIXTURES / r).exists()]
    check("no dead fixture refs in %s" % md, not dead, "refs=%s dead=%s" % (sorted(set(refs)), dead))

print("\n%d checks, %d failed" % (total, len(fails)))
if fails:
    print("FAILED: " + ", ".join(fails))
    sys.exit(1)
print("VALIDATE_OK")
