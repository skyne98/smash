use crate::events::{EventStatus, SmashEvent};
use crate::reactive::{FocusState, InteractionState, NavigatorFocusable, use_interaction};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use tui_term::vt100;
use tui_term::widget::PseudoTerminal;

#[derive(Clone)]
pub struct TerminalState {
    pub parser: Arc<Mutex<vt100::Parser>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    interaction: InteractionState,
    pub is_selected: FocusState,
    pub is_focused: FocusState,
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
    pair.slave.spawn_command(cmd)?;

    let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
    let parser_clone = Arc::clone(&parser);
    let mut reader = pair.master.try_clone_reader()?;

    thread::spawn(move || {
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
    let interaction = use_interaction(false, false);

    Ok(TerminalState {
        parser,
        master,
        writer,
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

        // Forward to PTY
        if let Ok(mut writer) = self.writer.lock() {
            let result = match key.code {
                KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(ctrl) = control_char(c) {
                        writer.write_all(&[ctrl])
                    } else {
                        Ok(())
                    }
                }
                KeyCode::Char(c) => {
                    let mut buf = [0u8; 4];
                    writer.write_all(c.encode_utf8(&mut buf).as_bytes())
                }
                KeyCode::Enter => writer.write_all(b"\r"),
                KeyCode::Backspace => writer.write_all(b"\x7f"),
                KeyCode::Up => writer.write_all(b"\x1b[A"),
                KeyCode::Down => writer.write_all(b"\x1b[B"),
                KeyCode::Right => writer.write_all(b"\x1b[C"),
                KeyCode::Left => writer.write_all(b"\x1b[D"),
                _ => Ok(()),
            };

            if result.is_ok() {
                let _ = writer.flush();
            }
        }

        true
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
