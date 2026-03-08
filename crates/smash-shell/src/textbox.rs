use arboard::Clipboard;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Widget};
use sycamore_reactive::*;
use std::cmp::min;
use std::sync::{Arc, Mutex};

// --- Composable ---

#[derive(Clone, Copy)]
pub struct TextBoxState {
    pub lines: Signal<Vec<String>>,
    pub cursor_y: Signal<usize>,
    pub cursor_x: Signal<usize>,
    pub scroll_y: Signal<usize>,
    pub scroll_x: Signal<usize>,
    pub selection_start: Signal<Option<(usize, usize)>>,
    pub show_line_numbers: Signal<bool>,
    pub selection_style: Signal<Style>,
    clipboard: Signal<Option<Arc<Mutex<Clipboard>>>>,
    pub is_focused: Signal<bool>,
}

pub fn use_textbox(initial_text: &str) -> TextBoxState {
    let lines = initial_text.lines().map(String::from).collect::<Vec<_>>();
    let lines = if lines.is_empty() { vec![String::new()] } else { lines };
    
    TextBoxState {
        lines: create_signal(lines),
        cursor_y: create_signal(0),
        cursor_x: create_signal(0),
        scroll_y: create_signal(0),
        scroll_x: create_signal(0),
        selection_start: create_signal(None),
        show_line_numbers: create_signal(true),
        selection_style: create_signal(Style::default().bg(Color::Blue).fg(Color::White)),
        clipboard: create_signal(Clipboard::new().ok().map(|c| Arc::new(Mutex::new(c)))),
        is_focused: create_signal(false),
    }
}

impl TextBoxState {
    pub fn handle_event(&self, key: &KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release { return false; }

        if !self.is_focused.get() {
            if key.code == KeyCode::Enter {
                self.is_focused.set(true);
                return true;
            }
            return false;
        }

        if key.code == KeyCode::Esc {
            self.is_focused.set(false);
            return true;
        }

        let is_shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let is_alt = key.modifiers.contains(KeyModifiers::ALT);
        let is_word_mod = is_ctrl || is_alt;

        // Helper to start selection if not active
        let start_selection = || {
            if self.selection_start.get().is_none() {
                self.selection_start.set(Some((self.cursor_y.get(), self.cursor_x.get())));
            }
        };

        let pos_before = (self.cursor_y.get(), self.cursor_x.get());
        let mut handled = true;

        match key.code {
            KeyCode::Char('c') if is_ctrl => self.copy(),
            KeyCode::Char('x') if is_ctrl => self.cut(),
            KeyCode::Char('v') if is_ctrl => self.paste(),
            KeyCode::Char('a') if is_ctrl => self.select_all(),
            
            // Movement & Selection
            KeyCode::Left => {
                if is_shift { start_selection(); }
                if is_word_mod { self.move_word_left(); } else { self.move_left(); }
            }
            KeyCode::Right => {
                if is_shift { start_selection(); }
                if is_word_mod { self.move_word_right(); } else { self.move_right(); }
            }
            KeyCode::Up => {
                if is_shift { start_selection(); }
                self.move_up();
            }
            KeyCode::Down => {
                if is_shift { start_selection(); }
                self.move_down();
            }
            KeyCode::Home => {
                if is_shift { start_selection(); }
                self.move_home();
            }
            KeyCode::End => {
                if is_shift { start_selection(); }
                self.move_end();
            }
            
            // Editing
            KeyCode::Enter => self.insert_newline(),
            KeyCode::Backspace | KeyCode::Char('h') if is_ctrl => self.delete_word_left(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => {
                if is_ctrl { self.delete_word_right(); } else { self.delete(); }
            }
            KeyCode::Char(c) if !is_ctrl => self.insert_char(c),
            _ => handled = false,
        }

        if handled {
            // Clear selection if we moved without Shift
            if !is_shift && !is_ctrl && matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End) {
                self.selection_start.set(None);
            }
            return (self.cursor_y.get(), self.cursor_x.get()) != pos_before;
        }
        false
    }

    // --- Internal Logic (Signal-based) ---

    fn move_left(&self) {
        let x = self.cursor_x.get();
        let y = self.cursor_y.get();
        if x > 0 {
            self.cursor_x.set(x - 1);
        } else if y > 0 {
            self.cursor_y.set(y - 1);
            self.cursor_x.set(self.lines.get_clone()[y - 1].chars().count());
        }
    }

    fn move_right(&self) {
        let x = self.cursor_x.get();
        let y = self.cursor_y.get();
        let lines = self.lines.get_clone();
        if x < lines[y].chars().count() {
            self.cursor_x.set(x + 1);
        } else if y < lines.len() - 1 {
            self.cursor_y.set(y + 1);
            self.cursor_x.set(0);
        }
    }

    fn move_up(&self) {
        let y = self.cursor_y.get();
        if y > 0 {
            self.cursor_y.set(y - 1);
            let next_len = self.lines.get_clone()[y - 1].chars().count();
            self.cursor_x.set(min(self.cursor_x.get(), next_len));
        }
    }

