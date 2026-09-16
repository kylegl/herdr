//! Connection-local attention leases. The queue never changes pane topology.
use super::*;
use crate::server::clients::AttentionViewLease;

impl HeadlessServer {
    fn queued_attention_target(
        &self,
        source: &str,
    ) -> Option<crate::app::attention_dock::AttentionSourceTarget> {
        let (_, pane_id) = self.app.parse_pane_id(source)?;
        let target = self.app.state.attention_target(pane_id)?;
        (target.public_pane_id == source).then_some(target)
    }

    fn resolve_attention_view(
        &self,
        params: api::schema::AttentionViewParams,
    ) -> Option<AttentionViewLease> {
        if params.cols == 0 || params.rows == 0 || params.cols > 1000 || params.rows > 1000 {
            return None;
        }
        let source_pane_id = params.source_pane_id?;
        let target = self.queued_attention_target(&source_pane_id)?;
        let (_, pane) = self.app.find_pane(target.pane_id)?;
        Some(AttentionViewLease {
            source_pane_id,
            terminal_id: pane.attached_terminal_id.clone(),
            cols: params.cols.max(4),
            rows: params.rows.max(2),
            view_id: params.view_id,
        })
    }

    pub(super) fn attention_view_matches(&self, client_id: u64, source: &str) -> bool {
        self.clients.get(&client_id).is_some_and(|client| {
            client.is_active_shell_client()
                && client
                    .attention_view
                    .as_ref()
                    .is_some_and(|lease| lease.source_pane_id == source)
        }) && self.queued_attention_target(source).is_some_and(|target| {
            self.app.find_pane(target.pane_id).is_some_and(|(_, pane)| {
                self.clients.get(&client_id).and_then(|client| {
                    client
                        .attention_view
                        .as_ref()
                        .map(|lease| &lease.terminal_id)
                }) == Some(&pane.attached_terminal_id)
            })
        })
    }

    pub(super) fn clear_attention_view(&mut self, client_id: u64) -> bool {
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        let Some(lease) = client.attention_view.take() else {
            return false;
        };
        client.attention_surface = None;
        client.attention_render_pending = false;
        // Release all held presses before a different terminal can acquire this lease.
        let held = client.drain_shell_held_inputs();
        for input in held {
            if input.target != ClientShellInputTarget::Pane(lease.source_pane_id.clone()) {
                self.release_client_shell_inputs(client_id, vec![input]);
                continue;
            }
            // Terminal identity is pinned separately from a public pane that can be rebound.
            if let Some(runtime) = self.app.terminal_runtimes.get(&lease.terminal_id) {
                if let Err(err) = apply_client_pane_input_events(runtime, &[input.release]) {
                    warn!(client_id, %err, "attention release failed");
                }
            }
        }
        true
    }

    pub(super) fn reconcile_attention_views(&mut self) -> bool {
        let stale = self
            .clients
            .iter()
            .filter_map(|(&id, client)| {
                let lease = client.attention_view.as_ref()?;
                (!self.attention_view_matches(id, &lease.source_pane_id)).then_some(id)
            })
            .collect::<Vec<_>>();
        let changed = !stale.is_empty();
        for id in stale {
            self.clear_attention_view(id);
        }
        changed
    }

    pub(super) fn handle_client_shell_attention_request(
        &mut self,
        client_id: u64,
        msg: api::ApiRequestMessage,
    ) -> bool {
        use api::schema::{ErrorBody, ErrorResponse, Method, ResponseResult, SuccessResponse};
        let mut changed = false;
        let result = if self.handoff_in_progress
            || !self
                .clients
                .get(&client_id)
                .is_some_and(ClientConnection::is_active_shell_client)
        {
            Err("attention view requires an active connection")
        } else {
            match msg.request.method {
                Method::AttentionView(params) => {
                    if params.source_pane_id.is_none() {
                        changed = self.clear_attention_view(client_id);
                        Ok(())
                    } else if let Some(lease) = self.resolve_attention_view(params) {
                        if self
                            .clients
                            .get(&client_id)
                            .and_then(|client| client.attention_view.as_ref())
                            != Some(&lease)
                        {
                            self.clear_attention_view(client_id);
                            if let Some(client) = self.clients.get_mut(&client_id) {
                                client.attention_view = Some(lease);
                            }
                            changed = true;
                        }
                        Ok(())
                    } else {
                        Err("attention target or dimensions are stale")
                    }
                }
                Method::AttentionAcknowledge(params) => {
                    if let Some(target) = self.queued_attention_target(&params.source_pane_id) {
                        changed = self.app.state.dismiss_attention(target.pane_id);
                        self.reconcile_attention_views();
                        Ok(())
                    } else {
                        Err("attention target is no longer queued")
                    }
                }
                Method::AttentionJump(params) => {
                    if let Some(target) = self.queued_attention_target(&params.source_pane_id) {
                        let focus_before = self.shell_focus_targets();
                        let tabs_before = self.focused_shell_tabs();
                        self.clear_attention_view(client_id);
                        let tab_id = crate::workspace::public_tab_id_for_number(
                            &target.workspace_id,
                            target.tab_number,
                        );
                        if let Some((workspace_index, tab_index)) = self.app.parse_tab_id(&tab_id) {
                            self.app.state.workspaces[workspace_index].tabs[tab_index]
                                .layout
                                .focus_pane(target.pane_id);
                        }
                        if let Some(client) = self.clients.get_mut(&client_id) {
                            client
                                .shell_location
                                .get_or_insert_with(Default::default)
                                .focus_tab(target.workspace_id, tab_id);
                        }
                        self.finish_shell_location_reconciliation(focus_before, &tabs_before);
                        self.claim_shell_tab_geometry(client_id, false);
                        changed = true;
                        Ok(())
                    } else {
                        Err("attention target is no longer queued")
                    }
                }
                // The old methods named the currently projected dock, not an arbitrary queue item.
                Method::AttentionOpen(_) | Method::AttentionDismiss(_) => {
                    Err("the attention dock is no longer projected")
                }
                _ => return false,
            }
        };
        let response = match result {
            Ok(()) => serde_json::to_string(&SuccessResponse {
                id: msg.request.id,
                result: ResponseResult::Ok {},
            }),
            Err(message) => serde_json::to_string(&ErrorResponse {
                id: msg.request.id,
                error: ErrorBody {
                    code: "stale_attention".into(),
                    message: message.into(),
                },
            }),
        };
        if let Ok(response) = response {
            let _ = msg.respond_to.send(response);
        }
        changed
    }

