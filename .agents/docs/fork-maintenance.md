# kylegl/herdr fork maintenance

This repository is the maintained fork used to extend Herdr's official Pi
lifecycle integration with a package-neutral `herdr:busy` sibling-event overlay
and explicit blocked-state tracking for Pi's interactive `ask` tool. The
integration remains part of the Herdr source tree and is installed by the Herdr
binary.

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

### Local checkout layout

On this workstation, `/home/linkdevk/repos/herdr` is a worktree container, not
a source checkout. Its shared Git directory is `.bare`, and the maintained fork
checkout is:

```text
/home/linkdevk/repos/herdr/master
```

Run fork builds, maintenance commands, and Pi integration development from that
`master` worktree. Normal Herdr startup uses the standalone executable at
`~/.local/bin/herdr`; it does not execute from the checkout.

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

### Integrating fork-only work

Sync fork `master` from upstream before integrating a task branch. After the
sync passes `just check` and is pushed to `origin`, rebase the unpublished task
branch onto the updated fork `master`, rerun `just check`, and push the task
branch only to `origin`:

```bash
# In the shared master worktree, follow "Syncing from original Herdr" below.

# In the task worktree:
git rebase master
just check
git push -u origin fork/<short-task-name>
```

Open a pull request against `kylegl/herdr:master` and prefer squash merge for
fork-only features. This leaves one clear, revertible patch commit on fork
`master` while upstream history continues to arrive through merge commits.
Never rebase or force-push published fork `master`.

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
3. reports an active `ask` tool call as `blocked` until that exact call finishes;
4. aggregates explicit and Ask-derived `blocked`, foreground `working`,
   busy-overlay `working`, then `idle`;
5. remains inert outside an eligible Herdr-managed Pi TUI;
6. preserves socket ordering, retries, reload handling, and platform mapping.

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

The interactive tool named `ask` is a special case because it waits for Operator
input without publishing a Herdr event. Classify it from Pi's documented
`tool_execution_start` and `tool_execution_end` lifecycle events inside the
Herdr adapter rather than adding Herdr-specific behavior to the Ask extension.
Track active calls by `toolCallId`, not a counter, so duplicate completion events
and concurrent questions cannot clear blocked state early. Keep the displayed
message generic (`Awaiting answer`) rather than coupling to Ask's argument
schema. This is a behavioral convention on the stable tool name, not a source or
package dependency.

### Patch provenance

The counted busy overlay is adapted from Magoz's public local Pi integration
patch:

- <https://github.com/magoz/.dotfiles/blob/f0a2696ab7a905e4a98e0c2a3ffb31f900e6963c/pi/.pi/agent/extensions/herdr-agent-state.ts>

The Herdr direction proposal and original explanation are recorded in
Discussion #1274, comment `discussioncomment-17868530`:

- <https://github.com/herdrdev/herdr/discussions/1274#discussioncomment-17868530>

`nicobailon/pi-subagents` later shipped the producer side as a forward-compatible
sibling event in PR #730. This fork carries only the generic Herdr consumer and
does not depend on or import that package.

Follow the integration-version rules in `AGENTS.md`. Run focused asset tests
while developing and `just check` before integrating changes into fork `master`.

## Runtime deployment model

A checked-out or pushed fork is not automatically used by the running Herdr
session. Keep these three artifacts distinct:

1. the Herdr server and ordinary CLI binary currently on `PATH`;
2. the fork binary used as the integration installer;
3. the managed Pi extension written to
   `~/.pi/agent/extensions/herdr-agent-state.ts`.

The running Herdr server may remain the normal upstream release because this
patch does not change the socket protocol. Only the installer must come from the
fork so it writes the version 9 Pi asset containing the `herdr:busy` listener
and Ask lifecycle tracking.
Build the fork from `/home/linkdevk/repos/herdr/master`. On this workstation,
install the resulting standalone executable to `~/.local/bin/herdr`, then use
that installed fork binary to refresh and inspect the managed Pi extension:

```bash
cd /home/linkdevk/repos/herdr/master
cargo build
install -m 755 target/debug/herdr ~/.local/bin/herdr
~/.local/bin/herdr integration install pi
~/.local/bin/herdr integration status
```

Confirm the deployed asset rather than assuming that a fork checkout or build is
active:

```bash
rg 'HERDR_INTEGRATION_VERSION=9|herdr:busy|tool_execution_start' \
  ~/.pi/agent/extensions/herdr-agent-state.ts
```

An upstream Herdr binary embeds its own Pi asset. Running
`herdr integration install pi` through that binary can replace the forked version
with the upstream version, even while the server itself remains compatible.
After Herdr updates or integration reinstalls, inspect the managed extension and
reinstall it explicitly from the maintained fork when necessary.

### Coordinated Pi cutover

The managed Herdr extension and any package-owned lifecycle authority must not
be active in the same Pi process. Installing the file does not alter an already
running Pi extension runtime, so prepare all changes before one restart:

1. build the maintained fork;
2. install the managed Pi extension with `./target/debug/herdr`;
3. disable or remove the old package-owned lifecycle authority without reloading
   Pi in between;
4. restart Pi once;
5. verify that detached work remains `working`, blocked state takes precedence,
   and completion returns the pane to idle.

## Relationship to Pi extensions

The Herdr fork remains the sole socket lifecycle authority. Pi extensions may
publish the generic counted `herdr:busy` sibling event but must not ship copied
lifecycle-authority implementations. No Pi extension is a source dependency of
this fork patch; Ask support relies only on Pi's public tool lifecycle and the
registered tool name.
