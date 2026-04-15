# cq Use Cases

Real examples of insights discovered through cq that would be difficult or impossible to find otherwise.

## Skill Activation Gaps

**Question:** "How often does the `git:commit` skill actually fire when Claude commits?"

**Finding:** Out of 166 sessions that ran `git commit` in a 7-day window, only 16 (~9.6%) activated any commit skill. 152 sessions just ran git commit directly via Bash, bypassing the skill entirely.

**Query:**
```bash
cq sql "
WITH commit_calls AS (
  SELECT DISTINCT session_id FROM tool_calls
  WHERE name = 'Bash'
    AND json_extract_string(input, '$.command') LIKE '%git commit%'
),
skill_sessions AS (
  SELECT DISTINCT session_id FROM tool_calls
  WHERE name = 'Skill'
    AND json_extract_string(input, '$.skill') IN ('git:commit', 'commit', 'commit-commands:commit')
),
without_skill AS (
  SELECT c.session_id FROM commit_calls c
  LEFT JOIN skill_sessions s ON c.session_id = s.session_id
  WHERE s.session_id IS NULL
)
SELECT
  (SELECT count(*) FROM without_skill) as without_skill,
  (SELECT count(*) FROM skill_sessions) as with_skill,
  (SELECT count(*) FROM commit_calls) as total
" --since 7d --table
```

**Why it's hard without cq:** You'd need to manually read through session transcripts to correlate Skill tool calls with Bash tool calls across hundreds of sessions. No single log file contains both pieces of information together.

**Impact:** Created bean gt-5uxp to investigate. Led to hypothesis that Claude's built-in commit instructions compete with the skill, and that subagents (which may lack the skill list) account for many of the misses.

## Silent Skill Failures: Wrong File Paths

**Question:** "The writing-voice skill sometimes fails to find voice samples. When and why?"

**Finding:** The skill's instructions reference excerpt files without the `references/` subdirectory prefix. Claude tries `writing-voice/blog-excerpts.md`, gets an error, then self-corrects by Glob-searching and finding them at `writing-voice/references/blog-excerpts.md`. This wastes tool calls and tokens every time.

**Query:**
```bash
# Find sessions that invoked the skill
cq tools Skill --since 30d --grep writing-voice --table

# Then find Read errors in those sessions
cq sql "
SELECT tc.session_id, tc.name, tc.timestamp,
  left(tc.input::text, 250) as input_preview,
  tr.is_error
FROM tool_calls tc
JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
WHERE tc.session_id IN (
  SELECT DISTINCT session_id FROM tool_calls
  WHERE name = 'Skill' AND json_extract_string(input, '$.skill') = 'writing-voice'
)
AND tc.name IN ('Glob', 'Grep', 'Read')
AND (tc.input::text LIKE '%voice%' OR tc.input::text LIKE '%sample%' OR tc.input::text LIKE '%writing%')
ORDER BY tc.timestamp DESC
" --since 30d --table
```

**Why it's hard without cq:** The skill "works" from the user's perspective (Claude recovers), so there's no visible error. You'd only notice it by watching tool calls in real time, or by reading full session transcripts looking for error-then-retry patterns. Across 23 writing-voice sessions over 30 days, this pattern repeated silently.

**Impact:** Created bean gt-9cuu to fix the path references. A small fix that saves wasted tool calls on every invocation.

## Plugin Marketplace Usage Audit

**Question:** "Which of my pickled-claude-plugins skills are actually getting used?"

**Finding:** Aggregated all Skill tool invocations, cross-referenced against the actual plugin list from the marketplace repo, and found clear usage tiers:

- **Dominant:** `agent-meta:park` (140), `agent-meta:unpark` (36)
- **Solid:** `git:commit` (16)
- **Occasional:** `dev-tools:designing-clis` (3), `second-brain:obsidian` (3), `git:checkout` (3), `git:pull-request` (3), `buildkite:*` (4 total)
- **Never fired (7d):** `agent-meta:snapshot`, `git:triage`, `git:update`, `git:inbox`, `sandbox-first`, most `second-brain:*` variants, `dev-tools:finding-api-docs/hk/working-with-mise`, `stay-on-target:scope-handoffs`, `working-in-monorepos`, `mcpproxy`

**Query:**
```bash
cq sql "
SELECT json_extract_string(input, '$.skill') as skill, count(*) as invocations
FROM tool_calls
WHERE name = 'Skill'
  AND json_extract_string(input, '$.skill') IN (
    'agent-meta:park', 'agent-meta:unpark', 'agent-meta:snapshot',
    -- ... full list of marketplace skills
  )
GROUP BY skill
ORDER BY invocations DESC
" --since 7d --table
```

**Why it's hard without cq:** The skill list spans 30+ skills across 13 plugins. There's no built-in analytics for "which skills fire." You'd need to grep through every session transcript and manually tally invocations, then cross-reference against the plugin repo to filter out skills from other sources (superpowers, local skills, etc.).

**Impact:** Revealed that most plugin value concentrates in 3-4 skills. Skills that never fire might have description/triggering issues, or might just not match current workflows.

## Context Budget Analysis: Which Tool Calls Burn the Most Tokens?

**Question:** "In a long session using `pup` (Datadog CLI), which calls added the most context to the conversation?"

**Finding:** In a session investigating production latency (58 `pup` calls total), the top 3 calls alone dumped ~47k characters into context. Two were trace searches piped through jq, one was a metrics query. Meanwhile, the bottom 30+ calls returned essentially nothing useful (31 chars each, empty responses or errors). The session was spending most of its context budget on a handful of large trace dumps while repeatedly retrying queries that returned nothing.

**Query:**
```bash
cq sql "
SELECT
  tc.tool_use_id,
  json_extract_string(tc.input, '$.command') AS command,
  length(tr.content) AS result_length
FROM tool_calls tc
JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
WHERE tc.session_id = '<SESSION_ID>'
  AND tc.name = 'Bash'
  AND json_extract_string(tc.input, '$.command') LIKE '%pup%'
ORDER BY result_length DESC
" --all
```

**Variations:**
```bash
# All Bash calls in a session, ranked by output size (not just pup)
cq sql "
SELECT
  json_extract_string(tc.input, '$.command') AS command,
  length(tr.content) AS result_chars,
  tr.is_error
FROM tool_calls tc
JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
WHERE tc.session_id = '<SESSION_ID>'
  AND tc.name = 'Bash'
ORDER BY result_chars DESC
LIMIT 20
" --all

# All tool calls ranked by result size (Read, Grep, Bash, etc.)
cq sql "
SELECT
  tc.name AS tool,
  left(tc.input::text, 120) AS input_preview,
  length(tr.content) AS result_chars
FROM tool_calls tc
JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
WHERE tc.session_id = '<SESSION_ID>'
ORDER BY result_chars DESC
LIMIT 20
" --all
```

**Why it's hard without cq:** You can't see tool result sizes in the conversation UI. Long outputs get truncated visually, so you don't notice that one `pup traces search` dumped 16k chars while a dozen other calls returned nothing. Without joining `tool_calls` to `tool_results` and measuring `length(content)`, there's no way to audit where context budget actually went.

**Impact:** Identified that trace search commands need more aggressive filtering (tighter jq selectors, `| head`, or narrower Datadog queries) to avoid flooding context. Also revealed a pattern of retrying queries that consistently return empty results, suggesting the session needed to stop and reassess its approach rather than keep trying variants.
