use anyhow::Result;
use smash_shell::prelude::*;
use smash_shell::tui_big_text::{BigTextBuilder, PixelSize};
use smash_shell::throbber_widgets_tui::{Throbber, ThrobberState};
use smash_shell::tui_piechart::{PieChart, PieSlice};
use smash_shell::tui_scrollview::{ScrollView, ScrollViewState};
use smash_shell::tachyonfx::*;
use smash_shell::textbox::{use_textbox, text_box_component, TextBoxState};
use smash_shell::terminal::{use_terminal, terminal_component, TerminalState};

use smash_shell::crossterm::event::{KeyEventKind, KeyModifiers};
use std::sync::{Arc, Mutex};

// --- Composables ---

#[derive(Clone, Copy)]
struct CookbookState {
    selected_tab: Signal<usize>,
    is_dark: Signal<bool>,
    selected_theme_idx: Signal<usize>,
    last_key_debug: Signal<Option<smash_shell::crossterm::event::KeyEvent>>,
    counter: Signal<i32>,
}

fn use_cookbook_state() -> CookbookState {
    CookbookState {
        selected_tab: create_signal(0),
        is_dark: create_signal(true),
        selected_theme_idx: create_signal(0),
        last_key_debug: create_signal(None),
        counter: create_signal(0),
    }
}

// --- App Entry ---

