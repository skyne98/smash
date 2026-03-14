use crate::events::{EventEmitter, EventStatus, SmashEvent};
use crate::reactive::{FocusState, NavigatorFocusable, use_focus};
use crate::theme::SmashTheme;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};
use std::time::{Duration, Instant};
use sycamore_reactive::*;

const KEYBOARD_PRESS_FEEDBACK: Duration = Duration::from_millis(120);
const BUTTON_MARGIN_X: u16 = 1;

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

#[derive(Clone, Copy)]
struct ButtonPalette {
    rest_fg: Color,
    rest_bg: Color,
    hover_fg: Color,
    hover_bg: Color,
    focus_fg: Color,
    focus_bg: Color,
    pressed_bg: Color,
    pressed_fg: Color,
}

// --- Composable ---

#[derive(Clone)]
pub struct ButtonState {
    pub variant: Signal<ButtonVariant>,
    pub is_focused: FocusState,
    pub is_hovered: Signal<bool>,
    pub is_pressed: Signal<bool>,
    pub label: Signal<String>,
    min_height: Signal<u16>,
    max_height: Signal<Option<u16>>,
    keyboard_press_deadline: Signal<Option<Instant>>,
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
        min_height: create_signal(0),
        max_height: create_signal(None),
        keyboard_press_deadline: create_signal(None),
        events: EventEmitter::new(),
        area: create_signal(Rect::default()),
    }
}

impl ButtonState {
    fn clear_pressed(&self) {
        self.is_pressed.set(false);
        self.keyboard_press_deadline.set(None);
    }

    fn sync_keyboard_press_feedback(&self) {
        if let Some(deadline) = self.keyboard_press_deadline.get()
            && Instant::now() >= deadline
        {
            self.clear_pressed();
        }
    }

    #[cfg(test)]
    pub(crate) fn expire_keyboard_press_feedback_for_test(&self) {
        self.keyboard_press_deadline
            .set(Some(Instant::now() - Duration::from_millis(1)));
    }

    fn set_focus(&self, focused: bool) {
        if !focused && self.is_pressed.get() {
            self.clear_pressed();
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

    pub fn set_min_height(&self, min_height: u16) {
        self.min_height.set(min_height);
        if let Some(max_height) = self.max_height.get()
            && max_height < min_height
        {
            self.max_height.set(Some(min_height));
        }
    }

    pub fn set_max_height(&self, max_height: Option<u16>) {
        self.max_height
            .set(max_height.map(|height| height.max(self.min_height.get())));
    }

    pub fn clear_max_height(&self) {
        self.max_height.set(None);
    }

    pub fn desired_height(&self) -> u16 {
        let label_height = self.label.get_clone().lines().count().max(1) as u16;
        let intrinsic_height = label_height;
        let mut height = intrinsic_height.max(self.min_height.get());
        if let Some(max_height) = self.max_height.get() {
            height = height.min(max_height.max(intrinsic_height));
        }
        height.max(1)
    }

    pub fn layout_area(&self, area: Rect) -> Rect {
        if area.width == 0 || area.height == 0 {
            return area;
        }

        let height = self.desired_height().min(area.height);
        Rect::new(
            area.x,
            area.y + area.height.saturating_sub(height) / 2,
            area.width,
            height,
        )
    }

    pub fn surface_area(&self, area: Rect) -> Rect {
        if area.width <= BUTTON_MARGIN_X * 2 || area.height == 0 {
            return area;
        }

        Rect::new(
            area.x + BUTTON_MARGIN_X,
            area.y,
            area.width.saturating_sub(BUTTON_MARGIN_X * 2),
            area.height,
        )
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
                            self.is_pressed.set(true);
                            self.keyboard_press_deadline
                                .set(Some(Instant::now() + KEYBOARD_PRESS_FEEDBACK));
                            self.events.emit(ButtonEvent::Click);
                            return EventStatus::Handled;
                        }
                        KeyEventKind::Repeat => {
                            self.keyboard_press_deadline
                                .set(Some(Instant::now() + KEYBOARD_PRESS_FEEDBACK));
                            return EventStatus::Handled;
                        }
                        KeyEventKind::Release => {
                            self.clear_pressed();
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
                        self.keyboard_press_deadline.set(None);
                        return EventStatus::Handled;
                    }
                    if let MouseEventKind::Up(MouseButton::Left) = mouse.kind {
                        if self.is_pressed.get() {
                            self.clear_pressed();
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
        self.set_area(self.surface_area(self.layout_area(area)));
        self.handle_event(event)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &SmashTheme) {
        button_component(frame, area, self, theme);
    }
}

impl NavigatorFocusable for ButtonState {
    fn sync_navigator_focus(&self, selected: bool) {
        if selected {
            self.focus();
        } else {
            self.blur();
        }
    }

    fn handle_navigator_event(&self, event: &SmashEvent) -> EventStatus {
        self.handle_event(event)
    }
}

// --- Component (Stateless Function) ---

pub fn button_component(frame: &mut Frame, area: Rect, state: &ButtonState, theme: &SmashTheme) {
    state.sync_keyboard_press_feedback();
    let area = state.layout_area(area);
    let surface = state.surface_area(area);
    state.set_area(surface);
    if area.width == 0 || area.height == 0 {
        return;
    }

    let variant = state.variant.get();
    let focused = state.is_focused.get();
    let hovered = state.is_hovered.get();
    let pressed = state.is_pressed.get();
    let palette = button_palette(variant, theme);
    let (bg, fg, label_style) = if pressed {
        (palette.pressed_bg, palette.pressed_fg, Modifier::BOLD)
    } else if focused {
        (palette.focus_bg, palette.focus_fg, Modifier::BOLD)
    } else if hovered {
        (palette.hover_bg, palette.hover_fg, Modifier::BOLD)
    } else {
        (palette.rest_bg, palette.rest_fg, Modifier::empty())
    };

    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background)),
        area,
    );
    if surface.width > 0 && surface.height > 0 {
        frame.render_widget(Block::default().style(Style::default().bg(bg)), surface);
    }
    let content_area = surface;
    if content_area.width == 0 || content_area.height == 0 {
        return;
    }

