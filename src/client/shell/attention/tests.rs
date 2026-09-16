use super::*;
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyModifiers, MouseEvent};

fn entry(id: &str, kind: EndpointAttentionKind) -> EndpointAttentionEntry {
    EndpointAttentionEntry {
        pane_id: id.into(),
        source_workspace_id: "source-workspace".into(),
        source_tab_id: "source-tab".into(),
        title: id.into(),
        kind,
    }
}

fn state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(super::super::tests::snapshot()));
    state.set_endpoint_methods(Some(
        ["attention.view", "attention.acknowledge", "attention.jump"]
            .map(str::to_owned)
            .to_vec(),
    ));
    state.last_composed_size = Some((120, 40));
    state.set_attention_queue(
        &ClientEndpointId::Local,
        vec![entry("source-a", EndpointAttentionKind::Done)],
    );
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 40));
    state.attention_widget.row = state.render_attention_row(&mut buffer, state.layout(120, 40));
    state
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> RawInputEvent {
    RawInputEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn open(state: &mut ClientShellState) -> ClientShellInput {
    let row = state.attention_widget.row;
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        row.x,
        row.y,
    )])
}

fn surface(state: &ClientShellState, id: &str) -> EndpointAttentionSurface {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 180, 80));
    buffer[(175, 75)].set_symbol("P");
    EndpointAttentionSurface {
        boot_id: "boot-1".into(),
        view_id: state.attention_view_id(),
        source_pane_id: id.into(),
        surface: crate::protocol::ClientShellPopupSurface {
            terminal_id: "not-the-pane-id".into(),
            title: "agent".into(),
            width: None,
            height: None,
            frame: FrameData::from_ratatui_buffer_with_hyperlinks(
                &buffer,
                Some(crate::protocol::CursorState {
                    x: 175,
                    y: 75,
                    visible: true,
                    shape: 2,
                }),
                &[],
            ),
            mouse_reporting: true,
            sgr_pixel_mouse: true,
            pixel_width: 1800,
            pixel_height: 1600,
        },
    }
}

fn methods(outcome: &ClientShellInput) -> Vec<&crate::api::schema::Method> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(&request.method),
            _ => None,
        })
        .collect()
}

#[test]
fn arrivals_never_open_or_replace_selected_identity_and_geometry_stays_fixed() {
    let mut state = state();
    assert!(!state.attention_open());
    let geometry = state.surface_size(120, 40);
    let host_snapshot = state.snapshot.clone();
    let opened = open(&mut state);
    assert!(
        matches!(methods(&opened).as_slice(), [crate::api::schema::Method::AttentionView(params)] if params.source_pane_id.as_deref() == Some("source-a"))
    );
    state.set_attention_queue(
        &ClientEndpointId::Local,
        vec![
            entry("source-b", EndpointAttentionKind::Blocked),
            entry("source-a", EndpointAttentionKind::Done),
        ],
    );
    assert_eq!(state.attention_selected(), Some("source-a"));
    let mut update = ClientShellInput::default();
    state.sync_attention_lease(&mut update);
    assert!(update.actions.is_empty());
    assert_eq!(state.surface_size(120, 40), geometry);
    assert_eq!(state.snapshot, host_snapshot);
    state.attention_action(AttentionAction::Next, &mut update);
    assert_eq!(state.attention_selected(), Some("source-b"));
    assert!(!state.set_attention_surface(&ClientEndpointId::Local, surface(&state, "source-a")));
    assert!(state.set_attention_surface(&ClientEndpointId::Local, surface(&state, "source-b")));
}