    fn move_down(&self) {
        let y = self.cursor_y.get();
        let lines = self.lines.get_clone();
        if y < lines.len() - 1 {
            self.cursor_y.set(y + 1);
            let next_len = lines[y + 1].chars().count();
            self.cursor_x.set(min(self.cursor_x.get(), next_len));
        }
    }

    fn move_home(&self) { self.cursor_x.set(0); }
    fn move_end(&self) { self.cursor_x.set(self.lines.get_clone()[self.cursor_y.get()].chars().count()); }

    fn move_word_left(&self) {
        let x = self.cursor_x.get();
        if x == 0 { self.move_left(); return; }
        let lines = self.lines.get_clone();
        let chars: Vec<char> = lines[self.cursor_y.get()].chars().collect();
        let mut i = x;
        while i > 0 && chars[i-1].is_whitespace() { i -= 1; }
        while i > 0 && !chars[i-1].is_whitespace() { i -= 1; }
        self.cursor_x.set(i);
    }

    fn move_word_right(&self) {
        let x = self.cursor_x.get();
        let lines = self.lines.get_clone();
        let chars: Vec<char> = lines[self.cursor_y.get()].chars().collect();
        let len = chars.len();
        if x == len { self.move_right(); return; }
        let mut i = x;
        while i < len && !chars[i].is_whitespace() { i += 1; }
        while i < len && chars[i].is_whitespace() { i += 1; }
        self.cursor_x.set(i);
    }

    fn insert_char(&self, c: char) {
        self.delete_selection();
        let y = self.cursor_y.get();
        let x = self.cursor_x.get();
        let mut lines = self.lines.get_clone();
        let line = &mut lines[y];
        let mut chars: Vec<char> = line.chars().collect();
        chars.insert(x.min(chars.len()), c);
        *line = chars.into_iter().collect();
        self.lines.set(lines);
        self.cursor_x.set(x + 1);
    }

    fn insert_newline(&self) {
        self.delete_selection();
        let y = self.cursor_y.get();
        let x = self.cursor_x.get();
        let mut lines = self.lines.get_clone();
        let line = &lines[y];
        let rest: String = line.chars().skip(x).collect();
        let keep: String = line.chars().take(x).collect();
        lines[y] = keep;
        lines.insert(y + 1, rest);
        self.lines.set(lines);
        self.cursor_y.set(y + 1);
        self.cursor_x.set(0);
    }

    fn backspace(&self) {
        if self.delete_selection() { return; }
        let x = self.cursor_x.get();
        let y = self.cursor_y.get();
        let mut lines = self.lines.get_clone();
        if x > 0 {
            let mut chars: Vec<char> = lines[y].chars().collect();
            chars.remove(x - 1);
            lines[y] = chars.into_iter().collect();
            self.lines.set(lines);
            self.cursor_x.set(x - 1);
        } else if y > 0 {
            let current_line = lines.remove(y);
            self.cursor_y.set(y - 1);
            self.cursor_x.set(lines[y - 1].chars().count());
            lines[y - 1].push_str(&current_line);
            self.lines.set(lines);
        }
    }

    fn delete(&self) {
        if self.delete_selection() { return; }
        let x = self.cursor_x.get();
        let y = self.cursor_y.get();
        let mut lines = self.lines.get_clone();
        if x < lines[y].chars().count() {
            let mut chars: Vec<char> = lines[y].chars().collect();
            chars.remove(x);
            lines[y] = chars.into_iter().collect();
            self.lines.set(lines);
        } else if y < lines.len() - 1 {
            let next_line = lines.remove(y + 1);
            lines[y].push_str(&next_line);
            self.lines.set(lines);
        }
    }

    fn delete_word_left(&self) {
        if self.delete_selection() { return; }
        let start_x = self.cursor_x.get();
        self.move_word_left();
        if self.cursor_x.get() < start_x {
            let mut lines = self.lines.get_clone();
            let mut chars: Vec<char> = lines[self.cursor_y.get()].chars().collect();
            chars.drain(self.cursor_x.get()..start_x);
            lines[self.cursor_y.get()] = chars.into_iter().collect();
            self.lines.set(lines);
        }
    }

    fn delete_word_right(&self) {
        if self.delete_selection() { return; }
        let start_x = self.cursor_x.get();
        let start_y = self.cursor_y.get();
        self.move_word_right();
        if self.cursor_y.get() == start_y {
            let end_x = self.cursor_x.get();
            self.cursor_x.set(start_x);
            let mut lines = self.lines.get_clone();
            let mut chars: Vec<char> = lines[start_y].chars().collect();
            chars.drain(start_x..end_x);
            lines[start_y] = chars.into_iter().collect();
            self.lines.set(lines);
        }
    }

