use crate::events::{EventStatus, SmashEvent};
use crate::reactive::{FocusState, InteractionState, NavigatorFocusable, use_interaction};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use tui_term::vt100;
use tui_term::widget::PseudoTerminal;

#[derive(Clone)]
pub struct TerminalState {
    pub parser: Arc<Mutex<vt100::Parser>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    resources: Arc<TerminalResources>,
    interaction: InteractionState,
    pub is_selected: FocusState,
    pub is_focused: FocusState,
}

struct TerminalResources {
    child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
    reader_thread: Mutex<Option<JoinHandle<()>>>,
}

pub fn use_terminal(rows: u16, cols: u16) -> Result<TerminalState> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = if cfg!(windows) { "cmd.exe" } else { "bash" };
    let cmd = CommandBuilder::new(shell);
    let child = pair.slave.spawn_command(cmd)?;

    let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
    let parser_clone = Arc::clone(&parser);
    let mut reader = pair.master.try_clone_reader()?;

    let reader_thread = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            if let Ok(mut p) = parser_clone.lock() {
                p.process(&buf[..n]);
            }
        }
    });

    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    let master = Arc::new(Mutex::new(pair.master));
    let resources = Arc::new(TerminalResources::new(child, reader_thread));
    let interaction = use_interaction(false, false);

    Ok(TerminalState {
        parser,
        master,
        writer,
        resources,
        interaction,
        is_selected: interaction.selected(),
        is_focused: interaction.focused(),
    })
}

impl TerminalState {
    pub fn select(&self) {
        self.interaction.select();
    }

    pub fn deselect(&self) {
        self.interaction.deselect();
    }

    pub fn focus(&self) {
        self.interaction.focus();
    }

    pub fn blur(&self) {
        self.interaction.blur();
    }

    pub fn shutdown(&self) {
        self.resources.shutdown();
    }

    pub fn handle_smash_event(&self, event: &SmashEvent) -> EventStatus {
        match event {
            SmashEvent::Key(key) if self.handle_event(key) => EventStatus::Handled,
            SmashEvent::Key(_) => EventStatus::Ignored,
            _ => EventStatus::Ignored,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &crate::theme::SmashTheme) {
        terminal_component(frame, area, self, theme);
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

        let Some(sequence) = terminal_key_sequence(key) else {
            return false;
        };

        if let Ok(mut writer) = self.writer.lock()
            && writer.write_all(&sequence).is_ok()
        {
            let _ = writer.flush();
            return true;
        }

        false
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        if let Ok(master) = self.master.lock() {
            master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
        }
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
        Ok(())
    }
}

impl NavigatorFocusable for TerminalState {
    fn sync_navigator_focus(&self, selected: bool) {
        self.interaction.sync_navigator(selected);
    }

    fn is_navigator_active(&self) -> bool {
        self.is_focused.get()
    }

    fn handle_navigator_event(&self, event: &SmashEvent) -> EventStatus {
        self.handle_smash_event(event)
    }
}

impl TerminalResources {
    fn new(child: Box<dyn Child + Send + Sync>, reader_thread: JoinHandle<()>) -> Self {
        Self {
            child: Mutex::new(Some(child)),
            reader_thread: Mutex::new(Some(reader_thread)),
        }
    }

    fn shutdown(&self) {
        if let Ok(mut child_slot) = self.child.lock()
            && let Some(mut child) = child_slot.take()
        {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }

        if let Ok(mut thread_slot) = self.reader_thread.lock()
            && let Some(reader_thread) = thread_slot.take()
        {
            let _ = reader_thread.join();
        }
    }
}

impl Drop for TerminalResources {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn control_char(c: char) -> Option<u8> {
    if !c.is_ascii() {
        return None;
    }

    let upper = c.to_ascii_uppercase() as u8;
    match upper {
        b'@'..=b'_' => Some(upper & 0x1f),
        _ => None,
    }
}

pub(crate) fn terminal_key_sequence(key: &KeyEvent) -> Option<Vec<u8>> {
    let modifiers = key.modifiers;
    let alt = modifiers.contains(KeyModifiers::ALT);
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        KeyCode::Char(c) if ctrl => {
            let ctrl = control_char(c)?;
            Some(with_alt_prefix(alt, vec![ctrl]))
        }
        KeyCode::Char(c) => {
            let mut bytes = Vec::new();
            if alt {
                bytes.push(0x1b);
            }
            let mut encoded = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut encoded).as_bytes());
            Some(bytes)
        }
        KeyCode::Enter => Some(b"\r".to_vec()),
        KeyCode::Backspace => Some(b"\x7f".to_vec()),
        KeyCode::Tab => Some(b"\t".to_vec()),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Up => Some(csi_final(b"\x1b[A", b'A', modifiers)),
        KeyCode::Down => Some(csi_final(b"\x1b[B", b'B', modifiers)),
        KeyCode::Right => Some(csi_final(b"\x1b[C", b'C', modifiers)),
        KeyCode::Left => Some(csi_final(b"\x1b[D", b'D', modifiers)),
        KeyCode::Home => Some(csi_final(b"\x1b[H", b'H', modifiers)),
        KeyCode::End => Some(csi_final(b"\x1b[F", b'F', modifiers)),
        KeyCode::Insert => Some(csi_tilde(2, modifiers)),
        KeyCode::Delete => Some(csi_tilde(3, modifiers)),
        KeyCode::PageUp => Some(csi_tilde(5, modifiers)),
        KeyCode::PageDown => Some(csi_tilde(6, modifiers)),
        KeyCode::F(n) => function_key_sequence(n, modifiers),
        _ => None,
    }
}

