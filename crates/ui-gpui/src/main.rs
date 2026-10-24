//! gpui client: translates gpui key/scroll events into `smash_ui::chat::Key`
//! and paints the rows `Chat::frame` lays out, in a monospace grid. All
//! behaviour lives in smash-ui, shared with the terminal client.

use std::ops::Range;
use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, ExternalPaths, FocusHandle, Font,
    FontStyle, FontWeight, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString,
    StrikethroughStyle, StyledText, Task, TextRun, UnderlineStyle, Window, WindowBounds,
    WindowOptions, div, font, img, prelude::*, px, rgb, size,
};
use gpui_platform::application;
use smash_ui::NIGHT;
use smash_ui::chat::{Chat, Cursor, Key, Mods, Outcome};
use smash_ui::layout::{self, Bar, Paint, Row};
use smash_ui::popup::Popup;

const FONT: &str = "JetBrainsMono Nerd Font";
/// Base metrics at zoom 1; everything scales with `Root::zoom`.
const FONT_SIZE: f32 = 14.;
const LINE_HEIGHT: f32 = 20.;
const PAD: f32 = 16.;
const ZOOM_STEP: f32 = 1.1;

struct Root {
    chat: Chat,
    focus: FocusHandle,
    /// Width of one monospace cell at zoom 1, measured from the font.
    base_cell: Pixels,
    /// ctrl+= / ctrl+- / ctrl+0; saved across runs.
    zoom: f32,
    _tick: Task<()>,
}

/// The zoom level kept in the data dir (one number).
fn load_zoom(dir: &std::path::Path) -> f32 {
    std::fs::read_to_string(dir.join("zoom"))
        .ok()
        .and_then(|text| text.trim().parse::<f32>().ok())
        .filter(|zoom| (0.5..=4.0).contains(zoom))
        .unwrap_or(1.0)
}

impl Root {
    fn cell(&self) -> Pixels {
        self.base_cell * self.zoom
    }

    fn line(&self) -> f32 {
        (LINE_HEIGHT * self.zoom).round()
    }

    fn pad(&self) -> f32 {
        PAD * self.zoom
    }

    /// Ctrl+V: images and copied files attach, text is typed.
    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let mut attached = false;
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    self.chat.attach_image(image.bytes.clone(), "clipboard");
                    attached = true;
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    for path in paths.paths() {
                        self.chat.attach_file(path.clone());
                    }
                    attached = true;
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if !attached && let Some(text) = item.text() {
            self.chat.paste(&text);
        }
    }

    /// Files dropped on the window: images attach, others are named in the
    /// prompt (as a terminal would paste them).
    fn on_drop(&mut self, paths: &ExternalPaths, _: &mut Window, cx: &mut Context<Self>) {
        let text: Vec<String> = paths
            .paths()
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        self.chat.paste(&text.join("\n"));
        cx.notify();
    }

    /// Change the zoom (1.0 = default), keep it for next time.
    fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(0.5, 4.0);
        let _ = std::fs::write(
            self.chat.data_dir().join("zoom"),
            format!("{:.3}", self.zoom),
        );
        self.chat
            .notice(format!("✓ zoom {:.0}% · ctrl+0 resets", self.zoom * 100.));
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = window.text_system();
        let id = text.resolve_font(&font(FONT));
        let base_cell = text
            .advance(id, px(FONT_SIZE), 'm')
            .map(|size| size.width)
            .unwrap_or(px(FONT_SIZE * 0.6));
        // Poll agent output and run motions: one display frame (16 ms) while
        // something moves, a slow tick otherwise; idle frames cost nothing.
        let tick = cx.spawn(async move |this, cx| {
            let mut wait = Duration::from_millis(50);
            loop {
                cx.background_executor().timer(wait).await;
                let alive = this.update(cx, |root, cx| {
                    if root.chat.poll() || root.chat.animating() {
                        cx.notify();
                    }
                    root.chat.frame_interval()
                });
                match alive {
                    Ok(next) => wait = next,
                    Err(_) => break,
                }
            }
        });
        let chat = Chat::open()
            .with_mouse()
            .with_pixels()
            .with_startup_prompt();
        let zoom = load_zoom(chat.data_dir());
        Self {
            chat,
            focus: cx.focus_handle(),
            base_cell,
            zoom,
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
        let zoom = match key {
            Key::Char('=' | '+') if mods.ctrl => Some(self.zoom * ZOOM_STEP),
            Key::Char('-') if mods.ctrl => Some(self.zoom / ZOOM_STEP),
            Key::Char('0') if mods.ctrl => Some(1.0),
            _ => None,
        };
        if let Some(zoom) = zoom {
            self.set_zoom(zoom);
        } else if mods.ctrl && key == Key::Char('v') {
            self.paste_clipboard(cx);
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
            Outcome::Open(path) => cx.open_with_system(&path),
            Outcome::Redraw => {}
        }
    }

    /// Window position → grid cell of the last frame.
    fn cell_at(&self, x: Pixels, y: Pixels) -> (usize, usize) {
        let col = ((f32::from(x) - self.pad()) / f32::from(self.cell())).max(0.) as usize;
        let row = ((f32::from(y) - self.pad()) / self.line()).max(0.) as usize;
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
        // In rows, as the system reports them: a wheel notch already comes
        // as its scroll-lines setting (3), a touchpad as pixels.
        let rows = match event.delta {
            ScrollDelta::Lines(lines) => lines.y,
            ScrollDelta::Pixels(pixels) => f32::from(pixels.y) / self.line(),
        };
        self.chat.wheel(rows);
        cx.notify();
    }
}

