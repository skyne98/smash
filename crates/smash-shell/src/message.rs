use crate::events::{EventStatus, SmashEvent};
use crate::reactive::{FocusState, NavigatorFocusable, use_focus};
use crate::theme::SmashTheme;
use arboard::Clipboard;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sycamore_reactive::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageEvent {
    Ignored,
    Handled,
    Dismissed,
}

#[derive(Clone)]
pub struct MessageState {
    pub title: Signal<String>,
    pub message: Signal<String>,
    pub is_open: FocusState,
    clipboard: Signal<Option<Arc<Mutex<Clipboard>>>>,
    copied_feedback: Signal<Option<Instant>>,
}

const COPY_FEEDBACK_DURATION: Duration = Duration::from_millis(1500);

pub fn use_message(title: &str, message: &str) -> MessageState {
    MessageState {
        title: create_signal(title.to_string()),
        message: create_signal(message.to_string()),
        is_open: use_focus(false),
        clipboard: create_signal(Clipboard::new().ok().map(|c| Arc::new(Mutex::new(c)))),
        copied_feedback: create_signal(None),
    }
}

impl MessageState {
    pub fn open(&self) {
        self.is_open.focus();
    }

    pub fn close(&self) {
        self.copied_feedback.set(None);
        self.is_open.blur();
    }

    pub fn is_open(&self) -> bool {
        self.is_open.get()
    }

    pub fn set_title(&self, title: impl Into<String>) {
        self.title.set(title.into());
    }

    pub fn set_message(&self, message: impl Into<String>) {
        self.message.set(message.into());
    }

    pub fn open_with_message(&self, message: impl Into<String>) {
        self.message.set(message.into());
        self.copied_feedback.set(None);
        self.open();
    }

    fn copy_message_to_clipboard(&self) {
        let text = self.message.get_clone();
        if let Some(cb) = self.clipboard.get_clone().as_ref()
            && let Ok(mut cb) = cb.lock()
        {
            let _ = cb.set_text(text);
            self.copied_feedback
                .set(Some(Instant::now() + COPY_FEEDBACK_DURATION));
        }
    }

    pub fn handle_smash_event(&self, event: &SmashEvent) -> MessageEvent {
        if !self.is_open() {
            return MessageEvent::Ignored;
        }

        match event {
            SmashEvent::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    return MessageEvent::Ignored;
                }

                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('c')
                {
                    self.copy_message_to_clipboard();
                    return MessageEvent::Handled;
                }

                match key.code {
                    KeyCode::Esc => {
                        self.close();
                        MessageEvent::Dismissed
                    }
                    _ => MessageEvent::Ignored,
                }
            }
            SmashEvent::Mouse(mouse) => {
                if matches!(
                    mouse.kind,
                    MouseEventKind::Down(MouseButton::Left)
                        | MouseEventKind::Up(MouseButton::Left)
                ) {
                    self.close();
                    MessageEvent::Dismissed
                } else {
                    MessageEvent::Handled
                }
            }
            _ => MessageEvent::Ignored,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &SmashTheme) {
        message_component(frame, area, self, theme);
    }
}

impl NavigatorFocusable for MessageState {
    fn sync_navigator_focus(&self, _selected: bool) {}

    fn handle_navigator_event(&self, event: &SmashEvent) -> EventStatus {
        match self.handle_smash_event(event) {
            MessageEvent::Ignored => EventStatus::Ignored,
            _ => EventStatus::Handled,
        }
    }
}

pub fn message_component(
    frame: &mut Frame,
    area: Rect,
    state: &MessageState,
    theme: &SmashTheme,
) {
    if !state.is_open() {
        return;
    }

    let popup = centered_rect(area, 50, 28);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(message_title(theme, state.title.get_clone()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(theme.surface));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let show_copied = state
        .copied_feedback
        .get()
        .is_some_and(|deadline| Instant::now() < deadline);

    frame.render_widget(
        Paragraph::new(state.message.get_clone())
            .wrap(Wrap { trim: true })
            .style(
                Style::default()
                    .fg(if show_copied {
                        theme.primary
                    } else {
                        theme.on_surface
                    })
                    .bg(theme.surface),
            ),
        sections[0],
    );

    let hint = if show_copied {
        "copied!  •  click or Esc to close"
    } else {
        "Ctrl+C to copy  •  click or Esc to close"
    };

    frame.render_widget(
        Paragraph::new(hint)
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(if show_copied {
                        theme.primary
                    } else {
                        theme.on_surface_variant
                    })
                    .bg(theme.surface),
            ),
        sections[1],
    );
}

fn message_title(theme: &SmashTheme, title: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {} ", title),
            Style::default()
                .fg(theme.on_surface)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            " message ",
            Style::default()
                .fg(theme.on_primary_container)
                .bg(theme.primary_container),
        ),
    ])
}

fn centered_rect(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}
