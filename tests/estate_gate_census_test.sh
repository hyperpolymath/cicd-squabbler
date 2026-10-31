#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin"
cat > "$tmp/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == api ]] || exit 2
case "$2" in
  repos/example/pass|repos/example/startup|repos/example/absent)
    printf '%s\n' '{"default_branch":"main","archived":false}';;
  'repos/example/pass/contents/.github/workflows?ref=main')
    printf '%s\n' '[{"name":"cicd-squabbler.yml","type":"file"}]';;
  'repos/example/startup/contents/.github/workflows?ref=main')
    printf '%s\n' '[{"name":"squabble.yml","type":"file"}]';;
  'repos/example/absent/contents/.github/workflows?ref=main')
    printf '%s\n' '[{"name":"ci.yml","type":"file"}]';;
  'repos/example/pass/actions/workflows/cicd-squabbler.yml/runs?branch=main&per_page=10')
    printf '%s\n' '{"workflow_runs":[{"id":7,"conclusion":"success"}]}';;
  'repos/example/startup/actions/workflows/squabble.yml/runs?branch=main&per_page=10')
    printf '%s\n' '{"workflow_runs":[{"id":8,"conclusion":"startup_failure"}]}';;
  'repos/example/pass/actions/runs/7/jobs?per_page=100')
    printf '%s\n' '{"jobs":[{"name":"Squabble chains (hard gate)","conclusion":"success"}]}';;
  'repos/example/startup/actions/runs/8/jobs?per_page=100')
    printf '%s\n' '{"jobs":[]}';;
  'repos/example/pass/rulesets?includes_parents=true')
    printf '%s\n' '[{"id":3,"enforcement":"active"}]';;
  'repos/example/startup/rulesets?includes_parents=true')
    printf '%s\n' '[]';;
  repos/example/pass/rulesets/3)
    printf '%s\n' '{"rules":[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"Squabble chains (hard gate)"}]}}],"bypass_actors":[{"actor_type":"RepositoryRole","actor_id":5,"bypass_mode":"always"}]}';;
  *) echo "unexpected mocked API call: $2" >&2; exit 1;;
esac
MOCK
chmod +x "$tmp/bin/gh"
bash -n "$root/scripts/estate-gate-census.sh"
PATH="$tmp/bin:$PATH" bash "$root/scripts/estate-gate-census.sh" \
  --repo example/pass --repo example/startup --repo example/absent --output "$tmp/rows.jsonl"
[[ "$(wc -l < "$tmp/rows.jsonl")" -eq 3 ]]
jq -e -s '
  (map(select(.repo=="example/pass"))[0] | .run.job_conclusion=="success" and
   .policy.rulesets=="required_by_ruleset" and (.policy.bypass | contains("always"))) and
  (map(select(.repo=="example/startup"))[0] | .run.status=="startup_failure" and
   .run.job_conclusion=="zero_jobs") and
  (map(select(.repo=="example/absent"))[0] | .state=="filename_absent")
' "$tmp/rows.jsonl" >/dev/null
echo "estate gate census: hermetic cases passed"