    let label = decorate_button_label(&state.label.get_clone(), focused, pressed);
    let label_height = label.lines().count().max(1) as u16;
    let text_style = Style::default().fg(fg).bg(bg).add_modifier(label_style);
    let p = Paragraph::new(label)
        .alignment(Alignment::Center)
        .style(text_style);
    let vert_center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(content_area.height.saturating_sub(label_height) / 2),
            Constraint::Length(label_height.min(content_area.height)),
            Constraint::Min(0),
        ])
        .split(content_area)[1];

    if vert_center.width > 0 && vert_center.height > 0 {
        frame.render_widget(p, vert_center);
    }
}

fn button_palette(variant: ButtonVariant, theme: &SmashTheme) -> ButtonPalette {
    match variant {
        ButtonVariant::Primary => ButtonPalette {
            rest_fg: theme.primary,
            rest_bg: theme.surface_variant,
            hover_fg: theme.on_primary_container,
            hover_bg: theme.primary_container,
            focus_fg: theme.on_primary_container,
            focus_bg: theme.primary_container,
            pressed_bg: theme.primary,
            pressed_fg: theme.on_primary,
        },
        ButtonVariant::Secondary => ButtonPalette {
            rest_fg: theme.secondary,
            rest_bg: theme.surface_variant,
            hover_fg: theme.on_secondary_container,
            hover_bg: theme.secondary_container,
            focus_fg: theme.on_secondary_container,
            focus_bg: theme.secondary_container,
            pressed_bg: theme.secondary,
            pressed_fg: theme.on_secondary,
        },
        ButtonVariant::Outline => ButtonPalette {
            rest_fg: theme.on_surface_variant,
            rest_bg: theme.surface_variant,
            hover_fg: theme.on_surface,
            hover_bg: theme.outline_variant,
            focus_fg: theme.on_surface,
            focus_bg: theme.outline_variant,
            pressed_bg: theme.surface_variant,
            pressed_fg: theme.on_surface,
        },
        ButtonVariant::Danger => ButtonPalette {
            rest_fg: theme.error,
            rest_bg: theme.surface_variant,
            hover_fg: theme.on_error_container,
            hover_bg: theme.error_container,
            focus_fg: theme.on_error_container,
            focus_bg: theme.error_container,
            pressed_bg: theme.error,
            pressed_fg: theme.on_error,
        },
    }
}

fn decorate_button_label(label: &str, focused: bool, pressed: bool) -> String {
    label
        .lines()
        .map(|line| {
            if pressed {
                format!("> {line} <")
            } else if focused {
                format!("[ {line} ]")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
