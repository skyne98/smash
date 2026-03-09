use anyhow::Result;
use smash_shell::button::{ButtonEvent, ButtonState, ButtonVariant, use_button_variant};
use smash_shell::prelude::*;
use smash_shell::tachyonfx::*;
use smash_shell::terminal::{TerminalState, use_terminal};
use smash_shell::textbox::{TextBoxState, use_textbox};
use smash_shell::tui_scrollview::{ScrollView, ScrollViewState};

use smash_shell::crossterm::event::{
    KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use std::sync::{Arc, Mutex};

const TAB_BUTTONS: usize = 0;
const TAB_TEXTBOXES: usize = 1;
const TAB_SCROLL_EFFECTS: usize = 2;
const TAB_TERMINAL: usize = 3;
const TAB_THEME: usize = 4;
const TAB_COUNT: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusId {
    Tabs,
    ButtonPrimary,
    ButtonSecondary,
    ButtonOutline,
    ButtonDanger,
    ButtonIncrement,
    ButtonDecrement,
    EditorBox,
    NotesBox,
    PreviewBox,
    ScrollArea,
    Terminal,
    ThemePresets,
    ThemeModeToggle,
}

#[derive(Clone)]
struct CookbookState {
    focus: FocusNavigator<FocusId>,
    selected_tab: SelectionState,
    is_dark: Signal<bool>,
    selected_theme_idx: SelectionState,
    quit_dialog: DialogState,
    last_key_debug: Signal<Option<KeyEvent>>,
    button_counter: Signal<i32>,
    button_message: Signal<String>,
    button_primary: ButtonState,
    button_secondary: ButtonState,
    button_outline: ButtonState,
    button_danger: ButtonState,
    button_increment: ButtonState,
    button_decrement: ButtonState,
    theme_mode_toggle: ButtonState,
    editor_box: TextBoxState,
    notes_box: TextBoxState,
    preview_box: TextBoxState,
}

#[derive(Clone, Copy)]
struct AppLayout {
    tabs: Rect,
    body: Rect,
    footer: Rect,
}

#[derive(Clone, Copy)]
struct ButtonGalleryLayout {
    intro: Rect,
    variants: [Rect; 4],
    playground_buttons: [Rect; 2],
    playground_info: Rect,
    guidance: Rect,
    contract: Rect,
}

#[derive(Clone, Copy)]
struct TextboxGalleryLayout {
    samples: [Rect; 3],
    selection: Rect,
    guide: Rect,
}

#[derive(Clone, Copy)]
struct ScrollEffectsLayout {
    scroll: Rect,
    effect: Rect,
}

#[derive(Clone, Copy)]
struct TerminalDemoLayout {
    intro: Rect,
    terminal: Rect,
}

#[derive(Clone, Copy)]
struct ThemeDemoLayout {
    presets: Rect,
    toggle: Rect,
    swatches: Rect,
    info: Rect,
}

fn use_cookbook_state() -> CookbookState {
    let editor_box = use_textbox(
        "fn greet(name: &str) {\n    println!(\"hello, {name}!\");\n}\n\n// edit this example",
    );
    editor_box.set_title("editor");

    let notes_box = use_textbox("quick note\nwithout line numbers");
    notes_box.set_title("plain text");
    notes_box.show_line_numbers.set(false);

    let preview_box = use_textbox(
        "component preview\n\nread-only textboxes are useful for logs,\nhelp output, and generated content.",
    );
    preview_box.set_title("preview");
    preview_box.show_line_numbers.set(false);
    preview_box.set_read_only(true);

    let state = CookbookState {
        focus: use_focus_navigator(Some(FocusId::ButtonPrimary)),
        selected_tab: use_selection(TAB_BUTTONS, TAB_COUNT),
        is_dark: create_signal(true),
        selected_theme_idx: use_selection(0, 5),
        quit_dialog: use_dialog(
            "quit component gallery?",
            "Press Ctrl+C again to quit immediately, or choose stay to keep exploring.",
        ),
        last_key_debug: create_signal(None),
        button_counter: create_signal(0),
        button_message: create_signal(
            "Move focus across the gallery and press Enter to activate a button.".to_string(),
        ),
        button_primary: use_button_variant("primary", ButtonVariant::Primary),
        button_secondary: use_button_variant("secondary", ButtonVariant::Secondary),
        button_outline: use_button_variant("outline", ButtonVariant::Outline),
        button_danger: use_button_variant("danger", ButtonVariant::Danger),
        button_increment: use_button_variant("increment", ButtonVariant::Primary),
        button_decrement: use_button_variant("decrement", ButtonVariant::Secondary),
        theme_mode_toggle: use_button_variant("switch to light mode", ButtonVariant::Secondary),
        editor_box,
        notes_box,
        preview_box,
    };

    state.quit_dialog.set_labels("stay", "quit");

    let message = state.button_message;
    state.button_primary.on_click(move |event| {
        if let ButtonEvent::Click = event {
            message.set("Primary buttons are for the main call to action.".to_string());
        }
    });
    register_button_focus(&state.button_primary, state.focus, FocusId::ButtonPrimary);

    let message = state.button_message;
    state.button_secondary.on_click(move |event| {
        if let ButtonEvent::Click = event {
            message.set("Secondary buttons support the primary flow.".to_string());
        }
    });
    register_button_focus(
        &state.button_secondary,
        state.focus,
        FocusId::ButtonSecondary,
    );

    let message = state.button_message;
    state.button_outline.on_click(move |event| {
        if let ButtonEvent::Click = event {
            message.set("Outline buttons work well for quiet, neutral actions.".to_string());
        }
    });
    register_button_focus(&state.button_outline, state.focus, FocusId::ButtonOutline);

    let message = state.button_message;
    state.button_danger.on_click(move |event| {
        if let ButtonEvent::Click = event {
            message.set("Danger buttons should be reserved for destructive actions.".to_string());
        }
    });
    register_button_focus(&state.button_danger, state.focus, FocusId::ButtonDanger);

    let counter = state.button_counter;
    let message = state.button_message;
    state.button_increment.on_click(move |event| {
        if let ButtonEvent::Click = event {
            let next = counter.get() + 1;
            counter.set(next);
            message.set(format!("Counter increased to {next}."));
        }
    });
    register_button_focus(
        &state.button_increment,
        state.focus,
        FocusId::ButtonIncrement,
    );

    let counter = state.button_counter;
    let message = state.button_message;
    state.button_decrement.on_click(move |event| {
        if let ButtonEvent::Click = event {
            let next = counter.get() - 1;
            counter.set(next);
            message.set(format!("Counter decreased to {next}."));
        }
    });
    register_button_focus(
        &state.button_decrement,
        state.focus,
        FocusId::ButtonDecrement,
    );

    update_theme_toggle_label(&state.theme_mode_toggle, state.is_dark.get());
    let is_dark = state.is_dark;
    let toggle_button = state.theme_mode_toggle.clone();
    state.theme_mode_toggle.on_click(move |event| {
        if let ButtonEvent::Click = event {
            let next = !is_dark.get();
            is_dark.set(next);
            update_theme_toggle_label(&toggle_button, next);
        }
    });
    register_button_focus(
        &state.theme_mode_toggle,
        state.focus,
        FocusId::ThemeModeToggle,
    );

    state
}

fn register_button_focus(button: &ButtonState, focus: FocusNavigator<FocusId>, id: FocusId) {
    button.on_focus(move |focused| {
        if focused {
            focus.set(Some(id));
        }
    });
}

fn update_theme_toggle_label(button: &ButtonState, is_dark: bool) {
    button.label.set(if is_dark {
        "switch to light mode".to_string()
    } else {
        "switch to dark mode".to_string()
    });
}

fn button_gallery_buttons(state: &CookbookState) -> [(FocusId, ButtonState); 6] {
    [
        (FocusId::ButtonPrimary, state.button_primary.clone()),
        (FocusId::ButtonSecondary, state.button_secondary.clone()),
        (FocusId::ButtonOutline, state.button_outline.clone()),
        (FocusId::ButtonDanger, state.button_danger.clone()),
        (FocusId::ButtonIncrement, state.button_increment.clone()),
        (FocusId::ButtonDecrement, state.button_decrement.clone()),
    ]
}

fn textbox_controls(state: &CookbookState) -> [(FocusId, TextBoxState); 3] {
    [
        (FocusId::EditorBox, state.editor_box),
        (FocusId::NotesBox, state.notes_box),
        (FocusId::PreviewBox, state.preview_box),
    ]
}

fn textbox_label(id: FocusId) -> &'static str {
    match id {
        FocusId::EditorBox => "editor",
        FocusId::NotesBox => "plain text",
        FocusId::PreviewBox => "preview",
        _ => "textbox",
    }
}

