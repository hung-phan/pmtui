#!/usr/bin/env bash
# Reference coordinator for the pmd tracer bullet (Milestone 1). NOT an LLM.
#
# Obeys the §5 step contract: boot from .project-state/, do exactly ONE unit of
# work, write state atomically, then exit. Advances a toy 3-step project:
#   fresh         -> step 1: awaiting_next
#   awaiting_next -> step 2: needs_you  (raises one stop carrying a risk_class)
#   needs_you     -> if answered: step 3: done (resolve + record); else re-emit
#   done          -> idempotent no-op
#
# Idempotent / re-runnable: the unit is a pure function of durable state, and
# every write is temp-file + atomic rename. `id` is bumped only on a real
# advance, so a waiting re-emit never confuses the daemon's progress detector.
set -euo pipefail

ROOT=""
TIER=""
RISK_CLASS="medium"
while [ $# -gt 0 ]; do
  case "$1" in
    --project-root) ROOT="${2:-}"; shift 2 ;;
    --tier)         TIER="${2:-}"; shift 2 ;;
    --risk-class)   RISK_CLASS="${2:-medium}"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$ROOT" ] || { echo "missing --project-root" >&2; exit 2; }
command -v jq >/dev/null 2>&1 || { echo "jq not found" >&2; exit 4; }

PS="$ROOT/.project-state"
STEP="$PS/step.json"
STOPS="$PS/stops.json"
ANSWERS="$PS/answers.json"
DEC="$PS/decisions.md"
CUR="$PS/CURRENT.md"
mkdir -p "$PS"

NOW="$(date +%s)"
STOP_ID="stop-ref-01"

# write_atomic <dst>  (content on stdin)
write_atomic() {
  local dst="$1" tmp
  tmp="$(mktemp "$(dirname "$dst")/.tmp.XXXXXX")"
  cat > "$tmp"
  mv -f "$tmp" "$dst"
}

# write_step <id> <status> <next_action> <next_check-json>
write_step() {
  jq -n --argjson id "$1" --arg status "$2" --arg action "$3" \
        --argjson nc "$4" --argjson started "$NOW" \
        '{id:$id, status:$status, next_action:$action, next_check:$nc, started_at:$started}' \
    | write_atomic "$STEP"
}

status="fresh"
if [ -f "$STEP" ]; then
  status="$(jq -r '.status' "$STEP")"
fi

case "$status" in
  fresh)
    write_step 1 awaiting_next "advance to step 2" null
    printf 'posture: working\nstep 1 started (tier=%s)\n' "${TIER:-?}" | write_atomic "$CUR"
    ;;

  awaiting_next)
    existing="[]"; [ -f "$STOPS" ] && existing="$(cat "$STOPS")"
    if ! printf '%s' "$existing" | jq -e --arg id "$STOP_ID" 'any(.[]; .id==$id)' >/dev/null; then
      printf '%s' "$existing" | jq --arg id "$STOP_ID" --arg rc "$RISK_CLASS" \
        '. + [{id:$id, kind:"ambiguity", risk_class:$rc, question:"Which persistence path for the token store?", context_ref:"task-1", status:"awaiting_reply"}]' \
        | write_atomic "$STOPS"
    fi
    write_step 2 needs_you "await answer for $STOP_ID" null
    printf 'posture: needs_you\nstep 2 raised %s (risk=%s)\n' "$STOP_ID" "$RISK_CLASS" | write_atomic "$CUR"
    ;;

  needs_you)
    answered=false
    if [ -f "$ANSWERS" ]; then
      answered="$(jq -r --arg id "$STOP_ID" 'any(.[]; .stop_id==$id)' "$ANSWERS")"
    fi
    if [ "$answered" = "true" ]; then
      ans="$(jq -r --arg id "$STOP_ID" 'map(select(.stop_id==$id)) | last | .answer' "$ANSWERS")"
      by="$(jq -r --arg id "$STOP_ID" 'map(select(.stop_id==$id)) | last | .answered_by' "$ANSWERS")"
      existing="[]"; [ -f "$STOPS" ] && existing="$(cat "$STOPS")"
      printf '%s' "$existing" | jq --arg id "$STOP_ID" 'map(select(.id != $id))' | write_atomic "$STOPS"
      # Idempotent: only record the resolution once, so a retried step (orphan /
      # non-zero exit re-run) never appends a duplicate decision line.
      if ! grep -q "resolved $STOP_ID " "$DEC" 2>/dev/null; then
        printf 'Source: %s — resolved %s = %s\n' "$by" "$STOP_ID" "$ans" >> "$DEC"
      fi
      write_step 3 done "project complete" null
      printf 'posture: done\nstep 3 resolved %s\n' "$STOP_ID" | write_atomic "$CUR"
    else
      # still waiting: re-emit needs_you WITHOUT bumping id
      write_step 2 needs_you "await answer for $STOP_ID" null
    fi
    ;;

  done)
    : # idempotent no-op
    ;;

  *)
    echo "unknown status: $status" >&2
    exit 3
    ;;
esac
exit 0
