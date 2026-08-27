#!/usr/bin/env bash
# Backfill lore (https://lorevcs.com) intent history for this project from
# cq's own indexed session transcripts. Distills each session's
# conversation into durable ask/decide pairs (lore's own
# AGENTS convention: record what was asked and the decision made, skip
# one-off asks and noise), then commits one lore commit per session.
#
# Idempotent: skips any session already referenced in `lore log`, so it's
# safe to rerun on this machine or point at a different machine's session
# history later.
#
# Must be run from a lore repository (`lore init` first) that isn't a
# throwaway worktree -- .lore isn't git-tracked, so a worktree removal
# deletes it.
#
# Usage:
#   scripts/backfill-lore.sh                # process all unprocessed sessions
#   scripts/backfill-lore.sh --limit 10     # pilot run: first N unprocessed sessions
#   scripts/backfill-lore.sh --dry-run      # print what would be staged/committed
#   scripts/backfill-lore.sh --chunk-size 60

set -euo pipefail

CHUNK_SIZE=40
LIMIT=0
DRY_RUN=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --limit) LIMIT="$2"; shift 2 ;;
    --chunk-size) CHUNK_SIZE="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

for bin in cq lore claude jq; do
  command -v "$bin" >/dev/null || { echo "error: '$bin' not found on PATH" >&2; exit 1; }
done

lore status >/dev/null 2>&1 || {
  echo "error: not a lore repository here (run 'lore init' first)" >&2
  exit 1
}

distill_chunk() {
  local chunk_json="$1"
  local prompt
  prompt=$(cat <<PROMPT
You are extracting durable project decisions from one chunk of a past
terminal session. The session's working directory was cq's repo (a Rust
CLI for querying Claude Code session transcripts via DuckDB), but that
does NOT mean the conversation is about cq -- people leave terminals
open in one repo while discussing something else entirely (another
tool, another project, an unrelated config file). If this chunk is not
actually about cq's own design, code, or workflow, output an empty
array. Do not extract decisions about any other project or tool, even
if they look decision-shaped.

For each DURABLE decision about cq itself recorded in this chunk -- a
rule about how cq should work going forward -- output one object with
"ask" (what was asked, distilled to one sentence) and "decide" (the
decision made and why, one sentence). Skip one-off bug reports, typos,
false starts, and anything not durable.

Output ONLY a JSON array of such objects (an empty array if there are
none, including if this chunk isn't about cq). No markdown code fences,
no other text before or after the JSON.

Conversation chunk (JSON messages, each with "type" and "text"):
$chunk_json
PROMPT
)
  claude -p "$prompt" --output-format text </dev/null 2>/dev/null | sed -e '/^```/d'
}

processed=0

while IFS= read -r session_id; do
  [[ -z "$session_id" ]] && continue

  if [[ "$LIMIT" -gt 0 && "$processed" -ge "$LIMIT" ]]; then
    break
  fi

  if lore log 2>/dev/null | grep -q "session $session_id"; then
    continue
  fi

  # Fetch once, filter out tool-only turns (text: null/""), then chunk
  # locally -- cq's --limit/--offset paginate raw rows, and a large
  # fraction of rows are text-less tool-use/tool-result turns that would
  # otherwise dilute every chunk with noise.
  all_messages=$(cq messages --session "$session_id" --fields type,text \
    --json --no-reindex --limit 0 2>/dev/null \
    | jq '[.[] | select(.text != null and .text != "")]')

  message_count=$(printf '%s\n' "$all_messages" | jq 'length')

  if [[ "$message_count" -eq 0 ]]; then
    processed=$((processed + 1))
    continue
  fi

  echo "session $session_id ($message_count non-empty messages)" >&2

  staged_any=0
  offset=0
  while [[ "$offset" -lt "$message_count" ]]; do
    chunk=$(printf '%s\n' "$all_messages" | jq ".[$offset:$((offset + CHUNK_SIZE))]")

    pairs=$(distill_chunk "$chunk")

    if printf '%s\n' "$pairs" | jq -e 'type == "array"' >/dev/null 2>&1; then
      count=$(printf '%s\n' "$pairs" | jq 'length')
      for ((i = 0; i < count; i++)); do
        ask=$(printf '%s\n' "$pairs" | jq -r ".[$i].ask")
        decide=$(printf '%s\n' "$pairs" | jq -r ".[$i].decide")
        [[ -z "$ask" || "$ask" == "null" ]] && continue
        if [[ "$DRY_RUN" -eq 1 ]]; then
          echo "  ASK:    $ask"
          echo "  DECIDE: $decide"
        else
          lore add "$ask" >/dev/null
          lore add "$decide" >/dev/null
        fi
        staged_any=1
      done
    else
      echo "  warning: chunk at offset $offset did not yield a JSON array, skipping" >&2
    fi

    offset=$((offset + CHUNK_SIZE))
  done

  if [[ "$staged_any" -eq 1 ]]; then
    started_at=$(cq sessions --session "$session_id" --fields started_at \
      --json --no-reindex 2>/dev/null | jq -r '.[0].started_at')
    if [[ "$DRY_RUN" -eq 1 ]]; then
      echo "  (dry-run: would commit session $session_id, started $started_at)"
    else
      lore commit -m "backfill: session $session_id ($started_at)"
    fi
  fi

  processed=$((processed + 1))
done < <(cq sessions --fields session_id,started_at --json --no-reindex --limit 0 \
  | jq -r 'sort_by(.started_at) | .[].session_id')

echo "processed $processed session(s)" >&2