fn default_focus_for_tab(tab: usize) -> FocusId {
    match tab {
        TAB_BUTTONS => FocusId::ButtonPrimary,
        TAB_TEXTBOXES => FocusId::EditorBox,
        TAB_SCROLL_EFFECTS => FocusId::ScrollArea,
        TAB_TERMINAL => FocusId::Terminal,
        TAB_THEME => FocusId::ThemePresets,
        _ => FocusId::Tabs,
    }
}

fn sync_visible_focus(state: &CookbookState, nodes: &[FocusNode<FocusId>]) {
    if nodes.is_empty() {
        state.focus.clear();
        return;
    }

    if let Some(selected) = state.focus.get()
        && nodes.iter().any(|node| node.id == selected)
    {
        return;
    }

    let preferred = default_focus_for_tab(state.selected_tab.get());
    let next = nodes
        .iter()
        .find(|node| node.id == preferred)
        .map(|node| node.id)
        .or_else(|| nodes.first().map(|node| node.id));
    state.focus.set(next);
}

fn sync_focus_visuals(state: &CookbookState, terminal: &TerminalState) {
    let selected = state.focus.get();

    for (id, button) in button_gallery_buttons(state) {
        if Some(id) == selected {
            button.focus();
        } else {
            button.blur();
        }
    }

    if Some(FocusId::ThemeModeToggle) == selected {
        state.theme_mode_toggle.focus();
    } else {
        state.theme_mode_toggle.blur();
    }

    for (id, textbox) in textbox_controls(state) {
        if Some(id) == selected {
            if textbox.is_focused.get() {
                textbox.focus();
            } else {
                textbox.select();
            }
        } else {
            textbox.deselect();
        }
    }

    if Some(FocusId::Terminal) == selected {
        if terminal.is_focused.get() {
            terminal.focus();
        } else {
            terminal.select();
        }
    } else {
        terminal.deselect();
    }
}

