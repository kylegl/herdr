# kylegl/herdr fork maintenance

This repository is the maintained fork used to extend Herdr's official Pi
lifecycle integration with a package-neutral `herdr:busy` sibling-event overlay.
The integration remains part of the Herdr source tree and is installed by the
Herdr binary.

## Repository authority

| Role | Repository | Local remote | Writes |
| --- | --- | --- | --- |
| Maintained fork | `kylegl/herdr` | `origin` | Allowed |
| Original project | `herdrdev/herdr` | `upstream` | Never from this workflow |

Expected local configuration:

```text
origin    git@github.com:kylegl/herdr.git
upstream  https://github.com/herdrdev/herdr.git
upstream push URL: DISABLED
master tracks origin/master
```

Run this before edits, commits, merges, or pushes:

```bash
scripts/fork-preflight.sh
```

Do not change `origin` to the original project. Do not create a writable
`upstream` push URL. Do not use `--force` when pushing fork `master`.

## Branch policy

Fork `master` contains both upstream Herdr history and our maintained patch.
Normal work uses a short-lived task branch:

```bash
git switch master
git pull --ff-only origin master
git switch -c fork/<short-task-name>
```

Push task branches only to the fork:

```bash
git push -u origin fork/<short-task-name>
```

If a pull request is used, its base must be `kylegl/herdr:master`. Opening a PR,
issue, discussion, or branch against `herdrdev/herdr` is a separate upstream
contribution workflow and requires explicit user direction plus Herdr's
external-contributor process.

## Syncing from original Herdr

Never rebase published fork `master`. Merge original Herdr into it so the custom
patch remains visible and audit-friendly:

```bash
scripts/fork-preflight.sh
git switch master
git pull --ff-only origin master
git fetch upstream master
git merge upstream/master
just check
git push origin master
```

Resolve conflicts in favor of current upstream behavior while preserving the
busy overlay. Pay particular attention to:

- `src/integration/assets/pi/herdr-agent-state.ts`
- `src/integration/assets/herdr-agent-state.test.ts`
- `src/integration/mod.rs`
- Pi integration version constants and markers

After every sync, verify that the Pi integration still:

1. reports native foreground lifecycle and session identity;
2. consumes counted `herdr:busy` `{ active, label? }` sibling events;
3. aggregates `blocked`, foreground `working`, busy-overlay `working`, then `idle`;
4. remains inert outside an eligible Herdr-managed Pi TUI;
5. preserves socket ordering, retries, reload handling, and platform mapping.

If original Herdr adopts an equivalent busy overlay, stop before resolving the
overlap. Compare behavior and tests, then remove the fork patch deliberately
rather than carrying both implementations.

## Pi integration development

The official Pi integration source is:

```text
src/integration/assets/pi/herdr-agent-state.ts
```

Herdr embeds it at compile time through `include_str!`. Tests belong in the
existing asset harness:

```text
src/integration/assets/herdr-agent-state.test.ts
```

Keep the lifecycle authority package-neutral. It must not import or name
`pi-subagents`. A sibling extension may hold semantic working state by emitting
`herdr:busy` with `{ active: true, label? }` and must later balance that ownership
with one `{ active: false }`. Label changes should clear and reacquire the count.
The existing `herdr:blocked` overlay retains higher precedence.

### Patch provenance

The counted busy overlay is adapted from Magoz's public local Pi integration
patch:

- https://github.com/magoz/.dotfiles/blob/f0a2696ab7a905e4a98e0c2a3ffb31f900e6963c/pi/.pi/agent/extensions/herdr-agent-state.ts

The Herdr direction proposal and original explanation are recorded in Discussion
#1274, comment `discussioncomment-17868530`:

- https://github.com/herdrdev/herdr/discussions/1274#discussioncomment-17868530

`nicobailon/pi-subagents` later shipped the producer side as a forward-compatible
sibling event in PR #730. This fork carries only the generic Herdr consumer and
does not depend on or import that package.

Follow the integration-version rules in `AGENTS.md`. Run focused asset tests
while developing and `just check` before integrating changes into fork `master`.

## Building and installing the forked integration

The installed Pi extension comes from the Herdr binary that performs the
installation. Build or run the fork, then invoke that fork's binary explicitly:

```bash
cargo build
./target/debug/herdr integration install pi
herdr integration status
```

Using an upstream Herdr binary to reinstall the Pi integration replaces the
forked asset with upstream's version. After Herdr updates, confirm which binary
is active and reinstall from the maintained fork when necessary.

## Relationship to Pi extensions

The Herdr fork remains the sole socket lifecycle authority. Pi extensions may
publish the generic counted `herdr:busy` sibling event but must not ship copied
lifecycle-authority implementations. No Pi extension is a source dependency of
this fork patch.
