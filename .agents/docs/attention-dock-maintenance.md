# Automatic attention dock maintenance

Read this before merging upstream Herdr into the maintained fork when the merge touches pane topology, workspace state, persistence, live handoff, agent lifecycle transitions, plugin context, or pane rendering.

## Contract to preserve

The attention dock is automatic server-owned coordination, not a designated workspace or persisted layout feature.

- One global queue prioritizes `blocked`, then unseen `done`, with FIFO ordering inside each class.
- A transition must remain `blocked` or `done` for 300 ms before it becomes eligible. Returning to `working` during that interval cancels it.
- The queue head is physically exchanged into a transient tiled split in the owner client's tab. The lowest-ID live shell client becomes the owner and remains owner until disconnect. Other clients' navigation does not choose the host. The real pane and PTY move; there is no mirrored terminal renderer.
- Placement is right of a single pane, below the right pane for two side-by-side panes, and right of the southeast pane otherwise.
- The split does not steal focus. A focused attention pane remains pinned while its entry is queued, so input cannot be redirected. `working` or agent exit removes the entry even while focused.
- Viewing or focusing does not acknowledge an entry. `prefix+o` restores and follows the focused attention pane home without resolving it.
- An agent already visible in the active tab is not duplicated.
- The border title is `WORKSPACE - <home workspace name>`. Workspace names and aggregate states remain attributed to canonical homes while panes are exchanged.
- Plugin actions use the source workspace, tab, pane, terminal, and public pane ID rather than the temporary host context.
- Snapshots and pane history describe canonical topology. Temporary panes, temporary public numbers, and displaced history never persist.
- Live handoff physically canonicalizes the exchange before pairing snapshot pane IDs with PTY file descriptors, then reconstructs blocked and unseen-done queue entries in the replacement server.

The physical exchange is shared server topology. All attached clients can observe it. A PTY has one geometry, so this is intentional: native wrapping and input behavior take priority over client-local projection.

## Ownership map

| Concern | Primary code |
| --- | --- |
| Queue, debounce, placement, exchange, restoration, pinning, canonical source context | `src/app/attention_dock.rs` |
| Transient split insertion/removal and public pane numbering | `src/workspace.rs` |
| Topology-changing actions that must undock first | `src/app/actions.rs`, `src/app/custom_commands.rs`, `src/app/api/` |
| Canonical snapshot and history cleanup | `src/persist/snapshot.rs` |
| Owner-client location and lifecycle synchronization | `src/server/headless/client_views.rs`, `notifications.rs`, `surface_interest.rs` |
| Live-handoff canonicalization and queue transfer | `src/server/headless/lifecycle.rs`, `bootstrap.rs`, `src/server/handoff.rs` |
| Source-aware plugin context | `src/app/api/plugins/context.rs`, `src/app/api/plugins/mod.rs` |
| Border title, source actions, and workspace presentation | `src/ui/panes.rs`, `src/client/shell/input.rs`, `mouse.rs`, `sidebar.rs`, `agent_sidebar.rs` |
| User documentation | `docs/next/website/src/content/docs/agents.mdx`, `session-state.mdx`, `quick-start.mdx` |

The manual `attention_dock.set` and `attention_dock.clear` API and context-menu actions were removed. Do not restore them when resolving an upstream conflict unless product behavior is intentionally being redesigned.

## Accepted 0.9.0 migration boundaries

- The old 0.8.2 exporter cannot send attention queue metadata. Its first handoff to this build may reset queued done items, ordering, and dismissals. Subsequent handoffs between patched builds preserve these facts through optional handoff-only metadata.
- Selecting a remote workspace in the sidebar does not activate its endpoint. Press Enter before rename, close, create, or worktree actions. Those actions are disabled for an inactive selection rather than dispatched to the active machine.
- A failed custom or overlay process launch can leave navigation at the canonical source workspace even though attention is redocked. This failure-path navigation change is accepted for this migration.

## Upstream merge procedure

1. Before merging, compare upstream changes in every ownership-map path and search new topology mutations:

   ```bash
   git diff HEAD..upstream/master -- src/app src/workspace.rs src/persist src/server src/ui
   rg -n 'split|move_pane|swap_panes|move_tab|move_workspace|apply_layout|zoom' src/app
   ```

2. For each new mutation that can change pane, tab, or workspace ownership, restore transient attention topology before the mutation. Use `prepare_attention_topology_mutation`, `prepare_attention_pane_mutation`, or the narrower existing seam rather than duplicating restoration logic.

3. Preserve stable identities across exchanges. Internal `PaneId`, terminal ownership, source public pane IDs, `next_public_pane_number`, focus references, zoom state, and tab/workspace indices must all return to their pre-dock meaning.

4. Review upstream snapshot and live-handoff changes together. Snapshot-only canonicalization is insufficient for handoff: the runtime pane-ID-to-FD map and snapshot must be captured from the same canonical topology.

5. Review agent lifecycle changes for both transition delivery and timer wakeups. Debounced entries require `next_attention_deadline` in interactive and headless loop scheduling, and due reconciliation in both scheduled-task paths.

6. Regenerate the API schema if upstream conflicts touch removed dock methods:

   ```bash
   just schema
   ```

7. Run focused checks, then the full fork gate:

   ```bash
   just test-one attention_dock
   just test-one live_handoff
   just test-one client_receives_notify_on_agent_state_change
   just test-one sidebar
   just ci
   just windows-lint
   python3.11 -m unittest \
     scripts.test_agent_detection_manifest_check \
     scripts.test_changelog \
     scripts.test_config_reference_check \
     scripts.test_docs_translation_parity \
     scripts.test_hermes_integration_asset \
     scripts.test_package_windows_conpty \
     scripts.test_preview \
     scripts.test_vendor_libghostty_vt \
     scripts.test_vendor_portable_pty
   ```

8. Deploy the checked fork binary and integration using the runtime deployment procedure in [`fork-maintenance.md`](fork-maintenance.md). A checkout build or passing test suite does not update the normal `~/.local/bin/herdr` runtime.

9. Live-test one blocked and one background-done transition against the installed binary. Verify native wrapping/input, focus pinning, return on `working`, queue advancement, `prefix+o`, Handy/plugin targeting, stable sidebar labels/states, zoom restoration, and a live handoff while an entry is projected.

The merge is complete only when snapshots are semantically identical with and without a transient placement, a handoff preserves PTY ownership and queued attention, no topology mutation can retain a displaced pane after attention state is lost, and the installed binary passes the live attention check.

## Upstream overlap and removal

If upstream adds an automatic attention surface, stop before combining implementations. Compare these contracts first: native PTY geometry, global priority/FIFO behavior, focus pinning, canonical persistence, public identity, plugin source context, multi-client semantics, and handoff. Adopt one implementation deliberately; do not retain two queue or projection systems.

The fork implementation can be removed when upstream provides equivalent behavior and the focused and live tests above pass without fork-specific attention code. Remove the feature as one coherent patch, including schema removals, documentation, topology hooks, snapshot canonicalization, and handoff reconstruction.
