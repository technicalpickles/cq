# cq Claude Code plugin

This directory is the Claude Code plugin source for `cq`. It's consumed by the
[`pickled-claude-plugins`](https://github.com/technicalpickles/pickled-claude-plugins)
marketplace via a `git-subdir` source pointing at `claude-plugin/` in this repo.

## Contents

- `.claude-plugin/plugin.json` — manifest
- `skills/cq/SKILL.md` — the skill that teaches Claude when and how to invoke `cq`

## Installing

Through the marketplace (preferred):

```bash
claude plugin install cq@pickled-claude-plugins
```

## Updating the skill

Edit `skills/cq/SKILL.md` in place and open a PR against this repo. The
marketplace tracks `ref: "main"` today, so merged changes flow out to installs
on next update.
