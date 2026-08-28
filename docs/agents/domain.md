# Domain docs

This is a single-context repository. Before exploring or changing the
codebase, engineering skills should read:

- `CONTEXT.md` at the repository root.
- ADRs in `docs/adr/` that touch the area being changed.

If either location does not exist, proceed silently. Domain-document producer
skills create these files only when the project resolves new terminology or
architectural decisions.

Use the glossary's vocabulary in issue titles, hypotheses, tests, and code.
Do not replace defined terms such as **Harness**, **Provider**, or **Source**
with synonyms that `CONTEXT.md` explicitly avoids.

If a proposed change conflicts with an accepted ADR, surface the conflict
instead of silently overriding the decision.
