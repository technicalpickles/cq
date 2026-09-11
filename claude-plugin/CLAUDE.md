# claude-plugin/

This directory is packaged and distributed as a Claude Code plugin (marketplace
installs point here, not at the repo root). It has its own version, independent
of the `cq` binary's version in `../Cargo.toml`.

## Bump the plugin version on every skill-file change

Any commit that changes `skills/cq/SKILL.md` must also bump `version` in
`.claude-plugin/plugin.json`, patch-level, in the same commit or an immediately
following one: `chore(plugin): bump version to X for skill doc update`. See
`git log -- claude-plugin/.claude-plugin/plugin.json` for the pattern.

**Why:** the marketplace/plugin install mechanism resolves and caches by this
version. A SKILL.md edit that lands without a bump is invisible to already-installed
copies of the plugin (`~/.claude/plugins/cache/pickled-claude-plugins/cq/<old-version>/`
keeps serving the stale skill text), so the fix or doc update silently doesn't
reach anyone until some unrelated change finally bumps it.

This applies to *behavior* changes to the skill's instructions: new flags,
changed output shape, new gotchas, corrected guidance. Typo fixes with no
change in meaning don't need a bump.

`README.md` in this directory is not skill-instruction content served to an
agent mid-session, so changing it alone doesn't require a bump.
