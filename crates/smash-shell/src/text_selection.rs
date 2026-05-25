use crate::events::SmashEvent;
use crate::theme::SmashTheme;
use arboard::Clipboard;
use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::prelude::*;
use std::sync::{Arc, Mutex};
use sycamore_reactive::*;

pub type TextPos = (u16, u16);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SelectionAction {
    None,
    Changed,
    Finished,
}

#[derive(Clone)]
pub struct TextSelection {
    anchor: Signal<Option<TextPos>>,
    focus: Signal<Option<TextPos>>,
    clipboard: Signal<Option<Arc<Mutex<Clipboard>>>>,
}

impl Default for TextSelection {
    fn default() -> Self {
        Self::new()
    }
}

impl TextSelection {
    pub fn new() -> Self {
        TextSelection {
            anchor: create_signal(None),
            focus: create_signal(None),
            clipboard: create_signal(Clipboard::new().ok().map(|c| Arc::new(Mutex::new(c)))),
        }
    }

    pub fn handle_mouse_event(&self, event: &SmashEvent, area: Rect) -> SelectionAction {
        let SmashEvent::Mouse(mouse) = event else {
            return SelectionAction::None;
        };
        let rel = (
            mouse.row.saturating_sub(area.y),
            mouse.column.saturating_sub(area.x),
        );
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.anchor.set(Some(rel));
                self.focus.set(Some(rel));
                SelectionAction::Changed
            }
            MouseEventKind::Drag(MouseButton::Left) if self.anchor.get().is_some() => {
                self.focus.set(Some(rel));
                SelectionAction::Changed
            }
            MouseEventKind::Up(MouseButton::Left) if self.anchor.get().is_some() => {
                SelectionAction::Finished
            }
            _ => SelectionAction::None,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &SmashTheme) {
        let Some(a) = self.anchor.get() else { return };
        let Some(f) = self.focus.get() else { return };
        let (top_row, top_col, bottom_row, bottom_col) = if a.0 <= f.0 {
            (a.0, a.1, f.0, f.1)
        } else {
            (f.0, f.1, a.0, a.1)
        };
        let right = area.width.saturating_sub(1);
        let buf = frame.buffer_mut();
        for row in top_row..=bottom_row {
            let sy = area.y + row;
            if sy >= buf.area.height {
                continue;
            }
            let (c0, c1) = if top_row == bottom_row {
                (top_col.min(bottom_col), top_col.max(bottom_col))
            } else if row == top_row {
                (top_col, right)
            } else if row == bottom_row {
                (0, bottom_col)
            } else {
                (0, right)
            };
            for col in c0..=c1 {
                let sx = area.x + col;
                if sx >= buf.area.width {
                    continue;
                }
                let cell = &mut buf[(sx, sy)];
                if cell.symbol().is_empty() || is_border_char(cell.symbol()) {
                    continue;
                }
                cell.set_bg(theme.primary_container);
                cell.set_fg(theme.on_primary_container);
            }
        }
    }

    pub fn copy_text(&self, text: &str) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        let clip = self.clipboard.get_clone();
        let cb = clip.as_ref()?;
        let mut cb = cb.lock().ok()?;
        cb.set_text(text).ok()?;
        Some(text.to_string())
    }

    pub fn copy(&self, provider: &impl TextProvider, area: Rect) -> Option<String> {
        let a = self.anchor.get()?;
        let f = self.focus.get()?;
        let (top, bottom) = if a.0 <= f.0 { (a, f) } else { (f, a) };
        let text = provider.text_in_range(top, bottom, area);
        if text.is_empty() {
            return None;
        }
        let clip = self.clipboard.get_clone();
        let cb = clip.as_ref()?;
        let mut cb = cb.lock().ok()?;
        cb.set_text(&text).ok()?;
        Some(text)
    }

    pub fn clear(&self) {
        self.anchor.set(None);
        self.focus.set(None);
    }

    pub fn normalized_range(&self) -> Option<(TextPos, TextPos)> {
        let a = self.anchor.get()?;
        let f = self.focus.get()?;
        let start = (a.0.min(f.0), a.1.min(f.1));
        let end = (a.0.max(f.0), a.1.max(f.1));
        if start == end {
            return None;
        }
        Some((start, end))
    }

    pub fn has_selection(&self) -> bool {
        self.normalized_range().is_some()
    }
}

pub trait TextProvider {
    fn text_in_range(&self, top: TextPos, bottom: TextPos, area: Rect) -> String;
    fn full_text(&self) -> String;
}

pub trait Copyable {
    fn copy_text(&self) -> String;
}

fn is_border_char(s: &str) -> bool {
    matches!(
        s,
        "─" | "━"
            | "│"
            | "┃"
            | "┌"
            | "┐"
            | "└"
            | "┘"
            | "├"
            | "┤"
            | "┬"
            | "┴"
            | "┼"
            | "╭"
            | "╮"
            | "╰"
            | "╯"
            | "┏"
            | "┓"
            | "┗"
            | "┛"
            | "┣"
            | "┫"
            | "┳"
            | "┻"
            | "╋"
            | "▁"
            | "▔"
            | "▏"
            | "▕"
            | "░"
            | "▒"
            | "▓"
            | "▀"
            | "▄"
            | "█"
            | "▌"
            | "▐"
    )
}