    pub(super) fn stream_attention_surfaces(&mut self, before_host: bool) {
        self.reconcile_attention_views();
        let mut leases = self
            .clients
            .iter()
            .filter_map(|(&id, client)| {
                let lease = client.attention_view.as_ref()?;
                (client.is_active_shell_client() && client.writer.is_some())
                    .then_some((id, lease.clone()))
            })
            .collect::<Vec<_>>();
        // Resize each hidden terminal through its elected viewer before peers draw it.
        leases.sort_by_key(|(id, _)| *id);
        let mut disconnected = Vec::new();
        for (client_id, lease) in &leases {
            let source = &lease.source_pane_id;
            if self.clients[client_id].attention_render_priority != before_host {
                continue;
            }
            let Some((workspace_index, pane_id)) = self.app.parse_pane_id(source) else {
                continue;
            };
            let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
                &self.app.terminal_runtimes,
                workspace_index,
                pane_id,
            ) else {
                continue;
            };
            let visible = self.clients.iter().any(|(&id, client)| {
                client.is_active_shell_client()
                    && self.shell_client_views_pane(id, workspace_index, pane_id)
            });
            let owner = leases
                .iter()
                .filter(|(_, other)| other.source_pane_id == *source)
                .map(|(id, _)| *id)
                .min();
            let locked = self.app.find_pane(pane_id).is_some_and(|(_, pane)| {
                self.app
                    .state
                    .direct_attach_resize_locks
                    .contains(&pane.attached_terminal_id)
            });
            if !visible
                && !locked
                && owner == Some(*client_id)
                && runtime.current_size() != (lease.rows, lease.cols)
            {
                runtime.resize(lease.rows, lease.cols, 0, 0);
            }
            if runtime.synchronized_output_active() {
                continue;
            }
            let (native_rows, native_cols) = runtime.current_size();
            let area = Rect::new(0, 0, native_cols, native_rows);
            let (buffer, cursor) =
                crate::server::render_stream::render_terminal_virtual(runtime, area);
            let hyperlinks = runtime.visible_hyperlinks(area);
            let surface = protocol::endpoint::EndpointAttentionSurface {
                boot_id: self.client_shell_boot_id.clone(),
                view_id: lease.view_id,
                source_pane_id: source.clone(),
                surface: protocol::ClientShellPopupSurface {
                    terminal_id: lease.terminal_id.to_string(),
                    title: source.clone(),
                    width: None,
                    height: None,
                    frame: FrameData::from_ratatui_buffer_with_hyperlinks(
                        &buffer,
                        cursor,
                        &hyperlinks,
                    ),
                    mouse_reporting: runtime.mouse_reporting_enabled(),
                    // Attention clients may crop the native frame. Cell coordinates remain exact.
                    sgr_pixel_mouse: false,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            };
            let Some(client) = self.clients.get_mut(client_id) else {
                continue;
            };
            if client.attention_surface.as_ref() == Some(&surface) {
                client.attention_render_pending = false;
                continue;
            }
            let message = match serde_json::to_string(&surface) {
                Ok(data) => ServerMessage::EndpointControl {
                    kind: protocol::endpoint::ATTENTION_SURFACE_KIND.into(),
                    data,
                },
                Err(err) => {
                    warn!(%err, "failed to encode attention surface");
                    continue;
                }
            };
            let framed = match Self::frame_server_message(&message) {
                Ok(framed) => framed,
                Err(err) => {
                    warn!(%err, "failed to frame attention surface");
                    continue;
                }
            };
            if let Some(writer) = &client.writer {
                match writer.render.try_send(framed) {
                    Ok(()) => {
                        client.attention_surface = Some(surface);
                        client.attention_render_pending = false;
                    }
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {
                        client.attention_render_pending = true;
                        client.defer_full_render();
                    }
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        disconnected.push(*client_id)
                    }
                }
            }
        }
        for client_id in disconnected {
            self.remove_client_and_resize_if_needed(client_id);
        }
    }
}