/// Grid metrics at the current zoom, for the free painting functions.
#[derive(Clone, Copy)]
struct Metrics {
    cell: Pixels,
    line: f32,
    pad: f32,
}

fn color(value: u32) -> Hsla {
    rgb(value).into()
}

/// One grid row as a single shaped text with per-run styles.
/// One grid row, split into parts pinned to the cell grid: a part spans
/// exactly its columns even when a fallback font (CJK, emoji) is wider or
/// narrower than its cells, so the scrollbar and click zones never drift.
fn row(cells: Row, zones: &[(Range<usize>, Cursor)], m: Metrics) -> impl IntoElement + use<> {
    let cell = m.cell;
    let total = layout::width(&cells);
    let mut parts = Vec::new();
    let mut col = 0;
    for (range, cursor) in zones {
        if range.start > col {
            parts.push((col..range.start, Cursor::Default));
        }
        parts.push((range.clone(), *cursor));
        col = range.end;
    }
    if col < total {
        parts.push((col..total, Cursor::Default));
    }
    let line = div().h(px(m.line)).whitespace_nowrap().flex();
    if parts.is_empty() {
        return line.child(styled(cells));
    }
    let last = parts.len() - 1;
    line.children(parts.into_iter().enumerate().map(|(n, (range, cursor))| {
        div()
            .flex_none()
            .overflow_hidden()
            .w(cell * range.len() as f32)
            // The last part reaches the window edge (clicks past the text).
            .when(n == last, |part| part.flex_1())
            .when(cursor == Cursor::Pointer, |part| part.cursor_pointer())
            .when(cursor == Cursor::Text, |part| part.cursor_text())
            .child(styled(layout::slice(&cells, range)))
    }))
}

