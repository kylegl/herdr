use super::*;

fn attention_fixture() -> (HeadlessServer, crate::layout::PaneId, String) {
    let mut server = test_headless_server();
    server.app.state.workspaces = ["source", "host", "other"]
        .map(crate::workspace::Workspace::test_new)
        .into();
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(1);
    server.app.state.selected = 1;
    server.app.state.mode = crate::app::Mode::Terminal;
    server.app.state.toast_config.delay_seconds = 0;
    let pane = server.app.state.workspaces[0].tabs[0].root_pane;
    queue_blocked(&mut server, pane);
    let source = server
        .app
        .state
        .attention_target(pane)
        .unwrap()
        .public_pane_id;
    (server, pane, source)
}

fn queue_blocked(server: &mut HeadlessServer, pane: crate::layout::PaneId) {
    let (_, state) = server.app.find_pane(pane).unwrap();
    let terminal = state.attached_terminal_id.clone();
    server
        .app
        .state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .set_detected_state(
            Some(crate::detect::Agent::Pi),
            crate::detect::AgentState::Blocked,
        );
    server.app.state.observe_attention_transition(
        pane,
        crate::detect::AgentState::Working,
        crate::detect::AgentState::Blocked,
        true,
    );
    server.app.state.make_attention_ready_for_test(pane);
}

