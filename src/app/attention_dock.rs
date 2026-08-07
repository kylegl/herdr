use std::collections::HashMap;

use crate::{app::state::AppState, detect::AgentState, layout::PaneId, workspace::Tab};

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CrossWorkspaceIdentity {
    attention_home_number: usize,
    displaced_home_number: usize,
    attention_home_public_id: String,
    displaced_home_public_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalAttentionExchange {
    pub(crate) attention_pane: PaneId,
    pub(crate) displaced_pane: PaneId,
    pub(crate) attention_home_number: Option<usize>,
    pub(crate) displaced_home_number: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DockPlacement {
    attention_pane: PaneId,
    displaced_pane: PaneId,
    dock_workspace_id: String,
    dock_focus_before_attention: PaneId,
    cross_workspace_identity: Option<CrossWorkspaceIdentity>,
}

#[derive(Debug, Default)]
pub(crate) struct AttentionDockState {
    dock_panes: HashMap<String, PaneId>,
    queue: Vec<AttentionEntry>,
    placement: Option<DockPlacement>,
    presented_at_home: Option<PaneId>,
    reconcile_suspended: bool,
    next_sequence: u64,
}

impl AppState {
    pub(crate) fn set_attention_dock(&mut self, ws_idx: usize, pane_id: PaneId) -> bool {
        let Some(workspace) = self.workspaces.get(ws_idx) else {
            return false;
        };
        if workspace.pane_state(pane_id).is_none() {
            return false;
        }
        let workspace_id = workspace.id.clone();
        if self
            .attention_dock
            .placement
            .as_ref()
            .is_some_and(|placement| placement.dock_workspace_id == workspace_id)
        {
            self.undock_attention();
        }
        self.attention_dock.dock_panes.insert(workspace_id, pane_id);
        self.reconcile_attention_dock();
        true
    }

    pub(crate) fn clear_attention_dock(&mut self, ws_idx: usize) -> bool {
        let Some(workspace_id) = self
            .workspaces
            .get(ws_idx)
            .map(|workspace| workspace.id.clone())
        else {
            return false;
        };
        if self
            .attention_dock
            .placement
            .as_ref()
            .is_some_and(|placement| placement.dock_workspace_id == workspace_id)
        {
            self.undock_attention();
        }
        self.attention_dock
            .dock_panes
            .remove(&workspace_id)
            .is_some()
    }

    pub(crate) fn is_attention_dock(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.workspaces.get(ws_idx).is_some_and(|workspace| {
            self.attention_dock.dock_panes.get(&workspace.id) == Some(&pane_id)
        })
    }

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
            if kind == AttentionKind::Blocked {
                entry.kind = AttentionKind::Blocked;
            }
        } else {
            self.attention_dock.next_sequence += 1;
            self.attention_dock.queue.push(AttentionEntry {
                pane_id,
                kind,
                sequence: self.attention_dock.next_sequence,
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

        let desired = self.attention_head();
        let active_workspace_id = self
            .active
            .and_then(|idx| self.workspaces.get(idx))
            .map(|workspace| workspace.id.clone());
        let placement_matches = self
            .attention_dock
            .placement
            .as_ref()
            .is_some_and(|placement| {
                Some(placement.attention_pane) == desired
                    && Some(&placement.dock_workspace_id) == active_workspace_id.as_ref()
            });
        if placement_matches {
            return;
        }
        self.undock_attention();

        let (Some(attention_pane), Some(active_ws_idx)) = (desired, self.active) else {
            return;
        };
        let Some(workspace) = self.workspaces.get(active_ws_idx) else {
            return;
        };
        let Some(&dock_pane) = self.attention_dock.dock_panes.get(&workspace.id) else {
            return;
        };
        let Some(dock_tab_idx) = workspace.find_tab_index_for_pane(dock_pane) else {
            return;
        };
        if dock_tab_idx != workspace.active_tab_index() || attention_pane == dock_pane {
            return;
        }
        let focused = workspace.tabs[dock_tab_idx].layout.focused();
        if focused == dock_pane || focused == attention_pane {
            return;
        }

        let dock_workspace_id = workspace.id.clone();
        let Some(cross_workspace_identity) = self.dock_exchange(attention_pane, dock_pane) else {
            return;
        };
        self.attention_dock
            .dock_panes
            .insert(dock_workspace_id.clone(), attention_pane);
        self.attention_dock.placement = Some(DockPlacement {
            attention_pane,
            displaced_pane: dock_pane,
            dock_workspace_id,
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

    pub(crate) fn prepare_attention_topology_mutation(&mut self) {
        self.undock_attention();
    }

    pub(crate) fn prepare_attention_pane_move(&mut self, pane_id: PaneId) {
        self.undock_attention();
        self.attention_dock
            .dock_panes
            .retain(|_, dock_pane| *dock_pane != pane_id);
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
        self.attention_dock
            .dock_panes
            .retain(|_, dock_pane| *dock_pane != pane_id);
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
        for (workspace_id, pane_id) in &self.attention_dock.dock_panes {
            let workspace = self
                .workspaces
                .iter()
                .find(|workspace| workspace.id == *workspace_id)
                .expect("attention dock workspace must be live");
            assert!(
                workspace.pane_state(*pane_id).is_some(),
                "attention dock pane must belong to its workspace"
            );
        }
        if let Some(placement) = &self.attention_dock.placement {
            assert_ne!(placement.attention_pane, placement.displaced_pane);
            assert_eq!(
                self.attention_dock
                    .dock_panes
                    .get(&placement.dock_workspace_id),
                Some(&placement.attention_pane)
            );
            assert!(self.pane_location(placement.attention_pane).is_some());
            assert!(self.pane_location(placement.displaced_pane).is_some());
        }
    }

    fn attention_head(&self) -> Option<PaneId> {
        self.attention_dock
            .queue
            .iter()
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
        self.attention_dock
            .dock_panes
            .retain(|workspace_id, pane_id| {
                self.workspaces.iter().any(|workspace| {
                    workspace.id == *workspace_id && workspace.pane_state(*pane_id).is_some()
                })
            });
    }

    fn undock_attention(&mut self) {
        let Some(placement) = self.attention_dock.placement.take() else {
            return;
        };
        if self.restore_dock_exchange(&placement) {
            self.attention_dock
                .dock_panes
                .insert(placement.dock_workspace_id, placement.displaced_pane);
        }
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
        self.public_pane_id_aliases
            .remove(&identity.attention_home_public_id);
        self.public_pane_id_aliases
            .remove(&identity.displaced_home_public_id);
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

#[cfg(test)]
mod tests {
    use ratatui::layout::Direction;

    use crate::{
        app::{state::AppState, Mode},
        detect::AgentState,
        workspace::Workspace,
    };

    fn app_with_dock() -> (AppState, crate::layout::PaneId, crate::layout::PaneId) {
        let home = Workspace::test_new("home");
        let attention_pane = home.tabs[0].root_pane;

        let mut work = Workspace::test_new("work");
        let focused_pane = work.tabs[0].root_pane;
        let dock_pane = work.test_split(Direction::Horizontal);
        work.tabs[0].layout.focus_pane(focused_pane);

        let mut state = AppState::test_new();
        state.workspaces = vec![home, work];
        state.ensure_test_terminals();
        state.active = Some(1);
        state.selected = 1;
        state.mode = Mode::Terminal;
        state.set_attention_dock(1, dock_pane);

        (state, attention_pane, dock_pane)
    }

    #[test]
    fn json_api_sets_and_clears_workspace_attention_dock() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("work")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = crate::workspace::public_pane_id_for_number(
            &app.state.workspaces[0].id,
            app.state.workspaces[0]
                .public_pane_number(pane_id)
                .expect("pane number"),
        );
        let workspace_id = app.state.workspaces[0].id.clone();

        app.dispatch_api_request(
            "set",
            crate::api::schema::Method::AttentionDockSet(crate::api::schema::PaneTarget {
                pane_id: public_pane_id,
            }),
        );
        assert!(app.state.is_attention_dock(0, pane_id));

        app.dispatch_api_request(
            "clear",
            crate::api::schema::Method::AttentionDockClear(crate::api::schema::WorkspaceTarget {
                workspace_id,
            }),
        );
        assert!(!app.state.is_attention_dock(0, pane_id));
    }

    #[test]
    fn rejected_pane_close_keeps_attention_entry_and_dock_designation() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut main = Workspace::test_new("main");
        main.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo".into(),
            label: "repo".into(),
            repo_root: "/repo".into(),
            checkout_path: "/repo".into(),
            is_linked_worktree: false,
        });
        let attention_pane = main.tabs[0].root_pane;
        let mut issue = Workspace::test_new("issue");
        issue.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo".into(),
            label: "repo".into(),
            repo_root: "/repo".into(),
            checkout_path: "/repo/issue".into(),
            is_linked_worktree: true,
        });
        let focused_pane = issue.tabs[0].root_pane;
        let dock_pane = issue.test_split(Direction::Horizontal);
        issue.tabs[0].layout.focus_pane(focused_pane);
        let public_attention_pane = crate::workspace::public_pane_id_for_number(
            &main.id,
            main.public_pane_number(attention_pane)
                .expect("attention pane number"),
        );
        app.state.workspaces = vec![main, issue];
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.mode = Mode::Terminal;
        app.state.set_attention_dock(1, dock_pane);
        app.state.observe_attention_transition(
            attention_pane,
            AgentState::Working,
            AgentState::Blocked,
            true,
        );
        app.state.reconcile_attention_dock();

        let response = app.dispatch_api_request(
            "close",
            crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget {
                pane_id: public_attention_pane,
            }),
        );

        assert!(response.contains("confirmation_required"));
        assert!(app.state.workspaces[1].pane_state(attention_pane).is_some());
        assert!(app.state.is_attention_dock(1, attention_pane));
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn blocked_agent_is_presented_in_active_workspace_without_stealing_focus() {
        let (mut state, attention_pane, dock_pane) = app_with_dock();
        let focused_before = state.workspaces[1].tabs[0].layout.focused();

        state.observe_attention_transition(
            attention_pane,
            AgentState::Working,
            AgentState::Blocked,
            true,
        );
        state.reconcile_attention_dock();

        assert!(state.workspaces[1].pane_state(attention_pane).is_some());
        assert!(state.workspaces[0].pane_state(dock_pane).is_some());
        assert_eq!(state.workspaces[1].tabs[0].layout.focused(), focused_before);
        state.assert_invariants_for_test();
    }

    #[test]
    fn open_docked_attention_returns_home_focuses_and_follows_after_leaving() {
        let (mut state, attention_pane, _) = app_with_dock();
        state.observe_attention_transition(
            attention_pane,
            AgentState::Working,
            AgentState::Blocked,
            true,
        );
        state.reconcile_attention_dock();
        state.focus_pane_in_workspace(1, attention_pane);

        assert!(state.open_docked_attention());
        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].tabs[0].layout.focused(), attention_pane);
        assert!(state.workspaces[0].pane_state(attention_pane).is_some());

        state.switch_workspace(1);
        assert!(state.workspaces[1].pane_state(attention_pane).is_some());
        assert_eq!(state.active, Some(1));
        state.assert_invariants_for_test();
    }

    #[test]
    fn docking_preserves_adversarial_identity_invariants() {
        let mut state = AppState::test_with_adversarial_identity_state();
        let attention_pane = state.workspaces[0].tabs[0].root_pane;
        let mut work = Workspace::test_new("work");
        let focused_pane = work.tabs[0].root_pane;
        let dock_pane = work.test_split(Direction::Horizontal);
        work.tabs[0].layout.focus_pane(focused_pane);
        state.workspaces.push(work);
        state.ensure_test_terminals();
        state.active = Some(1);
        state.selected = 1;
        state.mode = Mode::Terminal;
        state.set_attention_dock(1, dock_pane);

        state.observe_attention_transition(
            attention_pane,
            AgentState::Working,
            AgentState::Blocked,
            true,
        );
        state.reconcile_attention_dock();
        state.assert_invariants_for_test();

        state.prepare_attention_topology_mutation();
        state.assert_invariants_for_test();
    }

    #[test]
    fn session_snapshot_keeps_attention_pane_at_home_while_docked() {
        let (mut state, attention_pane, dock_pane) = app_with_dock();
        let attention_home_number = state.workspaces[0].public_pane_number(attention_pane);
        let dock_home_number = state.workspaces[1].public_pane_number(dock_pane);
        state.observe_attention_transition(
            attention_pane,
            AgentState::Working,
            AgentState::Blocked,
            true,
        );
        state.reconcile_attention_dock();

        let snapshot = crate::persist::capture(
            &state.workspaces,
            &state.terminals,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            state.active,
            state.selected,
            state.sidebar_width,
            state.sidebar_section_split,
            state.collapsed_space_keys.clone(),
            state.canonical_attention_exchange(),
        );

        assert!(snapshot.workspaces[0].tabs[0]
            .panes
            .contains_key(&attention_pane.raw()));
        assert!(snapshot.workspaces[1].tabs[0]
            .panes
            .contains_key(&dock_pane.raw()));
        assert_eq!(
            snapshot.workspaces[0].public_pane_numbers[&attention_pane.raw()],
            attention_home_number.expect("attention public number")
        );
        assert_eq!(
            snapshot.workspaces[1].public_pane_numbers[&dock_pane.raw()],
            dock_home_number.expect("dock public number")
        );
    }

    #[test]
    fn blocked_entries_advance_in_fifo_order() {
        let (mut state, first_pane, _) = app_with_dock();
        let second_workspace = Workspace::test_new("second");
        let second_pane = second_workspace.tabs[0].root_pane;
        state.workspaces.push(second_workspace);
        state.ensure_test_terminals();

        state.observe_attention_transition(
            first_pane,
            AgentState::Working,
            AgentState::Blocked,
            true,
        );
        state.observe_attention_transition(
            second_pane,
            AgentState::Working,
            AgentState::Blocked,
            true,
        );
        state.reconcile_attention_dock();
        assert!(state.workspaces[1].pane_state(first_pane).is_some());

        state.observe_attention_transition(
            first_pane,
            AgentState::Blocked,
            AgentState::Working,
            true,
        );
        state.reconcile_attention_dock();
        assert!(state.workspaces[1].pane_state(second_pane).is_some());
        state.assert_invariants_for_test();
    }

    #[test]
    fn blocked_preempts_done_and_working_advances_to_next_entry() {
        let (mut state, done_pane, _) = app_with_dock();
        let blocked_workspace = Workspace::test_new("blocked");
        let blocked_pane = blocked_workspace.tabs[0].root_pane;
        state.workspaces.push(blocked_workspace);
        state.ensure_test_terminals();

        state.observe_attention_transition(done_pane, AgentState::Working, AgentState::Idle, false);
        state.reconcile_attention_dock();
        assert!(state.workspaces[1].pane_state(done_pane).is_some());

        state.observe_attention_transition(
            blocked_pane,
            AgentState::Working,
            AgentState::Blocked,
            true,
        );
        state.reconcile_attention_dock();
        assert!(state.workspaces[1].pane_state(blocked_pane).is_some());
        assert!(state.workspaces[0].pane_state(done_pane).is_some());

        state.observe_attention_transition(
            blocked_pane,
            AgentState::Blocked,
            AgentState::Working,
            true,
        );
        state.reconcile_attention_dock();
        assert!(state.workspaces[1].pane_state(done_pane).is_some());
        assert!(state.workspaces[2].pane_state(blocked_pane).is_some());
        state.assert_invariants_for_test();
    }
}
