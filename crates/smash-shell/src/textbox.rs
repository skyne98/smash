use crate::events::{EventStatus, SmashEvent};
use crate::reactive::{FocusState, use_focus};
use crate::syntax::{
    SyntaxRequest, SyntaxThemeKind, SyntaxWorker, detect_language_label, theme_kind_for,
};
use arboard::Clipboard;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Widget};
use std::cmp::min;
use std::sync::{Arc, Mutex};
use sycamore_reactive::*;

// --- Composable ---

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextBoxLanguage {
    Auto,
    PlainText,
    Rust,
    Markdown,
    Json,
    Toml,
    Yaml,
    Shell,
}

impl TextBoxLanguage {
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::PlainText => "text",
            Self::Rust => "rust",
            Self::Markdown => "markdown",
            Self::Json => "json",
            Self::Toml => "toml",
            Self::Yaml => "yaml",
            Self::Shell => "shell",
        }
    }
}

#[derive(Clone, Copy)]
pub struct TextBoxState {
    pub title: Signal<String>,
    pub path_hint: Signal<Option<String>>,
    pub lines: Signal<Vec<String>>,
    pub cursor_y: Signal<usize>,
    pub cursor_x: Signal<usize>,
    pub scroll_y: Signal<usize>,
    pub scroll_x: Signal<usize>,
    pub selection_start: Signal<Option<(usize, usize)>>,
    pub show_line_numbers: Signal<bool>,
    pub read_only: Signal<bool>,
    pub language: Signal<TextBoxLanguage>,
    syntax_revision: Signal<u64>,
    syntax_request_revision: Signal<u64>,
    syntax_request_theme_kind: Signal<SyntaxThemeKind>,
    syntax_worker: Signal<Arc<SyntaxWorker>>,
    pub selection_style: Signal<Style>,
    clipboard: Signal<Option<Arc<Mutex<Clipboard>>>>,
    pub is_selected: FocusState,
    pub is_focused: FocusState,
}

pub fn use_textbox(initial_text: &str) -> TextBoxState {
    let lines = initial_text.lines().map(String::from).collect::<Vec<_>>();
    let lines = if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    };

    TextBoxState {
        title: create_signal("textbox".to_string()),
        path_hint: create_signal(None),
        lines: create_signal(lines),
        cursor_y: create_signal(0),
        cursor_x: create_signal(0),
        scroll_y: create_signal(0),
        scroll_x: create_signal(0),
        selection_start: create_signal(None),
        show_line_numbers: create_signal(true),
        read_only: create_signal(false),
        language: create_signal(TextBoxLanguage::Auto),
        syntax_revision: create_signal(0),
        syntax_request_revision: create_signal(u64::MAX),
        syntax_request_theme_kind: create_signal(SyntaxThemeKind::Dark),
        syntax_worker: create_signal(Arc::new(SyntaxWorker::new())),
        selection_style: create_signal(Style::default().bg(Color::Blue).fg(Color::White)),
        clipboard: create_signal(Clipboard::new().ok().map(|c| Arc::new(Mutex::new(c)))),
        is_selected: use_focus(false),
        is_focused: use_focus(false),
    }
}

impl TextBoxState {
    pub fn set_title(&self, title: impl Into<String>) {
        self.title.set(title.into());
        self.touch_syntax_revision();
    }

    /// Provides an optional filename/path hint for auto-detection.
    /// When present, linguist treats it as the primary filename clue and uses content to disambiguate when possible.
    pub fn set_path_hint(&self, path_hint: impl Into<String>) {
        self.path_hint.set(Some(path_hint.into()));
        self.touch_syntax_revision();
    }

    pub fn clear_path_hint(&self) {
        self.path_hint.set(None);
        self.touch_syntax_revision();
    }

    pub fn set_read_only(&self, read_only: bool) {
        self.read_only.set(read_only);
        if read_only {
            self.selection_start.set(None);
        }
    }

    pub fn set_language(&self, language: TextBoxLanguage) {
        self.language.set(language);
        self.touch_syntax_revision();
    }

    pub fn resolved_language_label(&self) -> String {
        detect_language_label(&self.syntax_request(SyntaxThemeKind::Dark))
    }

