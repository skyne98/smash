//! gpui client: translates gpui key/scroll events into `smash_ui::chat::Key`
//! and paints the rows `Chat::frame` lays out, in a monospace grid. All
//! behaviour lives in smash-ui, shared with the terminal client.

use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, Font, FontStyle, FontWeight, Hsla,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollDelta, ScrollWheelEvent,
    SharedString, StrikethroughStyle, StyledText, Task, TextRun, UnderlineStyle, Window,
    WindowBounds, WindowOptions, div, font, prelude::*, px, rgb, size,
};
use gpui_platform::application;
use smash_ui::NIGHT;
use smash_ui::chat::{Chat, Cursor, Key, Mods, Outcome, PickerView};
use smash_ui::layout::{Paint, Row};

const FONT: &str = "JetBrainsMono Nerd Font";
const FONT_SIZE: f32 = 14.;
const LINE_HEIGHT: f32 = 20.;
const PAD: f32 = 16.;

struct Root {
    chat: Chat,
    focus: FocusHandle,
    /// Width of one monospace cell, measured from the font at startup.
    cell: Pixels,
    /// Fractional rows from smooth (pixel) scrolling.
    scroll_rest: f32,
    _tick: Task<()>,
}

impl Root {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = window.text_system();
        let id = text.resolve_font(&font(FONT));
        let cell = text
            .advance(id, px(FONT_SIZE), 'm')
            .map(|size| size.width)
            .unwrap_or(px(FONT_SIZE * 0.6));
        // Poll agent output and animate the spinner; idle frames cost nothing.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let alive = this.update(cx, |root, cx| {
                    if root.chat.poll() || root.chat.animating() {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            chat: Chat::open().with_mouse(),
            focus: cx.focus_handle(),
            cell,
            scroll_rest: 0.,
            _tick: tick,
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let modifiers = &stroke.modifiers;
        let mods = Mods {
            ctrl: modifiers.control,
            shift: modifiers.shift,
            alt: modifiers.alt,
        };
        let key = match stroke.key.as_str() {
            "enter" => Key::Enter,
            "tab" => Key::Tab,
            "backspace" => Key::Backspace,
            "delete" => Key::Delete,
            "escape" => Key::Esc,
            "left" => Key::Left,
            "right" => Key::Right,
            "up" => Key::Up,
            "down" => Key::Down,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "home" => Key::Home,
            "end" => Key::End,
            _ if mods.ctrl => match stroke.key.chars().next() {
                Some(c) if stroke.key.chars().count() == 1 => Key::Char(c),
                _ => return,
            },
            // Typed text: key_char carries shift and layout (e.g. "A", "ß").
            _ => match stroke.key_char.as_deref().and_then(|s| s.chars().next()) {
                Some(c) if !c.is_control() => Key::Char(c),
                _ => return,
            },
        };
        if mods.ctrl && key == Key::Char('v') {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.chat.paste(&text);
            }
        } else {
            let outcome = self.chat.key(key, mods);
            self.apply(outcome, cx);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn apply(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        match outcome {
            Outcome::Quit => cx.quit(),
            Outcome::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Outcome::Redraw => {}
        }
    }

    /// Window position → grid cell of the last frame.
    fn cell_at(&self, x: Pixels, y: Pixels) -> (usize, usize) {
        let col = ((f32::from(x) - PAD) / f32::from(self.cell)).max(0.) as usize;
        let row = ((f32::from(y) - PAD) / LINE_HEIGHT).max(0.) as usize;
        (row, col)
    }

    fn on_click(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (row, col) = self.cell_at(event.position.x, event.position.y);
        let mods = Mods {
            ctrl: event.modifiers.control,
            shift: event.modifiers.shift,
            alt: event.modifiers.alt,
        };
        let outcome = self.chat.click(row, col, event.click_count, mods);
        self.apply(outcome, cx);
        cx.notify();
    }

    fn on_drag(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button == Some(MouseButton::Left) {
            let (row, col) = self.cell_at(event.position.x, event.position.y);
            if self.chat.drag(row, col) {
                cx.notify();
            }
        }
    }

    /// Selecting text puts it on the primary selection (middle-click paste),
    /// like every Linux app.
    fn on_release(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.chat.release() {
            cx.write_to_primary(ClipboardItem::new_string(text));
        }
    }

    /// Middle click: paste the primary selection, like a terminal.
    fn on_middle(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_primary().and_then(|item| item.text()) {
            self.chat.paste(&text);
            cx.notify();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let rows = match event.delta {
            ScrollDelta::Lines(lines) => lines.y * 3.,
            ScrollDelta::Pixels(pixels) => f32::from(pixels.y) / LINE_HEIGHT,
        } + self.scroll_rest;
        let whole = rows.trunc();
        self.scroll_rest = rows - whole;
        if whole != 0. {
            // Wheel up (positive y) shows older rows.
            self.chat.scroll_by(whole as isize);
            cx.notify();
        }
    }
}

fn color(value: u32) -> Hsla {
    rgb(value).into()
}

/// One grid row as a single shaped text with per-run styles.
fn row(cells: Row, cursor: Cursor) -> impl IntoElement {
    let mut text = String::new();
    let mut runs = Vec::with_capacity(cells.len());
    for cell in cells {
        if cell.text.is_empty() {
            continue;
        }
        runs.push(text_run(&cell.text, cell.paint));
        text.push_str(&cell.text);
    }
    if text.is_empty() {
        text.push(' ');
        runs.push(text_run(" ", Paint::fg(NIGHT.text)));
    }
    div()
        .h(px(LINE_HEIGHT))
        .whitespace_nowrap()
        .when(cursor == Cursor::Pointer, |row| row.cursor_pointer())
        .when(cursor == Cursor::Text, |row| row.cursor_text())
        .child(StyledText::new(SharedString::from(text)).with_runs(runs))
}

fn text_run(text: &str, paint: Paint) -> TextRun {
    TextRun {
        len: text.len(),
        font: Font {
            weight: if paint.bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if paint.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            ..font(FONT)
        },
        color: color(paint.fg),
        background_color: paint.bg.map(color),
        underline: paint.underline.then(|| UnderlineStyle {
            thickness: px(1.),
            color: Some(color(paint.fg)),
            wavy: false,
        }),
        strikethrough: paint.strike.then(|| StrikethroughStyle {
            thickness: px(1.),
            color: Some(color(paint.fg)),
        }),
    }
}

fn picker(view: PickerView, cx: &mut Context<Root>) -> impl IntoElement {
    let query = if view.query.is_empty() {
        div().text_color(color(NIGHT.dim)).child("type to search")
    } else {
        div()
            .text_color(color(NIGHT.text))
            .child(SharedString::from(view.query))
    };
    let empty = view.items.is_empty();
    let items = view.items.into_iter().map(|(index, label, selected)| {
        let item = div()
            .id(("session", index))
            .flex()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(color(NIGHT.surface)))
            .on_click(cx.listener(move |root, _, _, cx| {
                root.chat.pick(index);
                cx.notify();
            }));
        if selected {
            item.bg(color(NIGHT.surface))
                .child(div().text_color(color(NIGHT.accent)).child("▌ "))
                .child(
                    div()
                        .text_color(color(NIGHT.text))
                        .font_weight(FontWeight::BOLD)
                        .child(SharedString::from(label)),
                )
        } else {
            item.text_color(color(NIGHT.dim))
                .child(SharedString::from(format!("  {label}")))
        }
    });
    div()
        .absolute()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id("picker")
                .occlude()
                // Clicks inside the popup must not reach the grid (which closes it).
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .w(px(520.))
                .bg(color(NIGHT.bg))
                .border_1()
                .border_color(color(NIGHT.dim))
                .rounded_md()
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .justify_center()
                        .text_color(color(NIGHT.dim))
                        .child(SharedString::from(view.title)),
                )
                .child(
                    div()
                        .flex()
                        .child(div().text_color(color(NIGHT.accent)).child("❯ "))
                        .child(query),
                )
                .when(empty, |el| {
                    el.child(
                        div()
                            .text_color(color(NIGHT.dim))
                            .child("no match · esc closes"),
                    )
                })
                .children(items),
        )
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus.is_focused(window) {
            window.focus(&self.focus, cx);
        }
        // The window is a character grid: same cols × rows the terminal gets.
        let viewport = window.viewport_size();
        let cols = (f32::from(viewport.width) - 2. * PAD) / f32::from(self.cell);
        let rows = (f32::from(viewport.height) - 2. * PAD) / LINE_HEIGHT;
        let frame = self
            .chat
            .frame(cols.max(10.) as usize, rows.max(4.) as usize);

        div()
            .size_full()
            .bg(color(NIGHT.bg))
            .p(px(PAD))
            .font_family(FONT)
            .text_size(px(FONT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .text_color(color(NIGHT.text))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_click))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle))
            .on_mouse_move(cx.listener(Self::on_drag))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_release))
            .child(
                div().flex().flex_col().children(
                    frame
                        .rows
                        .into_iter()
                        .zip(frame.cursors)
                        .map(|(cells, cursor)| row(cells, cursor)),
                ),
            )
            .children(frame.picker.map(|view| picker(view, cx)))
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(640.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Root::new(window, cx)),
        );
        if let Err(err) = opened {
            eprintln!("smash: cannot open a window: {err}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}
