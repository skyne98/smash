//! Terminal frontend. The chat is the screen; menus are invoked, never docked.
//! All state shapes come from `smash_ui` so the gpui client behaves identically.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph};
use smash_ai::{Event as AiEvent, Message, Model};
use smash_backend::{Agent, Files};
use smash_client::Client;
use smash_core::{Action, View};
use smash_ui::markdown::Ink;
use smash_ui::{Clock, Item, Mode, Picker, Role, hints, spinner_word, transcript};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const THEME: smash_ui::Theme = smash_ui::NIGHT;
const ACCENT: Color = rgb(THEME.accent);
const DIM: Color = rgb(THEME.dim);
const TEXT: Color = rgb(THEME.text);
const SURFACE: Color = rgb(THEME.surface);
const BG: Color = rgb(THEME.bg);
const CODE: Color = Color::Rgb(214, 186, 130);

const fn rgb(color: u32) -> Color {
    Color::Rgb((color >> 16) as u8, (color >> 8) as u8, color as u8)
}

enum Update {
    Delta(String),
    Done { cost: f64 },
    Failed(String),
}

struct Worker {
    prompts: Sender<String>,
    updates: Receiver<Update>,
}

impl Worker {
    fn spawn(model: Model) -> Self {
        let (prompts, inbox) = mpsc::channel::<String>();
        let (outbox, updates) = mpsc::channel::<Update>();

        thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let mut messages: Vec<Message> = Vec::new();
            let mut agent = Agent::new(model).system("You are a terse coding assistant.");
            while let Ok(prompt) = inbox.recv() {
                messages.push(Message::user(prompt));
                let result = runtime.block_on(async {
                    agent
                        .run(&mut messages, |event| {
                            if let AiEvent::TextDelta { delta, .. } = event {
                                let _ = outbox.send(Update::Delta(delta.clone()));
                            }
                        })
                        .await
                });
                let update = match result {
                    Ok(run) => Update::Done {
                        cost: run.cost.total,
                    },
                    Err(err) => Update::Failed(err.to_string()),
                };
                if outbox.send(update).is_err() {
                    break;
                }
            }
        });

        Self { prompts, updates }
    }
}

struct App {
    client: Client,
    worker: Worker,
    input: String,
    streaming: String,
    cost: f64,
    status: String,
    /// One-shot message on the status row (errors, "copied"); next key clears it.
    notice: Option<String>,
    mode: Mode,
    picker: Picker,
    model: String,
    clock: Clock,
    output_chars: usize,
    /// Bumped whenever the stored transcript changes, so markdown is parsed
    /// once per change instead of once per frame.
    version: u64,
    cache: Option<Cache>,
    /// Redraw only when something changed; an idle chat costs zero frames.
    dirty: bool,
    /// Lines scrolled up from the bottom, plus the limits known at last draw.
    scroll: u16,
    max_scroll: u16,
    viewport: u16,
}

struct Cache {
    version: u64,
    width: u16,
    lines: Vec<Line<'static>>,
}

fn model() -> (Model, String) {
    match std::env::var("SMASH_MODEL") {
        Ok(spec) if spec.starts_with("anthropic:") => (Model::anthropic(&spec[10..]), spec),
        Ok(spec) if spec.starts_with("openai:") => (Model::openai(&spec[7..]), spec),
        Ok(spec) if !spec.is_empty() => (Model::deepseek(&spec), spec),
        _ => (Model::deepseek("deepseek-chat"), "deepseek-chat".into()),
    }
}

