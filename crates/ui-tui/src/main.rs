//! Terminal client: translates crossterm keys into `smash_ui::chat::Key` and
//! prints the rows `Chat::frame` lays out. All behaviour lives in smash-ui,
//! shared with the gpui client.

use std::io::Write;
use std::time::Duration;

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::supports_keyboard_enhancement;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use smash_ui::NIGHT;
use smash_ui::chat::{Chat, Key, Mods, Outcome};
use smash_ui::layout::{Paint, Row};
use smash_ui::popup::Popup;

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
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Esc => Key::Esc,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        _ => return None,
    })
}

fn mods(modifiers: KeyModifiers) -> Mods {
    Mods {
        ctrl: modifiers.contains(KeyModifiers::CONTROL),
        shift: modifiers.contains(KeyModifiers::SHIFT),
        alt: modifiers.contains(KeyModifiers::ALT),
    }
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
    let mut out = std::io::stdout();
    // Pastes arrive as one event instead of a burst of keys (and Enter in a
    // pasted text no longer sends the prompt).
    let _ = execute!(out, EnableBracketedPaste);
    // kitty protocol: shift+enter, ctrl+backspace, ctrl+shift+z become distinct.
    let enhanced = supports_keyboard_enhancement().unwrap_or(false)
        && execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok();
    let result = run(&mut terminal, &mut chat);
    if enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableBracketedPaste);
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
                for bar in &screen.bars {
                    draw_bar(frame.buffer_mut(), area, bar);
                }
                if let Some(popup) = screen.popup {
                    draw_popup(frame, area, popup);
                }
            })?;
            dirty = false;
        }

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(press) if press.kind == KeyEventKind::Press => {
                    let Some(key) = key(press.code) else { continue };
                    match chat.key(key, mods(press.modifiers)) {
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
                Event::Paste(text) => {
                    chat.paste(&text);
                    dirty = true;
                }
                Event::Resize(..) => dirty = true,
                _ => {}
            }
        }
        dirty |= chat.poll();
    }
}

/// Scrollbar as glyphs: thin track, heavy thumb.
fn draw_bar(buf: &mut ratatui::buffer::Buffer, area: Rect, bar: &smash_ui::layout::Bar) {
    for row in bar.rows.clone() {
        let (symbol, color) = if bar.thumb.contains(&row) {
            ("┃", NIGHT.dim)
        } else {
            ("│", NIGHT.faint)
        };
        let at = (area.x + bar.col as u16, area.y + row as u16);
        if let Some(cell) = buf.cell_mut(at) {
            cell.set_symbol(symbol).set_fg(rgb(color));
        }
    }
}

/// Any popup: centered bordered box, rows painted like the main grid.
fn draw_popup(frame: &mut ratatui::Frame, area: Rect, popup: Popup) {
    let width = (popup.width as u16 + 2).min(area.width);
    let height = (popup.rows.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(format!(" {} ", popup.title))
        .title_alignment(Alignment::Center)
        .border_style(Style::new().fg(rgb(NIGHT.dim)))
        .style(Style::new().bg(rgb(NIGHT.bg)));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let lines: Vec<Line> = popup.rows.into_iter().map(line).collect();
    frame.render_widget(Paragraph::new(lines), inner);
    if let Some((row, col)) = popup.caret {
        frame.set_cursor_position((inner.x + col as u16, inner.y + row as u16));
    }
}