fn active_focus(state: &CookbookState, terminal: &TerminalState) -> Option<FocusId> {
    match state.focus.get()? {
        FocusId::EditorBox if state.editor_box.is_focused.get() => Some(FocusId::EditorBox),
        FocusId::NotesBox if state.notes_box.is_focused.get() => Some(FocusId::NotesBox),
        FocusId::PreviewBox if state.preview_box.is_focused.get() => Some(FocusId::PreviewBox),
        FocusId::Terminal if terminal.is_focused.get() => Some(FocusId::Terminal),
        _ => None,
    }
}

fn textbox_for_focus(state: &CookbookState, id: FocusId) -> Option<TextBoxState> {
    match id {
        FocusId::EditorBox => Some(state.editor_box),
        FocusId::NotesBox => Some(state.notes_box),
        FocusId::PreviewBox => Some(state.preview_box),
        _ => None,
    }
}

fn app_layout(area: Rect) -> AppLayout {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    AppLayout {
        tabs: sections[0],
        body: sections[1],
        footer: sections[2],
    }
}

fn button_gallery_layout(area: Rect) -> ButtonGalleryLayout {
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Min(0),
        ])
        .split(layout[0]);

    let variants = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(left[1]);

    let playground = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(16),
            Constraint::Length(16),
            Constraint::Min(0),
        ])
        .split(left[2]);

    ButtonGalleryLayout {
        intro: left[0],
        variants: [variants[0], variants[1], variants[2], variants[3]],
        playground_buttons: [playground[0], playground[1]],
        playground_info: playground[2],
        guidance: left[3],
        contract: layout[1],
    }
}

fn textbox_gallery_layout(area: Rect) -> TextboxGalleryLayout {
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(area);

    let samples = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(45),
            Constraint::Percentage(25),
            Constraint::Percentage(30),
        ])
        .split(layout[0]);

    let sidebar = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(0)])
        .split(layout[1]);

    TextboxGalleryLayout {
        samples: [samples[0], samples[1], samples[2]],
        selection: sidebar[0],
        guide: sidebar[1],
    }
}

fn scroll_effects_layout(area: Rect) -> ScrollEffectsLayout {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    ScrollEffectsLayout {
        scroll: layout[0],
        effect: layout[1],
    }
}

fn terminal_demo_layout(area: Rect) -> TerminalDemoLayout {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(area);

    TerminalDemoLayout {
        intro: layout[0],
        terminal: layout[1],
    }
}

fn theme_demo_layout(area: Rect) -> ThemeDemoLayout {
    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(outer[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(4)])
        .split(outer[1]);

    ThemeDemoLayout {
        presets: left[0],
        toggle: left[1],
        swatches: right[0],
        info: right[1],
    }
}

fn focus_nodes_for_area(area: Rect, state: &CookbookState) -> Vec<FocusNode<FocusId>> {
    let app = app_layout(area);
    let mut nodes = vec![FocusNode::new(FocusId::Tabs, app.tabs)];

    match state.selected_tab.get() {
        TAB_BUTTONS => {
            let layout = button_gallery_layout(app.body);
            nodes.extend([
                FocusNode::new(FocusId::ButtonPrimary, layout.variants[0]),
                FocusNode::new(FocusId::ButtonSecondary, layout.variants[1]),
                FocusNode::new(FocusId::ButtonOutline, layout.variants[2]),
                FocusNode::new(FocusId::ButtonDanger, layout.variants[3]),
                FocusNode::new(FocusId::ButtonIncrement, layout.playground_buttons[0]),
                FocusNode::new(FocusId::ButtonDecrement, layout.playground_buttons[1]),
            ]);
        }
        TAB_TEXTBOXES => {
            let layout = textbox_gallery_layout(app.body);
            nodes.extend([
                FocusNode::new(FocusId::EditorBox, layout.samples[0]),
                FocusNode::new(FocusId::NotesBox, layout.samples[1]),
                FocusNode::new(FocusId::PreviewBox, layout.samples[2]),
            ]);
        }
        TAB_SCROLL_EFFECTS => {
            let layout = scroll_effects_layout(app.body);
            nodes.push(FocusNode::new(FocusId::ScrollArea, layout.scroll));
        }
        TAB_TERMINAL => {
            let layout = terminal_demo_layout(app.body);
            nodes.push(FocusNode::new(FocusId::Terminal, layout.terminal));
        }
        TAB_THEME => {
            let layout = theme_demo_layout(app.body);
            nodes.extend([
                FocusNode::new(FocusId::ThemePresets, layout.presets),
                FocusNode::new(FocusId::ThemeModeToggle, layout.toggle),
            ]);
        }
        _ => {}
    }

    nodes
}

