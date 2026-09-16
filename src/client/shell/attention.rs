//! Client-owned floating presentation. Queue membership remains server-owned.
use super::*;
use crate::protocol::endpoint::{
    EndpointAttentionEntry, EndpointAttentionKind, EndpointAttentionSurface,
};
use crossterm::event::{KeyEventKind, MouseButton, MouseEventKind};
mod render;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct AttentionWidget {
    queues: HashMap<ClientEndpointId, Vec<EndpointAttentionEntry>>,
    selected: Option<String>,
    surface: Option<crate::protocol::ClientShellPopupSurface>,
    lease: Option<(String, u16, u16)>,
    view_id: u64,
    pub(super) row: Rect,
    terminal: Option<PaneHit>,
    buttons: Vec<(Rect, AttentionAction)>,
    pending_input: Vec<ClientMessage>,
    offset: (u16, u16),
    mouse_down: Option<AttentionMouseGesture>,
}

struct AttentionMouseGesture {
    pane_id: String,
    button: crate::protocol::ClientMouseButton,
    position: ClientMousePosition,
    modifiers: u8,
}

#[derive(Clone, Copy)]
enum AttentionAction {
    Previous,
    Next,
    Dismiss,
    Jump,
    Close,
}

pub(super) fn supported(endpoint: &ClientShellEndpoint) -> bool {
    endpoint.status == ClientEndpointStatus::Online
        && endpoint.methods.as_ref().is_some_and(|methods| {
            ["attention.view", "attention.acknowledge", "attention.jump"]
                .iter()
                .all(|method| methods.contains(*method))
        })
}

impl ClientShellState {
    pub(super) fn attention_open(&self) -> bool {
        self.attention_widget.selected.is_some()
    }

    pub(super) fn attention_selected(&self) -> Option<&str> {
        self.attention_widget.selected.as_deref()
    }

    pub(super) fn attention_view_id(&self) -> u64 {
        self.attention_widget.view_id
    }

