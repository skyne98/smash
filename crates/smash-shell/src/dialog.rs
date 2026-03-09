use crate::events::SmashEvent;
use crate::reactive::{FocusState, SelectionState, use_focus, use_selection};
use crate::theme::SmashTheme;
use crossterm::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use sycamore_reactive::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogEvent {
    Ignored,
    Handled,
    Cancelled,
    Confirmed,
}

#[derive(Clone, Copy)]
pub struct DialogState {
    pub title: Signal<String>,
    pub message: Signal<String>,
    pub cancel_label: Signal<String>,
    pub confirm_label: Signal<String>,
    pub is_open: FocusState,
    selected_action: SelectionState,
    cancel_area: Signal<Rect>,
    confirm_area: Signal<Rect>,
}

pub fn use_dialog(title: &str, message: &str) -> DialogState {
    DialogState {
        title: create_signal(title.to_string()),
        message: create_signal(message.to_string()),
        cancel_label: create_signal("cancel".to_string()),
        confirm_label: create_signal("confirm".to_string()),
        is_open: use_focus(false),
        selected_action: use_selection(0, 2),
        cancel_area: create_signal(Rect::default()),
        confirm_area: create_signal(Rect::default()),
    }
}

impl DialogState {
    pub fn open(&self) {
        self.selected_action.set(0);
        self.is_open.focus();
    }

    pub fn close(&self) {
        self.selected_action.set(0);
        self.is_open.blur();
    }

    pub fn open_with_message(&self, message: impl Into<String>) {
        self.message.set(message.into());
        self.open();
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

    pub fn set_labels(&self, cancel_label: impl Into<String>, confirm_label: impl Into<String>) {
        self.cancel_label.set(cancel_label.into());
        self.confirm_label.set(confirm_label.into());
    }

    pub fn handle_smash_event(&self, event: &SmashEvent) -> DialogEvent {
        if !self.is_open() {
            return DialogEvent::Ignored;
        }

        match event {
            SmashEvent::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    return DialogEvent::Ignored;
                }

                match key.code {
                    KeyCode::Esc => {
                        self.close();
                        DialogEvent::Cancelled
                    }
                    KeyCode::Left | KeyCode::Up | KeyCode::BackTab => {
                        self.selected_action.set(0);
                        DialogEvent::Handled
                    }
                    KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                        self.selected_action.set(1);
                        DialogEvent::Handled
                    }
                    KeyCode::Enter => {
                        let result = if self.selected_action.get() == 1 {
                            DialogEvent::Confirmed
                        } else {
                            DialogEvent::Cancelled
                        };
                        self.close();
                        result
                    }
                    _ => DialogEvent::Ignored,
                }
            }
            SmashEvent::Mouse(mouse)
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) =>
            {
                let position = (mouse.column, mouse.row);
                if rect_contains(self.cancel_area.get(), position) {
                    self.selected_action.set(0);
                    self.close();
                    DialogEvent::Cancelled
                } else if rect_contains(self.confirm_area.get(), position) {
                    self.selected_action.set(1);
                    self.close();
                    DialogEvent::Confirmed
                } else {
                    DialogEvent::Handled
                }
            }
            _ => DialogEvent::Ignored,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &SmashTheme) {
        dialog_component(frame, area, self, theme);
    }
}

pub fn dialog_component(frame: &mut Frame, area: Rect, state: &DialogState, theme: &SmashTheme) {
    if !state.is_open() {
        return;
    }

    let popup = centered_rect(area, 60, 40);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(state.title.get_clone())
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
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
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(inner);

    frame.render_widget(
        Paragraph::new(state.message.get_clone())
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(theme.on_surface).bg(theme.surface)),
        sections[0],
    );

    let buttons = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(sections[1]);

    state.cancel_area.set(buttons[0]);
    state.confirm_area.set(buttons[1]);

    render_dialog_action(
        frame,
        buttons[0],
        &state.cancel_label.get_clone(),
        state.selected_action.get() == 0,
        false,
        theme,
    );
    render_dialog_action(
        frame,
        buttons[1],
        &state.confirm_label.get_clone(),
        state.selected_action.get() == 1,
        true,
        theme,
    );
}

fn render_dialog_action(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    selected: bool,
    confirm: bool,
    theme: &SmashTheme,
) {
    let (bg, fg, border_color, border_type) = if selected && confirm {
        (
            theme.primary,
            theme.on_primary,
            theme.primary,
            BorderType::Double,
        )
    } else if selected {
        (
            theme.surface_variant,
            theme.on_surface_variant,
            theme.primary,
            BorderType::Double,
        )
    } else if confirm {
        (
            theme.primary_container,
            theme.on_primary_container,
            theme.outline,
            BorderType::Rounded,
        )
    } else {
        (
            theme.surface_variant,
            theme.on_surface_variant,
            theme.outline,
            BorderType::Rounded,
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(theme.surface));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    frame.render_widget(Block::default().style(Style::default().bg(bg)), inner);
    frame.render_widget(
        Paragraph::new(if selected {
            format!("› {label} ‹")
        } else {
            label.to_string()
        })
        .alignment(Alignment::Center)
        .style(Style::default().fg(fg).bg(bg).add_modifier(if selected {
            Modifier::BOLD
        } else {
            Modifier::empty()
        })),
        inner,
    );
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

fn rect_contains(area: Rect, position: (u16, u16)) -> bool {
    let (x, y) = position;
    x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height
}