fn point_in_rect(column: u16, row: u16, area: Rect) -> bool {
    column >= area.x && column < area.x + area.width && row >= area.y && row < area.y + area.height
}

fn is_ctrl_c_press(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.code == KeyCode::Char('c')
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn focus_label(selected: Option<FocusId>) -> &'static str {
    match selected {
        Some(FocusId::Tabs) => "tabs",
        Some(FocusId::ButtonPrimary) => "primary button",
        Some(FocusId::ButtonSecondary) => "secondary button",
        Some(FocusId::ButtonOutline) => "outline button",
        Some(FocusId::ButtonDanger) => "danger button",
        Some(FocusId::ButtonIncrement) => "increment button",
        Some(FocusId::ButtonDecrement) => "decrement button",
        Some(FocusId::EditorBox) => "editor textbox",
        Some(FocusId::NotesBox) => "plain text textbox",
        Some(FocusId::PreviewBox) => "preview textbox",
        Some(FocusId::ScrollArea) => "scroll area",
        Some(FocusId::Terminal) => "terminal",
        Some(FocusId::ThemePresets) => "theme presets",
        Some(FocusId::ThemeModeToggle) => "theme mode toggle",
        None => "nothing",
    }
}

fn footer_help(state: &CookbookState, terminal: &TerminalState) -> String {
    let selected = state.focus.get();
    let specific = match active_focus(state, terminal) {
        Some(FocusId::EditorBox | FocusId::NotesBox | FocusId::PreviewBox) => {
            "textbox active: type normally, esc exits editing"
        }
        Some(FocusId::Terminal) => "terminal active: type normally, esc exits interaction",
        None => match selected {
            Some(FocusId::Tabs) => "tabs selected: left/right switches tabs, down enters content",
            Some(FocusId::ThemePresets) => {
                "theme presets selected: up/down changes the preset, right moves onward"
            }
            Some(FocusId::ScrollArea) => "scroll area selected: up/down scrolls the viewport",
            Some(FocusId::EditorBox | FocusId::NotesBox | FocusId::PreviewBox) => {
                "textbox selected: enter starts editing"
            }
            Some(FocusId::Terminal) => "terminal selected: enter starts interaction",
            _ => "tab/backtab cycles focus, arrows move spatially, enter activates",
        },
        _ => "tab/backtab cycles focus, arrows move spatially, enter activates",
    };

    if let Some(last_key) = state.last_key_debug.get() {
        let key_str = match last_key.code {
            KeyCode::Char(c) => format!("'{}'", c),
            _ => format!("{:?}", last_key.code),
        };
        let mod_str = if last_key.modifiers.is_empty() {
            String::new()
        } else {
            format!("+{:?}", last_key.modifiers)
        };
        format!(
            "focus: {} | {} | ctrl+left/right: switch tabs | ctrl+c twice: quit | ctrl+q: quit | last: {}{}",
            focus_label(selected),
            specific,
            key_str,
            mod_str
        )
    } else {
        format!(
            "focus: {} | {} | ctrl+left/right: switch tabs | ctrl+c twice: quit | ctrl+q: quit",
            focus_label(selected),
            specific,
        )
    }
}

fn handle_mouse_event(
    event: &SmashEvent,
    focus_nodes: &[FocusNode<FocusId>],
    state: &CookbookState,
) -> EventStatus {
    if let SmashEvent::Mouse(mouse) = event
        && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(node) = focus_nodes
            .iter()
            .find(|node| point_in_rect(mouse.column, mouse.row, node.area))
    {
        state.focus.set(Some(node.id));
    }

    match state.selected_tab.get() {
        TAB_BUTTONS => {
            for (_, button) in button_gallery_buttons(state) {
                if button.handle_event(event) == EventStatus::Handled {
                    return EventStatus::Handled;
                }
            }
        }
        TAB_THEME => {
            if state.theme_mode_toggle.handle_event(event) == EventStatus::Handled {
                return EventStatus::Handled;
            }
        }
        _ => {}
    }

    EventStatus::Ignored
}

