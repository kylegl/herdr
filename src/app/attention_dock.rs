use crate::{
    app::state::AppState,
    detect::AgentState,
    layout::{Node, PaneId, TileLayout},
    terminal::{TerminalId, TerminalRuntimeRegistry, TerminalState},
    workspace::{Tab, Workspace},
};
use ratatui::layout::{Direction, Rect};
use std::time::{Duration, Instant};

const ATTENTION_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttentionKind {
    Blocked,
    Done,
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
struct CrossWorkspaceIdentity {
    attention_home_number: usize,
    displaced_home_number: usize,
    attention_home_public_id: String,
    displaced_home_public_id: String,
    attention_home_legacy_id: String,
    displaced_home_legacy_id: String,
    attention_home_next_public_pane_number: usize,
    displaced_home_next_public_pane_number: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalAttentionExchange {
    pub(crate) attention_pane: PaneId,
    pub(crate) displaced_pane: PaneId,
    pub(crate) attention_home_number: Option<usize>,
    pub(crate) displaced_home_number: Option<usize>,
    pub(crate) transient_pane: Option<PaneId>,
    pub(crate) transient_home_next_public_pane_number: Option<usize>,
    pub(crate) attention_home_next_public_pane_number: Option<usize>,
    pub(crate) attention_home_workspace_id: Option<String>,
    pub(crate) attention_home_ws_idx: Option<usize>,
    pub(crate) attention_home_tab_idx: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DockPlacement {
    attention_pane: PaneId,
    displaced_pane: PaneId,
    transient_terminal: TerminalId,
    attention_home_workspace_id: String,
    attention_home_workspace_name: String,
    attention_home_ws_idx: usize,
    attention_home_tab_idx: usize,
    dock_workspace_id: String,
    dock_workspace_name: String,
    dock_tab_idx: usize,
    dock_was_zoomed: bool,
    dock_next_public_pane_number_before: usize,
    dock_focus_before_attention: PaneId,
    cross_workspace_identity: Option<CrossWorkspaceIdentity>,
}

#[derive(Debug, Default)]
pub(crate) struct AttentionDockState {
    queue: Vec<AttentionEntry>,
    placement: Option<DockPlacement>,
    presented_at_home: Option<PaneId>,
    reconcile_suspended: bool,
    next_sequence: u64,
}

impl AppState {
    pub(crate) fn observe_attention_transition(
        &mut self,
        pane_id: PaneId,
        previous_state: AgentState,
        state: AgentState,
        seen: bool,
    ) {
        if state == AgentState::Working {
            self.attention_dock
                .queue
                .retain(|entry| entry.pane_id != pane_id);
            if self.attention_dock.presented_at_home == Some(pane_id) {
                self.attention_dock.presented_at_home = None;
            }
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

    pub(crate) fn reconcile_attention_dock(&mut self) {
        if self.attention_dock.reconcile_suspended
            || self.mode == crate::app::state::Mode::ContextMenu
        {
            return;
        }
        self.prune_attention_state();

        if let Some(presented) = self.attention_dock.presented_at_home {
            let still_focused = self.current_pane_focus_target().is_some_and(|target| {
                target.pane_id == presented
                    && self
                        .active
                        .and_then(|idx| self.workspaces.get(idx))
                        .is_some_and(|workspace| workspace.id == target.workspace_id)
            });
            if still_focused {
                return;
            }
            self.attention_dock.presented_at_home = None;
        }

        if let Some(placement) = &self.attention_dock.placement {
            let pinned = self.active.is_some_and(|ws_idx| {
                ws_idx < self.workspaces.len()
                    && self.workspaces[ws_idx].id == placement.dock_workspace_id
                    && self.workspaces[ws_idx].active_tab_index() == placement.dock_tab_idx
                    && self.workspaces[ws_idx].focused_pane_id() == Some(placement.attention_pane)
            });
            if pinned {
                return;
            }
        }

        let desired = self.attention_head();
        if desired.is_some_and(|pane_id| self.pending_agent_notifications.contains_key(&pane_id)) {
            return;
        }
        let active_context = self.active.and_then(|ws_idx| {
            self.workspaces
                .get(ws_idx)
                .map(|workspace| (ws_idx, workspace.id.clone(), workspace.active_tab_index()))
        });
        let placement_matches = self
            .attention_dock
            .placement
            .as_ref()
            .is_some_and(|placement| {
                active_context
                    .as_ref()
                    .is_some_and(|(_, workspace_id, tab_idx)| {
                        Some(placement.attention_pane) == desired
                            && &placement.dock_workspace_id == workspace_id
                            && placement.dock_tab_idx == *tab_idx
                    })
            });
        if placement_matches {
            return;
        }
        self.undock_attention();

        let (Some(attention_pane), Some((active_ws_idx, dock_workspace_id, dock_tab_idx))) =
            (desired, active_context)
        else {
            return;
        };
        if self.pane_location(attention_pane) == Some((active_ws_idx, dock_tab_idx)) {
            self.mark_active_tab_seen();
            return;
        }
        let Some((attention_home_ws_idx, attention_home_tab_idx)) =
            self.pane_location(attention_pane)
        else {
            return;
        };
        let attention_home_workspace_id = self.workspaces[attention_home_ws_idx].id.clone();
        let attention_home_workspace_name =
            self.workspaces[attention_home_ws_idx].display_name_from_terminals(&self.terminals);
        let dock_workspace_name =
            self.workspaces[active_ws_idx].display_name_from_terminals(&self.terminals);
        let (anchor, direction, focused, dock_was_zoomed, cwd) = {
            let workspace = &self.workspaces[active_ws_idx];
            let tab = &workspace.tabs[dock_tab_idx];
            let Some((anchor, direction)) = automatic_dock_slot(&tab.layout) else {
                return;
            };
            (
                anchor,
                direction,
                tab.layout.focused(),
                tab.zoomed,
                workspace
                    .resolved_identity_cwd_from(
                        &self.terminals,
                        &crate::terminal::TerminalRuntimeRegistry::new(),
                    )
                    .unwrap_or_default(),
            )
        };
        let dock_next_public_pane_number_before =
            self.workspaces[active_ws_idx].next_public_pane_number;
        self.workspaces[active_ws_idx].tabs[dock_tab_idx].zoomed = false;
        let Some((dock_pane, transient_terminal)) =
            self.workspaces[active_ws_idx].insert_transient_pane(dock_tab_idx, anchor, direction)
        else {
            self.workspaces[active_ws_idx].tabs[dock_tab_idx].zoomed = dock_was_zoomed;
            return;
        };
        self.terminals.insert(
            transient_terminal.clone(),
            TerminalState::new(transient_terminal.clone(), cwd),
        );

        let Some(cross_workspace_identity) = self.dock_exchange(attention_pane, dock_pane) else {
            let _ = self.workspaces[active_ws_idx].remove_transient_pane(dock_pane);
            self.terminals.remove(&transient_terminal);
            self.workspaces[active_ws_idx].tabs[dock_tab_idx].zoomed = dock_was_zoomed;
            return;
        };
        self.attention_dock.placement = Some(DockPlacement {
            attention_pane,
            displaced_pane: dock_pane,
            transient_terminal,
            attention_home_workspace_id,
            attention_home_workspace_name,
            attention_home_ws_idx,
            attention_home_tab_idx,
            dock_workspace_id,
            dock_workspace_name,
            dock_tab_idx,
            dock_was_zoomed,
            dock_next_public_pane_number_before,
            dock_focus_before_attention: focused,
            cross_workspace_identity,
        });
    }

    pub(crate) fn open_docked_attention(&mut self) -> bool {
        let Some(placement) = self.attention_dock.placement.clone() else {
            return false;
        };
        let focused = self.current_pane_focus_target();
        if focused.as_ref().map(|target| target.pane_id) != Some(placement.attention_pane) {
            return false;
        }

        let attention_pane = placement.attention_pane;
        self.undock_attention();
        if let Some((dock_ws_idx, dock_tab_idx)) =
            self.pane_location(placement.dock_focus_before_attention)
        {
            if let Some(tab) = self
                .workspaces
                .get_mut(dock_ws_idx)
                .and_then(|workspace| workspace.tabs.get_mut(dock_tab_idx))
            {
                tab.layout.focus_pane(placement.dock_focus_before_attention);
            }
        }
        let Some((ws_idx, tab_idx)) = self.pane_location(attention_pane) else {
            return false;
        };
        self.attention_dock.reconcile_suspended = true;
        let switched = self.switch_workspace_tab(ws_idx, tab_idx);
        if switched {
            self.focus_pane_in_workspace(ws_idx, attention_pane);
        }
        self.attention_dock.reconcile_suspended = false;
        if !switched {
            return false;
        }
        self.attention_dock.presented_at_home = Some(attention_pane);
        true
    }

    pub(crate) fn attention_home_is_active_tab(&self, pane_id: PaneId) -> Option<bool> {
        let placement = self.attention_dock.placement.as_ref()?;
        if placement.attention_pane != pane_id {
            return None;
        }
        Some(
            self.active == Some(placement.attention_home_ws_idx)
                && self.workspaces[placement.attention_home_ws_idx].active_tab_index()
                    == placement.attention_home_tab_idx,
        )
    }

    pub(crate) fn focused_attention_source_context(
        &self,
    ) -> Option<(usize, usize, PaneId, String)> {
        let placement = self.attention_dock.placement.as_ref()?;
        let active_ws_idx = self.active?;
        if self.workspaces.get(active_ws_idx)?.focused_pane_id() != Some(placement.attention_pane) {
            return None;
        }
        let home_ws_idx = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == placement.attention_home_workspace_id)?;
        let public_pane_id = placement
            .cross_workspace_identity
            .as_ref()
            .map(|identity| identity.attention_home_public_id.clone())
            .or_else(|| {
                let workspace = self.workspaces.get(home_ws_idx)?;
                let number = workspace
                    .public_pane_numbers
                    .get(&placement.attention_pane)?;
                Some(crate::workspace::public_pane_id_for_number(
                    &workspace.id,
                    *number,
                ))
            })?;
        Some((
            home_ws_idx,
            placement.attention_home_tab_idx,
            placement.attention_pane,
            public_pane_id,
        ))
    }

    pub(crate) fn attention_dock_title_for_pane(&self, pane_id: PaneId) -> Option<String> {
        let placement = self.attention_dock.placement.as_ref()?;
        if placement.attention_pane != pane_id {
            return None;
        }
        Some(format!(
            "WORKSPACE - {}",
            placement.attention_home_workspace_name
        ))
    }

    pub(crate) fn workspace_display_name(&self, workspace: &Workspace) -> String {
        if let Some(name) = self.stable_attention_workspace_name(workspace) {
            return name.to_owned();
        }
        workspace.display_name_from_terminals(&self.terminals)
    }

    pub(crate) fn workspace_display_name_from(
        &self,
        workspace: &Workspace,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> String {
        if let Some(name) = self.stable_attention_workspace_name(workspace) {
            return name.to_owned();
        }
        workspace.display_name_from(&self.terminals, terminal_runtimes)
    }

    fn stable_attention_workspace_name<'a>(&'a self, workspace: &Workspace) -> Option<&'a str> {
        let placement = self.attention_dock.placement.as_ref()?;
        if workspace.id == placement.attention_home_workspace_id {
            return Some(&placement.attention_home_workspace_name);
        }
        if workspace.id == placement.dock_workspace_id {
            return Some(&placement.dock_workspace_name);
        }
        None
    }

    pub(crate) fn prepare_attention_topology_mutation(&mut self) {
        self.undock_attention();
    }

    pub(crate) fn prepare_attention_pane_move(&mut self, _pane_id: PaneId) {
        self.undock_attention();
    }

    pub(crate) fn prepare_attention_pane_mutation(&mut self, pane_id: PaneId) {
        if self
            .attention_dock
            .placement
            .as_ref()
            .is_some_and(|placement| {
                placement.attention_pane == pane_id || placement.displaced_pane == pane_id
            })
        {
            self.undock_attention();
        }
        self.attention_dock
            .queue
            .retain(|entry| entry.pane_id != pane_id);
        if self.attention_dock.presented_at_home == Some(pane_id) {
            self.attention_dock.presented_at_home = None;
        }
    }

    pub(crate) fn canonical_attention_exchange(&self) -> Option<CanonicalAttentionExchange> {
        self.attention_dock
            .placement
            .as_ref()
            .map(|placement| CanonicalAttentionExchange {
                attention_pane: placement.attention_pane,
                displaced_pane: placement.displaced_pane,
                attention_home_number: placement
                    .cross_workspace_identity
                    .as_ref()
                    .map(|identity| identity.attention_home_number),
                displaced_home_number: placement
                    .cross_workspace_identity
                    .as_ref()
                    .map(|identity| identity.displaced_home_number),
                transient_pane: Some(placement.displaced_pane),
                transient_home_next_public_pane_number: Some(
                    placement.dock_next_public_pane_number_before,
                ),
                attention_home_next_public_pane_number: placement
                    .cross_workspace_identity
                    .as_ref()
                    .map(|identity| identity.attention_home_next_public_pane_number),
                attention_home_workspace_id: Some(placement.attention_home_workspace_id.clone()),
                attention_home_ws_idx: Some(placement.attention_home_ws_idx),
                attention_home_tab_idx: Some(placement.attention_home_tab_idx),
            })
    }

    #[cfg(test)]
    pub(crate) fn assert_attention_dock_invariants_for_test(&self) {
        let mut queued = std::collections::HashSet::new();
        for entry in &self.attention_dock.queue {
            assert!(queued.insert(entry.pane_id), "attention pane queued twice");
            assert!(
                self.pane_location(entry.pane_id).is_some(),
                "queued attention pane is not live"
            );
        }
        if let Some(placement) = &self.attention_dock.placement {
            assert_ne!(placement.attention_pane, placement.displaced_pane);
            assert!(self.pane_location(placement.attention_pane).is_some());
            assert!(self.pane_location(placement.displaced_pane).is_some());
            assert!(self.terminals.contains_key(&placement.transient_terminal));
        }
    }

    pub(crate) fn next_attention_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        self.attention_dock
            .queue
            .iter()
            .filter_map(|entry| {
                (!entry.ready && entry.eligible_at > now).then_some(entry.eligible_at)
            })
            .min()
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

    pub(crate) fn reconcile_due_attention(&mut self, now: Instant) -> bool {
        let mut became_ready = false;
        for entry in &mut self.attention_dock.queue {
            if !entry.ready && entry.eligible_at <= now {
                entry.ready = true;
                became_ready = true;
            }
        }
        if became_ready {
            self.reconcile_attention_dock();
        }
        became_ready
    }

    fn attention_head(&self) -> Option<PaneId> {
        let now = Instant::now();
        self.attention_dock
            .queue
            .iter()
            .filter(|entry| entry.ready || entry.eligible_at <= now)
            .min_by_key(|entry| {
                let priority = match entry.kind {
                    AttentionKind::Blocked => 0,
                    AttentionKind::Done => 1,
                };
                (priority, entry.sequence)
            })
            .map(|entry| entry.pane_id)
    }

    fn prune_attention_state(&mut self) {
        let live_panes: std::collections::HashSet<_> = self
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.keys().copied())
            .collect();
        self.attention_dock
            .queue
            .retain(|entry| live_panes.contains(&entry.pane_id));
    }

    fn undock_attention(&mut self) {
        let Some(placement) = self.attention_dock.placement.take() else {
            return;
        };
        if !self.restore_dock_exchange(&placement) {
            self.attention_dock.placement = Some(placement);
            return;
        }
        if let Some((ws_idx, _)) = self.pane_location(placement.displaced_pane) {
            let removed_terminal =
                self.workspaces[ws_idx].remove_transient_pane(placement.displaced_pane);
            if let Some(terminal_id) = removed_terminal {
                self.terminals.remove(&terminal_id);
            }
            self.workspaces[ws_idx].next_public_pane_number =
                placement.dock_next_public_pane_number_before;
            if self.workspaces[ws_idx]
                .tabs
                .get(placement.dock_tab_idx)
                .is_some()
            {
                self.workspaces[ws_idx].tabs[placement.dock_tab_idx].zoomed =
                    placement.dock_was_zoomed;
            }
        }
        self.terminals.remove(&placement.transient_terminal);
        self.refresh_location_bound_references();
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

    fn dock_exchange(
        &mut self,
        attention_pane: PaneId,
        dock_pane: PaneId,
    ) -> Option<Option<CrossWorkspaceIdentity>> {
        let attention_location = self.pane_location(attention_pane)?;
        let dock_location = self.pane_location(dock_pane)?;
        if attention_location == dock_location {
            return self.workspaces[attention_location.0].tabs[attention_location.1]
                .layout
                .swap_panes(attention_pane, dock_pane)
                .then_some(None);
        }

        let cross_workspace_identity = if attention_location.0 != dock_location.0 {
            let attention_workspace = &self.workspaces[attention_location.0];
            let dock_workspace = &self.workspaces[dock_location.0];
            let attention_number = *attention_workspace
                .public_pane_numbers
                .get(&attention_pane)?;
            let dock_number = *dock_workspace.public_pane_numbers.get(&dock_pane)?;
            let attention_public_id = crate::workspace::public_pane_id_for_number(
                &attention_workspace.id,
                attention_number,
            );
            let dock_public_id =
                crate::workspace::public_pane_id_for_number(&dock_workspace.id, dock_number);
            Some(CrossWorkspaceIdentity {
                attention_home_number: attention_number,
                displaced_home_number: dock_number,
                attention_home_public_id: attention_public_id,
                displaced_home_public_id: dock_public_id,
                attention_home_legacy_id: format!(
                    "p_{}_{}",
                    attention_location.0 + 1,
                    attention_pane.raw()
                ),
                displaced_home_legacy_id: format!("p_{}_{}", dock_location.0 + 1, dock_pane.raw()),
                attention_home_next_public_pane_number: attention_workspace.next_public_pane_number,
                displaced_home_next_public_pane_number: dock_workspace.next_public_pane_number,
            })
        } else {
            None
        };

        if !exchange_panes_in_tabs(
            &mut self.workspaces,
            attention_location,
            attention_pane,
            dock_location,
            dock_pane,
        ) {
            return None;
        }
        if let Some(identity) = &cross_workspace_identity {
            self.assign_temporary_public_numbers(
                attention_location.0,
                attention_pane,
                dock_location.0,
                dock_pane,
                identity,
            );
        }
        self.refresh_location_bound_references();
        Some(cross_workspace_identity)
    }

    fn restore_dock_exchange(&mut self, placement: &DockPlacement) -> bool {
        let Some(attention_location) = self.pane_location(placement.attention_pane) else {
            return false;
        };
        let Some(displaced_location) = self.pane_location(placement.displaced_pane) else {
            return false;
        };
        if attention_location == displaced_location {
            return self.workspaces[attention_location.0].tabs[attention_location.1]
                .layout
                .swap_panes(placement.attention_pane, placement.displaced_pane);
        }
        if !exchange_panes_in_tabs(
            &mut self.workspaces,
            attention_location,
            placement.attention_pane,
            displaced_location,
            placement.displaced_pane,
        ) {
            return false;
        }
        if let Some(identity) = &placement.cross_workspace_identity {
            self.restore_public_numbers(
                displaced_location.0,
                placement.attention_pane,
                attention_location.0,
                placement.displaced_pane,
                identity,
            );
        }
        self.refresh_location_bound_references();
        true
    }

    fn refresh_location_bound_references(&mut self) {
        self.refresh_previous_pane_focus_location();
        let toast_pane = self
            .toast
            .as_ref()
            .and_then(|toast| toast.target.as_ref())
            .map(|target| target.pane_id);
        if let Some(pane_id) = toast_pane {
            let workspace_id = self.pane_location(pane_id).and_then(|(ws_idx, _)| {
                self.workspaces
                    .get(ws_idx)
                    .map(|workspace| workspace.id.clone())
            });
            if let (Some(target), Some(workspace_id)) = (
                self.toast.as_mut().and_then(|toast| toast.target.as_mut()),
                workspace_id,
            ) {
                target.workspace_id = workspace_id;
            }
        }
        let pending_locations = self
            .pending_agent_notifications
            .keys()
            .copied()
            .filter_map(|pane_id| {
                self.pane_location(pane_id).and_then(|(ws_idx, _)| {
                    self.workspaces
                        .get(ws_idx)
                        .map(|workspace| (pane_id, workspace.id.clone()))
                })
            })
            .collect::<Vec<_>>();
        for (pane_id, workspace_id) in pending_locations {
            if let Some(pending) = self.pending_agent_notifications.get_mut(&pane_id) {
                pending.workspace_id = workspace_id;
            }
        }
    }

    fn refresh_previous_pane_focus_location(&mut self) {
        let Some(pane_id) = self
            .previous_pane_focus
            .as_ref()
            .map(|target| target.pane_id)
        else {
            return;
        };
        let workspace_id = self.pane_location(pane_id).and_then(|(ws_idx, _)| {
            self.workspaces
                .get(ws_idx)
                .map(|workspace| workspace.id.clone())
        });
        if let (Some(target), Some(workspace_id)) =
            (self.previous_pane_focus.as_mut(), workspace_id)
        {
            target.workspace_id = workspace_id;
        }
    }

    fn assign_temporary_public_numbers(
        &mut self,
        attention_ws_idx: usize,
        attention_pane: PaneId,
        dock_ws_idx: usize,
        dock_pane: PaneId,
        identity: &CrossWorkspaceIdentity,
    ) {
        let (attention_workspace, dock_workspace) =
            two_workspaces_mut(&mut self.workspaces, attention_ws_idx, dock_ws_idx);
        attention_workspace
            .public_pane_numbers
            .remove(&attention_pane);
        dock_workspace.public_pane_numbers.remove(&dock_pane);

        let displaced_number = attention_workspace.next_public_pane_number;
        attention_workspace.next_public_pane_number += 1;
        attention_workspace
            .public_pane_numbers
            .insert(dock_pane, displaced_number);
        let attention_number = dock_workspace.next_public_pane_number;
        dock_workspace.next_public_pane_number += 1;
        dock_workspace
            .public_pane_numbers
            .insert(attention_pane, attention_number);

        self.public_pane_id_aliases
            .insert(identity.attention_home_public_id.clone(), attention_pane);
        self.public_pane_id_aliases
            .insert(identity.displaced_home_public_id.clone(), dock_pane);
        self.public_pane_id_aliases
            .insert(identity.attention_home_legacy_id.clone(), attention_pane);
        self.public_pane_id_aliases
            .insert(identity.displaced_home_legacy_id.clone(), dock_pane);
    }

    fn restore_public_numbers(
        &mut self,
        attention_home_ws_idx: usize,
        attention_pane: PaneId,
        dock_home_ws_idx: usize,
        dock_pane: PaneId,
        identity: &CrossWorkspaceIdentity,
    ) {
        let (attention_workspace, dock_workspace) = two_workspaces_mut(
            &mut self.workspaces,
            attention_home_ws_idx,
            dock_home_ws_idx,
        );
        attention_workspace.public_pane_numbers.remove(&dock_pane);
        dock_workspace.public_pane_numbers.remove(&attention_pane);
        attention_workspace
            .public_pane_numbers
            .insert(attention_pane, identity.attention_home_number);
        dock_workspace
            .public_pane_numbers
            .insert(dock_pane, identity.displaced_home_number);
        attention_workspace.next_public_pane_number =
            identity.attention_home_next_public_pane_number;
        dock_workspace.next_public_pane_number = identity.displaced_home_next_public_pane_number;
        self.public_pane_id_aliases
            .remove(&identity.attention_home_public_id);
        self.public_pane_id_aliases
            .remove(&identity.displaced_home_public_id);
        self.public_pane_id_aliases
            .remove(&identity.attention_home_legacy_id);
        self.public_pane_id_aliases
            .remove(&identity.displaced_home_legacy_id);
    }
}

fn exchange_panes_in_tabs(
    workspaces: &mut [crate::workspace::Workspace],
    first_location: (usize, usize),
    first_pane: PaneId,
    second_location: (usize, usize),
    second_pane: PaneId,
) -> bool {
    if first_location.0 == second_location.0 {
        let Some(workspace) = workspaces.get_mut(first_location.0) else {
            return false;
        };
        let (first_tab, second_tab) =
            two_tabs_mut(&mut workspace.tabs, first_location.1, second_location.1);
        return exchange_tab_panes(first_tab, first_pane, second_tab, second_pane);
    }

    let (first_workspace, second_workspace) =
        two_workspaces_mut(workspaces, first_location.0, second_location.0);
    let Some(first_tab) = first_workspace.tabs.get_mut(first_location.1) else {
        return false;
    };
    let Some(second_tab) = second_workspace.tabs.get_mut(second_location.1) else {
        return false;
    };
    exchange_tab_panes(first_tab, first_pane, second_tab, second_pane)
}

fn exchange_tab_panes(
    first_tab: &mut Tab,
    first_pane: PaneId,
    second_tab: &mut Tab,
    second_pane: PaneId,
) -> bool {
    if !first_tab.panes.contains_key(&first_pane)
        || !second_tab.panes.contains_key(&second_pane)
        || !first_tab.layout.pane_ids().contains(&first_pane)
        || !second_tab.layout.pane_ids().contains(&second_pane)
    {
        return false;
    }
    if !first_tab.layout.replace_pane_id(first_pane, second_pane)
        || !second_tab.layout.replace_pane_id(second_pane, first_pane)
    {
        return false;
    }

    let Some(first_state) = first_tab.panes.remove(&first_pane) else {
        return false;
    };
    let Some(second_state) = second_tab.panes.remove(&second_pane) else {
        first_tab.panes.insert(first_pane, first_state);
        return false;
    };
    first_tab.panes.insert(second_pane, second_state);
    second_tab.panes.insert(first_pane, first_state);
    if first_tab.root_pane == first_pane {
        first_tab.root_pane = second_pane;
    }
    if second_tab.root_pane == second_pane {
        second_tab.root_pane = first_pane;
    }
    true
}

fn two_workspaces_mut(
    workspaces: &mut [crate::workspace::Workspace],
    first: usize,
    second: usize,
) -> (
    &mut crate::workspace::Workspace,
    &mut crate::workspace::Workspace,
) {
    debug_assert_ne!(first, second);
    if first < second {
        let (left, right) = workspaces.split_at_mut(second);
        (&mut left[first], &mut right[0])
    } else {
        let (left, right) = workspaces.split_at_mut(first);
        (&mut right[0], &mut left[second])
    }
}

fn two_tabs_mut(tabs: &mut [Tab], first: usize, second: usize) -> (&mut Tab, &mut Tab) {
    debug_assert_ne!(first, second);
    if first < second {
        let (left, right) = tabs.split_at_mut(second);
        (&mut left[first], &mut right[0])
    } else {
        let (left, right) = tabs.split_at_mut(first);
        (&mut right[0], &mut left[second])
    }
}

fn automatic_dock_slot(layout: &TileLayout) -> Option<(PaneId, Direction)> {
    let pane_count = layout.pane_count();
    let anchor = layout
        .panes(Rect::new(0, 0, 10_000, 10_000))
        .into_iter()
        .max_by_key(|pane| {
            (
                pane.rect.y.saturating_add(pane.rect.height),
                pane.rect.x.saturating_add(pane.rect.width),
            )
        })?
        .id;
    let direction = if pane_count == 2
        && matches!(
            layout.root(),
            Node::Split {
                direction: Direction::Horizontal,
                ..
            }
        ) {
        Direction::Vertical
    } else {
        Direction::Horizontal
    };
    Some((anchor, direction))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Mode, workspace::Workspace};

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

    #[test]
    fn attention_waits_for_a_stable_state_before_projecting() {
        let (mut brief, brief_pane) = state_with_attention();
        brief.attention_dock.queue[0].eligible_at = Instant::now() + ATTENTION_DEBOUNCE;
        brief.attention_dock.queue[0].ready = false;

        brief.reconcile_attention_dock();
        assert!(brief.attention_dock.placement.is_none());
        brief.observe_attention_transition(
            brief_pane,
            AgentState::Blocked,
            AgentState::Working,
            true,
        );
        assert!(!brief.reconcile_due_attention(Instant::now() + ATTENTION_DEBOUNCE));
        assert!(brief.attention_dock.placement.is_none());

        let (mut stable, stable_pane) = state_with_attention();
        stable.attention_dock.queue[0].eligible_at = Instant::now() - ATTENTION_DEBOUNCE;
        stable.attention_dock.queue[0].ready = false;
        assert!(stable.reconcile_due_attention(Instant::now()));
        assert!(!stable.reconcile_due_attention(Instant::now()));
        assert_eq!(stable.pane_location(stable_pane), Some((1, 0)));
    }

    #[test]
    fn attention_creates_a_transient_split_without_stealing_focus() {
        let (mut state, attention_pane) = state_with_attention();
        let focused = state.workspaces[1].focused_pane_id();

        state.reconcile_attention_dock();

        assert_eq!(state.workspaces[1].tabs[0].layout.pane_count(), 2);
        assert_eq!(state.workspaces[1].focused_pane_id(), focused);
        assert_eq!(state.pane_location(attention_pane), Some((1, 0)));
        assert_eq!(
            state
                .attention_dock_title_for_pane(attention_pane)
                .as_deref(),
            Some("WORKSPACE - attention-home")
        );
        assert_eq!(
            state.workspace_display_name(&state.workspaces[0]),
            "attention-home"
        );
        assert_eq!(state.workspace_display_name(&state.workspaces[1]), "work");
        state.assert_invariants_for_test();
    }

    #[test]
    fn zoomed_tabs_temporarily_unzoom_to_show_attention_and_restore_afterward() {
        let (mut state, attention_pane) = state_with_attention();
        state.workspaces[1].tabs[0].zoomed = true;

        state.reconcile_attention_dock();

        assert!(!state.workspaces[1].tabs[0].zoomed);
        assert_eq!(state.pane_location(attention_pane), Some((1, 0)));

        state.observe_attention_transition(
            attention_pane,
            AgentState::Blocked,
            AgentState::Working,
            true,
        );
        state.reconcile_attention_dock();
        assert!(state.workspaces[1].tabs[0].zoomed);
    }

    #[test]
    fn two_side_by_side_panes_place_attention_below_the_right_pane() {
        let (mut state, attention_pane) = state_with_attention();
        state.workspaces[1].test_split(Direction::Horizontal);

        state.reconcile_attention_dock();

        let attention_rect = state.workspaces[1].tabs[0]
            .layout
            .panes(Rect::new(0, 0, 120, 60))
            .into_iter()
            .find(|pane| pane.id == attention_pane)
            .expect("attention pane")
            .rect;
        assert_eq!(attention_rect, Rect::new(60, 30, 60, 30));
    }

    #[test]
    fn working_restores_the_original_layout_and_source_home() {
        let (mut state, attention_pane) = state_with_attention();
        let work_ids = state.workspaces[1].tabs[0].layout.pane_ids();
        state.reconcile_attention_dock();

        state.observe_attention_transition(
            attention_pane,
            AgentState::Blocked,
            AgentState::Working,
            true,
        );
        state.reconcile_attention_dock();

        assert_eq!(state.workspaces[1].tabs[0].layout.pane_ids(), work_ids);
        assert_eq!(state.pane_location(attention_pane), Some((0, 0)));
        assert!(state.attention_dock.placement.is_none());
        state.assert_invariants_for_test();
    }

    #[test]
    fn placement_uses_right_then_down_then_southeast_right() {
        let one = Workspace::test_new("one");
        let (anchor, direction) = automatic_dock_slot(&one.tabs[0].layout).unwrap();
        assert_eq!(anchor, one.tabs[0].root_pane);
        assert_eq!(direction, Direction::Horizontal);

        let mut two = Workspace::test_new("two");
        two.test_split(Direction::Horizontal);
        assert_eq!(
            automatic_dock_slot(&two.tabs[0].layout).unwrap().1,
            Direction::Vertical
        );

        let mut stacked = Workspace::test_new("stacked");
        stacked.test_split(Direction::Vertical);
        assert_eq!(
            automatic_dock_slot(&stacked.tabs[0].layout).unwrap().1,
            Direction::Horizontal
        );
    }

    #[test]
    fn transient_dock_follows_workspace_context_and_restores_the_previous_layout() {
        let (mut state, attention_pane) = state_with_attention();
        state.workspaces.push(Workspace::test_new("other"));
        state.ensure_test_terminals();
        state.reconcile_attention_dock();
        assert_eq!(state.pane_location(attention_pane), Some((1, 0)));

        state.switch_workspace(2);

        assert_eq!(state.workspaces[1].tabs[0].layout.pane_count(), 1);
        assert_eq!(state.workspaces[2].tabs[0].layout.pane_count(), 2);
        assert_eq!(state.pane_location(attention_pane), Some((2, 0)));
    }

    #[test]
    fn visible_attention_is_not_duplicated_in_its_home_tab() {
        let (mut state, attention_pane) = state_with_attention();
        state.active = Some(0);
        state.selected = 0;

        state.reconcile_attention_dock();

        assert_eq!(state.workspaces[0].tabs[0].layout.pane_count(), 1);
        assert_eq!(state.pane_location(attention_pane), Some((0, 0)));
        assert!(state.attention_dock.placement.is_none());
    }

    #[test]
    fn focused_attention_stays_pinned_when_blocked_work_preempts_the_queue() {
        let (mut state, attention_pane) = state_with_attention();
        state.attention_dock.queue[0].kind = AttentionKind::Done;
        let blocked_home = Workspace::test_new("blocked-home");
        let blocked_pane = blocked_home.tabs[0].root_pane;
        state.workspaces.push(blocked_home);
        state.ensure_test_terminals();
        state.reconcile_attention_dock();
        state.focus_pane_in_workspace(1, attention_pane);
        state.observe_attention_transition(
            blocked_pane,
            AgentState::Working,
            AgentState::Blocked,
            true,
        );
        state.make_attention_ready_for_test(blocked_pane);

        state.reconcile_attention_dock();

        assert_eq!(state.pane_location(attention_pane), Some((1, 0)));
        assert_ne!(state.pane_location(blocked_pane), Some((1, 0)));
    }

    #[test]
    fn focused_transient_dock_keeps_the_source_public_id_for_plugins() {
        let (mut state, attention_pane) = state_with_attention();
        let source_number = state.workspaces[0].public_pane_numbers[&attention_pane];
        let source_public_id =
            crate::workspace::public_pane_id_for_number(&state.workspaces[0].id, source_number);
        state.reconcile_attention_dock();
        state.focus_pane_in_workspace(1, attention_pane);

        assert_eq!(
            state
                .focused_attention_source_context()
                .map(|(_, _, _, public_id)| public_id),
            Some(source_public_id)
        );
    }

    #[tokio::test]
    async fn snapshots_are_identical_with_or_without_a_visible_transient_dock() {
        let (mut state, attention_pane) = state_with_attention();
        let terminal_id = state.workspaces[0]
            .terminal_id(attention_pane)
            .expect("attention terminal")
            .clone();
        let mut terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        terminal_runtimes.insert(
            terminal_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(40, 10, b"attention history"),
        );
        let capture = |state: &AppState| {
            crate::persist::capture(
                &state.workspaces,
                &state.terminals,
                &terminal_runtimes,
                state.active,
                state.selected,
                state.sidebar_width,
                state.sidebar_section_split,
                state.collapsed_space_keys.clone(),
                state.canonical_attention_exchange(),
            )
        };
        let capture_history = |state: &AppState| {
            crate::persist::capture_history(
                &state.workspaces,
                &terminal_runtimes,
                state.canonical_attention_exchange(),
            )
        };
        let before = serde_json::to_value(capture(&state)).unwrap();
        let history_before = serde_json::to_value(capture_history(&state)).unwrap();

        state.reconcile_attention_dock();
        let during = serde_json::to_value(capture(&state)).unwrap();
        let history_during = serde_json::to_value(capture_history(&state)).unwrap();

        assert_eq!(during, before);
        assert_eq!(history_during, history_before);
    }

    #[test]
    fn prefix_open_restores_and_follows_the_attention_pane_home() {
        let (mut state, attention_pane) = state_with_attention();
        state.reconcile_attention_dock();
        state.focus_pane_in_workspace(1, attention_pane);

        assert!(state.open_docked_attention());

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(attention_pane));
        assert_eq!(state.workspaces[1].tabs[0].layout.pane_count(), 1);
    }
}
