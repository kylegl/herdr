# Attention queue maintenance

Read this before merging upstream changes to agent lifecycle, pane identity, client rendering, persistence, or live handoff.

## Contract

- The server owns a topology-neutral queue. Blocked entries precede unseen done entries, with FIFO ordering within each class.
- New entries wait 300 ms. Working cancels an entry and rearms a dismissed pane. Agent exit removes its entry. Controller-managed Pi agents remain excluded.
- Queue arrival never creates a split, moves a pane, changes focus, changes zoom, or consumes a public pane number.
- Each client explicitly selects its own interactive attention view. New arrivals cannot redirect that selection. Closing or viewing does not acknowledge an entry.
- The selected terminal may be resized for the view. Its canonical workspace, tab, pane identity, and PTY ownership do not change. A PTY still has one geometry shared across clients.
- Acknowledge targets one current public pane ID. Jump navigates to the canonical source without acknowledging it. Legacy projection actions reject stale targets rather than substituting the queue head.
- Snapshots and history capture ordinary canonical topology directly. No attention exchange or snapshot canonicalization is needed.
- Live handoff transfers queue order, kinds, and dismissals through optional handoff metadata. Restoration resolves current public pane IDs and excludes managed Pi entries.

## Ownership

| Concern | Owner |
| --- | --- |
| Queue, debounce, dismissal, source resolution, handoff reconstruction | `src/app/attention_dock.rs` |
| Lifecycle delivery and timer scheduling | `src/app/actions.rs`, `src/app/runtime.rs`, `src/server/headless/` |
| Advertised endpoint methods and optional queue data | `src/protocol/endpoint.rs`, `src/api/`, `src/server/client_shell.rs` |
| Client selection, overlay input, and rendering | `src/client/shell/` |
| Canonical snapshot and history | `src/persist/snapshot.rs` |
| Handoff metadata and PTY transfer | `src/server/headless/lifecycle.rs`, `bootstrap.rs`, `src/server/handoff.rs` |

The module filename is historical. Do not restore physical docking, owner-client host election, transient placeholder panes, or mutation preparation wrappers. The dock-only layout ID replacement and transient workspace insertion helpers were removed too. Ordinary pane splits, moves, and swaps retain their existing topology primitives.

## Merge checks

1. Check lifecycle transition delivery and timer wakeups together. `next_attention_deadline` must include overdue entries until `reconcile_due_attention` marks them ready.
2. Check close paths remove queue entries. Pane and workspace moves must preserve internal pane identity, while source resolution uses the current canonical public ID.
3. Keep client selection out of `AppState`. Check that new queue items, closing an overlay, and navigation do not implicitly acknowledge entries.
4. Preserve published codecs. New client features use optional data and separately advertised methods, not changes to frozen generation-1 types.
   After API schema changes, regenerate the bundle with `HERDR_UPDATE_API_SCHEMA=1 just test-one generated_protocol_schema_artifact_is_current`.
5. Check handoff snapshots and descriptor maps retain the same canonical pane ownership. Older exporters without attention metadata may reset queued done entries, ordering, and dismissals on their first handoff.
6. Run focused tests and the aggregate fork checks:

   ```bash
   just test-one attention
   just test-one live_handoff
   just check
   ```

Validate native input and wrapping, independent client selection, stale-target rejection, close without acknowledgment, working removal, managed Pi exclusion, and handoff with an open view in an isolated session. Deploy only when explicitly requested, following `fork-maintenance.md`.

If upstream adds an attention surface, compare queue ordering, identity, geometry, client-local selection, API compatibility, and handoff before combining implementations. Adopt one owner rather than retaining competing queues.