fn handle_key_event(
    key: KeyEvent,
    focus_nodes: &[FocusNode<FocusId>],
    state: &CookbookState,
    terminal: &TerminalState,
    scroll_state: &Arc<Mutex<ScrollViewState>>,
    quit_requested: &mut bool,
) -> EventStatus {
    state.last_key_debug.set(Some(key));

    let is_press = key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat;
    if !is_press {
        return EventStatus::Ignored;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('q') if key.kind == KeyEventKind::Press => {
                *quit_requested = true;
                return EventStatus::Handled;
            }
            KeyCode::Right => {
                state.selected_tab.next();
                state.focus.set(Some(FocusId::Tabs));
                return EventStatus::Handled;
            }
            KeyCode::Left => {
                state.selected_tab.prev();
                state.focus.set(Some(FocusId::Tabs));
                return EventStatus::Handled;
            }
            _ => {}
        }
    }

    if let Some(active) = active_focus(state, terminal) {
        match active {
            FocusId::EditorBox | FocusId::NotesBox | FocusId::PreviewBox => {
                if let Some(textbox) = textbox_for_focus(state, active)
                    && textbox.handle_smash_event(&SmashEvent::Key(key)) == EventStatus::Handled
                {
                    return EventStatus::Handled;
                }
            }
            FocusId::Terminal => {
                if terminal.handle_smash_event(&SmashEvent::Key(key)) == EventStatus::Handled {
                    return EventStatus::Handled;
                }
            }
            _ => {}
        }
    }

    if is_ctrl_c_press(key) {
        state.quit_dialog.open();
        return EventStatus::Handled;
    }

    match key.code {
        KeyCode::BackTab => {
            state.focus.prev(focus_nodes);
            return EventStatus::Handled;
        }
        KeyCode::Tab => {
            state.focus.next(focus_nodes);
            return EventStatus::Handled;
        }
        _ => {}
    }

    if state.selected_tab.get() == TAB_BUTTONS {
        match key.code {
            KeyCode::Char('+') => {
                let next = state.button_counter.get() + 1;
                state.button_counter.set(next);
                state
                    .button_message
                    .set(format!("Counter increased to {next}."));
                return EventStatus::Handled;
            }
            KeyCode::Char('-') => {
                let next = state.button_counter.get() - 1;
                state.button_counter.set(next);
                state
                    .button_message
                    .set(format!("Counter decreased to {next}."));
                return EventStatus::Handled;
            }
            _ => {}
        }
    }

    let selected = state
        .focus
        .get()
        .unwrap_or_else(|| default_focus_for_tab(state.selected_tab.get()));

    match selected {
        FocusId::Tabs => match key.code {
            KeyCode::Left => {
                state.selected_tab.prev();
                return EventStatus::Handled;
            }
            KeyCode::Right => {
                state.selected_tab.next();
                return EventStatus::Handled;
            }
            _ => {}
        },
        FocusId::ThemePresets => match key.code {
            KeyCode::Up => {
                state.selected_theme_idx.prev();
                return EventStatus::Handled;
            }
            KeyCode::Down => {
                state.selected_theme_idx.next();
                return EventStatus::Handled;
            }
            _ => {}
        },
        FocusId::ScrollArea => {
            let speed = if key.modifiers.contains(KeyModifiers::SHIFT) {
                5
            } else {
                1
            };
            if let Ok(mut scroll) = scroll_state.lock() {
                match key.code {
                    KeyCode::Up => {
                        for _ in 0..speed {
                            scroll.scroll_up();
                        }
                        return EventStatus::Handled;
                    }
                    KeyCode::Down => {
                        for _ in 0..speed {
                            scroll.scroll_down();
                        }
                        return EventStatus::Handled;
                    }
                    _ => {}
                }
            }
        }
        FocusId::EditorBox | FocusId::NotesBox | FocusId::PreviewBox => {
            if key.code == KeyCode::Enter
                && let Some(textbox) = textbox_for_focus(state, selected)
            {
                textbox.focus();
                return EventStatus::Handled;
            }
        }
        FocusId::Terminal => {
            if key.code == KeyCode::Enter {
                terminal.focus();
                return EventStatus::Handled;
            }
        }
        _ => {}
    }

    match key.code {
        KeyCode::Left => {
            state
                .focus
                .move_direction(focus_nodes, FocusDirection::Left);
            EventStatus::Handled
        }
        KeyCode::Right => {
            state
                .focus
                .move_direction(focus_nodes, FocusDirection::Right);
            EventStatus::Handled
        }
        KeyCode::Up => {
            state.focus.move_direction(focus_nodes, FocusDirection::Up);
            EventStatus::Handled
        }
        KeyCode::Down => {
            state
                .focus
                .move_direction(focus_nodes, FocusDirection::Down);
            EventStatus::Handled
        }
        _ => EventStatus::Ignored,
    }
}

