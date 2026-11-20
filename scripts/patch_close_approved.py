#!/usr/bin/env python3
"""One-shot: let the zukka profile auto-close approved milestones."""

import shutil
import subprocess
from pathlib import Path

BIN = Path("/home/momo/.local/bin")
PROFILE = BIN / "runner-profile.sh"
CLOSE = BIN / "runner-close-milestones.sh"

for p in (PROFILE, CLOSE):
    bak = Path(str(p) + ".bak-close-approved")
    if not bak.exists():
        shutil.copy2(p, bak)

text = PROFILE.read_text()
anchor = ': "${RUNNER_BASE_BRANCH:=main}"'
if "RUNNER_CLOSE_APPROVED" in text:
    print("profile: already present")
else:
    assert anchor in text, "anchor not found"
    text = text.replace(
        anchor,
        anchor
        + '\n: "${RUNNER_CLOSE_APPROVED:=0}"   # 1 = auch freigegebene Milestones schliessen',
        1,
    )
    PROFILE.write_text(text)
    print("profile: RUNNER_CLOSE_APPROVED added")

text = CLOSE.read_text()
old_frag = (
    """--arg re "$RUNNER_APPROVE_RE" '.[] | select(.open_issues == 0) | """
    """select(((.description // "") | test($re)) | not) | .number'"""
)
new_frag = (
    """--arg re "$RUNNER_APPROVE_RE" --arg ca "${RUNNER_CLOSE_APPROVED:-0}" '.[] | """
    """select(.open_issues == 0) | """
    """select(($ca == "1") or (((.description // "") | test($re)) | not)) | .number'"""
)

if new_frag in text:
    print("close-milestones: already patched")
elif old_frag in text:
    CLOSE.write_text(text.replace(old_frag, new_frag, 1))
    print("close-milestones: patched")
else:
    print("close-milestones: WARN pattern not found")

for p in (PROFILE, CLOSE):
    r = subprocess.run(["bash", "-n", str(p)], capture_output=True, text=True)
    print(f"bash -n {p.name}: {'ok' if r.returncode == 0 else r.stderr.strip()}")
