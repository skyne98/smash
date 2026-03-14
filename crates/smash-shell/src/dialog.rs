use crate::button::{ButtonEvent, ButtonState, ButtonVariant, use_button_variant};
use crate::events::{EventStatus, SmashEvent};
use crate::reactive::{
    FocusState, NavigatorFocusable, SelectionState, handle_selected_navigator_event,
    sync_navigator_focus, use_focus, use_selection,
};
use crate::theme::SmashTheme;
use crossterm::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sycamore_reactive::*;
use tachyonfx::{Effect, Interpolation, Motion, fx};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogEvent {
    Ignored,
    Handled,
    Cancelled,
    Confirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogAction {
    Cancel,
    Confirm,
}

#[derive(Clone)]
pub struct DialogState {
    pub title: Signal<String>,
    pub message: Signal<String>,
    pub cancel_label: Signal<String>,
    pub confirm_label: Signal<String>,
    pub is_open: FocusState,
    selected_action: SelectionState,
    cancel_button: ButtonState,
    confirm_button: ButtonState,
    pending_action: Signal<Option<DialogAction>>,
    show_generation: Signal<u64>,
    rendered_generation: Signal<u64>,
    show_effect: Signal<Option<Arc<Mutex<Effect>>>>,
    last_effect_tick: Signal<Option<Instant>>,
}

pub fn use_dialog(title: &str, message: &str) -> DialogState {
    let cancel_button = use_button_variant("cancel", ButtonVariant::Outline);
    let confirm_button = use_button_variant("confirm", ButtonVariant::Primary);
    let pending_action = create_signal(None);
    cancel_button.on_click({
        let pending_action = pending_action;
        move |event| {
            if let ButtonEvent::Click = event {
                pending_action.set(Some(DialogAction::Cancel));
            }
        }
    });
    confirm_button.on_click({
        let pending_action = pending_action;
        move |event| {
            if let ButtonEvent::Click = event {
                pending_action.set(Some(DialogAction::Confirm));
            }
        }
    });

    DialogState {
        title: create_signal(title.to_string()),
        message: create_signal(message.to_string()),
        cancel_label: create_signal("cancel".to_string()),
        confirm_label: create_signal("confirm".to_string()),
        is_open: use_focus(false),
        selected_action: use_selection(0, 2),
        cancel_button,
        confirm_button,
        pending_action,
        show_generation: create_signal(0),
        rendered_generation: create_signal(0),
        show_effect: create_signal(None),
        last_effect_tick: create_signal(None),
    }
}

impl DialogState {
    fn action_buttons(&self) -> [(usize, &dyn NavigatorFocusable); 2] {
        [
            (0, &self.cancel_button as &dyn NavigatorFocusable),
            (1, &self.confirm_button as &dyn NavigatorFocusable),
        ]
    }

    fn sync_action_buttons(&self) {
        sync_navigator_focus(Some(self.selected_action.get()), self.action_buttons());
    }

    fn set_selected_action(&self, index: usize) {
        self.selected_action.set(index);
        self.sync_action_buttons();
    }

    fn resolve_pending_action(&self) -> Option<DialogEvent> {
        let action = self.pending_action.get();
        self.pending_action.set(None);
        match action {
            Some(DialogAction::Cancel) => {
                self.close();
                Some(DialogEvent::Cancelled)
            }
            Some(DialogAction::Confirm) => {
                self.close();
                Some(DialogEvent::Confirmed)
            }
            None => None,
        }
    }

    fn sync_selected_action_from_buttons(&self) {
        if self.confirm_button.is_focused.get() {
            self.selected_action.set(1);
        } else if self.cancel_button.is_focused.get() {
            self.selected_action.set(0);
        }
    }

    pub fn open(&self) {
        self.pending_action.set(None);
        self.set_selected_action(0);
        self.show_generation
            .set(self.show_generation.get().wrapping_add(1));
        self.show_effect.set(None);
        self.last_effect_tick.set(None);
        self.is_open.focus();
    }

    pub fn close(&self) {
        self.selected_action.set(0);
        self.pending_action.set(None);
        self.cancel_button.blur();
        self.confirm_button.blur();
        self.show_effect.set(None);
        self.last_effect_tick.set(None);
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
        let cancel_label = cancel_label.into();
        let confirm_label = confirm_label.into();
        self.cancel_label.set(cancel_label.clone());
        self.confirm_label.set(confirm_label.clone());
        self.cancel_button.label.set(cancel_label);
        self.confirm_button.label.set(confirm_label);
    }

    pub fn handle_smash_event(&self, event: &SmashEvent) -> DialogEvent {
        if !self.is_open() {
            return DialogEvent::Ignored;
        }

        match event {
            SmashEvent::Key(key) => {
                if key.code == KeyCode::Enter {
                    let status = handle_selected_navigator_event(
                        Some(self.selected_action.get()),
                        event,
                        self.action_buttons(),
                    );
                    if let Some(result) = self.resolve_pending_action() {
                        return result;
                    }
                    return if status == EventStatus::Handled {
                        DialogEvent::Handled
                    } else {
                        DialogEvent::Ignored
                    };
                }

                if key.kind == KeyEventKind::Release {
                    return DialogEvent::Ignored;
                }

                match key.code {
                    KeyCode::Esc => {
                        self.close();
                        DialogEvent::Cancelled
                    }
                    KeyCode::Left | KeyCode::Up | KeyCode::BackTab => {
                        self.set_selected_action(0);
                        DialogEvent::Handled
                    }
                    KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                        self.set_selected_action(1);
                        DialogEvent::Handled
                    }
                    _ => DialogEvent::Ignored,
                }
            }
            SmashEvent::Mouse(mouse) => {
                let cancel_status = self.cancel_button.handle_event(event);
                let confirm_status = self.confirm_button.handle_event(event);
                self.sync_selected_action_from_buttons();

                if let Some(result) = self.resolve_pending_action() {
                    result
                } else if cancel_status == EventStatus::Handled
                    || confirm_status == EventStatus::Handled
                    || matches!(
                        mouse.kind,
                        MouseEventKind::Down(MouseButton::Left)
                            | MouseEventKind::Up(MouseButton::Left)
                            | MouseEventKind::Moved
                    )
                {
                    DialogEvent::Handled
                } else {
                    DialogEvent::Ignored
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

    let popup = centered_rect(area, 56, 34);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(dialog_title(theme, state.title.get_clone()))
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
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
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

    state.sync_action_buttons();
    state.cancel_button.render(frame, buttons[0], theme);
    state.confirm_button.render(frame, buttons[1], theme);

    frame.render_widget(
        Paragraph::new("Enter confirms  •  Esc stays here")
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(theme.on_surface_variant)
                    .bg(theme.surface),
            ),
        sections[2],
    );

    process_dialog_show_effect(frame, popup, state, theme);
}

fn process_dialog_show_effect(
    frame: &mut Frame,
    popup: Rect,
    state: &DialogState,
    theme: &SmashTheme,
) {
    if state.rendered_generation.get() != state.show_generation.get()
        || state.show_effect.get_clone().is_none()
    {
        state
            .show_effect
            .set(Some(Arc::new(Mutex::new(dialog_show_effect(theme, popup)))));
        state.rendered_generation.set(state.show_generation.get());
        state.last_effect_tick.set(None);
    }

    let Some(effect) = state.show_effect.get_clone() else {
        return;
    };

    let now = Instant::now();
    let elapsed = state
        .last_effect_tick
        .get()
        .map(|last| now.saturating_duration_since(last))
        .unwrap_or_else(|| Duration::from_millis(16));
    state.last_effect_tick.set(Some(now));

    let Ok(mut effect) = effect.lock() else {
        return;
    };

    if !effect.done() {
        effect.process(elapsed.into(), frame.buffer_mut(), popup);
    }
}

fn dialog_title(theme: &SmashTheme, title: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {} ", title),
            Style::default()
                .fg(theme.on_surface)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            " dialog ",
            Style::default()
                .fg(theme.on_primary_container)
                .bg(theme.primary_container),
        ),
    ])
}