fn attention_request(
    server: &mut HeadlessServer,
    client: u64,
    method: api::schema::Method,
) -> String {
    let (respond_to, response) = std::sync::mpsc::channel();
    server.handle_client_shell_api_request(
        client,
        api::ApiRequestMessage {
            request: api::schema::Request {
                id: "attention-test".into(),
                method,
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        },
    );
    response.try_recv().unwrap()
}

fn view(source: Option<&str>, cols: u16, rows: u16) -> api::schema::Method {
    api::schema::Method::AttentionView(api::schema::AttentionViewParams {
        source_pane_id: source.map(str::to_owned),
        view_id: 1,
        cols,
        rows,
    })
}

#[test]
fn attention_queue_and_lease_never_exchange_host_topology_or_focus() {
    let (mut server, pane, source) = attention_fixture();
    let _channels = connect_test_shell(&mut server, 10, 100, 30);
    let _peer = connect_test_shell(&mut server, 20, 70, 20);
    let focus = server.shell_focus_target(10);
    let snapshot = serde_json::to_value(server.app.session_snapshot()).unwrap();
    assert!(attention_request(&mut server, 10, view(Some(&source), 50, 12)).contains("\"result\""));
    assert_eq!(
        serde_json::to_value(server.app.session_snapshot()).unwrap(),
        snapshot
    );
    let other = server.app.state.workspaces[2].tabs[0].root_pane;
    queue_blocked(&mut server, other);
    assert_eq!(
        server.clients[&10]
            .attention_view
            .as_ref()
            .unwrap()
            .source_pane_id,
        source
    );
    assert!(server.clients[&20].attention_view.is_none());
    assert_eq!(server.app.find_pane(pane).unwrap().0, 0);
    assert_eq!(server.shell_focus_target(10), focus);
    assert!(attention_request(&mut server, 10, view(None, 0, 0)).contains("\"result\""));
    assert!(
        server.app.state.attention_target(pane).is_some(),
        "close is not acknowledgement"
    );
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}

#[test]
fn attention_stale_and_legacy_requests_cannot_dismiss_another_source() {
    let (mut server, pane, source) = attention_fixture();
    let _channels = connect_test_shell(&mut server, 10, 100, 30);
    for method in [
        api::schema::Method::AttentionOpen(api::schema::AttentionTarget {
            source_pane_id: source.clone(),
        }),
        api::schema::Method::AttentionDismiss(api::schema::AttentionTarget {
            source_pane_id: source.clone(),
        }),
        view(Some("missing:p1"), 50, 12),
    ] {
        assert!(attention_request(&mut server, 10, method).contains("stale_attention"));
    }
    attention_request(&mut server, 10, view(Some(&source), 50, 12));
    assert!(attention_request(
        &mut server,
        10,
        api::schema::Method::AttentionAcknowledge(api::schema::AttentionTarget {
            source_pane_id: source.clone()
        })
    )
    .contains("\"result\""));
    assert!(server.clients[&10].attention_view.is_none());
    assert!(server.app.state.attention_target(pane).is_none());
    let other = server.app.state.workspaces[2].tabs[0].root_pane;
    queue_blocked(&mut server, other);
    assert!(attention_request(
        &mut server,
        10,
        api::schema::Method::AttentionAcknowledge(api::schema::AttentionTarget {
            source_pane_id: source
        })
    )
    .contains("stale_attention"));
    assert!(server.app.state.attention_target(other).is_some());
    shutdown_test_runtimes(&mut server);
}

#[test]
fn attention_jump_is_client_local_and_deactivation_retires_lease() {
    let (mut server, pane, source) = attention_fixture();
    let _channels = connect_test_shell(&mut server, 10, 100, 30);
    let _peer = connect_test_shell(&mut server, 20, 70, 20);
    let peer_focus = server.shell_focus_target(20);
    attention_request(&mut server, 10, view(Some(&source), 50, 12));
    server.set_client_shell_surface_active(10, false);
    assert!(server.clients[&10].attention_view.is_none());
    assert!(!server.attention_view_matches(10, &source));
    server.set_client_shell_surface_active(10, true);
    assert!(attention_request(
        &mut server,
        10,
        api::schema::Method::AttentionJump(api::schema::AttentionTarget {
            source_pane_id: source
        })
    )
    .contains("\"result\""));
    assert_eq!(server.shell_focus_target(10).unwrap().pane_id, pane);
    assert_eq!(server.shell_focus_target(20), peer_focus);
    assert!(server.app.state.attention_target(pane).is_some());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn attention_geometry_has_one_hidden_owner_and_visible_source_wins() {
    let (mut server, pane, source) = attention_fixture();
    for index in 0..2 {
        let terminal = server.app.state.workspaces[index]
            .terminal_id(server.app.state.workspaces[index].tabs[0].root_pane)
            .unwrap()
            .clone();
        server.app.terminal_runtimes.insert(
            terminal,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(100, 30, b"prompt"),
        );
    }
    let owner = connect_test_shell(&mut server, 10, 100, 30);
    let peer = connect_test_shell(&mut server, 20, 70, 20);
    let host_pane = server.app.state.workspaces[1].tabs[0].root_pane;
    let size = |server: &HeadlessServer, index, pane| {
        server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, index, pane)
            .unwrap()
            .current_size()
    };
    let host_size = size(&server, 1, host_pane);
    attention_request(&mut server, 10, view(Some(&source), 50, 12));
    attention_request(&mut server, 20, view(Some(&source), 40, 10));
    server.stream_attention_surfaces(false);
    assert_eq!(size(&server, 0, pane), (12, 50));
    assert_eq!(size(&server, 1, host_pane), host_size);
    while owner.1.try_recv().is_ok() {}
    while peer.1.try_recv().is_ok() {}
    server.stream_attention_surfaces(false);
    assert!(owner.1.try_recv().is_err(), "unchanged surface is deduped");
    let home_tab = server.app.public_tab_id(0, 0).unwrap();
    server.focus_shell_client_on_tab(20, &home_tab);
    server.claim_shell_tab_geometry(20, false);
    let native = size(&server, 0, pane);
    server.stream_attention_surfaces(false);
    assert_eq!(size(&server, 0, pane), native);
    let sent = server.clients[&10].attention_surface.as_ref().unwrap();
    assert_eq!(
        (sent.surface.frame.height, sent.surface.frame.width),
        native
    );
    assert_eq!(size(&server, 1, host_pane), host_size);
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}

#[cfg(unix)]
#[test]
fn attention_handoff_capture_preserves_queue_and_canonical_topology() {
    let (mut server, pane, source) = attention_fixture();
    let snapshot = serde_json::to_value(server.app.session_snapshot()).unwrap();
    let queue = client_shell_attention_queue(&server.app);
    let handoff = server.app.attention_handoff_state();
    assert_eq!(handoff.queue.len(), 1);
    assert_eq!(handoff.queue[0].source_pane_id, source);
    assert_eq!(client_shell_attention_queue(&server.app), queue);
    assert_eq!(
        serde_json::to_value(server.app.session_snapshot()).unwrap(),
        snapshot
    );
    assert_eq!(
        server
            .app
            .state
            .attention_target(pane)
            .unwrap()
            .public_pane_id,
        source
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn attention_hidden_input_is_leased_without_host_focus_and_released_on_close() {
    let (mut server, pane, source) = attention_fixture();
    let terminal = server
        .app
        .find_pane(pane)
        .unwrap()
        .1
        .attached_terminal_id
        .clone();
    let (runtime, mut input) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            0,
            b"\x1b[>3u",
            8,
        );
    server.app.terminal_runtimes.insert(terminal, runtime);
    let _channels = connect_test_shell(&mut server, 10, 100, 30);
    let focus = server.shell_focus_target(10);
    let foreground = server.foreground_client_id;
    let geometry = server.tab_geometry_controllers.clone();
    let key = crate::protocol::ClientPaneInputEvent::Key {
        code: crate::protocol::ClientKeyCode::Char('x'),
        modifiers: 0,
        kind: crate::protocol::ClientKeyKind::Press,
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: None,
        tracks_release: true,
        physical_key_id: Some(0x2d),
        windows_record: None,
    };
    server.handle_server_event(ServerEvent::ClientShellPaneInput {
        client_id: 10,
        pane_id: source.clone(),
        events: vec![key.clone()],
    });
    assert!(input.try_recv().is_err(), "unleased hidden input rejected");
    attention_request(&mut server, 10, view(Some(&source), 50, 12));
    server.handle_server_event(ServerEvent::ClientShellPaneInput {
        client_id: 10,
        pane_id: source.clone(),
        events: vec![key],
    });
    assert!(
        input.try_recv().is_ok(),
        "leased input reaches native terminal"
    );
    assert_eq!(server.shell_focus_target(10), focus);
    assert_eq!(server.foreground_client_id, foreground);
    assert_eq!(server.tab_geometry_controllers, geometry);
    attention_request(&mut server, 10, view(None, 0, 0));
    assert!(input.try_recv().is_ok(), "closing releases held native key");
    assert!(server
        .clients
        .get_mut(&10)
        .unwrap()
        .drain_shell_held_inputs()
        .is_empty());
    assert!(server.app.state.attention_target(pane).is_some());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn attention_full_render_backpressure_retries_without_committing_unsent_frame() {
    let (mut server, pane, source) = attention_fixture();
    let terminal = server
        .app
        .find_pane(pane)
        .unwrap()
        .1
        .attached_terminal_id
        .clone();
    server.app.terminal_runtimes.insert(
        terminal,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"approval"),
    );
    let channels = connect_test_shell(&mut server, 10, 100, 30);
    attention_request(&mut server, 10, view(Some(&source), 50, 12));
    server.clients[&10]
        .writer
        .as_ref()
        .unwrap()
        .render
        .try_send(vec![0])
        .unwrap();
    server.stream_attention_surfaces(false);
    assert!(server.clients[&10].attention_surface.is_none());
    assert_eq!(server.clients[&10].deferred_render(), DeferredRender::Full);
    channels.1.try_recv().unwrap();
    assert!(server.handle_server_event(ServerEvent::ClientWriterDrained { client_id: 10 }));
    server.stream_attention_surfaces(false);
    assert!(server.clients[&10].attention_surface.is_some());
    let message = read_server_message(channels.1.try_recv().unwrap());
    assert!(
        matches!(message, ServerMessage::EndpointControl { kind, .. } if kind == "attention.surface.v1")
    );
    server.stream_attention_surfaces(false);
    assert!(channels.1.try_recv().is_err());
    shutdown_test_runtimes(&mut server);
}

#[test]
fn attention_debounce_timer_enqueues_without_moving_source() {
    let (mut server, pane, _) = attention_fixture();
    server.app.state.remove_attention_entry(pane);
    server.app.state.observe_attention_transition(
        pane,
        crate::detect::AgentState::Working,
        crate::detect::AgentState::Blocked,
        true,
    );
    let deadline = server.app.state.next_attention_deadline().unwrap();
    assert!(client_shell_attention_queue(&server.app).is_empty());
    server.handle_scheduled_tasks_headless(deadline, false);
    assert_eq!(client_shell_attention_queue(&server.app).len(), 1);
    assert_eq!(server.app.find_pane(pane).unwrap().0, 0);
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn attention_same_source_reopen_echoes_new_epoch_even_when_cells_match() {
    let (mut server, pane, source) = attention_fixture();
    let terminal = server
        .app
        .find_pane(pane)
        .unwrap()
        .1
        .attached_terminal_id
        .clone();
    server.app.terminal_runtimes.insert(
        terminal,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"approval"),
    );
    let channels = connect_test_shell(&mut server, 10, 100, 30);
    for view_id in [7, 8] {
        let method = api::schema::Method::AttentionView(api::schema::AttentionViewParams {
            source_pane_id: Some(source.clone()),
            cols: 50,
            rows: 12,
            view_id,
        });
        assert!(attention_request(&mut server, 10, method).contains("\"result\""));
        server.stream_attention_surfaces(false);
        let ServerMessage::EndpointControl { data, .. } =
            read_server_message(channels.1.try_recv().unwrap())
        else {
            panic!("attention surface");
        };
        let surface: protocol::endpoint::EndpointAttentionSurface =
            serde_json::from_str(&data).unwrap();
        assert_eq!(surface.boot_id, server.client_shell_boot_id);
        assert_eq!(surface.view_id, view_id);
        assert_eq!(surface.source_pane_id, source);
        attention_request(&mut server, 10, view(None, 0, 0));
    }
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn attention_and_busy_host_both_progress_through_one_render_slot() {
    let (mut server, pane, source) = attention_fixture();
    let host = server.app.state.workspaces[1].tabs[0].root_pane;
    for pane in [pane, host] {
        let terminal = server
            .app
            .find_pane(pane)
            .unwrap()
            .1
            .attached_terminal_id
            .clone();
        server.app.terminal_runtimes.insert(
            terminal,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
    }
    let channels = connect_test_shell(&mut server, 10, 100, 30);
    attention_request(&mut server, 10, view(Some(&source), 50, 12));
    let mut attention_frames = 0;
    let mut host_frames = 0;
    for _ in 0..6 {
        for pane in [pane, host] {
            let (index, _) = server.app.find_pane(pane).unwrap();
            server
                .app
                .state
                .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, index, pane)
                .unwrap()
                .test_process_pty_bytes(b"x");
        }
        server.render_and_stream();
        match read_server_message(channels.1.try_recv().unwrap()) {
            ServerMessage::EndpointControl { kind, .. } if kind == "attention.surface.v1" => {
                attention_frames += 1
            }
            ServerMessage::PaneSurface(_) => host_frames += 1,
            other => panic!("unexpected surface: {other:?}"),
        }
        server.handle_server_event(ServerEvent::ClientWriterDrained { client_id: 10 });
    }
    assert!(attention_frames >= 2, "busy host cannot starve attention");
    assert!(host_frames >= 2, "busy attention cannot starve host");
    shutdown_test_runtimes(&mut server);
}

#[test]
fn attention_sidebar_status_stays_home_as_client_changes_workspaces() {
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

#[tokio::test(flavor = "current_thread")]
#[ignore = "manual leased-attention render scaling profile"]
async fn render_scale_profile_attention() {
    const SAMPLES: usize = 30;
    const WARMUP: usize = 5;
    let history: String = (0..1_000)
        .map(|line| format!("populated terminal line {line}\r\n"))
        .collect();
    println!("attention full-render profile: fixed 120x40 host, 80x24 lease, one client");
    println!("populated background panes exclude the fixed empty host pane");
    println!("populated_panes leased median_us p95_us median_vs_1x");
    for leased in [false, true] {
        let mut baseline_us = 1;
        for count in [1, 15] {
            let mut server = test_headless_server();
            server.app.state.workspaces = (0..count)
                .map(|index| crate::workspace::Workspace::test_new(&format!("source-{index}")))
                .collect();
            server
                .app
                .state
                .workspaces
                .push(crate::workspace::Workspace::test_new("fixed-host"));
            server.app.state.ensure_test_terminals();
            server.app.state.active = Some(count);
            server.app.state.selected = count;
            server.app.state.mode = crate::app::Mode::Terminal;
            for index in 0..count {
                let pane = server.app.state.workspaces[index].tabs[0].root_pane;
                let terminal = server
                    .app
                    .find_pane(pane)
                    .unwrap()
                    .1
                    .attached_terminal_id
                    .clone();
                server.app.terminal_runtimes.insert(
                    terminal,
                    crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                        120,
                        40,
                        1024 * 1024,
                        history.as_bytes(),
                    ),
                );
                queue_blocked(&mut server, pane);
            }
            let pane = server.app.state.workspaces[0].tabs[0].root_pane;
            let source = server
                .app
                .state
                .attention_target(pane)
                .unwrap()
                .public_pane_id;
            let channels = connect_test_shell(&mut server, 10, 120, 40);
            if leased {
                assert!(
                    attention_request(&mut server, 10, view(Some(&source), 80, 24))
                        .contains("\"result\"")
                );
            }
            let mut samples = Vec::with_capacity(SAMPLES);
            for sample in 0..WARMUP + SAMPLES {
                write_shared_test_pane(
                    &mut server,
                    pane,
                    format!("\rprogress {sample}\x1b[K").as_bytes(),
                );
                let started = Instant::now();
                server.render_and_stream();
                let elapsed = started.elapsed();
                if sample >= WARMUP {
                    samples.push(elapsed);
                }
                while let Ok(frame) = channels.1.try_recv() {
                    std::hint::black_box(frame);
                }
                while channels.0.try_recv().is_ok() {}
                server.handle_server_event(ServerEvent::ClientWriterDrained { client_id: 10 });
            }
            samples.sort_unstable();
            let median = samples[SAMPLES / 2].as_micros();
            let p95 = samples[(SAMPLES - 1) * 95 / 100].as_micros();
            if count == 1 {
                baseline_us = median.max(1);
            }
            println!(
                "{count:>15} {leased:>6} {median:>9} {p95:>6} {:>12.2}",
                median as f64 / baseline_us as f64
            );
            if leased {
                let surface = server.clients[&10]
                    .attention_surface
                    .as_ref()
                    .expect("leased surface delivered");
                assert_eq!(
                    (surface.surface.frame.width, surface.surface.frame.height),
                    (80, 24)
                );
            }
            server.app.state.assert_invariants_for_test();
            shutdown_test_runtimes(&mut server);
        }
    }
}