fn main() -> std::io::Result<()> {
    let dir = Files::default_dir();
    let ext = smash_ext::Ext::load_dir(dir.join("extensions"));
    let hooks: Box<dyn smash_core::Hooks> =
        match smash_session::Session::create(dir.join("sessions")) {
            Ok(session) => Box::new((session, ext)),
            Err(_) => Box::new(ext),
        };
    let mut client = Client::with_hooks(Files::new(dir), hooks);
    client.start();

    if client.app().selected().is_none() {
        client.dispatch(Action::New);
    }

    let (model, label) = model();
    let mut app = App {
        client,
        worker: Worker::spawn(model),
        input: String::new(),
        streaming: String::new(),
        cost: 0.0,
        status: "ready".into(),
        notice: None,
        mode: Mode::Chat,
        picker: Picker::default(),
        model: label,
        clock: Clock::default(),
        output_chars: 0,
        version: 0,
        cache: None,
        dirty: true,
        scroll: 0,
        max_scroll: 0,
        viewport: 0,
    };

    let mut terminal = ratatui::init();
    // Alternate scroll mode: the wheel arrives as ↑/↓ keys, and mouse
    // selection still works (mouse capture would break copy/paste).
    print!("\x1b[?1007h");
    smash_ui::highlight::warm();
    let result = run(&mut terminal, &mut app);
    print!("\x1b[?1007l");
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    loop {
        // Idle: no parse, no clone, no draw. Generating: animate every tick.
        if app.dirty || app.clock.generating() {
            terminal.draw(|frame| draw(frame, app))?;
            app.dirty = false;
        }

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                CtEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    if !handle_key(app, key.code, key.modifiers) {
                        return Ok(());
                    }
                }
                // Terminal resize: the layout and the markdown cache both depend
                // on width, so force a full redraw.
                CtEvent::Resize(_, _) => app.dirty = true,
                _ => {}
            }
        }

        while let Ok(update) = app.worker.updates.try_recv() {
            app.dirty = true;
            match update {
                Update::Delta(delta) => {
                    app.output_chars += delta.len();
                    app.streaming.push_str(&delta);
                }
                Update::Done { cost } => {
                    // Streams often end with newlines; store exactly one blank
                    // line between turns instead of stacking them.
                    let text = std::mem::take(&mut app.streaming);
                    let text = text.trim_end();
                    if !text.is_empty() {
                        dispatch(app, Action::Append(format!("{text}\n\n")));
                    }
                    let took = app
                        .clock
                        .finish()
                        .map(|took| took.as_secs_f64())
                        .unwrap_or_default();
                    app.cost += cost;
                    let tokens = app.output_chars.div_ceil(4);
                    app.status =
                        format!("done in {took:.1}s · ~{tokens} tok · {:.4} usd", app.cost);
                }
                Update::Failed(err) => {
                    app.streaming.clear();
                    app.status = friendly_error(&err);
                    app.notice = Some(app.status.clone());
                }
            }
        }
    }
}

/// Ctrl+Y: last code block → system clipboard via OSC 52 (works over SSH,
/// in kitty, alacritty, wezterm, foot; tmux needs `set -g set-clipboard on`).
fn copy_last_code(app: &mut App) {
    use base64::Engine;
    use std::io::Write;
    let body = app.client.view().transcript.join("\n");
    let Some(code) = smash_ui::markdown::last_code_block(&body) else {
        app.notice = Some("no code block to copy".into());
        return;
    };
    let payload = base64::engine::general_purpose::STANDARD.encode(&code);
    let mut out = std::io::stdout();
    let sent = write!(out, "\x1b]52;c;{payload}\x07").and_then(|()| out.flush());
    let lines = code.lines().count();
    app.notice = Some(match sent {
        Ok(()) => format!("✓ copied {lines} line{}", if lines == 1 { "" } else { "s" }),
        Err(err) => format!("copy failed: {err}"),
    });
}

/// Every action that can change the transcript goes through here.
fn dispatch(app: &mut App, action: Action) {
    app.client.dispatch(action);
    app.version += 1;
}

fn friendly_error(err: &str) -> String {
    let lower = err.to_lowercase();
    if lower.contains("api key") || lower.contains("api_key") || lower.contains("missingkey") {
        return "no api key · export DEEPSEEK_API_KEY, restart".into();
    }
    let first = err.lines().next().unwrap_or(err).trim();
    let short: String = first.chars().take(88).collect();
    format!("error · {short}")
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    app.dirty = true;
    app.notice = None;
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    if ctrl && code == KeyCode::Char('y') {
        copy_last_code(app);
        return true;
    }

    if ctrl && matches!(code, KeyCode::Char('q') | KeyCode::Char('c')) {
        return false;
    }
    if ctrl && code == KeyCode::Char('p') {
        open_picker(app);
        return true;
    }

    match app.mode {
        Mode::Picker => picker_key(app, code, ctrl),
        Mode::Chat => chat_key(app, code, ctrl),
    }
    true
}

