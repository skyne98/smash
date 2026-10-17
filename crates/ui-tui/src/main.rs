//! Terminal client: translates crossterm keys into `smash_ui::chat::Key` and
//! prints the rows `Chat::frame` lays out. All behaviour lives in smash-ui,
//! shared with the gpui client.

use std::io::Write;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use smash_ui::NIGHT;
use smash_ui::chat::{Chat, Key, Outcome, PICKER_ROWS, PickerView};
use smash_ui::layout::{Paint, Row};

const fn rgb(color: u32) -> Color {
    Color::Rgb((color >> 16) as u8, (color >> 8) as u8, color as u8)
}

fn style(paint: Paint) -> Style {
    let mut style = Style::new().fg(rgb(paint.fg));
    if let Some(bg) = paint.bg {
        style = style.bg(rgb(bg));
    }
    for (on, modifier) in [
        (paint.bold, Modifier::BOLD),
        (paint.italic, Modifier::ITALIC),
        (paint.underline, Modifier::UNDERLINED),
        (paint.strike, Modifier::CROSSED_OUT),
    ] {
        if on {
            style = style.add_modifier(modifier);
        }
    }
    style
}

fn line(row: Row) -> Line<'static> {
    Line::from(
        row.into_iter()
            .map(|run| Span::styled(run.text, style(run.paint)))
            .collect::<Vec<_>>(),
    )
}

fn key(code: KeyCode) -> Option<Key> {
    Some(match code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Esc => Key::Esc,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        _ => return None,
    })
}

/// OSC 52: the terminal puts it on the system clipboard (works over SSH; tmux
/// needs `set -g set-clipboard on`).
fn copy(text: &str) -> std::io::Result<()> {
    use base64::Engine;
    let payload = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{payload}\x07")?;
    out.flush()
}

fn main() -> std::io::Result<()> {
    let mut chat = Chat::open();
    let mut terminal = ratatui::init();
    // Alternate scroll mode: the wheel arrives as ↑/↓ keys and mouse selection
    // keeps working (mouse capture would break copy/paste).
    print!("\x1b[?1007h");
    let result = run(&mut terminal, &mut chat);
    print!("\x1b[?1007l");
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, chat: &mut Chat) -> std::io::Result<()> {
    let mut dirty = true;
    loop {
        // Idle: no layout, no draw. Generating: animate every tick.
        if dirty || chat.animating() {
            terminal.draw(|frame| {
                let area = frame.area();
                let screen = chat.frame(area.width as usize, area.height as usize);
                let lines: Vec<Line> = screen.rows.into_iter().map(line).collect();
                frame.render_widget(Paragraph::new(lines), area);
                if let Some(picker) = screen.picker {
                    draw_picker(frame, area, picker);
                }
            })?;
            dirty = false;
        }

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(press) if press.kind == KeyEventKind::Press => {
                    let Some(key) = key(press.code) else { continue };
                    let ctrl = press.modifiers.contains(KeyModifiers::CONTROL);
                    match chat.key(key, ctrl) {
                        Outcome::Quit => return Ok(()),
                        Outcome::Copy(text) => {
                            if let Err(err) = copy(&text) {
                                chat.notice(format!("copy failed: {err}"));
                            }
                        }
                        Outcome::Redraw => {}
                    }
                    dirty = true;
                }
                Event::Resize(..) => dirty = true,
                _ => {}
            }
        }
        dirty |= chat.poll();
    }
}

fn draw_picker(frame: &mut ratatui::Frame, area: Rect, picker: PickerView) {
    let height =
        (PICKER_ROWS.min(picker.items.len().max(1)) as u16 + 3).min(area.height.saturating_sub(4));
    let width = 64.min(area.width.saturating_sub(8));
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .title(format!(" {} ", picker.title))
        .title_alignment(Alignment::Center)
        .border_style(Style::new().fg(rgb(NIGHT.dim)))
        .style(Style::new().bg(rgb(NIGHT.bg)));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let accent = Style::new().fg(rgb(NIGHT.accent));
    let query = if picker.query.is_empty() {
        Span::styled("type to search", Style::new().fg(rgb(NIGHT.dim)))
    } else {
        Span::styled(picker.query.clone(), Style::new().fg(rgb(NIGHT.text)))
    };
    let mut lines = vec![Line::from(vec![Span::styled("❯ ", accent), query])];
    if picker.items.is_empty() {
        lines.push(Line::styled(
            "no match · esc closes",
            Style::new().fg(rgb(NIGHT.dim)),
        ));
    }
    for (_, label, selected) in picker.items {
        lines.push(if selected {
            Line::from(vec![
                Span::styled("▌ ", accent),
                Span::styled(
                    label,
                    Style::new()
                        .fg(rgb(NIGHT.text))
                        .add_modifier(Modifier::BOLD),
                ),
            ])
            .style(Style::new().bg(rgb(NIGHT.surface)))
        } else {
            Line::styled(format!("  {label}"), Style::new().fg(rgb(NIGHT.dim)))
        });
    }
    frame.render_widget(Paragraph::new(lines), inner);
    frame.set_cursor_position((inner.x + 2 + picker.query.chars().count() as u16, inner.y));
}