    pub fn select(&self) {
        self.is_selected.focus();
    }

    pub fn deselect(&self) {
        self.is_selected.blur();
        self.is_focused.blur();
    }

    pub fn focus(&self) {
        self.select();
        self.is_focused.focus();
    }

    pub fn blur(&self) {
        self.is_focused.blur();
    }

    fn touch_syntax_revision(&self) {
        self.syntax_revision.set(self.syntax_revision.get() + 1);
    }

    fn syntax_request(&self, theme_kind: SyntaxThemeKind) -> SyntaxRequest {
        SyntaxRequest {
            revision: self.syntax_revision.get(),
            theme_kind,
            title: self.title.get_clone(),
            path_hint: self.path_hint.get_clone(),
            language: self.language.get(),
            lines: self.lines.get_clone(),
        }
    }

    fn schedule_syntax_if_needed(&self, theme_kind: SyntaxThemeKind) {
        if self.syntax_request_revision.get() == self.syntax_revision.get()
            && self.syntax_request_theme_kind.get() == theme_kind
        {
            return;
        }

        self.syntax_worker
            .get_clone()
            .schedule(self.syntax_request(theme_kind));
        self.syntax_request_revision.set(self.syntax_revision.get());
        self.syntax_request_theme_kind.set(theme_kind);
    }

    pub fn handle_smash_event(&self, event: &SmashEvent) -> EventStatus {
        match event {
            SmashEvent::Key(key) if self.handle_event(key) => EventStatus::Handled,
            SmashEvent::Key(_) => EventStatus::Ignored,
            _ => EventStatus::Ignored,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &crate::theme::SmashTheme) {
        text_box_component(frame, area, self, theme);
    }

    pub fn handle_event(&self, key: &KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }

        if !self.is_focused.get() {
            if key.code == KeyCode::Enter {
                self.focus();
                return true;
            }
            return false;
        }

        if key.code == KeyCode::Esc {
            self.blur();
            return true;
        }

        let is_shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let is_alt = key.modifiers.contains(KeyModifiers::ALT);
        let is_word_mod = is_ctrl || is_alt;
        let is_read_only = self.read_only.get();

        // Helper to start selection if not active
        let start_selection = || {
            if self.selection_start.get().is_none() {
                self.selection_start
                    .set(Some((self.cursor_y.get(), self.cursor_x.get())));
            }
        };

        let mut handled = true;
        let mut syntax_changed = false;

        match key.code {
            KeyCode::Char('c') if is_ctrl => self.copy(),
            KeyCode::Char('a') if is_ctrl => self.select_all(),
            KeyCode::Char('x') if is_ctrl && !is_read_only => {
                self.cut();
                syntax_changed = true;
            }
            KeyCode::Char('v') if is_ctrl && !is_read_only => {
                self.paste();
                syntax_changed = true;
            }

            // Movement & Selection
            KeyCode::Left => {
                if is_shift {
                    start_selection();
                }
                if is_word_mod {
                    self.move_word_left();
                } else {
                    self.move_left();
                }
            }
            KeyCode::Right => {
                if is_shift {
                    start_selection();
                }
                if is_word_mod {
                    self.move_word_right();
                } else {
                    self.move_right();
                }
            }
            KeyCode::Up => {
                if is_shift {
                    start_selection();
                }
                self.move_up();
            }
            KeyCode::Down => {
                if is_shift {
                    start_selection();
                }
                self.move_down();
            }
            KeyCode::Home => {
                if is_shift {
                    start_selection();
                }
                self.move_home();
            }
            KeyCode::End => {
                if is_shift {
                    start_selection();
                }
                self.move_end();
            }

            // Editing
            KeyCode::Enter if !is_read_only => {
                self.insert_newline();
                syntax_changed = true;
            }
            KeyCode::Backspace | KeyCode::Char('h') if is_ctrl && !is_read_only => {
                self.delete_word_left();
                syntax_changed = true;
            }
            KeyCode::Backspace if !is_read_only => {
                self.backspace();
                syntax_changed = true;
            }
            KeyCode::Delete if !is_read_only => {
                if is_ctrl {
                    self.delete_word_right();
                } else {
                    self.delete();
                }
                syntax_changed = true;
            }
            KeyCode::Char(c) if !is_ctrl && !is_read_only => {
                self.insert_char(c);
                syntax_changed = true;
            }
            _ => handled = false,
        }

        if handled {
            if syntax_changed {
                self.touch_syntax_revision();
            }
            // Clear selection if we moved without Shift
            if !is_shift
                && !is_ctrl
                && matches!(
                    key.code,
                    KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Home
                        | KeyCode::End
                )
            {
                self.selection_start.set(None);
            }
            return true;
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
            self.cursor_x
                .set(self.lines.get_clone()[y - 1].chars().count());
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

    fn move_home(&self) {
        self.cursor_x.set(0);
    }
    fn move_end(&self) {
        self.cursor_x
            .set(self.lines.get_clone()[self.cursor_y.get()].chars().count());
    }

    fn move_word_left(&self) {
        let x = self.cursor_x.get();
        if x == 0 {
            self.move_left();
            return;
        }
        let lines = self.lines.get_clone();
        let chars: Vec<char> = lines[self.cursor_y.get()].chars().collect();
        let mut i = x;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.cursor_x.set(i);
    }

    fn move_word_right(&self) {
        let x = self.cursor_x.get();
        let lines = self.lines.get_clone();
        let chars: Vec<char> = lines[self.cursor_y.get()].chars().collect();
        let len = chars.len();
        if x == len {
            self.move_right();
            return;
        }
        let mut i = x;
        while i < len && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
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
        if self.delete_selection() {
            return;
        }
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
        if self.delete_selection() {
            return;
        }
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
        if self.delete_selection() {
            return;
        }
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
        if self.delete_selection() {
            return;
        }
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
                for _ in 0..(y2 - y1) {
                    lines.remove(y1 + 1);
                }
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
        if start == end {
            return None;
        }
        if start.0 < end.0 || (start.0 == end.0 && start.1 < end.1) {
            Some((start, end))
        } else {
            Some((end, start))
        }
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
                for i in (y1 + 1)..y2 {
                    text.push_str(&lines[i]);
                    text.push('\n');
                }
                text.push_str(&lines[y2].chars().take(x2).collect::<String>());
            }
            if let Some(cb) = self.clipboard.get_clone().as_ref() {
                if let Ok(mut cb) = cb.lock() {
                    let _ = cb.set_text(text);
                }
            }
        }
    }