pub async fn run_cookbook() -> Result<()> {
    let tabs = [
        "buttons",
        "textboxes",
        "scroll & effects",
        "terminal",
        "theme",
    ];
    let theme_presets = [
        ("violet", presets::VIOLET),
        ("ocean", presets::OCEAN),
        ("forest", presets::FOREST),
        ("fire", presets::FIRE),
        ("gold", presets::GOLD),
    ];

    let mut outer_result = Ok(());

    let _root = create_root(|| {
        let mut window = match Window::new() {
            Ok(w) => w,
            Err(e) => {
                outer_result = Err(e);
                return;
            }
        };

        let state = use_cookbook_state();
        let terminal_state = match use_terminal(20, 80) {
            Ok(s) => s,
            Err(e) => {
                outer_result = Err(e);
                return;
            }
        };

        let scroll_state = Arc::new(Mutex::new(ScrollViewState::default()));

        let effect_fn = fx::effect_fn((), 2000u32, |_, ctx, mut cells| {
            let alpha = ctx.alpha();
            for cell in cells.by_ref() {
                let r = (alpha * 255.0) as u8;
                cell.1.set_fg(Color::Rgb(r, 100, 255 - r));
            }
        });
        let mut repeating_effect = fx::repeating(effect_fn);

        while window.update().expect("window update failed") {
            let terminal_area: Rect = window.terminal.size().expect("terminal size failed").into();
            let mut focus_nodes = focus_nodes_for_area(terminal_area, &state);
            sync_visible_focus(&state, &focus_nodes);
            sync_focus_visuals(&state, &terminal_state);

            let dispatcher = window.dispatcher;
            let mut quit_requested = false;
            let scroll_ref = Arc::clone(&scroll_state);

            dispatcher.dispatch(|event| {
                if state.quit_dialog.is_open() {
                    if let SmashEvent::Key(key) = event
                        && is_ctrl_c_press(*key)
                    {
                        quit_requested = true;
                        return EventStatus::Handled;
                    }

                    return match state.quit_dialog.handle_smash_event(event) {
                        DialogEvent::Confirmed => {
                            quit_requested = true;
                            EventStatus::Handled
                        }
                        DialogEvent::Cancelled | DialogEvent::Handled | DialogEvent::Ignored => {
                            EventStatus::Handled
                        }
                    };
                }

                if handle_mouse_event(event, &focus_nodes, &state) == EventStatus::Handled {
                    return EventStatus::Handled;
                }

                if let SmashEvent::Key(key) = event {
                    return handle_key_event(
                        *key,
                        &focus_nodes,
                        &state,
                        &terminal_state,
                        &scroll_ref,
                        &mut quit_requested,
                    );
                }

                EventStatus::Ignored
            });

            if quit_requested {
                window.should_quit = true;
            }

            focus_nodes = focus_nodes_for_area(terminal_area, &state);
            sync_visible_focus(&state, &focus_nodes);
            sync_focus_visuals(&state, &terminal_state);

            window.theme = SmashTheme::from_seed(
                theme_presets[state.selected_theme_idx.get()].1,
                state.is_dark.get(),
            );

            let current_theme = window.theme;
            let current_tab = state.selected_tab.get();
            let app = app_layout(terminal_area);
            let tabs_selected = state.focus.get() == Some(FocusId::Tabs);

            window
                .draw(|frame| {
                    let area = frame.area();
                    frame.render_widget(Block::default().bg(current_theme.background), area);

                    let tab_titles = tabs.iter().map(|tab| Line::from(*tab)).collect::<Vec<_>>();
                    let tab_block = if tabs_selected {
                        Block::default()
                            .borders(Borders::ALL)
                            .title("smash component gallery (selected)")
                            .border_style(Style::default().fg(current_theme.primary))
                    } else {
                        Block::default()
                            .borders(Borders::ALL)
                            .title("smash component gallery")
                            .border_style(Style::default().fg(current_theme.outline))
                    };

                    frame.render_widget(
                        Tabs::new(tab_titles)
                            .block(tab_block)
                            .select(current_tab)
                            .style(Style::default().fg(current_theme.on_surface))
                            .highlight_style(
                                Style::default()
                                    .fg(current_theme.primary)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        app.tabs,
                    );

                    match current_tab {
                        TAB_BUTTONS => draw_buttons(frame, app.body, &current_theme, &state),
                        TAB_TEXTBOXES => draw_textboxes(frame, app.body, &current_theme, &state),
                        TAB_SCROLL_EFFECTS => {
                            if let Ok(mut scroll) = scroll_state.lock() {
                                draw_scroll_effects(
                                    frame,
                                    app.body,
                                    &mut scroll,
                                    &mut repeating_effect,
                                    &current_theme,
                                    state.focus.get() == Some(FocusId::ScrollArea),
                                );
                            }
                        }
                        TAB_TERMINAL => {
                            draw_terminal_demo(frame, app.body, &current_theme, &terminal_state)
                        }
                        TAB_THEME => draw_theme_demo(
                            frame,
                            app.body,
                            &current_theme,
                            &theme_presets,
                            state.selected_theme_idx.get(),
                            state.is_dark.get(),
                            state.focus.get() == Some(FocusId::ThemePresets),
                            &state.theme_mode_toggle,
                        ),
                        _ => {}
                    }

                    frame.render_widget(
                        Paragraph::new(footer_help(&state, &terminal_state))
                            .style(Style::default().fg(current_theme.on_background).dim()),
                        app.footer,
                    );

                    state.quit_dialog.render(frame, area, &current_theme);
                })
                .expect("draw failed");
        }

        window.close().expect("close failed");
    });

    outer_result
}

fn draw_buttons(frame: &mut Frame, area: Rect, theme: &SmashTheme, state: &CookbookState) {
    let layout = button_gallery_layout(area);

    frame.render_widget(
        Paragraph::new(
            "The gallery now uses one app-wide focus system. Use Tab/Shift+Tab to cycle, arrow keys to move by layout, and Enter to activate the selected control. The selected button uses a bright double border and label markers.",
        )
        .block(
            Block::default()
                .title("button component")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.outline)),
        )
        .style(Style::default().fg(theme.on_surface)),
        layout.intro,
    );

    state
        .button_primary
        .render(frame, layout.variants[0], theme);
    state
        .button_secondary
        .render(frame, layout.variants[1], theme);
    state
        .button_outline
        .render(frame, layout.variants[2], theme);
    state.button_danger.render(frame, layout.variants[3], theme);

    state
        .button_increment
        .render(frame, layout.playground_buttons[0], theme);
    state
        .button_decrement
        .render(frame, layout.playground_buttons[1], theme);
    frame.render_widget(
        Paragraph::new(format!(
            "counter: {}\n{}",
            state.button_counter.get(),
            state.button_message.get_clone()
        ))
        .block(
            Block::default()
                .title("playground")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.outline)),
        )
        .style(Style::default().fg(theme.on_surface)),
        layout.playground_info,
    );

    frame.render_widget(
        Paragraph::new(
            "Variant guidance:\n- primary: the main call to action\n- secondary: supporting actions\n- outline: neutral or low-emphasis actions\n- danger: destructive actions\n\nVisual states:\n- rounded border: idle or hover\n- double bright border: selected\n- thick bright border: pressed",
        )
        .block(
            Block::default()
                .title("usage notes")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.outline)),
        )
        .style(Style::default().fg(theme.on_surface)),
        layout.guidance,
    );

    frame.render_widget(
        Paragraph::new(
            "Every sample above is a real ButtonState with the same API:\n- use_button_variant(label, variant)\n- on_click / on_focus / on_hover\n- render(frame, area, theme)\n\nThat keeps the cookbook close to how the component library is used in practice.",
        )
        .block(
            Block::default()
                .title("component contract")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.outline)),
        )
        .style(Style::default().fg(theme.on_surface)),
        layout.contract,
    );
}