#[test]
fn escape_hides_without_acknowledging_and_held_repeats_cannot_reach_host() {
    let mut state = state();
    open(&mut state);
    let key = crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::NONE);
    let typed = state.handle_raw_events(vec![RawInputEvent::Key(key.clone())]);
    assert!(
        matches!(typed.requests.as_slice(), [ClientMessage::ClientShellPaneInput { pane_id, .. }] if pane_id == "source-a")
    );
    let closed = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    ))]);
    assert!(!state.attention_open());
    assert!(
        matches!(methods(&closed).as_slice(), [crate::api::schema::Method::AttentionView(params)] if params.source_pane_id.is_none())
    );
    assert_eq!(state.attention_queue().len(), 1);
    let repeated = state.handle_raw_events(vec![RawInputEvent::Key(
        key.with_kind(KeyEventKind::Repeat),
    )]);
    assert!(repeated.requests.is_empty());
}

#[test]
fn mouse_and_paste_target_source_while_native_frame_crop_tracks_cursor() {
    let mut state = state();
    open(&mut state);
    state.set_attention_surface(&ClientEndpointId::Local, surface(&state, "source-a"));
    let mut frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::empty(Rect::new(0, 0, 120, 40)),
        None,
        &[],
    );
    state.compose_attention(&mut frame);
    let cursor = frame.cursor.as_ref().unwrap();
    assert_eq!(
        frame.to_ratatui_buffer().unwrap()[(cursor.x, cursor.y)].symbol(),
        "P"
    );
    let click = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        cursor.x,
        cursor.y,
    )]);
    assert!(
        matches!(click.requests.as_slice(), [ClientMessage::ClientShellPaneInput { pane_id, events }] if pane_id == "source-a" && matches!(events.as_slice(), [ClientPaneInputEvent::Mouse { position: ClientMousePosition::Cell {column: 175, row: 75}, geometry: None, .. }]))
    );
    let outside = state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        119,
        39,
    )]);
    assert!(outside.requests.is_empty());
    assert!(outside.actions.is_empty());
    let paste = state.handle_raw_events(vec![RawInputEvent::Paste("approval".into())]);
    assert!(
        matches!(paste.requests.as_slice(), [ClientMessage::ClientShellPaneInput { pane_id, events }] if pane_id == "source-a" && matches!(events.as_slice(), [ClientPaneInputEvent::Paste(text)] if text == "approval"))
    );
    assert_eq!(state.clipboard_image_target(), None);
}

#[test]
fn dismiss_and_jump_are_explicit_targeted_actions() {
    for action in [AttentionAction::Dismiss, AttentionAction::Jump] {
        let mut state = state();
        open(&mut state);
        let mut outcome = ClientShellInput::default();
        state.attention_action(action, &mut outcome);
        let methods = methods(&outcome);
        match action {
            AttentionAction::Dismiss => assert!(
                matches!(methods[0], crate::api::schema::Method::AttentionAcknowledge(target) if target.source_pane_id == "source-a")
            ),
            AttentionAction::Jump => assert!(
                matches!(methods[0], crate::api::schema::Method::AttentionJump(target) if target.source_pane_id == "source-a")
            ),
            _ => unreachable!(),
        }
        assert!(!state.attention_open());
    }
}

#[test]
fn invalidation_and_disconnect_clear_view_without_selecting_another_entry() {
    let mut state = state();
    open(&mut state);
    state.set_attention_queue(
        &ClientEndpointId::Local,
        vec![entry("source-b", EndpointAttentionKind::Blocked)],
    );
    assert!(!state.attention_open());
    let mut outcome = ClientShellInput::default();
    state.sync_attention_lease(&mut outcome);
    assert!(
        matches!(methods(&outcome).as_slice(), [crate::api::schema::Method::AttentionView(params)] if params.source_pane_id.is_none())
    );
    open(&mut state);
    state.mark_endpoint_disconnected(&ClientEndpointId::Local);
    assert!(!state.attention_open());
    assert!(state.attention_queue().is_empty());
    assert!(!state.set_attention_surface(&ClientEndpointId::Local, surface(&state, "source-b")));
}

