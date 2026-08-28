# Issue tracker: GitHub

Issues and product requirement documents (PRDs) for this repository live as
GitHub issues. Use the `gh` CLI for all operations.

## Conventions

- Create an issue with `gh issue create --title "..." --body "..."`.
- Read an issue with `gh issue view <number> --comments` and fetch its labels.
- List issues with `gh issue list`, using `--label`, `--state`, and JSON output
  as needed.
- Comment with `gh issue comment <number> --body "..."`.
- Apply or remove labels with `gh issue edit`.
- Close an issue with `gh issue close <number> --comment "..."`.

Infer the repository from `git remote -v`; `gh` does this automatically when
run inside the worktree.

When a skill says to publish to the issue tracker, create a GitHub issue. When
a skill says to fetch a ticket, read the corresponding GitHub issue and its
comments.