fn draw_textboxes(frame: &mut Frame, area: Rect, theme: &SmashTheme, state: &CookbookState) {
    let layout = textbox_gallery_layout(area);

    state.editor_box.render(frame, layout.samples[0], theme);
    state.notes_box.render(frame, layout.samples[1], theme);
    state.preview_box.render(frame, layout.samples[2], theme);

    let selected_focus = state.focus.get();
    let selection_items: Vec<ListItem> = textbox_controls(state)
        .iter()
        .map(|(id, _)| {
            let is_selected = Some(*id) == selected_focus;
            let style = if is_selected {
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.on_surface)
            };
            let marker = if is_selected { ">" } else { " " };
            ListItem::new(format!("{marker} {}", textbox_label(*id))).style(style)
        })
        .collect();

    frame.render_widget(
        List::new(selection_items).block(
            Block::default()
                .title("textboxes")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.outline)),
        ),
        layout.selection,
    );

    frame.render_widget(
        Paragraph::new(
            "Textbox variants in this gallery:\n- editor: multiline with line numbers\n- plain text: compact field without the gutter\n- preview: read-only rendering for generated output\n\nSelection is global now:\n- arrows move between controls by position\n- Enter starts editing the selected textbox\n- Esc returns to navigation mode\n- Ctrl+A/C still work in read-only mode",
        )
        .block(
            Block::default()
                .title("textbox guide")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.outline)),
        )
        .style(Style::default().fg(theme.on_surface)),
        layout.guide,
    );
}

