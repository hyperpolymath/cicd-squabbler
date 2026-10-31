#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Read-only squabbler deployment census. Unknown is NEVER a clean verdict.
set -euo pipefail
command -v gh >/dev/null && command -v jq >/dev/null || exit 2
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
out=""
while (($#)); do
  case "$1" in
    --owner)
      gh repo list "$2" --limit 10000 --json nameWithOwner,isArchived,isFork \
        | jq -r '.[] | select(.isArchived != true and .isFork != true) | .nameWithOwner' >> "$tmp" || exit 3
      shift 2 ;;
    --repo) printf '%s\n' "$2" >> "$tmp"; shift 2 ;;
    --output) out="$2"; shift 2 ;;
    -h|--help) echo "Usage: $0 [--owner OWNER]... [--repo OWNER/REPO]... [--output JSONL]"; exit 0 ;;
    *) echo "Bad argument: $1" >&2; exit 2 ;;
  esac
done
api() {
  local data
  data="$(gh api "$1" 2>/dev/null)" || return 1
  jq -e . >/dev/null 2>&1 <<< "$data" || return 1
  printf '%s\n' "$data"
}
if [[ -n "$out" ]]; then mkdir -p "$(dirname "$out")"; : > "$out"; fi
emit() {
  if [[ -n "$out" ]]; then printf '%s\n' "$1" >> "$out"
  else printf '%s\n' "$1"; fi
}
sort -u "$tmp" | while IFS= read -r repo; do
  [[ -n "$repo" ]] || continue
  [[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || exit 2
  meta="$(api "repos/$repo")" || {
    emit "$(jq -nc --arg r "$repo" '{repo:$r,state:"unknown",reason:"metadata_unreadable"}')"; continue
  }
  branch="$(jq -r '.default_branch // empty' <<<"$meta")"
  if [[ -z "$branch" ]]; then
    emit "$(jq -nc --arg r "$repo" '{repo:$r,state:"unknown",reason:"default_branch_missing"}')"; continue
  fi
  ref="$(jq -rn --arg x "$branch" '$x|@uri')"
  workflows="$(api "repos/$repo/contents/.github/workflows?ref=$ref")" || {
    emit "$(jq -nc --arg r "$repo" '{repo:$r,state:"unknown",reason:"workflow_listing_unavailable"}')"; continue
  }
  if ! jq -e 'type == "array"' <<<"$workflows" >/dev/null; then
    emit "$(jq -nc --arg r "$repo" '{repo:$r,state:"unknown",reason:"bad_listing"}')"; continue
  fi
  names="$(jq -r '.[] | select(.type=="file" and (.name|test("squabbl.*[.]ya?ml$";"i"))) | .name' <<<"$workflows")"
  if [[ -z "$names" ]]; then
    emit "$(jq -nc --arg r "$repo" '{repo:$r,state:"filename_absent",note:"embedded_callers_not_examined"}')"; continue
  fi
  rules="$(api "repos/$repo/rulesets?includes_parents=true")" || rules=""
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    rid=""; status=unknown; job=""; job_status=unknown
    runs="$(api "repos/$repo/actions/workflows/$name/runs?branch=$ref&per_page=10")" || runs=""
    if [[ -n "$runs" ]] && jq -e '.workflow_runs|type=="array"' <<<"$runs" >/dev/null; then
      rid="$(jq -r '.workflow_runs[0].id // empty' <<<"$runs")"
      status="$(jq -r '.workflow_runs[0].conclusion // .workflow_runs[0].status // "no_run"' <<<"$runs")"
      if [[ -n "$rid" ]]; then
        jobs="$(api "repos/$repo/actions/runs/$rid/jobs?per_page=100")" || jobs=""
        if [[ -n "$jobs" ]] && jq -e '.jobs|type=="array"' <<<"$jobs" >/dev/null; then
          job="$(jq -r '[.jobs[]|select((.name//"")|test("squabbl";"i"))|.name]|first//empty' <<<"$jobs")"
          if [[ -n "$job" ]]; then
            job_status="$(jq -r --arg j "$job" '[.jobs[]|select(.name==$j)|.conclusion//.status]|first//"unknown"' <<<"$jobs")"
          elif [[ "$status" == "startup_failure" ]]; then job_status=zero_jobs; fi
        fi
      fi
    fi
    required=unknown; bypass=unknown
    if [[ -n "$rules" ]] && jq -e 'type=="array"' <<<"$rules" >/dev/null; then
      required=not_in_readable_rulesets; bypass='[]'
      ids="$(jq -r '.[]|select(.enforcement=="active")|.id' <<<"$rules")"
      while IFS= read -r id; do
        [[ -n "$id" ]] || continue
        detail="$(api "repos/$repo/rulesets/$id")" || { required=unknown; bypass=unknown; break; }
        if [[ -n "$job" ]] && jq -e --arg j "$job" \
          '[.rules[]?|select(.type=="required_status_checks")|.parameters.required_status_checks[]?|.context]|index($j)!=null' \
          <<<"$detail" >/dev/null; then
          required=required_by_ruleset
          bypass="$(jq -c '[.bypass_actors[]?|{actor_type,actor_id,bypass_mode}]' <<<"$detail")"
        fi
      done <<<"$ids"
    fi
    emit "$(jq -nc --arg r "$repo" --arg wf "$name" --arg id "$rid" \
      --arg state "$status" --arg job "$job" --arg js "$job_status" \
      --arg req "$required" --arg by "$bypass" \
      '{repo:$r,state:"named_workflow_found",workflow:$wf,run:{id:$id,status:$state,job:$job,job_conclusion:$js},policy:{rulesets:$req,bypass:$by,classic_protection:"not_inspected"}}')"
  done <<<"$names"
done
