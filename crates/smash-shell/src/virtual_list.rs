use crate::theme::SmashTheme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Scrollbar;
use ratatui::widgets::ScrollbarOrientation;
use ratatui::widgets::ScrollbarState;
use std::rc::Rc;
use sycamore_reactive::*;

const SCROLLBAR_WIDTH: u16 = 1;

pub trait VirtualListItem {
    fn height(&self, width: u16) -> u16;
    fn render(&self, frame: &mut Frame, area: Rect, theme: &SmashTheme);
}

#[derive(Clone)]
pub struct VirtualList<T: VirtualListItem + 'static> {
    items: Rc<Vec<T>>,
    cum_heights: Vec<u16>,
    total_height: u16,
    pub scroll_offset: Signal<u16>,
    pub show_scrollbar: bool,
    width: u16,
}

impl<T: VirtualListItem + 'static> VirtualList<T> {
    pub fn new(items: Vec<T>, width: u16) -> Self {
        let items = Rc::new(items);
        let (cum_heights, total_height) = Self::compute_heights(&items, width);
        VirtualList {
            items,
            cum_heights,
            total_height,
            scroll_offset: create_signal(0),
            show_scrollbar: true,
            width,
        }
    }

    fn compute_heights(items: &[T], width: u16) -> (Vec<u16>, u16) {
        let mut cum = Vec::with_capacity(items.len());
        let mut total = 0u16;
        for item in items {
            cum.push(total);
            total = total.saturating_add(item.height(width));
        }
        (cum, total)
    }

    pub fn rebuild(&mut self, width: u16) {
        self.width = width;
        let (cum, total) = Self::compute_heights(&self.items, width);
        self.cum_heights = cum;
        self.total_height = total;
        let max_scroll = self.total_height.saturating_sub(1);
        let clamped = self.scroll_offset.get().min(max_scroll);
        self.scroll_offset.set(clamped);
    }

    pub fn count(&self) -> usize {
        self.items.len()
    }

    pub fn total_height(&self) -> u16 {
        self.total_height
    }

    fn index_at(&self, row: u16) -> usize {
        if self.cum_heights.is_empty() {
            return 0;
        }
        self.cum_heights
            .partition_point(|&h| h <= row)
            .saturating_sub(1)
    }

    fn item_height(&self, index: usize) -> u16 {
        if index >= self.items.len() {
            return 0;
        }
        if index + 1 < self.cum_heights.len() {
            self.cum_heights[index + 1] - self.cum_heights[index]
        } else {
            self.total_height - self.cum_heights[index]
        }
    }

    pub fn scroll_to(&self, offset: u16) {
        let max_scroll = self.total_height.saturating_sub(1);
        self.scroll_offset.set(offset.min(max_scroll));
    }

    pub fn scroll_by(&self, delta: i16, viewport: u16) {
        let current = self.scroll_offset.get();
        let Some(max_scroll) = self.total_height.checked_sub(viewport) else {
            return;
        };
        let new = if delta > 0 {
            current.saturating_add(delta as u16).min(max_scroll)
        } else {
            current.saturating_sub(delta.unsigned_abs())
        };
        self.scroll_offset.set(new);
    }

    pub fn scroll_to_bottom(&self, viewport: u16) {
        let Some(max_scroll) = self.total_height.checked_sub(viewport) else {
            return;
        };
        self.scroll_offset.set(max_scroll);
    }

    pub fn scroll_to_top(&self) {
        self.scroll_offset.set(0);
    }

    pub fn is_at_top(&self) -> bool {
        self.scroll_offset.get() == 0 || self.total_height == 0
    }

    pub fn is_at_bottom(&self, viewport: u16) -> bool {
        let Some(max_scroll) = self.total_height.checked_sub(viewport) else {
            return true;
        };
        self.scroll_offset.get() >= max_scroll
    }

    /// Returns the maximum scroll offset for a given viewport.
    pub fn max_scroll(&self, viewport: u16) -> u16 {
        self.total_height.saturating_sub(viewport)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &SmashTheme) {
        let viewport_height = area.height;
        if self.items.is_empty() || viewport_height == 0 {
            return;
        }

        // Clamp scroll offset to the viewport-aware maximum so the bottom
        // of the last item sits exactly at the bottom of the viewport.
        let max_valid = self.total_height.saturating_sub(viewport_height);
        let offset = self.scroll_offset.get().min(max_valid);

        let scrollbar_shrink = if self.show_scrollbar {
            SCROLLBAR_WIDTH
        } else {
            0
        };
        let content_width = area.width.saturating_sub(scrollbar_shrink);

        let start_idx = self.index_at(offset);
        let y_into_item = offset - self.cum_heights[start_idx];
        let mut y: i16 = -(y_into_item as i16);

        for i in start_idx..self.items.len() {
            let h = self.item_height(i) as i16;
            let end_y = y + h;

            if y >= viewport_height as i16 {
                break;
            }

            if end_y > 0 {
                let visible_y = y.max(0) as u16;
                let visible_end = end_y.min(viewport_height as i16) as u16;
                let visible_h = visible_end.saturating_sub(visible_y);

                if visible_h > 0 {
                    let item_area = Rect::new(area.x, area.y + visible_y, content_width, visible_h);
                    self.items[i].render(frame, item_area, theme);
                }
            }

            y = end_y;
        }

        if self.show_scrollbar && viewport_height > 0 {
            let scrollbar_area = Rect::new(
                area.x + content_width,
                area.y,
                SCROLLBAR_WIDTH,
                viewport_height,
            );
            let max = self.max_scroll(viewport_height);
            if max > 0 {
                let pos = offset.min(max);
                let mut state = ScrollbarState::new(max as usize).position(pos as usize);
                frame.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight)
                        .begin_style(ratatui::style::Style::default().fg(theme.surface_variant))
                        .end_style(ratatui::style::Style::default().fg(theme.surface_variant))
                        .style(ratatui::style::Style::default().fg(theme.surface_variant))
                        .thumb_style(ratatui::style::Style::default().fg(theme.primary)),
                    scrollbar_area,
                    &mut state,
                );
            }
        }
    }
}