fn with_alt_prefix(alt: bool, mut sequence: Vec<u8>) -> Vec<u8> {
    if alt {
        sequence.insert(0, 0x1b);
    }
    sequence
}

fn csi_final(unmodified: &[u8], final_byte: u8, modifiers: KeyModifiers) -> Vec<u8> {
    if let Some(modifier_code) = xterm_modifier_code(modifiers) {
        format!("\x1b[1;{}{}", modifier_code, final_byte as char).into_bytes()
    } else {
        unmodified.to_vec()
    }
}

fn csi_tilde(number: u8, modifiers: KeyModifiers) -> Vec<u8> {
    if let Some(modifier_code) = xterm_modifier_code(modifiers) {
        format!("\x1b[{};{}~", number, modifier_code).into_bytes()
    } else {
        format!("\x1b[{}~", number).into_bytes()
    }
}

fn function_key_sequence(number: u8, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    match number {
        1 => Some(csi_final(b"\x1bOP", b'P', modifiers)),
        2 => Some(csi_final(b"\x1bOQ", b'Q', modifiers)),
        3 => Some(csi_final(b"\x1bOR", b'R', modifiers)),
        4 => Some(csi_final(b"\x1bOS", b'S', modifiers)),
        5 => Some(csi_tilde(15, modifiers)),
        6 => Some(csi_tilde(17, modifiers)),
        7 => Some(csi_tilde(18, modifiers)),
        8 => Some(csi_tilde(19, modifiers)),
        9 => Some(csi_tilde(20, modifiers)),
        10 => Some(csi_tilde(21, modifiers)),
        11 => Some(csi_tilde(23, modifiers)),
        12 => Some(csi_tilde(24, modifiers)),
        _ => None,
    }
}

fn xterm_modifier_code(modifiers: KeyModifiers) -> Option<u8> {
    let mut code = 1;
    if modifiers.contains(KeyModifiers::SHIFT) {
        code += 1;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        code += 2;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        code += 4;
    }
    (code > 1).then_some(code)
}

pub fn terminal_component(
    frame: &mut Frame,
    area: Rect,
    state: &TerminalState,
    theme: &crate::theme::SmashTheme,
) {
    let is_focused = state.is_focused.get();
    let is_selected = state.is_selected.get();
    let border_color = if is_focused || is_selected {
        theme.primary
    } else {
        theme.outline_variant
    };
    let badge = if is_focused {
        Some((
            "active",
            Style::default()
                .fg(theme.on_primary_container)
                .bg(theme.primary_container),
        ))
    } else if is_selected {
        Some((
            "selected",
            Style::default()
                .fg(theme.on_secondary_container)
                .bg(theme.secondary_container),
        ))
    } else {
        None
    };
    let block = Block::default()
        .title(terminal_title(theme, badge))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .bg(if is_focused || is_selected {
            theme.surface_variant
        } else {
            theme.surface
        });

    let inner_area = block.inner(area);

    // Check for resize
    if let Ok(parser) = state.parser.lock() {
        let screen = parser.screen();
        if screen.size().0 != inner_area.height || screen.size().1 != inner_area.width {
            drop(parser);
            let _ = state.resize(inner_area.height, inner_area.width);
        }
    }

    if let Ok(parser) = state.parser.lock() {
        let term_widget = PseudoTerminal::new(parser.screen()).block(block);
        frame.render_widget(term_widget, area);
    }
}

fn terminal_title(theme: &crate::theme::SmashTheme, badge: Option<(&str, Style)>) -> Line<'static> {
    let mut spans = vec![Span::styled(
        " terminal ",
        Style::default()
            .fg(theme.on_surface)
            .add_modifier(Modifier::BOLD),
    )];

    if let Some((label, style)) = badge {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!(" {} ", label), style));
    }

    Line::from(spans)
}