    fn cut(&self) {
        self.copy();
        self.delete_selection();
    }

    fn paste(&self) {
        if let Some(cb) = self.clipboard.get_clone().as_ref() {
            let text = if let Ok(mut cb) = cb.lock() {
                cb.get_text().unwrap_or_default()
            } else {
                String::new()
            };
            self.delete_selection();
            for c in text.chars() {
                if c == '\n' {
                    self.insert_newline();
                } else {
                    self.insert_char(c);
                }
            }
        }
    }

    fn select_all(&self) {
        self.selection_start.set(Some((0, 0)));
        let lines = self.lines.get_clone();
        self.cursor_y.set(lines.len() - 1);
        self.cursor_x
            .set(lines.last().map(|l| l.chars().count()).unwrap_or(0));
    }
}

// --- Component (Stateless Function) ---

pub fn text_box_component(
    frame: &mut Frame,
    area: Rect,
    state: &TextBoxState,
    theme: &crate::theme::SmashTheme,
) {
    let is_focused = state.is_focused.get();
    let is_selected = state.is_selected.get();
    let title = state.title.get_clone();
    let is_read_only = state.read_only.get();
    let theme_kind = theme_kind_for(theme);
    state.schedule_syntax_if_needed(theme_kind);
    let syntax_snapshot = state.syntax_worker.get_clone().latest_snapshot();
    let border_color = if is_focused || is_selected {
        theme.primary
    } else {
        theme.outline_variant
    };
    let surface_bg = if is_focused || is_selected {
        theme.surface_variant
    } else {
        theme.surface
    };
    let mut badges = Vec::new();
    if is_read_only {
        badges.push((
            "read only",
            Style::default()
                .fg(theme.on_tertiary_container)
                .bg(theme.tertiary_container),
        ));
    } else if is_focused {
        badges.push((
            "editing",
            Style::default()
                .fg(theme.on_primary_container)
                .bg(theme.primary_container),
        ));
    } else if is_selected {
        badges.push((
            "selected",
            Style::default()
                .fg(theme.on_secondary_container)
                .bg(theme.secondary_container),
        ));
    }
    if let Some(label) = syntax_snapshot
        .as_ref()
        .map(|snapshot| snapshot.language_label.as_str())
    {
        badges.push((
            label,
            Style::default()
                .fg(theme.on_secondary_container)
                .bg(theme.secondary_container),
        ));
    } else if state.language.get() != TextBoxLanguage::Auto
        && state.language.get() != TextBoxLanguage::PlainText
    {
        badges.push((
            state.language.get().label(),
            Style::default()
                .fg(theme.on_secondary_container)
                .bg(theme.secondary_container),
        ));
    }

    let block = Block::default()
        .title(component_title(theme, title, &badges))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .bg(surface_bg);

    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());

    if inner.height == 0 {
        return;
    }

    frame.render_widget(
        Block::default().style(Style::default().bg(surface_bg)),
        inner,
    );

    // Sync scroll
    let cy = state.cursor_y.get();
    let mut sy = state.scroll_y.get();
    if cy < sy {
        sy = cy;
    } else if cy >= sy + inner.height as usize {
        sy = cy - inner.height as usize + 1;
    }
    state.scroll_y.set(sy);

    let gutter_width = if state.show_line_numbers.get() { 4 } else { 0 };
    let selection = state.get_normalized_selection();
    let lines = state.lines.get_clone();
    let sx = state.scroll_x.get();
    let text_width = (inner.width as usize).saturating_sub(gutter_width);
    let gutter_style = Style::default()
        .fg(if is_focused || is_selected {
            theme.primary
        } else {
            theme.on_surface_variant
        })
        .bg(surface_bg);
    let text_style = Style::default().fg(theme.on_surface).bg(surface_bg);

    for y in 0..inner.height as usize {
        let line_idx = sy + y;
        if line_idx >= lines.len() {
            break;
        }
        let line_y = inner.y + y as u16;

        if state.show_line_numbers.get() {
            frame.buffer_mut().set_string(
                inner.x,
                line_y,
                format!("{:3} ", line_idx + 1),
                gutter_style,
            );
        }

        let line_content = &lines[line_idx];
        let text_x = inner.x + gutter_width as u16;
        for (column, symbol) in line_content.chars().skip(sx).take(text_width).enumerate() {
            let style = syntax_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.line_styles.get(line_idx))
                .and_then(|styles| styles.get(sx + column))
                .copied()
                .unwrap_or(text_style)
                .bg(surface_bg);
            let cell = &mut frame.buffer_mut()[(text_x + column as u16, line_y)];
            cell.set_char(symbol);
            cell.set_style(style);
        }

        if let Some(((sy_sel, sx_sel), (ey_sel, ex_sel))) = selection {
            if line_idx >= sy_sel && line_idx <= ey_sel {
                let s = if line_idx == sy_sel {
                    sx_sel.saturating_sub(sx)
                } else {
                    0
                };
                let e = if line_idx == ey_sel {
                    ex_sel.saturating_sub(sx)
                } else {
                    line_content.chars().count().saturating_sub(sx)
                };
                let max_w = text_width;
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
        if vx < text_width && vy < inner.height as usize {
            frame.set_cursor_position((
                inner.x + gutter_width as u16 + vx as u16,
                inner.y + vy as u16,
            ));
        }
    }
}

fn component_title(
    theme: &crate::theme::SmashTheme,
    title: String,
    badges: &[(&str, Style)],
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(" {} ", title),
        Style::default()
            .fg(theme.on_surface)
            .add_modifier(Modifier::BOLD),
    )];

    for (label, style) in badges {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!(" {} ", label), *style));
    }

    Line::from(spans)
}