fn open_picker(app: &mut App) {
    let (items, selected) = {
        let view = app.client.view();
        (
            view.rows
                .iter()
                .map(|row| Item {
                    id: row.id,
                    label: row.text.clone(),
                })
                .collect::<Vec<_>>(),
            view.rows.iter().position(|row| row.selected),
        )
    };
    app.picker.set_items(items);
    app.picker.clear();
    if let Some(index) = selected {
        app.picker.select(index);
    }
    app.mode = Mode::Picker;
}

fn picker_key(app: &mut App, code: KeyCode, ctrl: bool) {
    match code {
        KeyCode::Esc => app.mode = Mode::Chat,
        KeyCode::Up => app.picker.move_by(-1),
        KeyCode::Down => app.picker.move_by(1),
        KeyCode::Backspace => app.picker.backspace(),
        KeyCode::Enter => {
            if let Some(id) = app.picker.selected() {
                dispatch(app, Action::Select(id));
                app.streaming.clear();
                app.scroll = 0;
            }
            app.mode = Mode::Chat;
        }
        KeyCode::Char('n') if ctrl => {
            dispatch(app, Action::New);
            app.streaming.clear();
            app.mode = Mode::Chat;
        }
        KeyCode::Char(character) if !ctrl => app.picker.push(character),
        _ => {}
    }
}

fn chat_key(app: &mut App, code: KeyCode, ctrl: bool) {
    match code {
        KeyCode::Char('n') if ctrl => {
            dispatch(app, Action::New);
            app.streaming.clear();
            app.input.clear();
        }
        KeyCode::Char('w') if ctrl => {
            if let Some(id) = app.client.app().selected().map(|doc| doc.id) {
                dispatch(app, Action::Close(id));
                app.streaming.clear();
            }
        }
        KeyCode::Up => app.scroll = (app.scroll + 1).min(app.max_scroll),
        KeyCode::Down => app.scroll = app.scroll.saturating_sub(1),
        KeyCode::PageUp => app.scroll = (app.scroll + app.viewport.max(1)).min(app.max_scroll),
        KeyCode::PageDown => app.scroll = app.scroll.saturating_sub(app.viewport.max(1)),
        KeyCode::Home if ctrl => app.scroll = app.max_scroll,
        KeyCode::End if ctrl => app.scroll = 0,
        KeyCode::Esc => app.input.clear(),
        KeyCode::Enter => send(app),
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(character) if !ctrl => app.input.push(character),
        _ => {}
    }
}

fn send(app: &mut App) {
    let prompt = app.input.trim().to_string();
    if prompt.is_empty() {
        return;
    }
    dispatch(app, Action::Append(smash_ui::user_turn(&prompt)));
    if app.worker.prompts.send(prompt).is_err() {
        app.status = "agent thread stopped · restart the app".into();
        app.notice = Some(app.status.clone());
    }
    app.input.clear();
    app.streaming.clear();
    app.output_chars = 0;
    app.scroll = 0;
    app.clock.start();
    app.status = "generating…".into();
}

