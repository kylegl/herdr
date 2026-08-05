# Issue tracker: GitHub

Issues and PRDs for this repo live as GitHub issues. Use the `gh` CLI for all operations.

## Conventions

- **Create an issue**: `gh issue create --title "..." --body "..."`.
- **Read an issue**: `gh issue view <number> --comments`.
- **List issues**: `gh issue list --state open` with appropriate label filters.
- **Comment on an issue**: `gh issue comment <number> --body "..."`.
- **Apply or remove labels**: use `gh issue edit`.
- **Close an issue**: `gh issue close <number>`.

Infer the repository from `git remote -v`. GitHub Issues are canonical; do not mirror task state locally.

## Pull requests as a triage surface

**PRs as a request surface: no.**

Pull requests are not included in issue discovery or triage unless this setting is deliberately changed.
