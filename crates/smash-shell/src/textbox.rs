use arboard::Clipboard;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Widget};
use std::cmp::min;

#[derive(Clone, Default)]
pub struct TextBox {
    pub lines: Vec<String>,
    pub cursor_y: usize,
    pub cursor_x: usize,
    pub scroll_y: usize,
    pub scroll_x: usize,
    pub selection_start: Option<(usize, usize)>, // (y, x)
    clipboard: Option<std::sync::Arc<std::sync::Mutex<Clipboard>>>,
    pub show_line_numbers: bool,
}

impl TextBox {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_y: 0,
            cursor_x: 0,
            scroll_y: 0,
            scroll_x: 0,
            selection_start: None,
            clipboard: Clipboard::new().ok().map(|c| std::sync::Arc::new(std::sync::Mutex::new(c))),
            show_line_numbers: true,
        }
    }

    pub fn with_text(mut self, text: &str) -> Self {
        self.lines = text.lines().map(String::from).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_y = self.lines.len() - 1;
        self.cursor_x = self.lines.last().map(|l| l.chars().count()).unwrap_or(0);
        self
    }

    pub fn handle_event(&mut self, key: &KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }

        let is_shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let is_alt = key.modifiers.contains(KeyModifiers::ALT);
        let is_word_mod = is_ctrl || is_alt;

        // Helper to start selection if not active
        let mut start_selection = || {
            if self.selection_start.is_none() {
                self.selection_start = Some((self.cursor_y, self.cursor_x));
            }
        };

        let pos_before = (self.cursor_y, self.cursor_x);
        let lines_before = self.lines.len();
        let text_before = if let Some(line) = self.lines.get(self.cursor_y) {
            line.len()
        } else { 0 };

        let mut handled = true;
        match key.code {
            KeyCode::Char('c') if is_ctrl => { self.copy(); return true; }
            KeyCode::Char('x') if is_ctrl => { self.cut(); return true; }
            KeyCode::Char('v') if is_ctrl => { self.paste(); return true; }
            KeyCode::Char('a') if is_ctrl => { self.select_all(); return true; }
            
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
            KeyCode::Backspace | KeyCode::Char('h') if is_ctrl => {
                self.delete_word_left();
            },
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => {
                if is_ctrl { self.delete_word_right(); } else { self.delete(); }
            }
            KeyCode::Char(c) if !is_ctrl => self.insert_char(c),
            _ => {
                handled = false;
            }
        }

        if handled {
            // Check if anything actually changed
            let pos_after = (self.cursor_y, self.cursor_x);
            let lines_after = self.lines.len();
            let text_after = if let Some(line) = self.lines.get(self.cursor_y) {
                line.len()
            } else { 0 };

            let changed = pos_before != pos_after || lines_before != lines_after || text_before != text_after;
            
            if !is_shift && !is_ctrl && matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End) {
                self.selection_start = None;
            }
            
            if is_word_mod && !is_shift && matches!(key.code, KeyCode::Left | KeyCode::Right) {
                 self.selection_start = None;
            }

            return changed;
        }

        false
    }

    // --- Movement ---

    fn move_left(&mut self) {
        if self.cursor_x > 0 {
            self.cursor_x -= 1;
        } else if self.cursor_y > 0 {
            self.cursor_y -= 1;
            self.cursor_x = self.lines[self.cursor_y].chars().count();
        }
    }

    fn move_right(&mut self) {
        if self.cursor_x < self.lines[self.cursor_y].chars().count() {
            self.cursor_x += 1;
        } else if self.cursor_y < self.lines.len() - 1 {
            self.cursor_y += 1;
            self.cursor_x = 0;
        }
    }

    fn move_up(&mut self) {
        if self.cursor_y > 0 {
            self.cursor_y -= 1;
            self.cursor_x = min(self.cursor_x, self.lines[self.cursor_y].chars().count());
        }
    }

    fn move_down(&mut self) {
        if self.cursor_y < self.lines.len() - 1 {
            self.cursor_y += 1;
            self.cursor_x = min(self.cursor_x, self.lines[self.cursor_y].chars().count());
        }
    }

    fn move_home(&mut self) {
        self.cursor_x = 0;
    }

    fn move_end(&mut self) {
        self.cursor_x = self.lines[self.cursor_y].chars().count();
    }

    fn move_word_left(&mut self) {
        if self.cursor_x == 0 { self.move_left(); return; }
        let chars: Vec<char> = self.lines[self.cursor_y].chars().collect();
        let mut i = self.cursor_x;
        while i > 0 && i <= chars.len() && chars[i-1].is_whitespace() { i -= 1; }
        while i > 0 && i <= chars.len() && !chars[i-1].is_whitespace() { i -= 1; }
        self.cursor_x = i;
    }

    fn move_word_right(&mut self) {
        let chars: Vec<char> = self.lines[self.cursor_y].chars().collect();
        let len = chars.len();
        if self.cursor_x == len { self.move_right(); return; }
        let mut i = self.cursor_x;
        while i < len && !chars[i].is_whitespace() { i += 1; }
        while i < len && chars[i].is_whitespace() { i += 1; }
        self.cursor_x = i;
    }

    // --- Editing ---

    fn insert_char(&mut self, c: char) {
        self.delete_selection();
        let line = &mut self.lines[self.cursor_y];
        let mut chars: Vec<char> = line.chars().collect();
        if self.cursor_x > chars.len() { self.cursor_x = chars.len(); }
        chars.insert(self.cursor_x, c);
        *line = chars.into_iter().collect();
        self.cursor_x += 1;
    }

    fn insert_newline(&mut self) {
        self.delete_selection();
        let line = &mut self.lines[self.cursor_y];
        let chars: Vec<char> = line.chars().collect();
        if self.cursor_x > chars.len() { self.cursor_x = chars.len(); }
        let rest: String = chars.iter().skip(self.cursor_x).collect();
        let keep: String = chars.iter().take(self.cursor_x).collect();
        *line = keep;
        self.lines.insert(self.cursor_y + 1, rest);
        self.cursor_y += 1;
        self.cursor_x = 0;
    }

    fn backspace(&mut self) {
        if self.delete_selection() { return; }
        if self.cursor_x > 0 {
            let line = &mut self.lines[self.cursor_y];
            let mut chars: Vec<char> = line.chars().collect();
            if self.cursor_x > chars.len() { self.cursor_x = chars.len(); }
            if self.cursor_x > 0 {
                chars.remove(self.cursor_x - 1);
                *line = chars.into_iter().collect();
                self.cursor_x -= 1;
            }
        } else if self.cursor_y > 0 {
            let current_line = self.lines.remove(self.cursor_y);
            self.cursor_y -= 1;
            self.cursor_x = self.lines[self.cursor_y].chars().count();
            self.lines[self.cursor_y].push_str(&current_line);
        }
    }

    fn delete(&mut self) {
        if self.delete_selection() { return; }
        let len = self.lines[self.cursor_y].chars().count();
        if self.cursor_x < len {
            let line = &mut self.lines[self.cursor_y];
            let mut chars: Vec<char> = line.chars().collect();
            if self.cursor_x < chars.len() {
                chars.remove(self.cursor_x);
                *line = chars.into_iter().collect();
            }
        } else if self.cursor_y < self.lines.len() - 1 {
            let next_line = self.lines.remove(self.cursor_y + 1);
            self.lines[self.cursor_y].push_str(&next_line);
        }
    }

    fn delete_word_left(&mut self) {
        if self.delete_selection() { return; }
        let start_x = self.cursor_x;
        self.move_word_left();
        let end_x = self.cursor_x;
        if self.cursor_y < self.lines.len() {
             if start_x == 0 {
                 self.cursor_x = start_x; 
                 self.backspace();
                 return;
             }
             let line = &mut self.lines[self.cursor_y];
             let mut chars: Vec<char> = line.chars().collect();
             if end_x < start_x && start_x <= chars.len() {
                 chars.drain(end_x..start_x);
                 *line = chars.into_iter().collect();
             }
        }
    }

    fn delete_word_right(&mut self) {
        if self.delete_selection() { return; }
        let start_x = self.cursor_x;
        let start_y = self.cursor_y;
        let len = self.lines[self.cursor_y].chars().count();
        if start_x == len {
            self.delete();
            return;
        }
        self.move_word_right();
        if start_y == self.cursor_y {
            let end_x = self.cursor_x;
            self.cursor_x = start_x;
            let line = &mut self.lines[self.cursor_y];
            let mut chars: Vec<char> = line.chars().collect();
            if end_x > start_x && end_x <= chars.len() {
                chars.drain(start_x..end_x);
                *line = chars.into_iter().collect();
            }
        } else {
            self.cursor_y = start_y;
            self.cursor_x = start_x;
            let line = &mut self.lines[self.cursor_y];
            let mut chars: Vec<char> = line.chars().collect();
            if start_x < chars.len() {
                chars.truncate(start_x);
                *line = chars.into_iter().collect();
            }
        }
    }

    // --- Selection & Clipboard ---

    pub(crate) fn get_normalized_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let start = self.selection_start?;
        let end = (self.cursor_y, self.cursor_x);
        if start == end { return None; }
        if start.0 < end.0 || (start.0 == end.0 && start.1 < end.1) {
            Some((start, end))
        } else {
            Some((end, start))
        }
    }

    fn delete_selection(&mut self) -> bool {
        if let Some(((y1, x1), (y2, x2))) = self.get_normalized_selection() {
            if y1 == y2 {
                let line = &mut self.lines[y1];
                let mut chars: Vec<char> = line.chars().collect();
                if x1 < chars.len() && x2 <= chars.len() {
                    chars.drain(x1..x2);
                    *line = chars.into_iter().collect();
                }
            } else {
                let s1: String = self.lines[y1].chars().take(x1).collect();
                let s2: String = self.lines[y2].chars().skip(x2).collect();
                self.lines[y1] = s1 + &s2;
                for _ in 0..(y2 - y1) {
                    if y1 + 1 < self.lines.len() {
                        self.lines.remove(y1 + 1);
                    }
                }
            }
            self.cursor_y = y1;
            self.cursor_x = x1;
            self.selection_start = None;
            return true;
        }
        false
    }

    fn copy(&self) {
        if let Some(((y1, x1), (y2, x2))) = self.get_normalized_selection() {
            let mut text = String::new();
            if y1 == y2 {
                text = self.lines[y1].chars().skip(x1).take(x2 - x1).collect();
            } else {
                text.push_str(&self.lines[y1].chars().skip(x1).collect::<String>());
                text.push('\n');
                for i in (y1 + 1)..y2 {
                    text.push_str(&self.lines[i]);
                    text.push('\n');
                }
                text.push_str(&self.lines[y2].chars().take(x2).collect::<String>());
            }
            if let Some(cb) = &self.clipboard
                && let Ok(mut cb) = cb.lock()
            {
                let _ = cb.set_text(text);
            }
        }
    }

    fn cut(&mut self) {
        self.copy();
        self.delete_selection();
    }

    fn paste(&mut self) {
        if let Some(cb) = &self.clipboard {
            let text = if let Ok(mut cb) = cb.lock() {
                cb.get_text().unwrap_or_default()
            } else { String::new() };
            self.delete_selection();
            for c in text.chars() {
                if c == '\n' { self.insert_newline(); } else { self.insert_char(c); }
            }
        }
    }

    fn select_all(&mut self) {
        self.selection_start = Some((0, 0));
        self.cursor_y = self.lines.len() - 1;
        self.cursor_x = self.lines.last().map(|l| l.chars().count()).unwrap_or(0);
    }

    // --- Rendering ---

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, block: Option<Block>) {
        let inner = if let Some(b) = block {
            let i = b.inner(area);
            b.render(area, buf);
            i
        } else {
            area
        };
        if inner.height == 0 { return; }
        if self.cursor_y < self.scroll_y {
            self.scroll_y = self.cursor_y;
        } else if self.cursor_y >= self.scroll_y + inner.height as usize {
            self.scroll_y = self.cursor_y - inner.height as usize + 1;
        }
        let gutter_width = if self.show_line_numbers { 4 } else { 0 };
        let selection = self.get_normalized_selection();
        for y in 0..inner.height as usize {
            let line_idx = self.scroll_y + y;
            if line_idx >= self.lines.len() { break; }
            let line_y = inner.y + y as u16;
            if self.show_line_numbers {
                buf.set_string(inner.x, line_y, format!("{:3} ", line_idx + 1), Style::default().fg(Color::DarkGray));
            }
            let line_content = &self.lines[line_idx];
            let visible_content: String = line_content.chars().skip(self.scroll_x).take(inner.width as usize - gutter_width).collect();
            let text_x = inner.x + gutter_width as u16;
            buf.set_string(text_x, line_y, &visible_content, Style::default());
            if let Some(((sy, sx), (ey, ex))) = selection
                && line_idx >= sy && line_idx <= ey
            {
                let s = if line_idx == sy { sx.saturating_sub(self.scroll_x) } else { 0 };
                let e = if line_idx == ey { ex.saturating_sub(self.scroll_x) } else { line_content.chars().count().saturating_sub(self.scroll_x) };
                let max_width = inner.width as usize - gutter_width;
                if s < max_width {
                    let draw_e = min(e, max_width);
                    for i in s..draw_e {
                        let cell = &mut buf[(text_x + i as u16, line_y)];
                        cell.set_bg(Color::Blue).set_fg(Color::White);
                    }
                }
            }
        }
    }

    pub fn cursor_position(&self, area: Rect) -> Option<(u16, u16)> {
        let gutter_width = if self.show_line_numbers { 4 } else { 0 };
        if self.cursor_y < self.scroll_y { return None; }
        let vy = self.cursor_y - self.scroll_y;
        if vy >= area.height as usize { return None; }
        let vx = self.cursor_x.saturating_sub(self.scroll_x);
        if vx >= area.width as usize - gutter_width { return None; }
        Some((area.x + gutter_width as u16 + vx as u16, area.y + vy as u16))
    }

    pub fn cursor_at_start(&self) -> bool { self.cursor_y == 0 && self.cursor_x == 0 }
    pub fn cursor_at_end(&self) -> bool {
        self.cursor_y == self.lines.len() - 1 && self.cursor_x == self.lines.last().map(|l| l.chars().count()).unwrap_or(0)
    }
}
