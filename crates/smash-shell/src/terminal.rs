use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use portable_pty::{native_pty_system, CommandBuilder, PtySize, MasterPty};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use sycamore_reactive::*;
use tui_term::vt100;
use tui_term::widget::PseudoTerminal;

#[derive(Clone)]
pub struct TerminalState {
    pub parser: Arc<Mutex<vt100::Parser>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub is_focused: Signal<bool>,
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
            if n == 0 { break; }
            if let Ok(mut p) = parser_clone.lock() {
                p.process(&buf[..n]);
            }
        }
    });

    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    let master = Arc::new(Mutex::new(pair.master));

    Ok(TerminalState {
        parser,
        master,
        writer,
        is_focused: create_signal(false),
    })
}

impl TerminalState {
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

        // Forward to PTY
        if let Ok(mut writer) = self.writer.lock() {
            let result = match key.code {
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

pub fn terminal_component(frame: &mut Frame, area: Rect, state: &TerminalState, theme: &crate::theme::SmashTheme) {
    let is_focused = state.is_focused.get();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.outline))
        .bg(theme.surface);
    
    if is_focused {
        block = block.title("terminal (focused - esc to unfocus)").border_style(Style::default().fg(theme.primary));
    } else {
        block = block.title("terminal (unfocused - enter to focus)");
    }

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
