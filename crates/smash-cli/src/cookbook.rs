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
    let tabs = ["Big Text", "Widgets", "Scroll & Effects", "Input", "Terminal"];
    
    let mut throbber_state = ThrobberState::default();
    let mut scroll_state = ScrollViewState::default();
    let mut frame_count = 0u64;
    
    let mut textbox = TextBox::new().with_text("Welcome to Smash Shell!\nType here to test multiline editing.\n\nTry Shift+Arrows to select,\nCtrl+C to copy, Ctrl+V to paste.");
    let mut terminal_sub = SmashTerminal::new(20, 80)?;
    let mut is_terminal_focused = false;

    // Simple color cycling effect using effect_fn
    let effect = fx::effect_fn((), 2000u32, |_, ctx, mut cells| {
        let alpha = ctx.alpha();
        for cell in cells.by_ref() {
            let r = (alpha * 255.0) as u8;
            cell.1.set_fg(Color::Rgb(r, 100, 255 - r));
        }
    });
    let mut repeating_effect = fx::repeating(effect);
    let mut last_key_debug: Option<smash_shell::crossterm::event::KeyEvent> = None;

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
                    event_handled = textbox.handle_event(&key);
                    if !event_handled {
                        if (key.code == KeyCode::Left && !textbox.cursor_at_start())
                            || (key.code == KeyCode::Right && !textbox.cursor_at_end())
                        {
                            event_handled = true; 
                        }
                        if matches!(key.code, KeyCode::Up | KeyCode::Down) {
                            event_handled = true;
                        }
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
            }

            // Tab switching and general actions
            if !event_handled && key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Char('q') => {
                        window.should_quit = true;
                    }
                    KeyCode::Tab | KeyCode::Right => {
                        selected_tab = (selected_tab + 1) % tabs.len();
                    }
                    KeyCode::Left => {
                        selected_tab = if selected_tab == 0 { tabs.len() - 1 } else { selected_tab - 1 };
                    }
                    _ => {}
                }
            }
        }

        window.draw(|frame| {
            let area = frame.area();
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
                .block(Block::default().borders(Borders::ALL).title("Smash Cookbook"))
                .select(selected_tab)
                .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
            frame.render_widget(tabs_widget, layout[0]);

            match selected_tab {
                0 => draw_big_text(frame, layout[1]),
                1 => draw_widgets(frame, layout[1], &mut throbber_state),
                2 => draw_scroll_effects(frame, layout[1], &mut scroll_state, &mut repeating_effect),
                3 => draw_input(frame, layout[1], &mut textbox),
                4 => draw_terminal(frame, layout[1], &terminal_sub, is_terminal_focused),
                _ => {}
            }

            let footer_text = if let Some(last_key) = last_key_debug {
                format!("Tab: Next Tab | Arrows: Navigate | Ctrl+Q: Quit | Last Key: {:?}", last_key)
            } else {
                "Tab: Next Tab | Arrows: Navigate | Ctrl+Q: Quit".to_string()
            };

            let footer = Paragraph::new(footer_text)
                .style(Style::default().dim());
            frame.render_widget(footer, layout[2]);
        })?;
    }

    window.close()?;
    Ok(())
}

fn draw_big_text(frame: &mut Frame, area: Rect) {
    let big_text = BigTextBuilder::default()
        .pixel_size(PixelSize::HalfHeight)
        .lines(vec!["SMASH".into(), "SHELL".into()])
        .style(Style::default().fg(Color::Cyan))
        .build();
    frame.render_widget(big_text, area);
}

fn draw_widgets(frame: &mut Frame, area: Rect, throbber_state: &mut ThrobberState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let throbber = Throbber::default()
        .label("Processing task...")
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
        PieSlice::new("Rust", 70.0, Color::Red),
        PieSlice::new("TUI", 20.0, Color::Blue),
        PieSlice::new("Fun", 10.0, Color::Green),
    ];
    let pie = PieChart::new(slices)
        .high_resolution(true)
        .block(Block::default().title("Pie Chart Demo").borders(Borders::ALL));
    frame.render_widget(pie, pie_area);
}

fn draw_scroll_effects(frame: &mut Frame, area: Rect, scroll_state: &mut ScrollViewState, effect: &mut Effect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let mut scroll_view = ScrollView::new(Size::new(layout[0].width, 30));
    let content = (0..30)
        .map(|i| format!("Line {} of scrollable content", i))
        .collect::<Vec<_>>()
        .join("\n");
    
    scroll_view.render_widget(
        Paragraph::new(content).block(Block::default().borders(Borders::ALL).title("Scroll Area")),
        Rect::new(0, 0, layout[0].width, 30),
    );
    frame.render_stateful_widget(scroll_view, layout[0], scroll_state);

    let effect_block = Block::default().borders(Borders::ALL).title("TachyonFX Effect");
    let inner_area = effect_block.inner(layout[1]);
    frame.render_widget(effect_block, layout[1]);
    frame.render_widget(Paragraph::new("COLOR ANIMATION").alignment(Alignment::Center), inner_area);
    effect.process(smash_shell::tachyonfx::Duration::from_millis(16), frame.buffer_mut(), inner_area);
}

fn draw_input(frame: &mut Frame, area: Rect, textbox: &mut TextBox) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Rich Text Editor (Shift+Arrows: Select | Ctrl/Alt+Arrows: Word Jump | Ctrl+C/X/V: Clipboard)");
    
    let inner_area = block.inner(area);
    textbox.render(area, frame.buffer_mut(), Some(block));
    
    if let Some((cx, cy)) = textbox.cursor_position(inner_area) {
        frame.set_cursor_position((cx, cy));
    }
}

fn draw_terminal(frame: &mut Frame, area: Rect, terminal: &SmashTerminal, is_focused: bool) {
    let parser = terminal.parser.lock().unwrap();
    let mut block = Block::default().borders(Borders::ALL);
    
    if is_focused {
        block = block.title("Embedded Bash (Focused - Press ESC to unfocus)").border_style(Style::default().fg(Color::Yellow));
    } else {
        block = block.title("Embedded Bash (Unfocused - Press ENTER to focus)");
    }

    let term_widget = PseudoTerminal::new(parser.screen()).block(block);
    frame.render_widget(term_widget, area);
}
