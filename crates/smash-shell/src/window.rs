use anyhow::Result;
use crossterm::{
    cursor,
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::prelude::*;
use std::io::{self, Stdout};
use std::time::Duration;
use sycamore_reactive::*;
use crate::events::{SmashEvent, Dispatcher};

pub struct Window {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    pub should_quit: bool,
    pub key_events: Vec<KeyEvent>,
    pub theme: crate::theme::SmashTheme,
    pub dispatcher: Dispatcher,
}

impl Window {
    pub fn new() -> Result<Self> {
        if let Err(e) = enable_raw_mode() {
            return Err(anyhow::anyhow!("Failed to enable raw mode: {}", e));
        }

        let mut stdout = io::stdout();
        if let Err(e) = execute!(
            stdout,
            EnterAlternateScreen,
            cursor::Hide,
            EnableBracketedPaste,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        ) {
            let _ = disable_raw_mode();
            return Err(anyhow::anyhow!("Failed to setup terminal: {}", e));
        }

        let backend = CrosstermBackend::new(stdout);
        let terminal = match Terminal::new(backend) {
            Ok(t) => t,
            Err(e) => {
                let _ = disable_raw_mode();
                let _ = execute!(
                    io::stdout(),
                    PopKeyboardEnhancementFlags,
                    DisableBracketedPaste,
                    LeaveAlternateScreen,
                    cursor::Show
                );
                return Err(anyhow::anyhow!("Failed to create terminal: {}", e));
            }
        };

        Ok(Self {
            terminal,
            should_quit: false,
            key_events: Vec::new(),
            theme: crate::theme::SmashTheme::from_seed(crate::theme::presets::VIOLET, true),
            dispatcher: Dispatcher { events: create_signal(None) },
        })
    }

    pub fn close(&mut self) -> Result<()> {
        disable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            LeaveAlternateScreen,
            cursor::Show
        )?;
        Ok(())
    }

    pub fn update(&mut self) -> Result<bool> {
        self.key_events.clear();
        self.dispatcher.events.set(None);
        
        while event::poll(Duration::from_millis(0))? {
            match event::read()? {
                Event::Key(key) => {
                    if (key.kind == event::KeyEventKind::Press || key.kind == event::KeyEventKind::Repeat)
                        && let KeyCode::Char('c') = key.code
                        && key.modifiers.contains(event::KeyModifiers::CONTROL)
                    {
                        self.should_quit = true;
                    }
                    self.key_events.push(key);
                    self.dispatcher.emit(SmashEvent::Key(key));
                }
                Event::Mouse(mouse) => {
                    self.dispatcher.emit(SmashEvent::Mouse(mouse));
                }
                Event::Resize(w, h) => {
                    self.terminal.resize(Rect::new(0, 0, w, h))?;
                    self.dispatcher.emit(SmashEvent::Resize(w, h));
                }
                _ => {}
            }
        }
        
        if self.key_events.is_empty() {
            std::thread::sleep(Duration::from_millis(16));
        }

        Ok(!self.should_quit)
    }

    pub fn draw<F>(&mut self, f: F) -> Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.terminal.draw(f)?;
        Ok(())
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
