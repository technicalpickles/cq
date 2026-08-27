# cq Use Cases

Real investigations, real findings. Each of these surfaced something that would have been difficult or impossible to spot without querying the session logs directly.

## Find a Session When You Only Remember the Topic

*You remember discussing how to review a document as a new reader, but not
whether you called it a “cold read,” an “outsider review,” or something else.*

```bash
cq search "cold reading unfamiliar vocabulary" --type user --all
```

`cq search` stems the query terms, finds matching message text, and ranks it by
BM25 relevance. Add `--type user` when you specifically want prompts you wrote;
omit it to search assistant replies too. This is still lexical search, so it can
bridge word forms such as “migrate” and “migration,” but not unrelated wording
that expresses the same concept.

## Skill Activation Gaps

You saw this one in the [README](../README.md). Here's the full picture.

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

Out of 166 sessions that ran `git commit` in a 7-day window, only 16 activated any commit skill. The rest went straight through Bash. Digging further, subagents (which may not have the skill list in their context) accounted for many of the misses. Claude's built-in commit instructions also compete with the skill, so even in main sessions, the skill gets skipped more often than you'd expect.

## The Silent Failure

*The writing-voice skill works. Mostly. But sometimes Claude burns three or four extra tool calls finding the reference files. It's not broken, it just feels... slow.*

```bash
cq tools Skill --since 30d --grep writing-voice --table
```

*23 sessions in the last month. OK. Let's look at what tool calls those sessions made around the voice files.*

```bash
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

*There it is. Read fails, then Glob searches, then Read succeeds at a different path. Every single time.*

**The skill instructions referenced the wrong path.** They pointed to `writing-voice/blog-excerpts.md` instead of `writing-voice/references/blog-excerpts.md`. Claude recovered every time by Glob-searching for the file, so from the outside everything looked fine. Across 23 sessions over 30 days, the same silent failure repeated.

One-line fix. Saved wasted tool calls on every invocation.

## The Audit

*I've built 30+ skills across 13 plugins. Which ones actually get used?*

```bash
cq sql "
SELECT json_extract_string(input, '$.skill') as skill, count(*) as invocations
FROM tool_calls
WHERE name = 'Skill'
GROUP BY skill
ORDER BY invocations DESC
" --since 7d --table
```

Clear tiers emerged:

- **Dominant:** `agent-meta:park` (140 invocations), `agent-meta:unpark` (36)
- **Solid:** `git:commit` (16)
- **Occasional:** `dev-tools:designing-clis` (3), `second-brain:obsidian` (3), `git:checkout` (3), `git:pull-request` (3), `buildkite:*` (4 total)
- **Never fired (7d):** `agent-meta:snapshot`, `git:triage`, `git:update`, `git:inbox`, `sandbox-first`, most `second-brain:*` variants, `dev-tools:finding-api-docs`, `stay-on-target:scope-handoffs`

**Most of the value concentrates in 3 or 4 skills. Seven never fired once in a week.**

Skills that never fire might have description or triggering issues, or they just don't match current workflows. Either way, you can't fix what you can't see.

## Where Did the Context Go?

*This Datadog session ran 58 pup commands investigating production latency. It still didn't find the issue. Something went wrong, but looking at the conversation, every individual step seemed reasonable. What happened?*

```bash
cq sql "
SELECT
  json_extract_string(tc.input, '$.command') AS command,
  length(tr.content) AS result_chars,
  tr.is_error
FROM tool_calls tc
JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
WHERE tc.session_id = '<SESSION_ID>'
  AND tc.name = 'Bash'
  AND json_extract_string(tc.input, '$.command') LIKE '%pup%'
ORDER BY result_chars DESC
" --all
```

*The top three calls dumped 47k characters into context. The bottom thirty returned 31 characters each. Empty responses, every one.*

**Three calls ate the context budget. Thirty more burned it retrying queries that would never work.** The session was spending most of its context on a handful of large trace dumps while repeatedly running variations of queries that returned nothing. No single call looked wrong. The pattern only showed up when you ranked them all by output size.

The fix was two-fold: tighter jq selectors on trace searches to avoid flooding context, and recognizing when a session is stuck in a retry loop on empty results.

You can run this same analysis on any session:

```bash
# All tool calls ranked by result size
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

## Trace what happened around a tool call

```bash
cq tools Read --grep '/etc/passwd' -C 2
```

Show the Read call plus two messages before and after. Useful for debugging why a tool was called, what context the agent had, and what it did with the result.

## What's Actually Getting Injected at Session Start

Every plugin that hooks `SessionStart` bundles its context into the same record. Rank them by size to see which ones are eating your budget before the conversation even begins.

```bash
cq sql "
SELECT hook_name, content_size
FROM hook_events
WHERE attachment_type = 'hook_additional_context'
ORDER BY content_size DESC
"
```
