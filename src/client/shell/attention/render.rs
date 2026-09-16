use super::*;
use ratatui::widgets::{Block, Borders, Clear, Widget};

impl ClientShellState {
    pub(in crate::client::shell) fn render_attention_row(
        &self,
        buffer: &mut Buffer,
        layout: ClientShellLayout,
    ) -> Rect {
        if !self.attention_supported() {
            return Rect::default();
        }
        let palette = &self.config.palette;
        if !self.sidebar_collapsed && layout.sidebar.height > 0 {
            let row = Rect::new(
                layout.sidebar.x,
                layout.sidebar.bottom() - 1,
                layout.sidebar.width.saturating_sub(1),
                1,
            );
            Clear.render(row, buffer);
            let blocked = self
                .attention_queue()
                .iter()
                .filter(|entry| entry.kind == EndpointAttentionKind::Blocked)
                .count();
            let done = self
                .attention_queue()
                .iter()
                .filter(|entry| entry.kind == EndpointAttentionKind::Done)
                .count();
            ratatui::widgets::Paragraph::new(ratatui::text::Line::from(vec![
                ratatui::text::Span::raw("Attention "),
                ratatui::text::Span::styled("●", Style::default().fg(palette.red)),
                ratatui::text::Span::raw(format!(" {blocked} ")),
                ratatui::text::Span::styled("●", Style::default().fg(palette.green)),
                ratatui::text::Span::raw(format!(" {done}")),
            ]))
            .style(Style::default().fg(palette.text).bg(palette.sidebar_bg))
            .render(row, buffer);
            return row;
        }
        Rect::default()
    }

    pub(in crate::client::shell) fn compose_attention(&mut self, frame: &mut FrameData) {
        self.attention_widget.buttons.clear();
        self.attention_widget.terminal = None;
        if !self.attention_supported() {
            return;
        }
        let Some(id) = self.attention_widget.selected.as_ref() else {
            return;
        };
        let Some(geometry) = self.attention_geometry() else {
            return;
        };
        let Some(mut buffer) = frame.to_ratatui_buffer() else {
            return;
        };
        let palette = &self.config.palette;
        let title = self
            .attention_queue()
            .iter()
            .find(|entry| &entry.pane_id == id)
            .map(|entry| entry.title.as_str())
            .unwrap_or("Attention");
        Clear.render(geometry.outer, &mut buffer);
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(palette.accent))
            .style(Style::default().bg(palette.panel_bg))
            .render(geometry.outer, &mut buffer);
        let mut x = geometry.inner.x;
        for (label, action) in [
            (" Prev ", AttentionAction::Previous),
            (" Next ", AttentionAction::Next),
            (" Dismiss ", AttentionAction::Dismiss),
            (" Jump ", AttentionAction::Jump),
            (" Close ", AttentionAction::Close),
        ] {
            let width = (label.len() as u16).min(geometry.outer.right().saturating_sub(x + 1));
            let rect = Rect::new(x, geometry.inner.y - 1, width, 1);
            super::super::render::put_text(
                &mut buffer,
                rect.x,
                rect.y,
                rect.width,
                label,
                Style::default().fg(palette.accent),
            );
            self.attention_widget.buttons.push((rect, action));
            x += width;
        }
        frame.replace_from_ratatui_buffer_preserving_effects(&buffer, None);
        if let Some(surface) = self.attention_widget.surface.as_ref() {
            self.attention_widget.offset =
                blit_attention_surface(frame, &surface.frame, geometry.inner);
            self.attention_widget.terminal = Some(PaneHit {
                rect: geometry.outer,
                inner_rect: Rect::new(
                    geometry.inner.x,
                    geometry.inner.y,
                    geometry.inner.width.min(surface.frame.width),
                    geometry.inner.height.min(surface.frame.height),
                ),
                scrollbar_rect: None,
                scroll: None,
                pane_id: id.clone(),
                popup: false,
                mouse_reporting: surface.mouse_reporting,
                sgr_pixel_mouse: surface.sgr_pixel_mouse,
                pixel_width: surface.pixel_width,
                pixel_height: surface.pixel_height,
            });
        }
    }
}

fn blit_attention_surface(target: &mut FrameData, source: &FrameData, area: Rect) -> (u16, u16) {
    let width = source.width.min(area.width);
    let height = source.height.min(area.height);
    let max_x = source.width.saturating_sub(width);
    let max_y = source.height.saturating_sub(height);
    let (x, y) = source
        .cursor
        .as_ref()
        .filter(|cursor| cursor.visible)
        .map_or((0, max_y), |cursor| {
            (
                cursor.x.saturating_sub(width.saturating_sub(1)).min(max_x),
                cursor.y.saturating_sub(height.saturating_sub(1)).min(max_y),
            )
        });
    let hyperlink_base = target.hyperlinks.len() as u32;
    target.hyperlinks.extend(source.hyperlinks.iter().cloned());
    for row in 0..height {
        for col in 0..width {
            let source_index =
                usize::from(row + y) * usize::from(source.width) + usize::from(col + x);
            let target_index =
                usize::from(row + area.y) * usize::from(target.width) + usize::from(col + area.x);
            if let (Some(from), Some(to)) = (
                source.cells.get(source_index),
                target.cells.get_mut(target_index),
            ) {
                *to = from.clone();
                to.hyperlink = from
                    .hyperlink
                    .filter(|index| (*index as usize) < source.hyperlinks.len())
                    .map(|index| hyperlink_base + index);
            }
        }
    }
    target.cursor = source
        .cursor
        .as_ref()
        .filter(|cursor| {
            cursor.x >= x && cursor.y >= y && cursor.x < x + width && cursor.y < y + height
        })
        .map(|cursor| crate::protocol::CursorState {
            x: area.x + cursor.x - x,
            y: area.y + cursor.y - y,
            visible: cursor.visible,
            shape: cursor.shape,
        });
    (x, y)
}
