# SPDX-License-Identifier: MPL-2.0
# Rules for one squabbler corpus case. Input: the case object.
# Arg $fn: the file's basename (used for rule V003).
# Output: one line per finding, "CODE message". Silent means valid and scrubbed.

def TOP: ["id","schema_version","repo","pr","run_id","job","step","attempt","observed_at",
          "evidence","classification","main_comparison","fix","retry_policy","provenance"];
def GROUPS: ["rate-limit","infra-flake","cancelled-timeout","lock-drift","stale-generated-file",
             "real-failure","missing-tool","quality-gate"];
def CONF: ["proven","likely","hypothesis"];
def MAIN: ["passes","fails","absent","unknown"];
def OWNERS: ["ci-gate-owner","pr-author","repo-owner","estate-owner","none"];
def RISK: ["low","medium","high"];
def RETRY: ["none","rerun","rerun-after-window","fix-then-rerun"];
def READBY: ["log","api"];

def inlist($v; $list): ($list | any(. == $v));
def isint: (type == "number") and (. == floor);
def isstr: type == "string";
def isobj: type == "object";
def nonempty_str: isstr and (length > 0);

def SECRET: "(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----|(?i:bearer)\\s+[A-Za-z0-9._~+/=-]{20,}|(?i:(?:token|secret|password|apikey)\\s*[:=]\\s*[^\\s\"']{8,}))";
def EMAIL: "[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\\.[A-Za-z0-9-]+)*\\.[A-Za-z]{2,}\\b";
def INTERNAL: "(\\b[\\w-]+\\.(?:internal|corp|local|lan|intranet)\\b|\\b(?:10|192\\.168)\\.\\d+\\.\\d+(?:\\.\\d+)?\\b|\\b172\\.(?:1[6-9]|2\\d|3[01])\\.\\d+\\.\\d+\\b)";
def TS: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}Z$";

def lines_of($s): ($s | split("\n") | if (.[-1] == "") then .[:-1] else . end | length);

# Structural checks (V002). Each entry is [condition, message].
def structural:
  . as $c
  | [
    [($c.schema_version == "1"), "schema_version must be \"1\""],
    [(($c.id | isstr) and ($c.id | test("^SQ-CASE-[0-9]{4}$"))), "id must match SQ-CASE-NNNN"],
    [(($c.repo | isstr) and ($c.repo | test("^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$"))), "repo must be owner/name"],
    [(($c.pr == null) or ($c.pr | isint)), "pr must be integer or null (null = main)"],
    [(($c.run_id == null) or ($c.run_id | isint)), "run_id must be integer or null"],
    [(($c.attempt == null) or ($c.attempt | isint)), "attempt must be integer or null"],
    [($c.job | nonempty_str), "job must be a non-empty string"],
    [(($c.step == null) or ($c.step | isstr)), "step must be string or null"],
    [(($c.evidence | isobj) and (($c.evidence | keys | sort) == (["error_strings","exit_code","log_excerpt"] | sort))), "evidence fields"],
    [(($c.classification | isobj) and inlist($c.classification.group; GROUPS) and inlist($c.classification.confidence; CONF)), "classification group/confidence"],
    [(($c.main_comparison | isobj) and inlist($c.main_comparison.state; MAIN) and ($c.main_comparison.note | isstr)), "main_comparison"],
    [(($c.fix | isobj) and inlist($c.fix.owner; OWNERS) and inlist($c.fix.risk; RISK)
        and (($c.fix.files | type) == "array") and (($c.fix.requires_owner_go | type) == "boolean")
        and ($c.fix.description | isstr)), "fix fields"],
    [(($c.retry_policy | isobj) and inlist($c.retry_policy.action; RETRY)), "retry_policy.action"],
    [(($c.provenance | isobj) and inlist($c.provenance.read_by; READBY)), "provenance.read_by must be log or api"]
  ];

def findings($fn):
  . as $c
  | if ($c | isobj | not) then [["V001", "case is not an object"]]
    elif ((($c | keys | sort)) != (TOP | sort)) then
      [["V001", "fields differ: missing \((TOP - ($c | keys)) | sort) extra \((($c | keys) - TOP) | sort)"]]
    else
      ($c | structural | [ .[] | select(.[0] | not) | ["V002", .[1]] ]) as $s
      | if ($s | length) > 0 then $s
        else
          [
            (if ($c.observed_at != null) and ((($c.observed_at | isstr) and ($c.observed_at | test(TS))) | not)
               then ["V011", "observed_at must be UTC YYYY-MM-DDThh:mm:ssZ or null"] else empty end),
            (if ($fn != null) and ((($c.id) + ".json") != $fn)
               then ["V003", "file name must equal the case id"] else empty end),
            (if ($c.evidence.log_excerpt != null)
                and ((($c.evidence.log_excerpt | isstr) | not)
                     or (lines_of($c.evidence.log_excerpt) > 30))
               then ["V004", "log_excerpt must be a string of at most 30 lines, or null"] else empty end),
            (if (($c.evidence.error_strings | type) != "array")
                or ([ $c.evidence.error_strings[] | (isstr and length > 0) ] | all | not)
               then ["V002", "error_strings must be a list of non-empty strings"]
               else
                 ($c.evidence.error_strings) as $strings
                 | ([ .. | strings ]) as $blob
                 | (if ($blob | any(test(SECRET))) then ["V005", "possible secret or token"] else empty end),
                   (if ($blob | any(test(EMAIL))) then ["V006", "email address"] else empty end),
                   (if ($blob | any(test(INTERNAL; "i"))) then ["V007", "internal hostname or private address"] else empty end),
                   (if ($c.classification.confidence == "proven") and ($strings | length == 0)
                      then ["V009", "proven needs at least one verbatim error string"] else empty end),
                   (if ($c.evidence.log_excerpt != null)
                      and ([ $strings[] | . as $s | ($c.evidence.log_excerpt | contains($s)) ] | all | not)
                      then ["V012", "error string not present in log_excerpt"] else empty end),
                   (if ($c.fix.owner != "none") and ($c.fix.requires_owner_go != true)
                      then ["V010", "a fix owned by someone needs requires_owner_go true"] else empty end)
               end)
          ]
        end
    end;

findings($fn) | .[] | "\(.[0]) \(.[1])"