pub async fn run_cookbook() -> Result<()> {
    let tabs = ["big text", "widgets", "scroll & effects", "input", "terminal", "theme"];
    let theme_presets = [
        ("violet", presets::VIOLET),
        ("ocean", presets::OCEAN),
        ("forest", presets::FOREST),
        ("fire", presets::FIRE),
        ("gold", presets::GOLD),
    ];

    let mut outer_result = Ok(());

    // We MUST run the loop inside create_root to keep signals accessible
    let _root = create_root(|| {
        let mut window = match Window::new() {
            Ok(w) => w,
            Err(e) => {
                outer_result = Err(e);
                return;
            }
        };

        let state = use_cookbook_state();
        let textbox_state = use_textbox("welcome to smash shell.\ntype here to test multiline editing.\n\ntry shift+arrows to select,\nctrl+c to copy, ctrl+v to paste.");
        let terminal_state = match use_terminal(20, 80) {
            Ok(s) => s,
            Err(e) => {
                outer_result = Err(e);
                return;
            }
        };

        // Manual states wrapped in Arc<Mutex> for shared reactive access
        let throbber_state = Arc::new(Mutex::new(ThrobberState::default()));
        let scroll_state = Arc::new(Mutex::new(ScrollViewState::default()));
        let mut frame_count = 0u64;

        let effect_fn = fx::effect_fn((), 2000u32, |_, ctx, mut cells| {
            let alpha = ctx.alpha();
            for cell in cells.by_ref() {
                let r = (alpha * 255.0) as u8;
                cell.1.set_fg(Color::Rgb(r, 100, 255 - r));
            }
        });
        let mut repeating_effect = fx::repeating(effect_fn);

        while window.update().expect("window update failed") {
            frame_count += 1;
            if frame_count.is_multiple_of(6) {
                if let Ok(mut ts) = throbber_state.lock() {
                    ts.calc_next();
                }
            }

            let current_tab = state.selected_tab.get();
            let dispatcher = window.dispatcher;

            let terminal_ref = &terminal_state;
            let textbox_ref = &textbox_state;
            let scroll_ref = Arc::clone(&scroll_state);

            dispatcher.on(|event| {
                match event {
                    SmashEvent::Key(key) => {
                        state.last_key_debug.set(Some(*key));
                        
                        if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                            let speed = if key.modifiers.contains(KeyModifiers::CONTROL) { 5 } else { 1 };
                            let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                            
                            // big text tab controls
                        if current_tab == 0 {
                            match key.code {
                                KeyCode::Char('+') | KeyCode::Up => {
                                    state.counter.set(state.counter.get() + 1);
                                    return EventStatus::Handled;
                                }
                                KeyCode::Char('-') | KeyCode::Down => {
                                    state.counter.set(state.counter.get() - 1);
                                    return EventStatus::Handled;
                                }
                                _ => {}
                            }
                        }

                        // Terminal
                            if current_tab == 4 {
                                if terminal_ref.handle_event(key) {
                                    return EventStatus::Handled;
                                }
                            }

                            // Input (Reactive TextBox)
                            if current_tab == 3 {
                                if textbox_ref.handle_event(key) {
                                    return EventStatus::Handled;
                                }
                                // Always consume vertical arrows in textbox
                                if matches!(key.code, KeyCode::Up | KeyCode::Down) {
                                    return EventStatus::Handled;
                                }
                            }

                            // Theme
                            if current_tab == 5 {
                                match key.code {
                                    KeyCode::Up => {
                                        let next = if state.selected_theme_idx.get() == 0 { theme_presets.len() - 1 } else { state.selected_theme_idx.get() - 1 };
                                        state.selected_theme_idx.set(next);
                                        return EventStatus::Handled;
                                    }
                                    KeyCode::Down => {
                                        let next = (state.selected_theme_idx.get() + 1) % theme_presets.len();
                                        state.selected_theme_idx.set(next);
                                        return EventStatus::Handled;
                                    }
                                    KeyCode::Char('d') => {
                                        state.is_dark.set(!state.is_dark.get());
                                        return EventStatus::Handled;
                                    }
                                    _ => {}
                                }
                            }

                            // Scroll
                            if current_tab == 2 {
                                if let Ok(mut ss) = scroll_ref.lock() {
                                    match key.code {
                                        KeyCode::Down => {
                                            for _ in 0..speed { ss.scroll_down(); }
                                            return EventStatus::Handled;
                                        }
                                        KeyCode::Up => {
                                            for _ in 0..speed { ss.scroll_up(); }
                                            return EventStatus::Handled;
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            // Global Tab Switch
                            if is_ctrl {
                                match key.code {
                                    KeyCode::Right => {
                                        state.selected_tab.set((current_tab + 1) % tabs.len());
                                        return EventStatus::Handled;
                                    }
                                    KeyCode::Left => {
                                        state.selected_tab.set(if current_tab == 0 { tabs.len() - 1 } else { current_tab - 1 });
                                        return EventStatus::Handled;
                                    }
                                    _ => {}
                                }
                            }
                        }

                        if key.kind == KeyEventKind::Press {
                            match key.code {
                                KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    return EventStatus::Handled; 
                                }
                                KeyCode::Tab => {
                                    state.selected_tab.set((current_tab + 1) % tabs.len());
                                    return EventStatus::Handled;
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                EventStatus::Ignored
            });

            window.theme = SmashTheme::from_seed(theme_presets[state.selected_theme_idx.get()].1, state.is_dark.get());

            let current_theme = window.theme;
            let last_key = state.last_key_debug.get();

            window.draw(|frame| {
                let area = frame.area();
                frame.render_widget(Block::default().bg(current_theme.background), area);

                let layout = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)])
                    .split(area);

                let tab_titles = tabs.iter().map(|t| Line::from(*t)).collect::<Vec<_>>();
                frame.render_widget(
                    Tabs::new(tab_titles)
                        .block(Block::default().borders(Borders::ALL).title("smash cookbook").border_style(Style::default().fg(current_theme.outline)))
                        .select(state.selected_tab.get())
                        .style(Style::default().fg(current_theme.on_surface))
                        .highlight_style(Style::default().fg(current_theme.primary).add_modifier(Modifier::BOLD)),
                    layout[0]
                );

                match state.selected_tab.get() {
                    0 => draw_big_text(frame, layout[1], &current_theme, state.counter.get()),
                    1 => {
                        if let Ok(mut ts) = throbber_state.lock() {
                            draw_widgets(frame, layout[1], &mut ts, &current_theme);
                        }
                    }
                    2 => {
                        if let Ok(mut ss) = scroll_state.lock() {
                            draw_scroll_effects(frame, layout[1], &mut ss, &mut repeating_effect, &current_theme);
                        }
                    }
                    3 => text_box_component(frame, layout[1], &textbox_state, &current_theme),
                    4 => terminal_component(frame, layout[1], &terminal_state, &current_theme),
                    5 => draw_theme_demo(frame, layout[1], &current_theme, &theme_presets, state.selected_theme_idx.get(), state.is_dark.get()),
                    _ => {}
                }

                let footer_text = if let Some(lk) = last_key {
                    let key_str = match lk.code {
                        KeyCode::Char(c) => format!("'{}'", c),
                        _ => format!("{:?}", lk.code),
                    };
                    let mod_str = if lk.modifiers.is_empty() { "".to_string() } else { format!("+{:?}", lk.modifiers) };
                    format!("tab: next | ctrl+arrows: switch | ctrl+q: quit | last: {}{}", key_str, mod_str)
                } else {
                    "tab: next | ctrl+arrows: switch | ctrl+q: quit".to_string()
                };
                frame.render_widget(Paragraph::new(footer_text).style(Style::default().fg(current_theme.on_background).dim()), layout[2]);
            }).expect("draw failed");
        }

        window.close().expect("close failed");
    });

    outer_result
}

// --- Components ---

fn draw_big_text(frame: &mut Frame, area: Rect, theme: &SmashTheme, counter: i32) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    let big_text = BigTextBuilder::default()
        .pixel_size(PixelSize::HalfHeight)
        .lines(vec![format!("count: {}", counter).into()])
        .style(Style::default().fg(theme.primary))
        .build();
    frame.render_widget(big_text, layout[0]);

    // Draw buttons
    let button_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(15), Constraint::Length(15)])
        .split(layout[1]);

    let btn_plus = Paragraph::new("  [+] increment")
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme.secondary)))
        .style(Style::default().fg(theme.on_surface));
    frame.render_widget(btn_plus, button_layout[0]);

    let btn_minus = Paragraph::new("  [-] decrement")
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme.secondary)))
        .style(Style::default().fg(theme.on_surface));
    frame.render_widget(btn_minus, button_layout[1]);
}