fn draw(frame: &mut Frame, app: &mut App) {
    let view = app.client.view();
    let area = frame.area();
    // Claude-Code grammar: transcript owns the screen, input is a boxed bar at
    // the bottom, one status line under it.
    let input = input_lines(app, area.width);
    let [main, rule_area, input_area, hint_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(input.len().clamp(1, 6) as u16),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_transcript(
        frame,
        main,
        &view,
        &mut app.cache,
        app.version,
        &app.streaming,
        app.clock.generating(),
        app.clock.running_secs(),
        app.scroll,
        &mut app.max_scroll,
        &mut app.viewport,
    );
    frame.render_widget(
        Paragraph::new("─".repeat(area.width as usize))
            .style(Style::new().fg(Color::Rgb(52, 54, 64))),
        rule_area,
    );
    let skip = input.len().saturating_sub(input_area.height as usize);
    frame.render_widget(Paragraph::new(input[skip..].to_vec()), input_area);
    draw_status(frame, hint_area, &view, app);

    if app.mode == Mode::Picker {
        draw_picker(frame, area, app);
    }
}

/// The prompt, wrapped with a hanging indent; the caret sits after the text.
fn input_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let prompt = vec![Span::styled("> ", Style::new().fg(ACCENT))];
    let body = if app.input.is_empty() {
        vec![Span::styled(
            placeholder(app).to_string(),
            Style::new().fg(DIM),
        )]
    } else {
        vec![
            Span::styled(app.input.clone(), Style::new().fg(TEXT)),
            Span::styled("▏", Style::new().fg(ACCENT)),
        ]
    };
    wrap(
        prompt,
        vec![Span::raw("  ")],
        body,
        width.saturating_sub(1) as usize,
        None,
    )
}

fn draw_status(frame: &mut Frame, area: Rect, view: &View, app: &App) {
    let typing = match app.mode {
        Mode::Picker => app.picker.query_typed(),
        Mode::Chat => !app.input.is_empty(),
    };
    let session = view
        .rows
        .iter()
        .find(|row| row.selected)
        .map(|row| row.text.clone())
        .unwrap_or_else(|| "no session".into());
    let clock = app.clock.label();
    // Narrow terminals keep the key hints; the model name goes first.
    let model = if area.width >= 72 {
        format!(" · {}", app.model)
    } else {
        String::new()
    };
    let right = if clock.is_empty() {
        format!("  │  {session}{model}")
    } else {
        format!("  │  {session}{model} · {clock}")
    };

    // Left and right share one row: truncate the hints, never overlap.
    let reserve = right.chars().count() + 2;
    let room = (area.width as usize).saturating_sub(reserve);
    let mut hint = hints(app.mode, typing, app.clock.generating()).to_string();
    // Contextual hint first: it is the one that changes, so it must survive truncation.
    if app.mode == Mode::Chat
        && !typing
        && view
            .transcript
            .iter()
            .any(|line| line.trim_start().starts_with("```"))
    {
        hint = format!("ctrl+y copy code · {hint}");
    }
    if let Some(notice) = &app.notice {
        let style = if notice.starts_with('✓') {
            ACCENT
        } else {
            CODE
        };
        frame.render_widget(
            Paragraph::new(notice.clone()).style(Style::new().fg(style)),
            area,
        );
        frame.render_widget(
            Paragraph::new(right)
                .alignment(Alignment::Right)
                .style(Style::new().fg(DIM)),
            area,
        );
        return;
    }
    let mut left = String::new();
    for item in hint.split(" · ") {
        let next = if left.is_empty() {
            item.to_string()
        } else {
            format!("{left} · {item}")
        };
        if next.chars().count() > room {
            break;
        }
        left = next;
    }
    frame.render_widget(Paragraph::new(left).style(Style::new().fg(DIM)), area);
    frame.render_widget(
        Paragraph::new(right)
            .alignment(Alignment::Right)
            .style(Style::new().fg(DIM)),
        area,
    );
}

fn placeholder(_app: &App) -> &'static str {
    "type a message"
}

