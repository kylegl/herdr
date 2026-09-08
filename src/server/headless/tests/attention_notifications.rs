use super::*;

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
