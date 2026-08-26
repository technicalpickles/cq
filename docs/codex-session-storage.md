# Codex session storage

Codex writes one JSONL rollout transcript per session under `~/.codex/sessions/`,
organized by date. cq discovers those files recursively; set
`CQ_CODEX_SESSIONS_DIR` to use a different root.

Each file combines session metadata with response items:

- `session_meta` supplies the session id and working directory.
- `response_item.message` supplies user and assistant messages.
- `response_item.function_call` and `response_item.custom_tool_call` supply tool calls.
- Their corresponding `*_output` records supply tool results, joined by `call_id`.
- `turn_context` supplies the current model for messages.

Codex rows are stored in cq's normal JSONL cache and exposed with
`harness = 'codex'`. They have no `source`, because `source` identifies only
Claude config roots. When cq runs inside Codex, it automatically filters normal
commands to `harness = 'codex'` and skips Claude's automatic source selection.
Use `--all` to span harnesses or `--harness codex` outside Codex. An explicit
`--source` selects Claude rows and cannot be combined with `--harness`.

Codex does not currently map hook events or collaboration/subagent state into
cq's Claude-oriented columns. Those views remain empty or `NULL` for Codex
until a stable mapping is established.

## Fixture capture

`tests/fixtures/codex_session.jsonl` is a redacted fixture derived from a
controlled `codex exec` session. The capture ran in a disposable directory
with a read-only sandbox and a prompt limited to `pwd` and fixed `printf`
output. The committed fixture preserves the observed record families,
including `event_msg`, developer messages, reasoning, custom tool calls, and
world state, while replacing IDs, timestamps, paths, instructions, and model
contents with safe values.

Capture a new fixture only in a disposable directory with fixed prompts and
outputs. Do not commit an unredacted session file: session metadata and
developer messages can contain local paths, configuration, or private context.