    pub(super) fn attention_supported(&self) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .is_some_and(supported)
    }

    fn attention_queue(&self) -> &[EndpointAttentionEntry] {
        self.attention_widget
            .queues
            .get(&self.active_endpoint_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(crate) fn set_attention_queue(
        &mut self,
        endpoint: &ClientEndpointId,
        mut queue: Vec<EndpointAttentionEntry>,
    ) {
        queue.retain(|entry| entry.kind != EndpointAttentionKind::Unknown);
        self.attention_widget.queues.insert(endpoint.clone(), queue);
        if endpoint == &self.active_endpoint_id
            && self.attention_widget.selected.as_ref().is_some_and(|id| {
                !self
                    .attention_queue()
                    .iter()
                    .any(|entry| &entry.pane_id == id)
            })
        {
            // Invalidation hides rather than redirecting keys to a newly arrived pane.
            self.hide_attention();
        }
    }

    pub(crate) fn set_attention_surface(
        &mut self,
        endpoint: &ClientEndpointId,
        surface: EndpointAttentionSurface,
    ) -> bool {
        if endpoint != &self.active_endpoint_id
            || !self.attention_supported()
            || self
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.boot_id != surface.boot_id)
            || self.attention_widget.view_id != surface.view_id
            || self.attention_widget.selected.as_deref() != Some(surface.source_pane_id.as_str())
        {
            return false;
        }
        self.attention_widget.surface = Some(surface.surface);
        true
    }

    pub(super) fn hide_attention(&mut self) {
        if let Some(id) = self.attention_widget.selected.as_ref() {
            for lease in self
                .input_leases
                .remove_target(&ClientInputTarget::Pane(id.clone()))
            {
                let key = crate::input::InputLeaseKey::new(0, &lease.key);
                self.input_leases
                    .complete_press(key, &lease.key, None, None, None);
                if let Some(event) = ClientPaneInputEvent::from_terminal_key(
                    lease.key.with_kind(KeyEventKind::Release),
                ) {
                    self.attention_widget
                        .pending_input
                        .push(target_event_message(lease.target, event));
                }
            }
        }
        self.attention_widget.selected = None;
        self.attention_widget.surface = None;
        self.attention_widget.terminal = None;
        self.attention_widget.buttons.clear();
        self.attention_widget.mouse_down = None;
    }

    pub(super) fn clear_attention_endpoint(&mut self, endpoint: &ClientEndpointId) {
        self.attention_widget.queues.remove(endpoint);
        if endpoint == &self.active_endpoint_id {
            self.hide_attention();
            self.attention_widget.lease = None;
            self.attention_widget.pending_input.clear();
        }
    }

    pub(super) fn reset_attention_projection(&mut self) {
        self.hide_attention();
        self.attention_widget.lease = None;
        self.attention_widget.pending_input.clear();
        self.attention_widget.row = Rect::default();
    }

    fn attention_geometry(&self) -> Option<crate::popup_size::PopupResolvedGeometry> {
        let (cols, rows) = self.last_composed_size?;
        let mut geometry = crate::popup_size::resolve_popup_geometry(
            Some(crate::popup_size::PopupSize::Percent(85)),
            Some(crate::popup_size::PopupSize::Percent(85)),
            self.layout(cols, rows).pane_surface,
        )?;
        // Reserve a controls row inside the border, never in the host pane layout.
        geometry.inner.y += 1;
        geometry.inner.height = geometry.inner.height.saturating_sub(1);
        (!geometry.inner.is_empty()).then_some(geometry)
    }

    pub(crate) fn sync_attention_lease(&mut self, outcome: &mut ClientShellInput) {
        outcome
            .requests
            .append(&mut self.attention_widget.pending_input);
        if !self.attention_supported() {
            self.hide_attention();
            self.attention_widget.lease = None;
            return;
        }
        let desired = self.attention_widget.selected.clone().and_then(|id| {
            self.attention_geometry()
                .map(|g| (id, g.inner.width, g.inner.height))
        });
        if desired == self.attention_widget.lease {
            return;
        }
        let (source_pane_id, cols, rows) = desired
            .as_ref()
            .map(|(id, cols, rows)| (Some(id.clone()), *cols, *rows))
            .unwrap_or((None, 1, 1));
        self.attention_widget.view_id = self.attention_widget.view_id.saturating_add(1);
        let view_id = self.attention_widget.view_id;
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::AttentionView(crate::api::schema::AttentionViewParams {
                source_pane_id: source_pane_id.clone(),
                cols,
                rows,
                view_id,
            }),
            PendingEndpointKind::AttentionView {
                source_pane_id,
                view_id,
            },
            outcome,
        );
        self.attention_widget.lease = desired;
        outcome.repaint = true;
    }

    fn attention_action(&mut self, action: AttentionAction, outcome: &mut ClientShellInput) {
        let Some(id) = self.attention_widget.selected.clone() else {
            return;
        };
        match action {
            AttentionAction::Close => self.hide_attention(),
            AttentionAction::Previous | AttentionAction::Next => {
                let queue = self.attention_queue();
                if let Some(index) = queue.iter().position(|entry| entry.pane_id == id) {
                    let next = if matches!(action, AttentionAction::Next) {
                        (index + 1) % queue.len()
                    } else {
                        (index + queue.len() - 1) % queue.len()
                    };
                    let next = queue[next].pane_id.clone();
                    if next == id {
                        return;
                    }
                    self.hide_attention();
                    self.attention_widget.selected = Some(next);
                }
            }
            AttentionAction::Dismiss | AttentionAction::Jump => {
                let target = crate::api::schema::AttentionTarget { source_pane_id: id };
                let method = if matches!(action, AttentionAction::Dismiss) {
                    crate::api::schema::Method::AttentionAcknowledge(target)
                } else {
                    crate::api::schema::Method::AttentionJump(target)
                };
                self.push_endpoint_method(method, outcome);
                self.hide_attention();
            }
        }
        self.sync_attention_lease(outcome);
        outcome.repaint = true;
    }

    /// This gate precedes ordinary key leases, popup routing, and host pane mouse focus.
    pub(super) fn handle_attention_input(
        &mut self,
        event: &crate::raw_input::RawInputEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crate::raw_input::RawInputEvent;
        outcome
            .requests
            .append(&mut self.attention_widget.pending_input);
        if !self.attention_supported() {
            return false;
        }
        if let RawInputEvent::Mouse(mouse) = event {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && contains(self.attention_widget.row, (mouse.column, mouse.row))
            {
                if self.attention_open() {
                    self.hide_attention();
                } else if self.overlay.is_none()
                    && self.popup_terminal_id.is_none()
                    && !self.popup_pending
                {
                    self.release_input_leases(outcome);
                    self.copy_mode = None;
                    self.reset_copy_pipeline();
                    self.selection = None;
                    self.mode = ClientShellMode::Terminal;
                    self.attention_widget.selected = self
                        .attention_queue()
                        .first()
                        .map(|entry| entry.pane_id.clone());
                }
                self.sync_attention_lease(outcome);
                outcome.repaint = true;
                return true;
            }
        }
        let Some(id) = self.attention_widget.selected.clone() else {
            return false;
        };
        let input = match event {
            RawInputEvent::OuterFocusLost => {
                if let Some(gesture) = self.attention_widget.mouse_down.take() {
                    push_target_event(
                        ClientInputTarget::Pane(gesture.pane_id),
                        ClientPaneInputEvent::Mouse {
                            kind: crate::protocol::ClientMouseKind::Up(gesture.button),
                            position: gesture.position,
                            geometry: None,
                            modifiers: gesture.modifiers,
                            lines: self.config.mouse_scroll_lines.min(u16::MAX as usize) as u16,
                        },
                        outcome,
                    );
                }
                // The ordinary focus path must still release keys and notify the server.
                return false;
            }
            RawInputEvent::Key(key) if key.code == KeyCode::Esc => {
                if key.kind == KeyEventKind::Press {
                    self.attention_action(AttentionAction::Close, outcome);
                    self.input_leases.complete_press(
                        crate::input::InputLeaseKey::new(0, key),
                        key,
                        None,
                        None,
                        None,
                    );
                }
                return true;
            }
            RawInputEvent::Key(key) => {
                self.handle_key(key.clone(), outcome);
                return true;
            }
            RawInputEvent::Text(text) => {
                Some(ClientPaneInputEvent::TextCommit(text.as_str().to_owned()))
            }
            RawInputEvent::Paste(text) => Some(ClientPaneInputEvent::Paste(text.clone())),
            RawInputEvent::Mouse(mouse) => {
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    if let Some(action) = self
                        .attention_widget
                        .buttons
                        .iter()
                        .find(|(rect, _)| contains(*rect, (mouse.column, mouse.row)))
                        .map(|(_, action)| *action)
                    {
                        self.attention_action(action, outcome);
                        return true;
                    }
                }
                if let Some(hit) = self.attention_widget.terminal.as_ref().filter(|hit| {
                    !hit.inner_rect.is_empty()
                        && (contains(hit.inner_rect, (mouse.column, mouse.row))
                            || self.attention_widget.mouse_down.is_some()
                                && matches!(
                                    mouse.kind,
                                    MouseEventKind::Up(_) | MouseEventKind::Drag(_)
                                ))
                }) {
                    if hit.mouse_reporting {
                        if let Some(kind) =
                            crate::protocol::ClientMouseKind::from_crossterm(mouse.kind)
                        {
                            let position = ClientMousePosition::Cell {
                                column: mouse.column.clamp(
                                    hit.inner_rect.x,
                                    hit.inner_rect.right().saturating_sub(1),
                                ) - hit.inner_rect.x
                                    + self.attention_widget.offset.0,
                                row: mouse.row.clamp(
                                    hit.inner_rect.y,
                                    hit.inner_rect.bottom().saturating_sub(1),
                                ) - hit.inner_rect.y
                                    + self.attention_widget.offset.1,
                            };
                            match kind {
                                crate::protocol::ClientMouseKind::Down(button) => {
                                    self.attention_widget.mouse_down =
                                        Some(AttentionMouseGesture {
                                            pane_id: id.clone(),
                                            button,
                                            position,
                                            modifiers: mouse.modifiers.bits(),
                                        });
                                }
                                crate::protocol::ClientMouseKind::Up(_) => {
                                    self.attention_widget.mouse_down = None
                                }
                                _ => {
                                    if let Some(gesture) = self.attention_widget.mouse_down.as_mut()
                                    {
                                        gesture.position = position;
                                        gesture.modifiers = mouse.modifiers.bits();
                                    }
                                }
                            }
                            push_target_event(
                                ClientInputTarget::Pane(id),
                                ClientPaneInputEvent::Mouse {
                                    kind,
                                    position,
                                    geometry: None,
                                    modifiers: mouse.modifiers.bits(),
                                    lines: self.config.mouse_scroll_lines.min(u16::MAX as usize)
                                        as u16,
                                },
                                outcome,
                            );
                        }
                    }
                }
                return true;
            }
            _ => return false,
        };
        if let Some(input) = input {
            push_target_event(ClientInputTarget::Pane(id), input, outcome);
        }
        true
    }
}
