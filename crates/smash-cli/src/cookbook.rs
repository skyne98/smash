use anyhow::Result;
use smash_shell::prelude::*;
use smash_shell::tui_big_text::{BigTextBuilder, PixelSize};
use smash_shell::throbber_widgets_tui::{Throbber, ThrobberState};
use smash_shell::tui_piechart::{PieChart, PieSlice};
use smash_shell::tui_scrollview::{ScrollView, ScrollViewState};
use smash_shell::tachyonfx::*;
use smash_shell::textbox::TextBox;
use smash_shell::terminal::Terminal as SmashTerminal;
use smash_shell::tui_term::widget::PseudoTerminal;

use smash_shell::crossterm::event::{KeyEventKind, KeyModifiers};

pub async fn run_cookbook() -> Result<()> {
    let mut window = Window::new()?;
    let mut selected_tab = 0;
    let tabs = ["big text", "widgets", "scroll & effects", "input", "terminal", "theme"];
    
    let mut throbber_state = ThrobberState::default();
    let mut scroll_state = ScrollViewState::default();
    let mut frame_count = 0u64;
    
    let mut textbox = TextBox::new().with_text("welcome to smash shell.\ntype here to test multiline editing.\n\ntry shift+arrows to select,\nctrl+c to copy, ctrl+v to paste.");
    let mut terminal_sub = SmashTerminal::new(20, 80)?;
    let mut is_terminal_focused = false;
    let mut is_textbox_focused = false;
    let mut last_key_debug: Option<smash_shell::crossterm::event::KeyEvent> = None;

    let theme_presets = [
        ("violet", presets::VIOLET),
        ("ocean", presets::OCEAN),
        ("forest", presets::FOREST),
        ("fire", presets::FIRE),
        ("gold", presets::GOLD),
    ];
    let mut selected_theme_idx = 0;
    let mut is_dark = true;

    // Simple color cycling effect using effect_fn
    let effect = fx::effect_fn((), 2000u32, |_, ctx, mut cells| {
        let alpha = ctx.alpha();
        for cell in cells.by_ref() {
            let r = (alpha * 255.0) as u8;
            cell.1.set_fg(Color::Rgb(r, 100, 255 - r));
        }
    });
    let mut repeating_effect = fx::repeating(effect);

    while window.update()? {
        frame_count += 1;
        if frame_count.is_multiple_of(6) {
            throbber_state.calc_next();
        }

        // Process all events in this frame
        for key in window.key_events.clone() {
            last_key_debug = Some(key);
            let mut event_handled = false;

            if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                let speed = if key.modifiers.contains(KeyModifiers::CONTROL) { 5 } else { 1 };
                let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                
                // Terminal input handling
                if selected_tab == 4 {
                    if is_terminal_focused {
                        if key.code == KeyCode::Esc {
                            is_terminal_focused = false;
                            event_handled = true;
                        } else {
                            if let KeyCode::Char(c) = key.code {
                                let mut buf = [0u8; 4];
                                let s = c.encode_utf8(&mut buf);
                                terminal_sub.write(s.as_bytes())?;
                            } else if key.code == KeyCode::Enter {
                                terminal_sub.write(b"\r")?;
                            } else if key.code == KeyCode::Backspace {
                                terminal_sub.write(b"\x7f")?;
                            } else if key.code == KeyCode::Up {
                                terminal_sub.write(b"\x1b[A")?;
                            } else if key.code == KeyCode::Down {
                                terminal_sub.write(b"\x1b[B")?;
                            } else if key.code == KeyCode::Right {
                                terminal_sub.write(b"\x1b[C")?;
                            } else if key.code == KeyCode::Left {
                                terminal_sub.write(b"\x1b[D")?;
                            }
                            event_handled = true;
                        }
                    } else if key.code == KeyCode::Enter {
                        is_terminal_focused = true;
                        event_handled = true;
                    }
                }

                // Textbox input handling
                if !event_handled && selected_tab == 3 {
                    if is_textbox_focused {
                        if key.code == KeyCode::Esc {
                            is_textbox_focused = false;
                            event_handled = true;
                        } else {
                            event_handled = textbox.handle_event(&key);
                            if !event_handled && matches!(key.code, KeyCode::Up | KeyCode::Down) {
                                event_handled = true;
                            }
                        }
                    } else if key.code == KeyCode::Enter {
                        is_textbox_focused = true;
                        event_handled = true;
                    }
                }

                // Theme handling
                if !event_handled && selected_tab == 5 {
                    match key.code {
                        KeyCode::Up => {
                            selected_theme_idx = if selected_theme_idx == 0 { theme_presets.len() - 1 } else { selected_theme_idx - 1 };
                            window.theme = SmashTheme::from_seed(theme_presets[selected_theme_idx].1, is_dark);
                            event_handled = true;
                        }
                        KeyCode::Down => {
                            selected_theme_idx = (selected_theme_idx + 1) % theme_presets.len();
                            window.theme = SmashTheme::from_seed(theme_presets[selected_theme_idx].1, is_dark);
                            event_handled = true;
                        }
                        KeyCode::Char('d') => {
                            is_dark = !is_dark;
                            window.theme = SmashTheme::from_seed(theme_presets[selected_theme_idx].1, is_dark);
                            event_handled = true;
                        }
                        _ => {}
                    }
                }

                // Scroll view handling
                if !event_handled && selected_tab == 2 {
                    match key.code {
                        KeyCode::Down => {
                            for _ in 0..speed { scroll_state.scroll_down(); }
                            event_handled = true;
                        }
                        KeyCode::Up => {
                            for _ in 0..speed { scroll_state.scroll_up(); }
                            event_handled = true;
                        }
                        _ => {}
                    }
                }

                // Tab switching with Ctrl + Arrows
                if !event_handled && is_ctrl {
                    match key.code {
                        KeyCode::Right => {
                            selected_tab = (selected_tab + 1) % tabs.len();
                            event_handled = true;
                        }
                        KeyCode::Left => {
                            selected_tab = if selected_tab == 0 { tabs.len() - 1 } else { selected_tab - 1 };
                            event_handled = true;
                        }
                        _ => {}
                    }
                }
            }

            // Tab switching with Tab key and Quit
            if !event_handled && key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        window.should_quit = true;
                    }
                    KeyCode::Tab => {
                        selected_tab = (selected_tab + 1) % tabs.len();
                    }
                    _ => {}
                }
            }
        }

        let last_key_debug_val = last_key_debug;
        let current_theme = window.theme;

        window.draw(|frame| {
            let area = frame.area();
            
            // Fill background
            frame.render_widget(Block::default().bg(current_theme.background), area);

            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(0),
                    Constraint::Length(1),
                ])
                .split(area);

            let tab_titles = tabs.iter().map(|t| Line::from(*t)).collect::<Vec<_>>();
            let tabs_widget = Tabs::new(tab_titles)
                .block(Block::default()
                    .borders(Borders::ALL)
                    .title("smash cookbook")
                    .border_style(Style::default().fg(current_theme.outline))
                )
                .select(selected_tab)
                .style(Style::default().fg(current_theme.on_surface))
                .highlight_style(Style::default().fg(current_theme.primary).add_modifier(Modifier::BOLD));
            frame.render_widget(tabs_widget, layout[0]);

            match selected_tab {
                0 => draw_big_text(frame, layout[1], &current_theme),
                1 => draw_widgets(frame, layout[1], &mut throbber_state, &current_theme),
                2 => draw_scroll_effects(frame, layout[1], &mut scroll_state, &mut repeating_effect, &current_theme),
                3 => draw_input(frame, layout[1], &mut textbox, is_textbox_focused, &current_theme),
                4 => draw_terminal(frame, layout[1], &mut terminal_sub, is_terminal_focused, &current_theme),
                5 => draw_theme_demo(frame, layout[1], &current_theme, &theme_presets, selected_theme_idx, is_dark),
                _ => {}
            }

            let footer_text = if let Some(last_key) = last_key_debug_val {
                let key_str = match last_key.code {
                    KeyCode::Char(c) => format!("'{}'", c),
                    _ => format!("{:?}", last_key.code),
                };
                let mod_str = if last_key.modifiers.is_empty() {
                    "".to_string()
                } else {
                    format!("+{:?}", last_key.modifiers)
                };
                format!("tab: next | ctrl+arrows: switch | ctrl+q: quit | last: {}{}", key_str, mod_str)
            } else {
                "tab: next | ctrl+arrows: switch | ctrl+q: quit".to_string()
            };

            let footer = Paragraph::new(footer_text)
                .style(Style::default().fg(current_theme.on_background).dim());
            frame.render_widget(footer, layout[2]);
        })?;
    }

    window.close()?;
    Ok(())
}

