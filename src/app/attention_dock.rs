#[cfg(unix)]
use crate::app::App;
use crate::{app::state::AppState, detect::AgentState, layout::PaneId, terminal::TerminalState};
use std::time::{Duration, Instant};
const ATTENTION_DEBOUNCE: Duration = Duration::from_millis(300);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttentionHandoffKind {
    Blocked,
    Done,
}
use AttentionHandoffKind as AttentionKind;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AttentionQueueItem {
    pub(crate) pane_id: PaneId,
    pub(crate) kind: AttentionHandoffKind,
    pub(crate) sequence: u64,
}
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttentionHandoffEntry {
    pub(crate) source_pane_id: String,
    pub(crate) kind: AttentionHandoffKind,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct AttentionHandoffState {
    pub(crate) queue: Vec<AttentionHandoffEntry>,
    pub(crate) dismissed_source_pane_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttentionEntry {
    pane_id: PaneId,
    kind: AttentionKind,
    sequence: u64,
    eligible_at: Instant,
    ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttentionSourceTarget {
    pub(crate) workspace_id: String,
    pub(crate) tab_number: usize,
    pub(crate) pane_id: PaneId,
    pub(crate) public_pane_id: String,
}

#[derive(Debug, Default)]
pub(crate) struct AttentionDockState {
    queue: Vec<AttentionEntry>,
    dismissed: std::collections::HashSet<PaneId>,
    next_sequence: u64,
}
impl AppState {
    #[cfg(any(unix, test))]
    pub(crate) fn rebuild_attention_queue_after_handoff(&mut self) {
        let dismissed = std::mem::take(&mut self.attention_dock.dismissed);
        let mut candidates = self
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.panes.iter())
            .filter_map(|(pane_id, pane)| {
                if dismissed.contains(pane_id) {
                    return None;
                }
                let terminal = self.terminals.get(&pane.attached_terminal_id)?;
                if !attention_eligible(terminal) {
                    return None;
                }
                let kind = match (terminal.state, pane.seen) {
                    (AgentState::Blocked, _) => AttentionKind::Blocked,
                    (AgentState::Idle, false) => AttentionKind::Done,
                    _ => return None,
                };
                Some((
                    *pane_id,
                    kind,
                    terminal.last_agent_state_change_seq.unwrap_or(0),
                ))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(_, _, sequence)| *sequence);

        self.attention_dock = AttentionDockState {
            dismissed,
            ..AttentionDockState::default()
        };
        let eligible_at = Instant::now() + ATTENTION_DEBOUNCE;
        for (pane_id, kind, _) in candidates {
            self.attention_dock.next_sequence += 1;
            self.attention_dock.queue.push(AttentionEntry {
                pane_id,
                kind,
                sequence: self.attention_dock.next_sequence,
                eligible_at,
                ready: false,
            });
        }
    }

    pub(crate) fn observe_attention_transition(
        &mut self,
        pane_id: PaneId,
        previous_state: AgentState,
        state: AgentState,
        seen: bool,
    ) {
        if state == AgentState::Working {
            self.attention_dock.dismissed.remove(&pane_id);
            self.remove_attention_entry(pane_id);
            return;
        }
        let terminal = self
            .workspaces
            .iter()
            .find_map(|workspace| self.terminals.get(workspace.terminal_id(pane_id)?));
        if terminal.is_some_and(|terminal| !attention_eligible(terminal)) {
            self.remove_attention_entry(pane_id);
            return;
        }
        if self.attention_dock.dismissed.contains(&pane_id) {
            return;
        }

        let kind = if state == AgentState::Blocked {
            Some(AttentionKind::Blocked)
        } else if state == AgentState::Idle && !seen && previous_state != AgentState::Idle {
            Some(AttentionKind::Done)
        } else {
            None
        };
        let Some(kind) = kind else {
            return;
        };

        if let Some(entry) = self
            .attention_dock
            .queue
            .iter_mut()
            .find(|entry| entry.pane_id == pane_id)
        {
            if kind == AttentionKind::Blocked && entry.kind != AttentionKind::Blocked {
                entry.kind = AttentionKind::Blocked;
                entry.eligible_at = Instant::now() + ATTENTION_DEBOUNCE;
                entry.ready = false;
            }
        } else {
            self.attention_dock.next_sequence += 1;
            self.attention_dock.queue.push(AttentionEntry {
                pane_id,
                kind,
                sequence: self.attention_dock.next_sequence,
                eligible_at: Instant::now() + ATTENTION_DEBOUNCE,
                ready: false,
            });
        }
    }

    pub(crate) fn attention_entries(&self) -> Vec<AttentionQueueItem> {
        let mut entries: Vec<_> = self
            .attention_dock
            .queue
            .iter()
            .filter(|entry| entry.ready)
            .map(|entry| AttentionQueueItem {
                pane_id: entry.pane_id,
                kind: entry.kind,
                sequence: entry.sequence,
            })
            .collect();
        entries.sort_by_key(|entry| {
            (
                match entry.kind {
                    AttentionKind::Blocked => 0,
                    AttentionKind::Done => 1,
                },
                entry.sequence,
            )
        });
        entries
    }
    pub(crate) fn attention_target(&self, pane_id: PaneId) -> Option<AttentionSourceTarget> {
        if !self
            .attention_dock
            .queue
            .iter()
            .any(|entry| entry.pane_id == pane_id && entry.ready)
        {
            return None;
        }
        let (ws_idx, tab_idx) = self.pane_location(pane_id)?;
        let workspace = &self.workspaces[ws_idx];
        Some(AttentionSourceTarget {
            workspace_id: workspace.id.clone(),
            tab_number: workspace.tabs[tab_idx].number,
            pane_id,
            public_pane_id: crate::workspace::public_pane_id_for_number(
                &workspace.id,
                *workspace.public_pane_numbers.get(&pane_id)?,
            ),
        })
    }
    pub(crate) fn dismiss_attention(&mut self, pane_id: PaneId) -> bool {
        if self.attention_target(pane_id).is_none() {
            return false;
        }
        self.remove_attention_entry(pane_id);
        self.attention_dock.dismissed.insert(pane_id);
        true
    }
    pub(crate) fn next_attention_deadline(&self) -> Option<Instant> {
        self.attention_dock
            .queue
            .iter()
            .filter(|entry| !entry.ready)
            .map(|entry| entry.eligible_at)
            .min()
    }
    pub(crate) fn reconcile_due_attention(&mut self, now: Instant) -> bool {
        if self.attention_dock.queue.is_empty() && self.attention_dock.dismissed.is_empty() {
            return false;
        }
        let before = self.attention_dock.queue.len();
        self.prune_attention_state();
        let mut changed = before != self.attention_dock.queue.len();
        for entry in &mut self.attention_dock.queue {
            if !entry.ready && entry.eligible_at <= now {
                entry.ready = true;
                changed = true;
            }
        }
        changed
    }
    pub(crate) fn remove_attention_entry(&mut self, pane_id: PaneId) {
        self.attention_dock
            .queue
            .retain(|entry| entry.pane_id != pane_id);
    }
    fn prune_attention_state(&mut self) {
        let eligible: std::collections::HashSet<_> = self
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .filter(|(_, pane)| {
                self.terminals
                    .get(&pane.attached_terminal_id)
                    .is_some_and(attention_eligible)
            })
            .map(|(id, _)| *id)
            .collect();
        self.attention_dock
            .queue
            .retain(|entry| eligible.contains(&entry.pane_id));
        self.attention_dock
            .dismissed
            .retain(|id| eligible.contains(id));
    }
    fn pane_location(&self, pane_id: PaneId) -> Option<(usize, usize)> {
        self.workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, workspace)| {
                workspace
                    .find_tab_index_for_pane(pane_id)
                    .map(|tab_idx| (ws_idx, tab_idx))
            })
    }

    #[cfg(test)]
    pub(crate) fn make_attention_ready_for_test(&mut self, pane_id: PaneId) {
        if let Some(entry) = self
            .attention_dock
            .queue
            .iter_mut()
            .find(|entry| entry.pane_id == pane_id)
        {
            entry.eligible_at = Instant::now();
            entry.ready = true;
        }
    }
    #[cfg(test)]
    pub(crate) fn assert_attention_dock_invariants_for_test(&self) {
        let mut queued = std::collections::HashSet::new();
        for entry in &self.attention_dock.queue {
            assert!(queued.insert(entry.pane_id));
            assert!(!self.attention_dock.dismissed.contains(&entry.pane_id));
            assert!(self.pane_location(entry.pane_id).is_some());
        }
    }
}
#[cfg(unix)]
impl App {
    pub(crate) fn attention_handoff_state(&self) -> AttentionHandoffState {
        let queue = self
            .state
            .attention_dock
            .queue
            .iter()
            .filter_map(|entry| {
                let (workspace_index, _) = self.find_pane(entry.pane_id)?;
                Some(AttentionHandoffEntry {
                    source_pane_id: self.public_pane_id(workspace_index, entry.pane_id)?,
                    kind: match entry.kind {
                        AttentionKind::Blocked => AttentionHandoffKind::Blocked,
                        AttentionKind::Done => AttentionHandoffKind::Done,
                    },
                })
            })
            .collect();
        let mut dismissed_source_pane_ids = self
            .state
            .attention_dock
            .dismissed
            .iter()
            .filter_map(|pane_id| {
                let (workspace_index, _) = self.find_pane(*pane_id)?;
                self.public_pane_id(workspace_index, *pane_id)
            })
            .collect::<Vec<_>>();
        dismissed_source_pane_ids.sort();
        AttentionHandoffState {
            queue,
            dismissed_source_pane_ids,
        }
    }

    pub(crate) fn restore_attention_handoff_state(&mut self, state: AttentionHandoffState) {
        let dismissed = state
            .dismissed_source_pane_ids
            .iter()
            .filter_map(|pane_id| self.parse_current_public_pane_id(pane_id))
            .map(|(_, pane_id)| pane_id)
            .collect::<std::collections::HashSet<_>>();
        let queue = state
            .queue
            .iter()
            .filter_map(|entry| {
                let (workspace_index, pane_id) =
                    self.parse_current_public_pane_id(&entry.source_pane_id)?;
                let terminal_id = self.state.workspaces[workspace_index].terminal_id(pane_id)?;
                if !attention_eligible(self.state.terminals.get(terminal_id)?) {
                    return None;
                }
                (!dismissed.contains(&pane_id)).then_some((workspace_index, pane_id, entry.kind))
            })
            .collect::<Vec<_>>();

        self.state.attention_dock.queue.clear();
        self.state.attention_dock.dismissed = dismissed;
        self.state.attention_dock.next_sequence = 0;
        let now = Instant::now();
        for (workspace_index, pane_id, kind) in queue {
            if self
                .state
                .attention_dock
                .queue
                .iter()
                .any(|entry| entry.pane_id == pane_id)
            {
                continue;
            }
            if kind == AttentionHandoffKind::Done {
                if let Some(pane) =
                    self.state
                        .workspaces
                        .get_mut(workspace_index)
                        .and_then(|workspace| {
                            workspace
                                .tabs
                                .iter_mut()
                                .find_map(|tab| tab.panes.get_mut(&pane_id))
                        })
                {
                    pane.seen = false;
                }
            }
            let sequence = self.state.attention_dock.next_sequence;
            self.state.attention_dock.next_sequence = sequence.saturating_add(1);
            self.state.attention_dock.queue.push(AttentionEntry {
                pane_id,
                kind: match kind {
                    AttentionHandoffKind::Blocked => AttentionKind::Blocked,
                    AttentionHandoffKind::Done => AttentionKind::Done,
                },
                sequence,
                eligible_at: now,
                ready: true,
            });
        }
    }
}

fn attention_eligible(terminal: &TerminalState) -> bool {
    // Treat agent.start Pi launches as controller-managed, including CLI launches.
    // Keep lifecycle reporting intact and suppress only attention queue participation.
    terminal.managed_agent_kind() != Some(crate::detect::Agent::Pi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Mode, workspace::Workspace};

    fn test_app() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn state_with_attention() -> (AppState, PaneId) {
        let home = Workspace::test_new("attention-home");
        let attention_pane = home.tabs[0].root_pane;
        let work = Workspace::test_new("work");
        let mut state = AppState::test_new();
        state.workspaces = vec![home, work];
        state.ensure_test_terminals();
        state.active = Some(1);
        state.selected = 1;
        state.mode = Mode::Terminal;
        state.observe_attention_transition(
            attention_pane,
            AgentState::Working,
            AgentState::Blocked,
            true,
        );
        state.attention_dock.queue[0].eligible_at = Instant::now();
        state.attention_dock.queue[0].ready = true;
        (state, attention_pane)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn serialized_handoff_restores_attention_fifo_kind_and_dismissal_into_fresh_app() {
        let homes = [
            Workspace::test_new("blocked"),
            Workspace::test_new("done"),
            Workspace::test_new("dismissed"),
            Workspace::test_new("host"),
        ];
        let blocked = homes[0].tabs[0].root_pane;
        let done = homes[1].tabs[0].root_pane;
        let dismissed = homes[2].tabs[0].root_pane;
        let mut source = test_app();
        source.state.workspaces = homes.into_iter().collect();
        source.state.ensure_test_terminals();
        source.state.active = Some(3);
        source.state.selected = 3;
        source.state.mode = Mode::Terminal;
        source.state.attention_dock.queue = vec![
            AttentionEntry {
                pane_id: blocked,
                kind: AttentionKind::Blocked,
                sequence: 7,
                eligible_at: Instant::now(),
                ready: true,
            },
            AttentionEntry {
                pane_id: done,
                kind: AttentionKind::Done,
                sequence: 8,
                eligible_at: Instant::now(),
                ready: true,
            },
        ];
        source.state.attention_dock.dismissed.insert(dismissed);
        source.state.workspaces[1].tabs[0]
            .panes
            .get_mut(&done)
            .expect("done pane")
            .seen = false;

        let attention = source.attention_handoff_state();
        let wire_attention = crate::server::handoff::HandoffAttentionState {
            queue: attention
                .queue
                .into_iter()
                .map(|entry| crate::server::handoff::HandoffAttentionEntry {
                    source_pane_id: entry.source_pane_id,
                    kind: match entry.kind {
                        AttentionHandoffKind::Blocked => {
                            crate::server::handoff::HandoffAttentionKind::Blocked
                        }
                        AttentionHandoffKind::Done => {
                            crate::server::handoff::HandoffAttentionKind::Done
                        }
                    },
                })
                .collect(),
            dismissed_source_pane_ids: attention.dismissed_source_pane_ids,
        };
        let snapshot = crate::persist::capture(
            &source.state.workspaces,
            &source.state.terminals,
            &source.terminal_runtimes,
            source.state.active,
            source.state.selected,
        );
        let manifest = crate::server::handoff::manifest_for(
            snapshot,
            Vec::new(),
            None,
            None,
            None,
            Some(wire_attention),
        );
        let encoded = serde_json::to_vec(&manifest).expect("serialize handoff manifest");
        let mut manifest: crate::server::handoff::HandoffManifest =
            serde_json::from_slice(&encoded).expect("deserialize handoff manifest");

        let mut config = crate::config::Config::default();
        config.terminal.default_shell = crate::app::exiting_test_command().to_owned();
        let mut fresh = test_app();
        let (workspaces, terminals, runtimes) = crate::persist::restore(
            &manifest.snapshot,
            None,
            24,
            80,
            config.advanced.scrollback_limit_bytes,
            &config.terminal.default_shell,
            config.terminal.shell_mode,
            false,
            fresh.event_tx.clone(),
            fresh.render_notify.clone(),
            fresh.render_dirty.clone(),
        );
        fresh.state.workspaces = workspaces;
        fresh.state.terminals = terminals;
        fresh.terminal_runtimes = runtimes.into();
        let restored = manifest.attention.take().expect("attention metadata");
        fresh.restore_attention_handoff_state(AttentionHandoffState {
            queue: restored
                .queue
                .into_iter()
                .map(|entry| AttentionHandoffEntry {
                    source_pane_id: entry.source_pane_id,
                    kind: match entry.kind {
                        crate::server::handoff::HandoffAttentionKind::Blocked => {
                            AttentionHandoffKind::Blocked
                        }
                        crate::server::handoff::HandoffAttentionKind::Done => {
                            AttentionHandoffKind::Done
                        }
                    },
                })
                .collect(),
            dismissed_source_pane_ids: restored.dismissed_source_pane_ids,
        });

        let restored = fresh.attention_handoff_state();
        assert_eq!(restored.queue.len(), 2);
        assert_eq!(restored.queue[0].kind, AttentionHandoffKind::Blocked);
        assert_eq!(restored.queue[1].kind, AttentionHandoffKind::Done);
        assert_eq!(restored.dismissed_source_pane_ids.len(), 1);
        let done_id = &restored.queue[1].source_pane_id;
        let (workspace_index, done_pane) = fresh
            .parse_current_public_pane_id(done_id)
            .expect("restored done pane");
        assert!(
            !fresh.state.workspaces[workspace_index]
                .tabs
                .iter()
                .find_map(|tab| tab.panes.get(&done_pane))
                .expect("restored done pane state")
                .seen
        );
        super::super::api::test_support::shutdown_test_runtimes(&mut fresh);
    }

    #[test]
    fn managed_pi_attention_is_suppressed_until_ownership_is_released() {
        for next in [AgentState::Blocked, AgentState::Idle] {
            let (mut state, pane) = state_with_attention();
            state.remove_attention_entry(pane);
            let terminal_id = state.workspaces[0].terminal_id(pane).unwrap().clone();
            state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .begin_managed_agent(
                    "worker".into(),
                    crate::detect::Agent::Pi,
                    Instant::now(),
                    Duration::ZERO,
                    Duration::from_secs(30),
                );
            state.observe_attention_transition(pane, AgentState::Working, next, false);
            assert!(state.attention_dock.queue.is_empty());

            state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .restore_managed_agent("worker".into(), crate::detect::Agent::Pi);
            state.observe_attention_transition(pane, AgentState::Working, next, false);
            assert!(state.attention_dock.queue.is_empty());

            state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .clear_agent_name();
            state.observe_attention_transition(pane, AgentState::Working, next, false);
            assert_eq!(state.attention_dock.queue.len(), 1);
            state.make_attention_ready_for_test(pane);
            state.reconcile_due_attention(Instant::now());
            assert_eq!(state.attention_entries()[0].pane_id, pane);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_handoff_queue_excludes_managed_pi_without_marking_it_unseen() {
        for kind in [AttentionHandoffKind::Blocked, AttentionHandoffKind::Done] {
            let mut app = test_app();
            let (state, pane) = state_with_attention();
            app.state = state;
            let terminal_id = app.state.workspaces[0].terminal_id(pane).unwrap().clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .restore_managed_agent("worker".into(), crate::detect::Agent::Pi);
            let ordinary = app.state.workspaces[1].tabs[0].root_pane;
            let ordinary_id = app.public_pane_id(1, ordinary).unwrap();

            app.restore_attention_handoff_state(AttentionHandoffState {
                queue: vec![
                    AttentionHandoffEntry {
                        source_pane_id: app.public_pane_id(0, pane).unwrap(),
                        kind,
                    },
                    AttentionHandoffEntry {
                        source_pane_id: ordinary_id.clone(),
                        kind,
                    },
                ],
                dismissed_source_pane_ids: vec![],
            });

            let restored = app.attention_handoff_state();
            assert_eq!(
                restored.queue,
                vec![AttentionHandoffEntry {
                    source_pane_id: ordinary_id,
                    kind,
                }]
            );
            assert!(app.state.workspaces[0].tabs[0].panes[&pane].seen);
        }
    }

    #[test]
    fn managed_non_pi_and_named_unmanaged_pi_keep_attention() {
        for managed in [false, true] {
            let (mut state, pane) = state_with_attention();
            state.remove_attention_entry(pane);
            let terminal_id = state.workspaces[0].terminal_id(pane).unwrap().clone();
            let terminal = state.terminals.get_mut(&terminal_id).unwrap();
            if managed {
                terminal.restore_managed_agent("worker".into(), crate::detect::Agent::Codex);
            } else {
                terminal.detected_agent = Some(crate::detect::Agent::Pi);
                terminal.set_agent_name("interactive".into());
            }
            for next in [AgentState::Blocked, AgentState::Idle] {
                state.remove_attention_entry(pane);
                state.observe_attention_transition(pane, AgentState::Working, next, false);
                assert_eq!(state.attention_dock.queue.len(), 1);
            }
        }
    }

    #[test]
    fn managed_pi_is_pruned_even_when_already_queued() {
        let (mut state, pane) = state_with_attention();
        let terminal_id = state.workspaces[0].terminal_id(pane).unwrap().clone();
        state.reconcile_due_attention(Instant::now());
        state.focus_pane_in_workspace(0, pane);
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .restore_managed_agent("worker".into(), crate::detect::Agent::Pi);

        state.reconcile_due_attention(Instant::now());

        assert!(state.attention_dock.queue.is_empty());
        assert!(state.attention_entries().is_empty());
        assert_eq!(state.pane_location(pane), Some((0, 0)));
        state.assert_invariants_for_test();
    }

    #[test]
    fn fallback_handoff_rebuilds_only_unseen_completions() {
        for seen in [false, true] {
            let (mut state, pane) = state_with_attention();
            let terminal_id = state.workspaces[0].terminal_id(pane).unwrap().clone();
            state.terminals.get_mut(&terminal_id).unwrap().state = AgentState::Idle;
            state.workspaces[0].tabs[0]
                .panes
                .get_mut(&pane)
                .unwrap()
                .seen = seen;

            state.rebuild_attention_queue_after_handoff();

            state.assert_invariants_for_test();
            if seen {
                assert!(state.attention_entries().is_empty());
                assert!(state.next_attention_deadline().is_none());
            } else {
                assert!(state.attention_entries().is_empty());
                let deadline = state.next_attention_deadline().expect("rebuilt debounce");
                assert!(state.reconcile_due_attention(deadline));
                let entries = state.attention_entries();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].pane_id, pane);
                assert_eq!(entries[0].kind, AttentionHandoffKind::Done);
            }
            assert_eq!(state.workspaces[0].tabs[0].panes[&pane].seen, seen);
            state.assert_invariants_for_test();
        }
    }

    #[test]
    fn fallback_handoff_preserves_dismissals_for_blocked_and_done() {
        for terminal_state in [AgentState::Blocked, AgentState::Idle] {
            let (mut state, dismissed) = state_with_attention();
            let terminal_id = state.workspaces[0].terminal_id(dismissed).unwrap().clone();
            state.terminals.get_mut(&terminal_id).unwrap().state = terminal_state;
            state.workspaces[0].tabs[0]
                .panes
                .get_mut(&dismissed)
                .unwrap()
                .seen = false;
            assert!(state.dismiss_attention(dismissed));
            let remaining = state.workspaces[1].tabs[0].root_pane;
            let remaining_terminal = state.workspaces[1].terminal_id(remaining).unwrap().clone();
            state.terminals.get_mut(&remaining_terminal).unwrap().state = AgentState::Blocked;

            state.rebuild_attention_queue_after_handoff();

            assert!(state.attention_dock.dismissed.contains(&dismissed));
            state.assert_invariants_for_test();
            let deadline = state
                .next_attention_deadline()
                .expect("remaining blocked entry");
            assert!(state.reconcile_due_attention(deadline));
            let entries = state.attention_entries();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].pane_id, remaining);
            assert_eq!(entries[0].kind, AttentionHandoffKind::Blocked);
            assert!(state.attention_target(dismissed).is_none());
            assert!(state.attention_dock.dismissed.contains(&dismissed));
            state.assert_invariants_for_test();
        }
    }

    #[test]
    fn handoff_does_not_requeue_managed_pi() {
        for next in [AgentState::Blocked, AgentState::Idle] {
            let (mut state, pane) = state_with_attention();
            let terminal_id = state.workspaces[0].terminal_id(pane).unwrap().clone();
            let terminal = state.terminals.get_mut(&terminal_id).unwrap();
            terminal.restore_managed_agent("worker".into(), crate::detect::Agent::Pi);
            terminal.state = next;
            state.workspaces[0].tabs[0]
                .panes
                .get_mut(&pane)
                .unwrap()
                .seen = false;

            state.rebuild_attention_queue_after_handoff();

            assert!(state.attention_dock.queue.is_empty());
            assert_eq!(state.terminals[&terminal_id].state, next);
            assert!(state.terminals[&terminal_id].managed_agent_interactive_ready());
        }
    }

    #[test]
    fn queue_debounce_priority_fifo_and_dismiss_rearm() {
        let (mut state, first) = state_with_attention();
        let second = state.workspaces[1].tabs[0].root_pane;
        state.remove_attention_entry(first);
        state.observe_attention_transition(first, AgentState::Working, AgentState::Idle, false);
        let deadline = state.next_attention_deadline().unwrap();
        assert!(state.attention_entries().is_empty());
        assert!(!state.reconcile_due_attention(deadline - Duration::from_millis(1)));
        assert!(state.reconcile_due_attention(deadline));
        state.observe_attention_transition(second, AgentState::Working, AgentState::Blocked, true);
        state.make_attention_ready_for_test(second);
        assert_eq!(
            state
                .attention_entries()
                .iter()
                .map(|e| e.pane_id)
                .collect::<Vec<_>>(),
            vec![second, first]
        );
        state.observe_attention_transition(first, AgentState::Idle, AgentState::Blocked, false);
        assert_eq!(state.attention_entries().len(), 1);
        state.make_attention_ready_for_test(first);
        assert_eq!(
            state
                .attention_entries()
                .iter()
                .map(|e| e.pane_id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(state.dismiss_attention(first));
        assert!(!state.dismiss_attention(first));
        state.observe_attention_transition(first, AgentState::Blocked, AgentState::Blocked, false);
        assert!(state.attention_target(first).is_none());
        state.observe_attention_transition(first, AgentState::Blocked, AgentState::Working, false);
        state.observe_attention_transition(first, AgentState::Working, AgentState::Idle, false);
        state.make_attention_ready_for_test(first);
        assert_eq!(state.attention_entries()[1].pane_id, first);
        state.observe_attention_transition(first, AgentState::Idle, AgentState::Working, true);
        assert!(state.attention_target(first).is_none());
        state.assert_invariants_for_test();
    }

    #[test]
    fn queue_does_not_change_topology_focus_identity_or_snapshots() {
        let mut state = AppState::test_with_adversarial_identity_state();
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let capture = |state: &AppState| {
            serde_json::to_value(crate::persist::capture(
                &state.workspaces,
                &state.terminals,
                &runtimes,
                state.active,
                state.selected,
            ))
            .unwrap()
        };
        let before = capture(&state);
        let panes: Vec<_> = state
            .workspaces
            .iter()
            .flat_map(|ws| &ws.tabs)
            .flat_map(|tab| tab.panes.keys().copied())
            .collect();
        for pane in panes {
            state.observe_attention_transition(
                pane,
                AgentState::Working,
                AgentState::Blocked,
                true,
            );
            state.make_attention_ready_for_test(pane);
            let target = state.attention_target(pane).unwrap();
            let (ws, tab) = state.pane_location(pane).unwrap();
            assert_eq!(target.workspace_id, state.workspaces[ws].id);
            assert_eq!(target.tab_number, state.workspaces[ws].tabs[tab].number);
            assert_eq!(
                target.public_pane_id,
                crate::workspace::public_pane_id_for_number(
                    &state.workspaces[ws].id,
                    state.workspaces[ws].public_pane_numbers[&pane]
                )
            );
            state.reconcile_due_attention(Instant::now());
            assert_eq!(capture(&state), before);
            assert!(state.dismiss_attention(pane));
            assert_eq!(capture(&state), before);
            state.assert_invariants_for_test();
        }
    }

    #[test]
    fn removed_panes_are_pruned_and_seen_done_stays_queued() {
        let (mut state, pane) = state_with_attention();
        state.attention_dock.queue[0].kind = AttentionKind::Done;
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane)
            .unwrap()
            .seen = true;
        state.reconcile_due_attention(Instant::now());
        assert!(state.attention_target(pane).is_some());
        state.workspaces.remove(0);
        assert!(state.reconcile_due_attention(Instant::now()));
        assert!(state.attention_entries().is_empty());
    }

    #[tokio::test]
    async fn targeted_actions_jump_without_ack_and_reject_stale_legacy_targets() {
        use crate::api::schema::{AttentionTarget, Method, Request};
        let (state, pane) = state_with_attention();
        let mut app = test_app();
        app.state = state;
        let target = AttentionTarget {
            source_pane_id: app.public_pane_id(0, pane).unwrap(),
        };
        for method in [
            Method::AttentionOpen(target.clone()),
            Method::AttentionDismiss(target.clone()),
        ] {
            let response = app.handle_api_request(Request {
                id: "legacy".into(),
                method,
            });
            let response: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(response["error"]["code"], "stale_target");
            assert_eq!(app.state.active, Some(1));
            assert!(app.state.attention_target(pane).is_some());
        }
        let response = app.handle_api_request(Request {
            id: "jump".into(),
            method: Method::AttentionJump(target.clone()),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(response.get("result").is_some());
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane));
        assert!(app.state.attention_target(pane).is_some());
        let response = app.handle_api_request(Request {
            id: "ack".into(),
            method: Method::AttentionAcknowledge(target.clone()),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(response.get("result").is_some());
        assert!(app.state.attention_entries().is_empty());
        let response = app.handle_api_request(Request {
            id: "stale".into(),
            method: Method::AttentionJump(target),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["error"]["code"], "stale_target");
        app.state.assert_invariants_for_test();
    }
}