fn draw_transcript(
    frame: &mut Frame,
    area: Rect,
    view: &View,
    cache: &mut Option<Cache>,
    version: u64,
    streaming: &str,
    generating: bool,
    elapsed: Option<f64>,
    scroll: u16,
    max_scroll: &mut u16,
    viewport: &mut u16,
) {
    // Same left axis as the input; one column of air on the right.
    let area = Rect {
        width: area.width.saturating_sub(1),
        ..area
    };

    let fresh = cache
        .as_ref()
        .is_some_and(|cache| cache.version == version && cache.width == area.width);
    if !fresh {
        *cache = Some(Cache {
            version,
            width: area.width,
            lines: rendered_transcript(view, area.width),
        });
    }
    let mut lines = cache
        .as_ref()
        .map(|cache| cache.lines.clone())
        .unwrap_or_default();

    if !streaming.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(assistant_lines(streaming, area.width));
    } else if generating {
        let elapsed = elapsed.unwrap_or_default();
        lines.push(Line::from(Span::styled(
            format!("✻ {}… ({elapsed:.0}s)", spinner_word(elapsed)),
            Style::new().fg(DIM),
        )));
    }

    // History grows upward from the input line: no void between chat and prompt.
    let height = area.height as usize;
    // Lines are pre-wrapped, so one line is exactly one row.
    let rows = lines.len();
    if rows < height {
        let mut padded = vec![Line::default(); height - rows];
        padded.append(&mut lines);
        lines = padded;
    }
    let rows = rows.max(height);
    let over = rows.saturating_sub(height) as u16;
    *max_scroll = over;
    *viewport = height as u16;
    // scroll == 0 means "pinned to the newest line".
    let top = over.saturating_sub(scroll.min(over));

    frame.render_widget(Paragraph::new(Text::from(lines)).scroll((top, 0)), area);
}

/// The parsed transcript: markdown per assistant turn, banded user prompts,
/// one blank row between turns.
fn rendered_transcript(view: &View, width: u16) -> Vec<Line<'static>> {
    let parsed = transcript(&view.transcript);
    let width = width as usize;
    let band = Style::new().fg(TEXT).bg(SURFACE);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(parsed.len() * 4);
    let mut index = 0;
    while index < parsed.len() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        match parsed[index].role {
            Role::User => {
                for (row, text) in parsed[index].text.split('\n').enumerate() {
                    let first = Span::styled(if row == 0 { "> " } else { "  " }, band.fg(DIM));
                    lines.extend(wrap(
                        vec![first],
                        vec![Span::styled("  ", band)],
                        vec![Span::styled(text.to_string(), band)],
                        width,
                        Some(band),
                    ));
                }
                index += 1;
            }
            Role::Assistant => {
                let mut text = String::new();
                while index < parsed.len() && parsed[index].role == Role::Assistant {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&parsed[index].text);
                    index += 1;
                }
                lines.extend(assistant_lines(&text, width as u16));
            }
        }
    }
    lines
}

/// Word-wrap styled spans to `width` columns. Row one starts with `first`,
/// later rows with `hang` so wrapped text keeps its column. `fill` pads each
/// row to the full width (bands for prompts and code).
fn wrap(
    first: Vec<Span<'static>>,
    hang: Vec<Span<'static>>,
    body: Vec<Span<'static>>,
    width: usize,
    fill: Option<Style>,
) -> Vec<Line<'static>> {
    fn finish(
        row: &mut Vec<Span<'static>>,
        col: usize,
        width: usize,
        fill: Option<Style>,
        rows: &mut Vec<Line<'static>>,
    ) {
        if let Some(style) = fill
            && col < width
        {
            row.push(Span::styled(" ".repeat(width - col), style));
        }
        rows.push(Line::from(std::mem::take(row)));
    }
    let width = width.max(8);
    let lead = |spans: &[Span<'static>]| spans.iter().map(Span::width).sum::<usize>();
    let mut rows = Vec::new();
    let mut col = lead(&first);
    let mut row = first;
    let mut empty = true;
    for span in body {
        let style = span.style;
        for word in span.content.split_inclusive(' ') {
            let mut rest = word;
            while !rest.is_empty() {
                if col + rest.trim_end().width() <= width {
                    col += rest.width();
                    row.push(Span::styled(rest.to_string(), style));
                    empty = false;
                    break;
                }
                if !empty {
                    finish(&mut row, col, width, fill, &mut rows);
                    row = hang.clone();
                    col = lead(&row);
                    empty = true;
                    rest = rest.trim_start();
                    continue;
                }
                // One word wider than the row: hard split it.
                let room = width.saturating_sub(col).max(1);
                let mut taken = 0;
                let mut end = 0;
                for (at, ch) in rest.char_indices() {
                    let w = ch.width().unwrap_or(0);
                    if taken + w > room && end > 0 {
                        break;
                    }
                    taken += w;
                    end = at + ch.len_utf8();
                }
                col += taken;
                row.push(Span::styled(rest[..end].to_string(), style));
                empty = false;
                rest = &rest[end..];
            }
        }
    }
    finish(&mut row, col, width, fill, &mut rows);
    rows
}