fn draw_big_text(frame: &mut Frame, area: Rect, theme: &SmashTheme) {
    let big_text = BigTextBuilder::default()
        .pixel_size(PixelSize::HalfHeight)
        .lines(vec!["smash".into(), "shell".into()])
        .style(Style::default().fg(theme.primary))
        .build();
    frame.render_widget(big_text, area);
}

fn draw_widgets(frame: &mut Frame, area: Rect, throbber_state: &mut ThrobberState, theme: &SmashTheme) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let throbber = Throbber::default()
        .label("processing...")
        .style(Style::default().fg(theme.secondary))
        .throbber_set(smash_shell::throbber_widgets_tui::BRAILLE_SIX);
    frame.render_stateful_widget(throbber, chunks[0], throbber_state);

    let pie_area = {
        let width = chunks[1].width;
        let height = chunks[1].height;
        let target_width = ((height as f32 * 3.0) as u16).min(width);
        let target_height = (target_width as f32 / 3.0) as u16;
        
        Rect::new(
            chunks[1].x + (width - target_width) / 2,
            chunks[1].y + (height - target_height) / 2,
            target_width,
            target_height,
        )
    };

    let slices = vec![
        PieSlice::new("rust", 70.0, theme.primary),
        PieSlice::new("tui", 20.0, theme.secondary_container),
        PieSlice::new("fun", 10.0, theme.tertiary_container),
    ];
    let pie = PieChart::new(slices)
        .high_resolution(true)
        .block(Block::default()
            .title("pie chart")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.outline))
            .bg(theme.surface)
        )
        .style(Style::default().fg(theme.on_surface_variant));
    frame.render_widget(pie, pie_area);
}