fn styled(cells: Row) -> StyledText {
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
    StyledText::new(SharedString::from(text)).with_runs(runs)
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

/// One picture, placed on its cells inside the transcript box. A picture cut
/// by the top edge keeps its full size and slides up under the clip.
fn picture(shown: &smash_ui::chat::Shown, shift: Pixels, m: Metrics) -> impl IntoElement {
    let p = &shown.picture;
    let top = px((p.row as f32 - shown.skip as f32) * m.line) + shift;
    div()
        .absolute()
        .left(m.cell * p.col as f32)
        .top(top)
        .w(m.cell * p.cols as f32)
        .h(px(p.rows as f32 * m.line))
        .bg(color(NIGHT.bg))
        .overflow_hidden()
        .opacity(shown.fade)
        .child(img(p.png.clone()).size_full().object_fit(ObjectFit::Fill))
}

/// Scrollbar overlay: faint rounded track, rounded thumb, centered in its
/// cell column. Not interactive itself; clicks land on the grid underneath.
fn scrollbar(bar: Bar, m: Metrics) -> impl IntoElement {
    scrollbar_at(bar, m, (m.pad, m.pad))
}

/// A scrollbar on a grid whose first cell is at `origin` (px) in its parent.
fn scrollbar_at(bar: Bar, m: Metrics, origin: (f32, f32)) -> impl IntoElement {
    let (cell, line) = (m.cell, m.line);
    let rows = |range: std::ops::Range<usize>| px(range.len() as f32 * line);
    let inset = px(2.);
    div()
        .absolute()
        .left(px(origin.0) + cell * bar.col as f32 + (cell - px(6.)) / 2.)
        .top(px(origin.1 + bar.rows.start as f32 * line) + inset)
        .w(px(6.))
        .h(rows(bar.rows.clone()) - inset * 2.)
        .rounded_full()
        .bg(color(NIGHT.surface))
        .child(
            div()
                .absolute()
                .left_0()
                .top(px((bar.thumb.start - bar.rows.start) as f32 * line))
                .w_full()
                .h(rows(bar.thumb.clone()))
                .rounded_full()
                .bg(color(NIGHT.dim)),
        )
}

/// Any popup: centered bordered overlay; rows painted like the main grid,
/// item rows clickable with hover. Clicks inside never reach the grid.
fn popup(view: Popup, m: Metrics, cx: &mut Context<Root>) -> impl IntoElement {
    let cell = m.cell;
    let row_width = cell * view.width as f32;
    let open = view.open.clamp(0.0, 1.0);
    // Border and title arrive with the rows; the box grows to its width with
    // the spring (overshoot included), so it visibly pops.
    let border = smash_ui::motion::mix(NIGHT.bg, NIGHT.dim, open);
    let grow = (0.92 + 0.08 * view.open) as f32;
    // Its rows start under the title line; its column is past the rows, so
    // shift the origin to center the 6 px bar in the right padding.
    let edge = m.pad + (m.pad - f32::from(m.cell)) / 2.;
    let bar = view
        .bar
        .map(|bar| scrollbar_at(bar, m, (edge, m.pad / 2. + m.line)));
    let (window, shift) = (view.window, view.shift);
    let mut rows = view
        .rows
        .into_iter()
        .zip(view.items)
        .enumerate()
        .map(|(n, (cells, item))| {
            // Exactly the row's cells wide: hover and selection bands match.
            let line = div()
                .id(("popup-row", n))
                .w(row_width)
                .child(row(cells, &[], m));
            match item {
                Some(index) => line
                    .cursor_pointer()
                    .hover(|style| style.bg(color(NIGHT.surface)))
                    .on_click(cx.listener(move |root, _, _, cx| {
                        let outcome = root.chat.pick(index);
                        root.apply(outcome, cx);
                        cx.notify();
                    })),
                None => line,
            }
        });
    // The filter row stays; the items scroll in a clipped box under it.
    let field = rows.next();
    let items = div().h(px(window as f32 * m.line)).overflow_hidden().child(
        div()
            .flex()
            .flex_col()
            .mt(px(-shift * m.line))
            .children(rows),
    );
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id("popup")
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                // The box hides the window's own wheel handler: scroll here.
                .on_scroll_wheel(cx.listener(Root::on_scroll))
                .w((cell * view.width as f32 + px(2. * m.pad)) * grow)
                .overflow_hidden()
                .bg(color(NIGHT.bg))
                .border_1()
                .border_color(color(border))
                .rounded_md()
                .px(px(m.pad))
                .py(px(m.pad / 2.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .justify_center()
                        .text_color(color(border))
                        .child(SharedString::from(view.title)),
                )
                .children(field)
                .child(items)
                .children(bar),
        )
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus.is_focused(window) {
            window.focus(&self.focus, cx);
        }
        // The window is a character grid: same cols × rows the terminal gets.
        let viewport = window.viewport_size();
        let m = Metrics {
            cell: self.cell(),
            line: self.line(),
            pad: self.pad(),
        };
        let cols = (f32::from(viewport.width) - 2. * m.pad) / f32::from(m.cell);
        let rows = (f32::from(viewport.height) - 2. * m.pad) / m.line;
        let frame = self
            .chat
            .frame(cols.max(10.) as usize, rows.max(4.) as usize);

        // The transcript scrolls by pixels: its rows sit `shift` rows lower,
        // with the row above peeking in, clipped to the transcript area.
        let mut rows: Vec<_> = frame.rows.into_iter().zip(frame.cursors).collect();
        let below = rows.split_off(frame.viewport.min(rows.len()));
        let shift = px(frame.shift * m.line);
        // Real pictures over their half-block cells, in the same clip and
        // the same smooth-scroll shift, so they move and fade with the text.
        let pictures = frame
            .pictures
            .iter()
            .filter(|shown| shown.picture.row < frame.viewport)
            .map(|shown| picture(shown, shift, m));
        let transcript = div()
            .relative()
            .h(px(frame.viewport as f32 * m.line))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .mt(shift - px(m.line))
                    .child(row(frame.above.unwrap_or_default(), &[], m))
                    .children(rows.into_iter().map(|(cells, zones)| row(cells, &zones, m))),
            )
            .children(pictures);

        div()
            .size_full()
            .bg(color(NIGHT.bg))
            .p(px(m.pad))
            .font_family(FONT)
            .text_size(px(FONT_SIZE * self.zoom))
            .line_height(px(m.line))
            .text_color(color(NIGHT.text))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_click))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle))
            .on_mouse_move(cx.listener(Self::on_drag))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_release))
            .on_drop(cx.listener(Self::on_drop))
            .child(
                div().flex().flex_col().child(transcript).children(
                    below
                        .into_iter()
                        .map(|(cells, zones)| row(cells, &zones, m)),
                ),
            )
            .children(frame.bars.into_iter().map(|bar| scrollbar(bar, m)))
            .children(frame.popup.map(|view| popup(view, m, cx)))
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