fn ink_style(ink: Ink) -> Style {
    match ink {
        Ink::Plain => Style::new().fg(TEXT),
        Ink::Bold => Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
        Ink::Italic => Style::new().fg(TEXT).add_modifier(Modifier::ITALIC),
        Ink::Strike => Style::new().fg(DIM).add_modifier(Modifier::CROSSED_OUT),
        Ink::Code => Style::new().fg(CODE).bg(SURFACE),
        Ink::Syntax(token) => Style::new().fg(rgb(token.color())),
        // Four visible tiers: underlined accent, accent, bright, dim.
        Ink::Heading(1) => Style::new()
            .fg(ACCENT)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        Ink::Heading(2) => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        Ink::Heading(3) => Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
        Ink::Heading(_) => Style::new().fg(DIM).add_modifier(Modifier::BOLD),
        Ink::Dim => Style::new().fg(DIM),
    }
}

/// Markdown block lines with the assistant marker on the first line only.
/// Markdown blocks laid out on the content column (2), marker on row one.
fn assistant_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let width = width as usize;
    let mut lines = Vec::new();
    let blocks = smash_ui::markdown::render(text, width.saturating_sub(4).max(8));
    let mut skip_to = 0;
    for (index, block) in blocks.iter().cloned().enumerate() {
        if index < skip_to {
            continue;
        }
        if block.code {
            let end = blocks[index..]
                .iter()
                .position(|block| !block.code)
                .map_or(blocks.len(), |offset| index + offset);
            lines.extend(code_panel(&blocks[index..end], width, index == 0));
            skip_to = end;
            continue;
        }
        if block.spans.is_empty() {
            lines.push(Line::default());
            continue;
        }
        let pad = " ".repeat(block.indent);
        let marker = if index == 0 {
            Span::styled("● ", Style::new().fg(ACCENT))
        } else {
            Span::raw("  ")
        };
        // Wrapped list items hang under their text, not under the bullet.
        let bullet = match block.spans.first() {
            Some(span)
                if span.ink == Ink::Dim && span.text.ends_with(' ') && span.text.len() <= 5 =>
            {
                span.text.width()
            }
            _ => 0,
        };
        // Quote bars repeat on wrapped rows; bullets and numbers do not.
        let quote: Vec<Span<'static>> = block
            .spans
            .iter()
            .take_while(|span| span.ink == Ink::Dim && span.text == "│ ")
            .map(|span| Span::styled(span.text.clone(), ink_style(span.ink)))
            .collect();
        let hang = if quote.is_empty() {
            vec![Span::raw(" ".repeat(2 + block.indent + bullet))]
        } else {
            let mut hang = vec![Span::raw(" ".repeat(2 + block.indent))];
            hang.extend(quote);
            hang
        };
        let body = block
            .spans
            .into_iter()
            .map(|span| Span::styled(span.text, ink_style(span.ink)))
            .collect();
        lines.extend(wrap(vec![marker, Span::raw(pad)], hang, body, width, None));
    }
    lines
}