fn draw_scroll_effects(
    frame: &mut Frame,
    area: Rect,
    scroll_state: &mut ScrollViewState,
    effect: &mut Effect,
    theme: &SmashTheme,
    is_selected: bool,
) {
    let layout = scroll_effects_layout(area);

    let scroll_block = Block::default()
        .borders(Borders::ALL)
        .title(if is_selected {
            "scroll area (selected - up/down scrolls, shift speeds up)"
        } else {
            "scroll area"
        })
        .border_style(Style::default().fg(if is_selected {
            theme.primary
        } else {
            theme.outline
        }));
    let scroll_inner = scroll_block.inner(layout.scroll);
    frame.render_widget(scroll_block, layout.scroll);

    let mut scroll_view = ScrollView::new(Size::new(scroll_inner.width, 30))
        .scrollbars_visibility(smash_shell::tui_scrollview::ScrollbarVisibility::Never);

    for cell in scroll_view.buf_mut().content.iter_mut() {
        cell.set_bg(theme.background);
    }

    let content = (0..30)
        .map(|i| format!("line {} of scrollable content", i))
        .collect::<Vec<_>>()
        .join("\n");
    scroll_view.render_widget(
        Paragraph::new(content).style(Style::default().fg(theme.on_surface)),
        Rect::new(0, 0, scroll_inner.width, 30),
    );
    frame.render_stateful_widget(scroll_view, scroll_inner, scroll_state);

    let mut scrollbar_state =
        ScrollbarState::new(30usize.saturating_sub(scroll_inner.height as usize))
            .position(scroll_state.offset().y as usize);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(theme.primary)),
        layout.scroll,
        &mut scrollbar_state,
    );

    let effect_block = Block::default()
        .borders(Borders::ALL)
        .title("tachyonfx")
        .border_style(Style::default().fg(theme.outline));
    let inner_area = effect_block.inner(layout.effect);
    frame.render_widget(effect_block, layout.effect);
    frame.render_widget(
        Paragraph::new("color animation")
            .alignment(Alignment::Center)
            .fg(theme.on_surface),
        inner_area,
    );
    effect.process(
        smash_shell::tachyonfx::Duration::from_millis(16),
        frame.buffer_mut(),
        inner_area,
    );
}

fn draw_terminal_demo(frame: &mut Frame, area: Rect, theme: &SmashTheme, state: &TerminalState) {
    let layout = terminal_demo_layout(area);

    frame.render_widget(
        Paragraph::new(
            "The terminal component stays in the same focus graph as the rest of the gallery. Select it with arrows or Tab, press Enter to interact with the shell, and press Esc to return to navigation.",
        )
        .block(
            Block::default()
                .title("terminal component")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.outline)),
        )
        .style(Style::default().fg(theme.on_surface)),
        layout.intro,
    );

    state.render(frame, layout.terminal, theme);
}

fn draw_theme_demo(
    frame: &mut Frame,
    area: Rect,
    theme: &SmashTheme,
    presets: &[(&str, u32)],
    selected_idx: usize,
    is_dark: bool,
    presets_selected: bool,
    toggle_button: &ButtonState,
) {
    let layout = theme_demo_layout(area);

    let items: Vec<ListItem> = presets
        .iter()
        .enumerate()
        .map(|(idx, (name, _))| {
            let style = if idx == selected_idx {
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.on_surface)
            };
            let marker = if idx == selected_idx { ">" } else { " " };
            ListItem::new(format!("{marker} {name}")).style(style)
        })
        .collect();

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(if presets_selected {
                    "presets (selected - up/down changes theme)"
                } else {
                    "presets"
                })
                .border_style(Style::default().fg(if presets_selected {
                    theme.primary
                } else {
                    theme.outline
                })),
        ),
        layout.presets,
    );

    toggle_button.render(frame, layout.toggle, theme);

    let colors = [
        ("primary", theme.primary, theme.on_primary),
        (
            "primary container",
            theme.primary_container,
            theme.on_primary_container,
        ),
        ("secondary", theme.secondary, theme.on_secondary),
        (
            "secondary container",
            theme.secondary_container,
            theme.on_secondary_container,
        ),
        ("tertiary", theme.tertiary, theme.on_tertiary),
        (
            "tertiary container",
            theme.tertiary_container,
            theme.on_tertiary_container,
        ),
        ("error", theme.error, theme.on_error),
        ("background", theme.background, theme.on_background),
        ("surface", theme.surface, theme.on_surface),
        ("outline", theme.outline, theme.on_surface),
    ];

    let swatch_block = Block::default()
        .borders(Borders::ALL)
        .title("theme tokens")
        .border_style(Style::default().fg(theme.outline));
    let swatch_inner = swatch_block.inner(layout.swatches);
    frame.render_widget(swatch_block, layout.swatches);

    let swatch_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(1); colors.len()])
        .split(swatch_inner);
    for (idx, (name, bg, fg)) in colors.iter().enumerate() {
        if idx >= swatch_rows.len() {
            break;
        }
        frame.render_widget(
            Paragraph::new(format!(" {}", name)).style(Style::default().bg(*bg).fg(*fg)),
            swatch_rows[idx],
        );
    }

    frame.render_widget(
        Paragraph::new(format!(
            "current mode: {}\nSelect the presets list to change palette, or move focus to the button to toggle light and dark mode.",
            if is_dark { "dark" } else { "light" }
        ))
        .style(Style::default().fg(theme.on_surface)),
        layout.info,
    );
}