#[test]
fn absent_methods_hide_only_attention_and_row_uses_colored_counts() {
    let mut state = state();
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 40));
    let row = state.render_attention_row(&mut buffer, state.layout(120, 40));
    let text: String = (row.x..row.right())
        .map(|x| buffer[(x, row.y)].symbol())
        .collect();
    assert_eq!(text.trim(), "Attention ● 0 ● 1");
    assert_eq!(buffer[(row.x + 10, row.y)].fg, state.config.palette.red);
    assert_eq!(buffer[(row.x + 14, row.y)].fg, state.config.palette.green);
    state.set_endpoint_methods(Some(vec!["attention.view".into()]));
    assert!(state
        .render_attention_row(&mut buffer, state.layout(120, 40))
        .is_empty());
    assert!(state.endpoint_is_online(&ClientEndpointId::Local));
}

#[test]
fn reopen_same_source_rejects_old_epoch_and_other_boot() {
    let mut state = state();
    open(&mut state);
    let stale = surface(&state, "source-a");
    state.attention_action(AttentionAction::Close, &mut ClientShellInput::default());
    open(&mut state);
    assert!(!state.set_attention_surface(&ClientEndpointId::Local, stale));
    let mut wrong_boot = surface(&state, "source-a");
    wrong_boot.boot_id = "retired-boot".into();
    assert!(!state.set_attention_surface(&ClientEndpointId::Local, wrong_boot));
    assert!(state.set_attention_surface(&ClientEndpointId::Local, surface(&state, "source-a")));
}

#[test]
fn singleton_navigation_retains_cached_surface_without_requesting_a_new_lease() {
    let mut state = state();
    open(&mut state);
    assert!(state.set_attention_surface(&ClientEndpointId::Local, surface(&state, "source-a")));
    let cached = state.attention_widget.surface.clone();
    let view_id = state.attention_view_id();
    for action in [AttentionAction::Previous, AttentionAction::Next] {
        let mut outcome = ClientShellInput::default();
        state.attention_action(action, &mut outcome);
        assert_eq!(state.attention_widget.surface, cached);
        assert_eq!(state.attention_selected(), Some("source-a"));
        assert_eq!(state.attention_view_id(), view_id);
        assert!(outcome.actions.is_empty());
        assert!(outcome.requests.is_empty());
    }
}

#[test]
fn focus_loss_releases_attention_mouse_at_last_translated_source_position() {
    let mut state = state();
    open(&mut state);
    assert!(state.set_attention_surface(&ClientEndpointId::Local, surface(&state, "source-a")));
    let mut frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::empty(Rect::new(0, 0, 120, 40)),
        None,
        &[],
    );
    state.compose_attention(&mut frame);
    let cursor = frame.cursor.as_ref().unwrap();
    state.handle_raw_events(vec![
        mouse(MouseEventKind::Down(MouseButton::Left), cursor.x, cursor.y),
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            cursor.x - 2,
            cursor.y - 1,
        ),
    ]);
    // Reprojection must not change the coordinates of the outstanding native gesture.
    state.attention_widget.offset = (0, 0);
    let lost = state.handle_raw_events(vec![RawInputEvent::OuterFocusLost]);
    assert!(matches!(lost.requests.as_slice(), [
        ClientMessage::ClientShellPaneInput { pane_id, events },
        ClientMessage::ClientShellFocus { focused: false },
    ] if pane_id == "source-a" && matches!(events.as_slice(), [ClientPaneInputEvent::Mouse {
        kind: crate::protocol::ClientMouseKind::Up(crate::protocol::ClientMouseButton::Left),
        position: ClientMousePosition::Cell { column: 173, row: 74 },
        geometry: None, ..
    }])));
    assert!(state.attention_widget.mouse_down.is_none());
    assert!(state.attention_open());
    let repeated = state.handle_raw_events(vec![RawInputEvent::OuterFocusLost]);
    assert!(matches!(
        repeated.requests.as_slice(),
        [ClientMessage::ClientShellFocus { focused: false }]
    ));
}
