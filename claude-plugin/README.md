# cq Claude Code plugin

Teaches Claude Code when and how to query your session history with `cq`.

Installed through the [`pickled-claude-plugins`](https://github.com/technicalpickles/pickled-claude-plugins)
marketplace, which sparse-clones this directory via a `git-subdir` source.

## Install

The `cq` CLI needs to be on your PATH first. See the [top-level README](../README.md#install).

Then add the marketplace (if you haven't already) and install the plugin:

```bash
claude plugin marketplace add technicalpickles/pickled-claude-plugins
claude plugin install cq@pickled-claude-plugins
```

Restart Claude Code so it picks up the skill.

## What you get

Once installed, Claude will reach for `cq` automatically when you ask things like:

- "What tools have I used today?"
- "Show me errors from the last week"
- "Which files have I been editing most?"
- "What was that cargo command I ran yesterday?"
- "How many sessions did I have about auth?"
- "Is my git-commit skill actually firing?"

You can also invoke it directly as a slash command: `/cq`.

Claude has the full cq subcommand and schema reference in the skill, so it
can write `cq sql` queries against the `sessions`, `messages`, `tool_calls`,
and `tool_results` views when the canned subcommands aren't enough.

## When it doesn't fire

Sometimes the skill doesn't kick in on its own. The question might not match
its description closely enough, or there's a competing skill that looks more
relevant. If Claude is running raw `cq` commands through `Bash` instead of
going through the skill, or is floundering with schema questions, try:

- Say "use cq" explicitly
- Invoke `/cq` as a slash command
- Phrase the question more concretely ("query the messages view for...")

If it consistently misses on a question shape it should catch, that's worth
an issue. The skill description can always use a tune.

## Contents

- `.claude-plugin/plugin.json`: manifest
- `skills/cq/SKILL.md`: the skill that teaches Claude when and how to use cq

## Updating the skill

Edit `skills/cq/SKILL.md` in place and open a PR against this repo. The
marketplace tracks `ref: "main"` today, so merged changes flow out to installs
on next update.
