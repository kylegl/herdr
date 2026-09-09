use super::*;

#[test]
fn attention_sidebar_status_stays_home_as_the_owner_changes_workspaces() {
    use crate::api::schema::{AgentStatus, Method, WorkspaceTarget};
    use crate::detect::{Agent, AgentState};

    let mut server = test_headless_server();
    server.app.state.workspaces = ["source", "first-host", "second-host"]
        .map(crate::workspace::Workspace::test_new)
        .into();
    server.app.state.ensure_test_terminals();
    let attention_pane = server.app.state.workspaces[0].tabs[0].root_pane;
    for (index, workspace) in server.app.state.workspaces.iter().enumerate() {
        let terminal_id = workspace.terminal_id(workspace.tabs[0].root_pane).unwrap();
        server
            .app
            .state
            .terminals
            .get_mut(terminal_id)
            .unwrap()
            .set_detected_state(
                Some(Agent::Pi),
                if index == 0 {
                    AgentState::Blocked
                } else {
                    AgentState::Working
                },
            );
    }
    server.app.state.active = Some(1);
    server.app.state.selected = 1;
    server.app.state.observe_attention_transition(
        attention_pane,
        AgentState::Working,
        AgentState::Blocked,
        true,
    );
    server
        .app
        .state
        .make_attention_ready_for_test(attention_pane);
    let (_control, render) = connect_test_shell(&mut server, 10, 100, 30);

    for host in [1, 2, 1, 0, 2] {
        let (respond_to, _response) = std::sync::mpsc::channel();
        server.handle_client_shell_api_request(
            10,
            crate::api::ApiRequestMessage {
                request: crate::api::schema::Request {
                    id: format!("focus-{host}"),
                    method: Method::WorkspaceFocus(WorkspaceTarget {
                        workspace_id: server.app.state.workspaces[host].id.clone(),
                    }),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            },
        );
        server.render_and_stream();
        while render.try_recv().is_ok() {}
        let snapshot = server.clients[&10].shell_snapshot.as_ref().unwrap();
        assert_eq!(snapshot.workspaces[0].agent_status, AgentStatus::Blocked);
        for workspace in &snapshot.workspaces[1..] {
            assert_eq!(workspace.agent_status, AgentStatus::Working);
        }
        let blocked = snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_status == AgentStatus::Blocked)
            .unwrap();
        assert_eq!(blocked.workspace_id, snapshot.workspaces[0].workspace_id);
        server.app.state.assert_invariants_for_test();
    }
    shutdown_test_runtimes(&mut server);
}

