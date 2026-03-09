use crate::events::{EventEmitter, EventStatus, SmashEvent};
use crate::reactive::{FocusState, use_focus};
use crate::theme::SmashTheme;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};
use sycamore_reactive::*;

// --- Events ---

#[derive(Clone, Debug)]
pub enum ButtonEvent {
    Click,
    Hover(bool),
    Focus(bool),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    #[default]
    Outline,
    Danger,
}

// --- Composable ---

#[derive(Clone)]
pub struct ButtonState {
    pub variant: Signal<ButtonVariant>,
    pub is_focused: FocusState,
    pub is_hovered: Signal<bool>,
    pub is_pressed: Signal<bool>,
    pub label: Signal<String>,
    pub events: EventEmitter<ButtonEvent>,
    area: Signal<Rect>,
}

pub fn use_button(label: &str) -> ButtonState {
    use_button_variant(label, ButtonVariant::Outline)
}

pub fn use_button_variant(label: &str, variant: ButtonVariant) -> ButtonState {
    ButtonState {
        variant: create_signal(variant),
        is_focused: use_focus(false),
        is_hovered: create_signal(false),
        is_pressed: create_signal(false),
        label: create_signal(label.to_string()),
        events: EventEmitter::new(),
        area: create_signal(Rect::default()),
    }
}

impl ButtonState {
    fn set_focus(&self, focused: bool) {
        if !focused && self.is_pressed.get() {
            self.is_pressed.set(false);
        }
        if self.is_focused.get() != focused {
            self.is_focused.set(focused);
            self.events.emit(ButtonEvent::Focus(focused));
        }
    }

    pub fn focus(&self) {
        self.set_focus(true);
    }

    pub fn blur(&self) {
        self.set_focus(false);
    }

    pub fn set_variant(&self, variant: ButtonVariant) {
        self.variant.set(variant);
    }

    pub fn set_area(&self, area: Rect) {
        self.area.set(area);
    }

    pub fn area(&self) -> Rect {
        self.area.get()
    }

    pub fn on_click(&self, f: impl Fn(ButtonEvent) + 'static) {
        self.events.subscribe(f);
    }

    pub fn on_hover(&self, f: impl Fn(bool) + 'static) {
        self.events.subscribe(move |event| {
            if let ButtonEvent::Hover(is_hovered) = event {
                f(is_hovered);
            }
        });
    }

    pub fn on_focus(&self, f: impl Fn(bool) + 'static) {
        self.events.subscribe(move |event| {
            if let ButtonEvent::Focus(is_focused) = event {
                f(is_focused);
            }
        });
    }

    pub fn handle_event(&self, event: &SmashEvent) -> EventStatus {
        let area = self.area();
        match event {
            SmashEvent::Key(key) if self.is_focused.get() => {
                let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                // Allow Ctrl+Arrows to bubble up for tab switching
                if is_ctrl
                    && matches!(
                        key.code,
                        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                    )
                {
                    return EventStatus::Ignored;
                }

                if key.code == KeyCode::Enter {
                    match key.kind {
                        KeyEventKind::Press => {
                            self.is_pressed.set(false);
                            self.events.emit(ButtonEvent::Click);
                            return EventStatus::Handled;
                        }
                        KeyEventKind::Repeat => return EventStatus::Handled,
                        KeyEventKind::Release => {
                            self.is_pressed.set(false);
                            return EventStatus::Handled;
                        }
                    }
                }
            }
            SmashEvent::Mouse(mouse) => {
                let mx = mouse.column;
                let my = mouse.row;
                // Use a slightly smaller hit area to match visual shrinking if needed,
                // but for now keeping hit area as the full passed rect is standard.
                let over = mx >= area.x
                    && mx < area.x + area.width
                    && my >= area.y
                    && my < area.y + area.height;

                let was_hovered = self.is_hovered.get();
                if over != was_hovered {
                    self.is_hovered.set(over);
                    self.events.emit(ButtonEvent::Hover(over));
                    if !over && !self.is_pressed.get() {
                        self.blur();
                    }
                }

                if over {
                    if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                        self.focus();
                        self.is_pressed.set(true);
                        return EventStatus::Handled;
                    }
                    if let MouseEventKind::Up(MouseButton::Left) = mouse.kind {
                        if self.is_pressed.get() {
                            self.is_pressed.set(false);
                            self.events.emit(ButtonEvent::Click);
                            return EventStatus::Handled;
                        }
                    }
                } else if self.is_pressed.get() {
                    self.blur();
                }
            }
            _ => {}
        }
        EventStatus::Ignored
    }

    pub fn handle_smash_event(&self, event: &SmashEvent, area: Rect) -> EventStatus {
        self.set_area(area);
        self.handle_event(event)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &SmashTheme) {
        button_component(frame, area, self, theme);
    }
}

// --- Component (Stateless Function) ---

pub fn button_component(frame: &mut Frame, area: Rect, state: &ButtonState, theme: &SmashTheme) {
    state.set_area(area);
    let variant = state.variant.get();
    let focused = state.is_focused.get();
    let hovered = state.is_hovered.get();
    let pressed = state.is_pressed.get();

    let (default_bg, default_fg, hover_bg, hover_fg, active_bg, active_fg) = match variant {
        ButtonVariant::Primary => (
            theme.primary_container,
            theme.on_primary_container,
            theme.primary,
            theme.on_primary,
            theme.primary,
            theme.on_primary,
        ),
        ButtonVariant::Secondary => (
            theme.secondary_container,
            theme.on_secondary_container,
            theme.secondary,
            theme.on_secondary,
            theme.secondary,
            theme.on_secondary,
        ),
        ButtonVariant::Outline => (
            theme.surface,
            theme.on_surface,
            theme.surface_variant,
            theme.on_surface_variant,
            theme.primary_container,
            theme.on_primary_container,
        ),
        ButtonVariant::Danger => (
            theme.error_container,
            theme.on_error_container,
            theme.error,
            theme.on_error,
            theme.error,
            theme.on_error,
        ),
    };

    let (bg, fg, border_style, label_style) = if pressed {
        (
            active_bg,
            active_fg,
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
            Modifier::BOLD,
        )
    } else if focused {
        (
            active_bg,
            active_fg,
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
            Modifier::BOLD,
        )
    } else if hovered {
        (
            hover_bg,
            hover_fg,
            Style::default().fg(theme.outline),
            Modifier::empty(),
        )
    } else {
        (
            default_bg,
            default_fg,
            Style::default().fg(theme.outline_variant),
            Modifier::empty(),
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .border_type(BorderType::Rounded)
        .bg(theme.surface);

    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    frame.render_widget(Block::default().style(Style::default().bg(bg)), inner);

    let accent_width = if (focused || pressed) && inner.width > 1 {
        1
    } else {
        0
    };
    if accent_width > 0 {
        frame.render_widget(
            Block::default().style(Style::default().bg(fg)),
            Rect::new(inner.x, inner.y, accent_width, inner.height),
        );
    }

    let label = state.label.get_clone();
    let text_style = Style::default().fg(fg).bg(bg).add_modifier(label_style);
    let p = Paragraph::new(label)
        .alignment(Alignment::Center)
        .style(text_style);

    let vert_center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(inner.height.saturating_sub(1) / 2),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner)[1];

    let label_area = Rect::new(
        vert_center.x + accent_width,
        vert_center.y,
        vert_center.width.saturating_sub(accent_width),
        vert_center.height,
    );
    if label_area.width > 0 && label_area.height > 0 {
        frame.render_widget(p, label_area);
    }
}
