# Lore

cq records the intent behind its code using [lore](https://lorevcs.com), a version-control tool for prompts, notes, and decisions rather than diffs. Where git tracks committed code, lore tracks the asks and decisions that produced it — a separate, content-addressed history under `.lore/` (gitignored, like `.git/`). AGENTS.md has the short version of the day-to-day convention; this is the full reference.

## Recording intent

Each turn, record what was asked and the decision made:

```
lore add "what the user asked for"
lore add "the decision made and why"
```

Record durable decisions — rules for how cq should work going forward — not one-off asks, bug reports, or typos. It's cheap; do it often. Commit related intent once a unit of work lands:

```
lore commit -m "short summary"
```

Never record secrets. Repositories get shared and lore's history is permanent — paraphrase around a secret ("auth uses the token from the environment") instead of quoting it. Staged something sensitive? `lore reset` before committing. Already pushed? Rotate the secret; history won't forget it.

## Reading accumulated intent

```
lore log                    commit history, newest first
lore show <commit>          a commit and its recorded intent
lore grep "rate limit"      search intent, case-insensitively
lore materialize            render accumulated intent into a brief
```

`lore materialize` is the fastest way to catch up on *why* cq works the way it does, beyond what's written in this repo's docs.

## Branching and merging

Lore branches work like git branches, but merges never conflict — each entry is content-addressed by the hash of its text, so merging two branches just unions their intent:

```
lore checkout -b experiment
lore add "try an alternative approach"
lore commit -m "encoding experiment"
lore checkout main
lore merge experiment
```

Made a mistake? Objects are never deleted, so nothing is lost:

```
lore reset                         unstage everything
lore reset --to <commit>           point the branch elsewhere
lore amend -m "better summary"     fold staged intent into HEAD
lore rebase main                   replay this branch onto main
```

## Backfilling from session history

`scripts/backfill-lore.sh` distills cq's own indexed Claude Code session transcripts (queried via cq itself) into lore commits, for retroactively recording the intent behind history that predates lore adoption in this repo. It's idempotent (safe to rerun as more session history becomes available, e.g. on another machine) and checks that each chunk is actually about cq before extracting anything — a lot of sessions scoped to this repo's directory turn out to be about unrelated work. See the script's header comment for usage and flags.

## Remotes

lore syncs like git — remotes can be a filesystem path or a [lorehub](https://hub.lorevcs.com) URL:

```
lore config user.email you@example.com
lore remote add origin <path-or-url>
lore push                     send the current branch
lore clone <url> [dir]        copy a remote into a new directory
lore pull                     fetch, then merge
```

Pushing rewritten history is refused unless you force it (`lore push --force`), since remotes only fast-forward.

## Full reference

Storage internals, lorehub account/token setup, and everything else: https://lorevcs.com