fn draw_scroll_effects(frame: &mut Frame, area: Rect, scroll_state: &mut ScrollViewState, effect: &mut Effect, theme: &SmashTheme) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let mut scroll_view = ScrollView::new(Size::new(layout[0].width, 30))
        .scrollbars_visibility(smash_shell::tui_scrollview::ScrollbarVisibility::Never);
    
    // Fill the internal buffer with theme background to avoid black holes
    for cell in scroll_view.buf_mut().content.iter_mut() {
        cell.set_bg(theme.background);
    }
    
    let content = (0..30)
        .map(|i| format!("line {} of scrollable content", i))
        .collect::<Vec<_>>()
        .join("\n");
    
    scroll_view.render_widget(
        Paragraph::new(content)
            .block(Block::default()
                .borders(Borders::ALL)
                .title("scroll area")
                .border_style(Style::default().fg(theme.outline))
            )
            .style(Style::default().fg(theme.on_surface)),
        Rect::new(0, 0, layout[0].width, 30),
    );
    frame.render_stateful_widget(scroll_view, layout[0], scroll_state);

    // Render themed scrollbar manually
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .style(Style::default().fg(theme.primary));
    
    let mut scrollbar_state = ScrollbarState::new(30usize.saturating_sub(layout[0].height as usize))
        .position(scroll_state.offset().y as usize);
        
    frame.render_stateful_widget(scrollbar, layout[0], &mut scrollbar_state);

    let effect_block = Block::default()
        .borders(Borders::ALL)
        .title("tachyonfx")
        .border_style(Style::default().fg(theme.outline));
    let inner_area = effect_block.inner(layout[1]);
    frame.render_widget(effect_block, layout[1]);
    frame.render_widget(Paragraph::new("color animation").alignment(Alignment::Center).fg(theme.on_surface), inner_area);
    effect.process(smash_shell::tachyonfx::Duration::from_millis(16), frame.buffer_mut(), inner_area);
}