#[test]
fn delayed_notification_completion_presents_ready_attention_without_another_event() {
    let mut server = test_headless_server();
    let home = crate::workspace::Workspace::test_new("attention-home");
    let attention_pane = home.tabs[0].root_pane;
    let host = crate::workspace::Workspace::test_new("attention-host");
    let target = crate::app::attention_dock::AttentionHostTarget {
        workspace_id: host.id.clone(),
        tab_number: host.tabs[0].number,
    };
    server.app.state.workspaces = vec![home, host];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(1);
    server.app.state.selected = 1;
    server.app.state.toast_config.delay_seconds = 1;
    server
        .app
        .state
        .set_attention_host(Some(target), &server.app.terminal_runtimes);
    server.app.state.handle_app_event(AppEvent::StateChanged {
        pane_id: attention_pane,
        agent: Some(crate::detect::Agent::Pi),
        state: crate::detect::AgentState::Blocked,
        visible_blocker: false,
        visible_working: false,
        process_exited: false,
        observed_at: Instant::now(),
    });
    let attention_deadline = server.app.state.next_attention_deadline().unwrap();
    let notification_deadline = server
        .app
        .state
        .next_pending_agent_notification_deadline()
        .unwrap();
    assert!(attention_deadline < notification_deadline);

    server.handle_scheduled_tasks_headless(attention_deadline, false);
    assert!(server.app.state.docked_attention_pane().is_none());
    server.handle_scheduled_tasks_headless(notification_deadline, false);

    assert!(server.app.state.pending_agent_notifications.is_empty());
    assert_eq!(
        server.app.state.docked_attention_pane(),
        Some(attention_pane)
    );
    assert!(server.app.state.workspaces[1].tabs[0]
        .panes
        .contains_key(&attention_pane));
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn attention_geometry_tracks_debounced_dock_and_dismiss_on_first_frame() {
    let mut server = test_headless_server();
    let home = crate::workspace::Workspace::test_new("attention-home");
    let attention_pane = home.tabs[0].root_pane;
    let host = crate::workspace::Workspace::test_new("attention-host");
    let host_pane = host.tabs[0].root_pane;
    server.app.state.workspaces = vec![home, host];
    server.app.state.ensure_test_terminals();
    for (workspace_index, pane_id) in [(0, attention_pane), (1, host_pane)] {
        let terminal_id = server.app.state.workspaces[workspace_index]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        server.app.terminal_runtimes.insert(
            terminal_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(100, 30, b""),
        );
    }
    server.app.state.active = Some(1);
    server.app.state.selected = 1;
    server.app.state.mode = crate::app::Mode::Terminal;
    server.app.state.toast_config.delay_seconds = 0;
    let (_control, render) = connect_test_shell(&mut server, 10, 100, 30);
    server.render_and_stream();
    while render.try_recv().is_ok() {}
    let initial_size = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 1, host_pane)
        .unwrap()
        .current_size();

    server.app.state.handle_app_event(AppEvent::StateChanged {
        pane_id: attention_pane,
        agent: Some(crate::detect::Agent::Pi),
        state: crate::detect::AgentState::Blocked,
        visible_blocker: false,
        visible_working: false,
        process_exited: false,
        observed_at: Instant::now(),
    });
    let deadline = server.app.state.next_attention_deadline().unwrap();
    server.handle_scheduled_tasks_headless(deadline, false);
    assert_eq!(
        server.app.state.docked_attention_pane(),
        Some(attention_pane)
    );
    server.render_and_stream();
    while render.try_recv().is_ok() {}

    let docked_size = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 1, attention_pane)
        .unwrap()
        .current_size();
    assert!(
        docked_size.1 < initial_size.1,
        "docked PTY must use the split width"
    );
    let host_size = server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 1, host_pane)
        .unwrap()
        .current_size();
    assert!(host_size.1 < initial_size.1, "host PTY must also shrink");
    let snapshot = server.clients[&10].shell_snapshot.as_ref().unwrap();
    assert_eq!(
        snapshot.workspaces[0].agent_status,
        crate::api::schema::AgentStatus::Blocked
    );
    assert_eq!(
        snapshot.workspaces[1].agent_status,
        crate::api::schema::AgentStatus::Unknown
    );
    let projection = client_shell_attention_projection(&server.app).unwrap();
    let (respond_to, response) = std::sync::mpsc::channel();
    assert!(server.handle_client_shell_api_request(
        10,
        crate::api::ApiRequestMessage {
            request: crate::api::schema::Request {
                id: "dismiss-attention".into(),
                method: crate::api::schema::Method::AttentionDismiss(
                    crate::api::schema::AttentionTarget {
                        source_pane_id: projection.source_pane_id
                    },
                ),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        }
    ));
    assert!(response.try_recv().unwrap().contains("\"result\""));
    server.render_and_stream();

    assert!(server.clients[&10].shell_attention.is_none());
    for (workspace_index, pane_id) in [(0, attention_pane), (1, host_pane)] {
        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, workspace_index, pane_id)
            .unwrap();
        assert_eq!(
            runtime.current_size(),
            initial_size,
            "dismiss restores geometry without another input or resize event"
        );
        assert_eq!(
            runtime.terminal_dimensions(),
            Some((initial_size.1, initial_size.0))
        );
    }
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn attention_geometry_respects_two_client_controllers_and_restores_on_working() {
    let mut server = test_headless_server();
    let home = crate::workspace::Workspace::test_new("attention-home");
    let attention_pane = home.tabs[0].root_pane;
    let host = crate::workspace::Workspace::test_new("attention-host");
    let host_pane = host.tabs[0].root_pane;
    server.app.state.workspaces = vec![home, host];
    server.app.state.ensure_test_terminals();
    for (workspace_index, pane_id) in [(0, attention_pane), (1, host_pane)] {
        let terminal_id = server.app.state.workspaces[workspace_index]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        server.app.terminal_runtimes.insert(
            terminal_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(100, 30, b""),
        );
    }
    server.app.state.active = Some(1);
    server.app.state.selected = 1;
    server.app.state.mode = crate::app::Mode::Terminal;
    server.app.state.toast_config.delay_seconds = 0;
    let (_owner_control, owner_render) = connect_test_shell(&mut server, 10, 100, 30);
    let (_peer_control, peer_render) = connect_test_shell(&mut server, 20, 70, 20);
    let home_tab = server.app.public_tab_id(0, 0).unwrap();
    assert!(server.focus_shell_client_on_tab(20, &home_tab));
    assert!(server.claim_shell_tab_geometry(20, false));
    server.claim_shell_tab_geometry(10, false);
    assert!(server.reapply_controlled_shell_tab_geometry(false));
    let sizes = [(0, attention_pane), (1, host_pane)].map(|(workspace_index, pane_id)| {
        server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, workspace_index, pane_id)
            .unwrap()
            .current_size()
    });
    server.render_and_stream();
    while owner_render.try_recv().is_ok() {}
    while peer_render.try_recv().is_ok() {}

    for state in [
        crate::detect::AgentState::Blocked,
        crate::detect::AgentState::Working,
    ] {
        server.handle_internal_event_with_forwarding(AppEvent::StateChanged {
            pane_id: attention_pane,
            agent: Some(crate::detect::Agent::Pi),
            state,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: Instant::now(),
        });
        if let Some(deadline) = server.app.state.next_attention_deadline() {
            server.handle_scheduled_tasks_headless(deadline, false);
        }
        server.render_and_stream();
        while owner_render.try_recv().is_ok() {}
        while peer_render.try_recv().is_ok() {}
        if state == crate::detect::AgentState::Blocked {
            let runtime = server
                .app
                .state
                .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 1, attention_pane)
                .unwrap();
            assert!(runtime.current_size().1 < sizes[0].1);
            let dock_id = &server.clients[&10]
                .shell_attention
                .as_ref()
                .unwrap()
                .pane_id;
            let surface = server.clients[&10]
                .render_state
                .last_pane_surface()
                .unwrap();
            let dock = surface
                .panes
                .iter()
                .find(|pane| &pane.pane_id == dock_id)
                .unwrap();
            assert_eq!(
                runtime.current_size(),
                (dock.inner_rect.height, dock.inner_rect.width)
            );
            assert!(
                runtime.current_size().0 > sizes[0].0,
                "dock uses the owner's height, not the source viewer's height"
            );
        }
    }
    assert!(server.app.state.docked_attention_pane().is_none());
    for ((workspace_index, pane_id), size) in
        [(0, attention_pane), (1, host_pane)].into_iter().zip(sizes)
    {
        assert_eq!(
            server
                .app
                .state
                .runtime_for_pane_in_workspace(
                    &server.app.terminal_runtimes,
                    workspace_index,
                    pane_id,
                )
                .unwrap()
                .current_size(),
            size
        );
    }
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn attention_navigation_preserves_hidden_host_size_until_viewed() {
    use crate::api::schema::{Method, WorkspaceTarget};
    use crate::detect::{Agent, AgentState};

    for with_peer in [false, true] {
        let mut server = test_headless_server();
        server.app.state.workspaces = ["source", "first-host", "second-host"]
            .map(crate::workspace::Workspace::test_new)
            .into();
        server.app.state.ensure_test_terminals();
        let panes: Vec<_> = server
            .app
            .state
            .workspaces
            .iter()
            .map(|workspace| workspace.tabs[0].root_pane)
            .collect();
        for (index, pane) in panes.iter().enumerate() {
            let terminal = server.app.state.workspaces[index]
                .terminal_id(*pane)
                .unwrap()
                .clone();
            server.app.terminal_runtimes.insert(
                terminal,
                crate::terminal::TerminalRuntime::test_with_screen_bytes(100, 30, b""),
            );
        }
        server.app.state.active = Some(1);
        server.app.state.selected = 1;
        server.app.state.mode = crate::app::Mode::Terminal;
        server.app.state.toast_config.delay_seconds = 0;
        let owner = connect_test_shell(&mut server, 10, 100, 30);
        let peer = with_peer.then(|| connect_test_shell(&mut server, 20, 70, 20));
        if with_peer {
            let home = server.app.public_tab_id(0, 0).unwrap();
            server.focus_shell_client_on_tab(20, &home);
            server.claim_shell_tab_geometry(20, false);
        }
        let size = |server: &HeadlessServer, index: usize| {
            server
                .app
                .state
                .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, index, panes[index])
                .unwrap()
                .current_size()
        };
        let full_size = size(&server, 1);
        server.handle_internal_event_with_forwarding(AppEvent::StateChanged {
            pane_id: panes[0],
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: Instant::now(),
        });
        let deadline = server.app.state.next_attention_deadline().unwrap();
        server.handle_scheduled_tasks_headless(deadline, false);
        server.render_and_stream();
        let docked_size = size(&server, 1);
        assert!(docked_size.1 < full_size.1);
        let first_host_terminal = server.app.state.workspaces[1]
            .terminal_id(panes[1])
            .unwrap()
            .clone();
        let docked_content_seq = server
            .app
            .terminal_runtimes
            .get(&first_host_terminal)
            .unwrap()
            .content_seq();

        for host in [2, 1, 2, 1] {
            let (respond_to, response) = std::sync::mpsc::channel();
            let request = crate::api::ApiRequestMessage {
                request: crate::api::schema::Request {
                    id: format!("focus-{host}"),
                    method: Method::WorkspaceFocus(WorkspaceTarget {
                        workspace_id: server.app.state.workspaces[host].id.clone(),
                    }),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            };
            // Exercise both public navigation and the client-owned sidebar route.
            if with_peer {
                server.handle_client_shell_api_request(10, request);
            } else {
                server.handle_api_request_with_shutdown_check(request);
            }
            assert!(response.try_recv().unwrap().contains("\"result\""));
            server.render_and_stream();
            assert_eq!(
                size(&server, 1),
                docked_size,
                "moving the dock must not expand its now-hidden former host"
            );
            assert_eq!(
                server
                    .app
                    .terminal_runtimes
                    .get(&first_host_terminal)
                    .unwrap()
                    .content_seq(),
                docked_content_seq,
                "returning must not resize through an intermediate full-width layout"
            );
            assert_eq!(size(&server, 2), docked_size);
            server.app.state.assert_invariants_for_test();
            while owner.1.try_recv().is_ok() {}
            if let Some(peer) = &peer {
                while peer.1.try_recv().is_ok() {}
            }
        }
        let second_host_size = if with_peer {
            // Another viewer makes the former host visible at a different size.
            let second_host = server.app.public_tab_id(2, 0).unwrap();
            server.focus_shell_client_on_tab(20, &second_host);
            server.claim_shell_tab_geometry(20, false);
            server.reapply_controlled_shell_tab_geometry(false);
            let layout = crate::ui::compute_tab_surface_for(
                &server.app.state,
                &server.app.terminal_runtimes,
                Some(crate::ui::TabSurfaceTarget {
                    workspace_index: 2,
                    tab_index: 0,
                }),
                Rect::new(0, 0, 70, 20),
                false,
                Default::default(),
            );
            let rect = layout.pane_infos[0].inner_rect;
            assert_eq!(size(&server, 2), (rect.height, rect.width));
            assert_eq!(size(&server, 1), docked_size);
            size(&server, 2)
        } else {
            full_size
        };
        server.handle_internal_event_with_forwarding(AppEvent::StateChanged {
            pane_id: panes[0],
            agent: Some(Agent::Pi),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: Instant::now(),
        });
        server.render_and_stream();
        assert!(server.app.state.docked_attention_pane().is_none());
        assert_eq!(
            size(&server, 1),
            full_size,
            "removing the dock must restore its visible host immediately"
        );
        assert_eq!(size(&server, 2), second_host_size);
        shutdown_test_runtimes(&mut server);
    }
}