/// One fenced block as a panel: language label row, then numbered lines (3+). Wrapped lines hang under the code, not the numbers.
fn code_panel(rows: &[smash_ui::markdown::Block], width: usize, first: bool) -> Vec<Line<'static>> {
    let surface = Style::new().bg(SURFACE);
    let is_bar = |span: &smash_ui::markdown::Span| span.ink == Ink::Dim && span.text == "│ ";
    let bars = rows[0].spans.iter().take_while(|span| is_bar(span)).count();
    let marker = if first {
        Span::styled("● ", Style::new().fg(ACCENT))
    } else {
        Span::raw("  ")
    };
    // Everything left of the panel: answer marker, list indent, quote bars.
    let mut outside = vec![marker, Span::raw(" ".repeat(rows[0].indent))];
    outside.extend((0..bars).map(|_| Span::styled("│ ", ink_style(Ink::Dim))));
    let blank: Vec<Span<'static>> = outside
        .iter()
        .map(|span| {
            if span.content == "● " {
                Span::raw("  ")
            } else {
                span.clone()
            }
        })
        .collect();

    let label: String = rows[0].spans[bars..]
        .iter()
        .map(|span| span.text.as_str())
        .collect();
    let code = &rows[1..];
    let numbered = code.len() >= 3;
    let digits = if numbered {
        code.len().to_string().len()
    } else {
        0
    };
    let number = Style::new().fg(Color::Rgb(0x5c, 0x60, 0x70)).bg(SURFACE);

    let mut lines = Vec::with_capacity(code.len() + 2);
    let mut head = outside;
    head.push(Span::styled(
        format!(" {label}"),
        surface.fg(DIM).add_modifier(Modifier::ITALIC),
    ));
    lines.extend(wrap(head, blank.clone(), Vec::new(), width, Some(surface)));
    for (row, block) in code.iter().enumerate() {
        let mut first = blank.clone();
        let mut hang = blank.clone();
        if numbered {
            first.push(Span::styled(format!(" {:>digits$}  ", row + 1), number));
            hang.push(Span::styled(format!(" {}  ", " ".repeat(digits)), number));
        } else {
            first.push(Span::styled("  ", surface));
            hang.push(Span::styled("  ", surface));
        }
        let body = block.spans[bars.min(block.spans.len())..]
            .iter()
            .map(|span| Span::styled(span.text.clone(), ink_style(span.ink).bg(SURFACE)))
            .collect();
        lines.extend(wrap(first, hang, body, width, Some(surface)));
    }
    lines
}

fn draw_picker(frame: &mut Frame, area: Rect, app: &App) {
    let height = (5u16.min(app.picker.len().max(1) as u16) + 3).min(area.height.saturating_sub(4));
    let width = 64.min(area.width.saturating_sub(8));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let block = Block::bordered()
        .title(format!(" sessions {} ", app.picker.counter()))
        .title_alignment(Alignment::Center)
        .border_style(Style::new().fg(DIM));
    let inner = block.inner(popup);
    frame.render_widget(block.style(Style::new().bg(BG)), popup);

    let [query_row, list_row] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);

    let query = if app.picker.query_typed() {
        Line::from(vec![
            Span::styled("❯ ", Style::new().fg(ACCENT)),
            Span::styled(app.picker.query.clone(), Style::new().fg(TEXT)),
        ])
    } else {
        Line::from(vec![
            Span::styled("❯ ", Style::new().fg(ACCENT)),
            Span::styled("type to search", Style::new().fg(DIM)),
        ])
    };
    frame.render_widget(Paragraph::new(query), query_row);
    frame.set_cursor_position((
        (query_row.x + 2 + app.picker.query.len() as u16).min(query_row.right().saturating_sub(1)),
        query_row.y,
    ));

    let visible = app.picker.visible(list_row.height as usize);
    if visible.is_empty() {
        frame.render_widget(
            Paragraph::new("no match · esc closes").style(Style::new().fg(DIM)),
            list_row,
        );
        return;
    }
    let items: Vec<ListItem> = visible
        .iter()
        .map(|(index, item)| {
            if *index == app.picker.index() {
                ListItem::new(Line::from(vec![
                    Span::styled("▌ ", Style::new().fg(ACCENT)),
                    Span::styled(
                        item.label.clone(),
                        Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                ]))
                .style(Style::new().bg(SURFACE))
            } else {
                ListItem::new(format!("  {}", item.label)).style(Style::new().fg(DIM))
            }
        })
        .collect();
    frame.render_widget(List::new(items), list_row);
}