fn draw_input(frame: &mut Frame, area: Rect, textbox: &mut TextBox, is_focused: bool, theme: &SmashTheme) {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.outline))
        .bg(theme.surface);
    
    if is_focused {
        block = block.title("rich text editor (focused - esc to unfocus)").border_style(Style::default().fg(theme.primary));
    } else {
        block = block.title("rich text editor (unfocused - enter to focus)");
    }
    
    textbox.selection_style = Style::default().bg(theme.primary_container).fg(theme.on_primary_container);
    
    let inner_area = block.inner(area);
    textbox.render(area, frame.buffer_mut(), Some(block));
    
    if is_focused {
        if let Some((cx, cy)) = textbox.cursor_position(inner_area) {
            frame.set_cursor_position((cx, cy));
        }
    }
}

fn draw_terminal(frame: &mut Frame, area: Rect, terminal: &mut SmashTerminal, is_focused: bool, theme: &SmashTheme) {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.outline))
        .bg(theme.surface);
    
    if is_focused {
        block = block.title("embedded bash (focused - esc to unfocus)").border_style(Style::default().fg(theme.primary));
    } else {
        block = block.title("embedded bash (unfocused - enter to focus)");
    }

    let inner_area = block.inner(area);
    
    // Check if resize is needed
    let mut needs_resize = false;
    if let Ok(parser) = terminal.parser.lock() {
        let screen = parser.screen();
        if screen.size().0 != inner_area.height || screen.size().1 != inner_area.width {
            needs_resize = true;
        }
    }

    if needs_resize {
        let _ = terminal.resize(inner_area.height, inner_area.width);
    }

    // Render terminal safely
    if let Ok(parser) = terminal.parser.lock() {
        let term_widget = PseudoTerminal::new(parser.screen()).block(block);
        frame.render_widget(term_widget, area);
    }
}

fn draw_theme_demo(frame: &mut Frame, area: Rect, theme: &SmashTheme, presets: &[(&str, u32)], selected_idx: usize, is_dark: bool) {
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    // Preset list
    let items: Vec<ListItem> = presets.iter().enumerate().map(|(i, (name, _))| {
        let style = if i == selected_idx {
            Style::default().fg(theme.primary).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.on_surface)
        };
        ListItem::new(format!("  {}", name)).style(style)
    }).collect();

    let list = List::new(items)
        .block(Block::default()
            .borders(Borders::ALL)
            .title("presets")
            .border_style(Style::default().fg(theme.outline))
        );
    frame.render_widget(list, layout[0]);

    // Color swatches
    let colors = [
        ("primary", theme.primary, theme.on_primary),
        ("primary container", theme.primary_container, theme.on_primary_container),
        ("secondary", theme.secondary, theme.on_secondary),
        ("secondary container", theme.secondary_container, theme.on_secondary_container),
        ("tertiary", theme.tertiary, theme.on_tertiary),
        ("tertiary container", theme.tertiary_container, theme.on_tertiary_container),
        ("error", theme.error, theme.on_error),
        ("background", theme.background, theme.on_background),
        ("surface", theme.surface, theme.on_surface),
        ("outline", theme.outline, theme.on_surface),
    ];

    let swatches_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(2); colors.len()])
        .split(layout[1]);

    for (i, (name, bg, fg)) in colors.iter().enumerate() {
        let p = Paragraph::new(format!("  {}", name))
            .style(Style::default().bg(*bg).fg(*fg));
        frame.render_widget(p, swatches_layout[i]);
    }

    let hints = Paragraph::new(format!("\n  arrows: select preset\n  d: toggle dark/light (currently: {})\n", if is_dark { "dark" } else { "light" }))
        .style(Style::default().fg(theme.on_surface));
    frame.render_widget(hints, layout[1]);
}
