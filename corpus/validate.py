#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath)
"""Offline validator for squabbler corpus cases. Standard library only.
Usage: validate.py FILE.json [FILE.json ...]   (exit 0 = silent, 1 = findings)"""
import json, os, re, sys

TOP = {"id","schema_version","repo","pr","run_id","job","step","attempt","observed_at",
       "evidence","classification","main_comparison","fix","retry_policy","provenance"}
GROUPS = {"rate-limit","infra-flake","cancelled-timeout","lock-drift","stale-generated-file",
          "real-failure","missing-tool","quality-gate"}
CONF = {"proven","likely","hypothesis"}
MAIN = {"passes","fails","absent","unknown"}
OWNERS = {"ci-gate-owner","pr-author","repo-owner","estate-owner","none"}
RISK = {"low","medium","high"}
RETRY = {"none","rerun","rerun-after-window","fix-then-rerun"}
READBY = {"log","api"}
SECRET = re.compile(r"(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|"
                    r"-----BEGIN [A-Z ]*PRIVATE KEY-----|(?i:bearer)\s+[A-Za-z0-9._~+/=-]{20,}|"
                    r"(?i:(?:token|secret|password|apikey)\s*[:=]\s*[^\s\"']{8,}))")
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b")
INTERNAL = re.compile(r"(\b[\w-]+\.(?:internal|corp|local|lan|intranet)\b|\b(?:10|192\.168)\.\d+\.\d+(?:\.\d+)?\b|\b172\.(?:1[6-9]|2\d|3[01])\.\d+\.\d+\b)", re.I)
TS = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")

def texts(o):
    if isinstance(o, str): yield o
    elif isinstance(o, dict):
        for v in o.values(): yield from texts(v)
    elif isinstance(o, list):
        for v in o: yield from texts(v)

def isint(v): return isinstance(v, int) and not isinstance(v, bool)

def check(c, filename=None):
    out = []
    def f(code, msg): out.append((code, msg))
    if not isinstance(c, dict): return [("V001", "case is not an object")]
    if set(c) != TOP:
        f("V001", f"fields differ: missing {sorted(TOP-set(c))} extra {sorted(set(c)-TOP)}")
        return out
    ev, cl, mc, fx, rp, pv = (c["evidence"], c["classification"], c["main_comparison"],
                              c["fix"], c["retry_policy"], c["provenance"])
    ok = True
    def need(cond, msg):
        nonlocal ok
        if not cond: ok = False; f("V002", msg)
    need(c["schema_version"] == "1", "schema_version must be \"1\"")
    need(isinstance(c["id"], str) and re.fullmatch(r"SQ-CASE-\d{4}", c["id"] or ""), "id must match SQ-CASE-NNNN")
    need(isinstance(c["repo"], str) and re.fullmatch(r"[\w.-]+/[\w.-]+", c["repo"] or ""), "repo must be owner/name")
    need(c["pr"] is None or isint(c["pr"]), "pr must be integer or null (null = main)")
    need(c["run_id"] is None or isint(c["run_id"]), "run_id must be integer or null")
    need(c["attempt"] is None or isint(c["attempt"]), "attempt must be integer or null")
    need(isinstance(c["job"], str) and c["job"], "job must be a non-empty string")
    need(c["step"] is None or isinstance(c["step"], str), "step must be string or null")
    need(isinstance(ev, dict) and set(ev) == {"log_excerpt","exit_code","error_strings"}, "evidence fields")
    need(isinstance(cl, dict) and cl.get("group") in GROUPS and cl.get("confidence") in CONF, "classification group/confidence")
    need(isinstance(mc, dict) and mc.get("state") in MAIN and isinstance(mc.get("note"), str), "main_comparison")
    need(isinstance(fx, dict) and fx.get("owner") in OWNERS and fx.get("risk") in RISK and
         isinstance(fx.get("files"), list) and isinstance(fx.get("requires_owner_go"), bool) and
         isinstance(fx.get("description"), str), "fix fields")
    need(isinstance(rp, dict) and rp.get("action") in RETRY, "retry_policy.action")
    need(isinstance(pv, dict) and pv.get("read_by") in READBY, "provenance.read_by must be log or api")
    if not ok: return out
    if c["observed_at"] is not None and not (isinstance(c["observed_at"], str) and TS.match(c["observed_at"])):
        f("V011", "observed_at must be UTC YYYY-MM-DDThh:mm:ssZ or null")
    if filename and os.path.splitext(os.path.basename(filename))[0] != c["id"]:
        f("V003", "file name must equal the case id")
    ex = ev["log_excerpt"]
    if ex is not None and (not isinstance(ex, str) or len(ex.splitlines()) > 30):
        f("V004", "log_excerpt must be a string of at most 30 lines, or null")
    strings = ev["error_strings"]
    if not isinstance(strings, list) or not all(isinstance(s, str) and s for s in strings):
        f("V002", "error_strings must be a list of non-empty strings"); return out
    blob = list(texts(c))
    if any(SECRET.search(t) for t in blob): f("V005", "possible secret or token")
    if any(EMAIL.search(t) for t in blob): f("V006", "email address")
    if any(INTERNAL.search(t) for t in blob): f("V007", "internal hostname or private address")
    if cl["confidence"] == "proven" and not strings: f("V009", "proven needs at least one verbatim error string")
    if ex is not None and any(s not in ex for s in strings): f("V012", "error string not present in log_excerpt")
    if fx["owner"] != "none" and not fx["requires_owner_go"]: f("V010", "a fix owned by someone needs requires_owner_go true")
    return out

def main(argv):
    bad = 0
    for p in argv:
        try:
            with open(p, encoding="utf-8") as fh: c = json.load(fh)
        except (OSError, ValueError) as e:
            print(f"{p}: V000 unreadable: {e}"); bad += 1; continue
        for code, msg in check(c, p):
            print(f"{p}: {code} {msg}"); bad += 1
    return 1 if bad else 0

if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
