#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath)
"""Fire and silence tests for validate.py. Offline. Exit 0 only if every test behaves as expected."""
import copy, glob, json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
from validate import check

BASE = json.load(open(os.path.join(HERE, "..", "cases", "SQ-CASE-0012.json"), encoding="utf-8"))
def mut(fn):
    c = copy.deepcopy(BASE); fn(c); return c
def setp(path, value):
    def fn(c):
        o = c
        for k in path[:-1]: o = o[k]
        o[path[-1]] = value
    return fn

FIRE = {  # rule -> (mutation, must fire exactly this rule)
 "V001": lambda c: c.pop("job"),
 "V002": setp(["classification","group"], "nonsense"),
 "V003": None,  # handled below (file name)
 "V004": lambda c: c["evidence"].update(log_excerpt="\n".join(["x"]*31), error_strings=["x"]),
 "V005": setp(["fix","description"], "use ghp_" + "A"*24 + " to push"),
 "V006": setp(["fix","description"], "ask someone@example.org"),
 "V007": setp(["fix","description"], "runner build01.internal is slow"),
 "V009": lambda c: (c["evidence"].update(error_strings=[]), c["classification"].update(confidence="proven")),
 "V010": setp(["fix","requires_owner_go"], False),
 "V011": setp(["observed_at"], "20 Nov 2026"),
 "V012": lambda c: c["evidence"].update(log_excerpt="unrelated line", error_strings=["not in the excerpt"]),
}
SILENT = {  # near-misses that must stay silent
 "plain case": lambda c: None,
 "hash-like text, no token prefix": setp(["fix","description"], "see commit c519c452 and sha256 6cbeb457"),
 "ghp inside a word": setp(["fix","description"], "ghpath is a variable name"),
 "private-looking but public host": setp(["fix","description"], "archive.ubuntu.com timed out"),
 "version number, not an address": setp(["fix","description"], "upload-artifact v7.0.2 and 10.2 seconds"),
 "none owner without owner go": lambda c: c["fix"].update(owner="none", requires_owner_go=False),
 "null observed_at": setp(["observed_at"], None),
 "hypothesis without strings": lambda c: (c["evidence"].update(error_strings=[]), c["classification"].update(confidence="hypothesis")),
 "30-line excerpt": lambda c: c["evidence"].update(log_excerpt="\n".join(["x"]*30), error_strings=["x"]),
 "action@version is not an email": setp(["fix","description"], "actions/upload-artifact@v7.0.2 is missing"),
 "pr null (main)": setp(["pr"], None),
}
fails = []; n = 0
for code, fn in FIRE.items():
    n += 1
    if code == "V003":
        got = [x[0] for x in check(copy.deepcopy(BASE), "SQ-CASE-9999.json")]
    else:
        got = [x[0] for x in check(mut(fn), None)]
    if got != [code]:
        fails.append(f"FIRE {code}: expected exactly [{code}], got {got}")
for name, fn in SILENT.items():
    n += 1
    got = [x[0] for x in check(mut(fn), None)]
    if got:
        fails.append(f"SILENT '{name}': expected no findings, got {got}")
real = sorted(glob.glob(os.path.join(HERE, "..", "cases", "*.json")))
for p in real:
    n += 1
    got = [x[0] for x in check(json.load(open(p, encoding="utf-8")), p)]
    if got:
        fails.append(f"SILENT {os.path.basename(p)}: expected no findings, got {got}")
n += 1
if len(real) != 15: fails.append(f"expected 15 cases, found {len(real)}")
for f in fails: print("FAIL", f)
print(f"{n - len(fails)}/{n} tests behaved as expected ({len(FIRE)} fire, {len(SILENT)} near-miss silence, {len(real)} real cases silent)")
sys.exit(1 if fails else 0)