fn draw_widgets(frame: &mut Frame, area: Rect, throbber_state: &mut ThrobberState, theme: &SmashTheme) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    frame.render_stateful_widget(
        Throbber::default().label("processing...").style(Style::default().fg(theme.secondary)).throbber_set(smash_shell::throbber_widgets_tui::BRAILLE_SIX),
        chunks[0],
        throbber_state
    );

    let pie_area = centered_square(chunks[1], 3.0);
    let slices = vec![
        PieSlice::new("rust", 70.0, theme.primary),
        PieSlice::new("tui", 20.0, theme.secondary_container),
        PieSlice::new("fun", 10.0, theme.tertiary_container),
    ];
    frame.render_widget(
        PieChart::new(slices)
            .high_resolution(true)
            .block(Block::default().title("pie chart").borders(Borders::ALL).border_style(Style::default().fg(theme.outline)).bg(theme.surface))
            .style(Style::default().fg(theme.on_surface_variant)),
        pie_area
    );
}

fn draw_scroll_effects(frame: &mut Frame, area: Rect, scroll_state: &mut ScrollViewState, effect: &mut Effect, theme: &SmashTheme) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let mut scroll_view = ScrollView::new(Size::new(layout[0].width, 30))
        .scrollbars_visibility(smash_shell::tui_scrollview::ScrollbarVisibility::Never);
    
    for cell in scroll_view.buf_mut().content.iter_mut() { cell.set_bg(theme.background); }
    
    let content = (0..30).map(|i| format!("line {} of scrollable content", i)).collect::<Vec<_>>().join("\n");
    scroll_view.render_widget(
        Paragraph::new(content)
            .block(Block::default().borders(Borders::ALL).title("scroll area").border_style(Style::default().fg(theme.outline)))
            .style(Style::default().fg(theme.on_surface)),
        Rect::new(0, 0, layout[0].width, 30),
    );
    frame.render_stateful_widget(scroll_view, layout[0], scroll_state);

    let mut scrollbar_state = ScrollbarState::new(30usize.saturating_sub(layout[0].height as usize)).position(scroll_state.offset().y as usize);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight).style(Style::default().fg(theme.primary)),
        layout[0],
        &mut scrollbar_state
    );

    let effect_block = Block::default().borders(Borders::ALL).title("tachyonfx").border_style(Style::default().fg(theme.outline));
    let inner_area = effect_block.inner(layout[1]);
    frame.render_widget(effect_block, layout[1]);
    frame.render_widget(Paragraph::new("color animation").alignment(Alignment::Center).fg(theme.on_surface), inner_area);
    effect.process(smash_shell::tachyonfx::Duration::from_millis(16), frame.buffer_mut(), inner_area);
}

fn draw_theme_demo(frame: &mut Frame, area: Rect, theme: &SmashTheme, presets: &[(&str, u32)], selected_idx: usize, is_dark: bool) {
    let layout = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(30), Constraint::Percentage(70)]).split(area);

    let items: Vec<ListItem> = presets.iter().enumerate().map(|(i, (name, _))| {
        let style = if i == selected_idx { Style::default().fg(theme.primary).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.on_surface) };
        ListItem::new(format!("  {}", name)).style(style)
    }).collect();

    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title("presets").border_style(Style::default().fg(theme.outline))),
        layout[0]
    );

    let colors = [
        ("primary", theme.primary, theme.on_primary), ("primary container", theme.primary_container, theme.on_primary_container),
        ("secondary", theme.secondary, theme.on_secondary), ("secondary container", theme.secondary_container, theme.on_secondary_container),
        ("tertiary", theme.tertiary, theme.on_tertiary), ("tertiary container", theme.tertiary_container, theme.on_tertiary_container),
        ("error", theme.error, theme.on_error), ("background", theme.background, theme.on_background),
        ("surface", theme.surface, theme.on_surface), ("outline", theme.outline, theme.on_surface),
    ];

    let swatches_layout = Layout::default().direction(Direction::Vertical).constraints(vec![Constraint::Length(2); colors.len()]).split(layout[1]);
    for (i, (name, bg, fg)) in colors.iter().enumerate() {
        frame.render_widget(Paragraph::new(format!("  {}", name)).style(Style::default().bg(*bg).fg(*fg)), swatches_layout[i]);
    }

    frame.render_widget(
        Paragraph::new(format!("\n  arrows: select preset\n  d: toggle dark/light (currently: {})\n", if is_dark { "dark" } else { "light" })).style(Style::default().fg(theme.on_surface)),
        layout[1]
    );
}

// --- Utils ---

fn centered_square(area: Rect, bias: f32) -> Rect {
    let width = area.width;
    let height = area.height;
    let target_width = ((height as f32 * bias) as u16).min(width);
    let target_height = (target_width as f32 / bias) as u16;
    Rect::new(area.x + (width - target_width) / 2, area.y + (height - target_height) / 2, target_width, target_height)
}