    fn delete_selection(&self) -> bool {
        if let Some(((y1, x1), (y2, x2))) = self.get_normalized_selection() {
            let mut lines = self.lines.get_clone();
            if y1 == y2 {
                let mut chars: Vec<char> = lines[y1].chars().collect();
                chars.drain(x1..x2);
                lines[y1] = chars.into_iter().collect();
            } else {
                let s1: String = lines[y1].chars().take(x1).collect();
                let s2: String = lines[y2].chars().skip(x2).collect();
                lines[y1] = s1 + &s2;
                for _ in 0..(y2 - y1) { lines.remove(y1 + 1); }
            }
            self.lines.set(lines);
            self.cursor_y.set(y1);
            self.cursor_x.set(x1);
            self.selection_start.set(None);
            return true;
        }
        false
    }

    pub fn get_normalized_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let start = self.selection_start.get()?;
        let end = (self.cursor_y.get(), self.cursor_x.get());
        if start == end { return None; }
        if start.0 < end.0 || (start.0 == end.0 && start.1 < end.1) { Some((start, end)) } else { Some((end, start)) }
    }

    fn copy(&self) {
        if let Some(((y1, x1), (y2, x2))) = self.get_normalized_selection() {
            let lines = self.lines.get_clone();
            let mut text = String::new();
            if y1 == y2 {
                text = lines[y1].chars().skip(x1).take(x2 - x1).collect();
            } else {
                text.push_str(&lines[y1].chars().skip(x1).collect::<String>());
                text.push('\n');
                for i in (y1 + 1)..y2 { text.push_str(&lines[i]); text.push('\n'); }
                text.push_str(&lines[y2].chars().take(x2).collect::<String>());
            }
            if let Some(cb) = self.clipboard.get_clone().as_ref() {
                if let Ok(mut cb) = cb.lock() { let _ = cb.set_text(text); }
            }
        }
    }

    fn cut(&self) { self.copy(); self.delete_selection(); }

    fn paste(&self) {
        if let Some(cb) = self.clipboard.get_clone().as_ref() {
            let text = if let Ok(mut cb) = cb.lock() { cb.get_text().unwrap_or_default() } else { String::new() };
            self.delete_selection();
            for c in text.chars() { if c == '\n' { self.insert_newline(); } else { self.insert_char(c); } }
        }
    }

    fn select_all(&self) {
        self.selection_start.set(Some((0, 0)));
        let lines = self.lines.get_clone();
        self.cursor_y.set(lines.len() - 1);
        self.cursor_x.set(lines.last().map(|l| l.chars().count()).unwrap_or(0));
    }
}

// --- Component (Stateless Function) ---

pub fn text_box_component(frame: &mut Frame, area: Rect, state: &TextBoxState, theme: &crate::theme::SmashTheme) {
    let is_focused = state.is_focused.get();
    let mut block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme.outline)).bg(theme.surface);
    if is_focused {
        block = block.title("rich text editor (focused - esc to unfocus)").border_style(Style::default().fg(theme.primary));
    } else {
        block = block.title("rich text editor (unfocused - enter to focus)");
    }
    
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());

    if inner.height == 0 { return; }

    // Sync scroll
    let cy = state.cursor_y.get();
    let mut sy = state.scroll_y.get();
    if cy < sy { sy = cy; }
    else if cy >= sy + inner.height as usize { sy = cy - inner.height as usize + 1; }
    state.scroll_y.set(sy);

    let gutter_width = if state.show_line_numbers.get() { 4 } else { 0 };
    let selection = state.get_normalized_selection();
    let lines = state.lines.get_clone();
    let sx = state.scroll_x.get();

    for y in 0..inner.height as usize {
        let line_idx = sy + y;
        if line_idx >= lines.len() { break; }
        let line_y = inner.y + y as u16;
        
        if state.show_line_numbers.get() {
            frame.buffer_mut().set_string(inner.x, line_y, format!("{:3} ", line_idx + 1), Style::default().fg(Color::DarkGray));
        }

        let line_content = &lines[line_idx];
        let visible: String = line_content.chars().skip(sx).take(inner.width as usize - gutter_width).collect();
        let text_x = inner.x + gutter_width as u16;
        frame.buffer_mut().set_string(text_x, line_y, &visible, Style::default());

        if let Some(((sy_sel, sx_sel), (ey_sel, ex_sel))) = selection {
            if line_idx >= sy_sel && line_idx <= ey_sel {
                let s = if line_idx == sy_sel { sx_sel.saturating_sub(sx) } else { 0 };
                let e = if line_idx == ey_sel { ex_sel.saturating_sub(sx) } else { line_content.chars().count().saturating_sub(sx) };
                let max_w = inner.width as usize - gutter_width;
                for i in s..min(e, max_w) {
                    let cell = &mut frame.buffer_mut()[(text_x + i as u16, line_y)];
                    cell.set_style(state.selection_style.get());
                }
            }
        }
    }

    if is_focused {
        let vx = state.cursor_x.get().saturating_sub(sx);
        let vy = cy - sy;
        if vx < inner.width as usize - gutter_width && vy < inner.height as usize {
            frame.set_cursor_position((inner.x + gutter_width as u16 + vx as u16, inner.y + vy as u16));
        }
    }
}