fn dialog_show_effect(theme: &SmashTheme, popup: Rect) -> Effect {
    let gradient = popup.height.saturating_sub(1).clamp(3, 8);
    fx::parallel(&[
        fx::slide_in(
            Motion::UpToDown,
            gradient,
            0,
            theme.background,
            (140, Interpolation::QuadOut),
        ),
        fx::fade_from_fg(theme.primary_container, (220, Interpolation::SineOut)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::presets;
    use ratatui::backend::TestBackend;
    use sycamore_reactive::create_root;

    #[test]
    fn opening_dialog_resets_show_effect_state() {
        let _root = create_root(|| {
            let dialog = use_dialog("quit", "leave the app?");
            let theme = SmashTheme::from_seed(presets::VIOLET, true);
            let backend = TestBackend::new(80, 24);
            let mut terminal = Terminal::new(backend).unwrap();

            dialog.open();
            assert_eq!(dialog.show_generation.get(), 1);
            assert!(dialog.show_effect.get_clone().is_none());
            assert_eq!(dialog.last_effect_tick.get(), None);

            terminal
                .draw(|frame| {
                    dialog.render(frame, frame.area(), &theme);
                })
                .unwrap();

            assert_eq!(dialog.rendered_generation.get(), 1);
            assert!(dialog.show_effect.get_clone().is_some());
            assert!(dialog.last_effect_tick.get().is_some());

            dialog.close();
            dialog.open();

            assert_eq!(dialog.show_generation.get(), 2);
            assert!(dialog.show_effect.get_clone().is_none());
            assert_eq!(dialog.last_effect_tick.get(), None);
        });
    }

    #[test]
    fn dialog_selection_syncs_button_focus() {
        let _root = create_root(|| {
            let dialog = use_dialog("quit", "leave the app?");
            dialog.open();

            assert!(dialog.cancel_button.is_focused.get());
            assert!(!dialog.confirm_button.is_focused.get());

            assert_eq!(
                dialog.handle_smash_event(&SmashEvent::Key(crossterm::event::KeyEvent {
                    code: KeyCode::Right,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                    kind: KeyEventKind::Press,
                    state: crossterm::event::KeyEventState::empty(),
                })),
                DialogEvent::Handled
            );

            assert!(!dialog.cancel_button.is_focused.get());
            assert!(dialog.confirm_button.is_focused.get());
        });
    }
}
