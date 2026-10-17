//! The chat controller both clients run. It owns the session client, the agent
//! worker thread, the prompt editor, scroll and the picker. Clients feed it
//! keys, clicks and pastes, and paint the [`Frame`] it returns; nothing here
//! knows about terminals or gpui.

use std::ops::Range;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use smash_ai::{Content, Event as AiEvent, Message, Model};
use smash_backend::{Agent, Files};
use smash_client::Client;
use smash_core::Action;

use crate::editor::Editor;
use crate::layout::{self, ACCENT, DIM, Laid, Paint, Row, run};
use crate::popup::{self, Popup};
use crate::{Clock, Item, Mode, NIGHT, Picker, hints, spinner_word};

/// Keys after the client has translated its native event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Delete,
    Esc,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// Modifier keys held with a key or click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// What the client must do after an input.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Redraw,
    Quit,
    /// Put this text on the system clipboard (each client has its own way).
    Copy(String),
}

/// Mouse cursor a client should show over a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    Default,
    /// Something clickable (copy button, session name).
    Pointer,
    /// Editable text (the prompt).
    Text,
}

/// One screen, top to bottom: transcript, rule, input, status. Every row is
/// at most `cols` cells wide and there are exactly `rows` of them.
pub struct Frame {
    pub rows: Vec<Row>,
    /// Per row: column ranges that get a non-default mouse cursor.
    pub cursors: Vec<Vec<(Range<usize>, Cursor)>>,
    /// Scrollbars (transcript, prompt), drawn over the last column.
    pub bars: Vec<layout::Bar>,
    /// Overlay (session picker), drawn centered over the rows.
    pub popup: Option<Popup>,
}

/// Matches shown at once in the session picker.
const PICKER_ROWS: usize = 5;

enum Update {
    Delta(String),
    Done { cost: f64 },
    Failed(String),
}

struct Worker {
    /// Full conversation to answer; the last message is the new prompt.
    prompts: Sender<Vec<Message>>,
    updates: Receiver<Update>,
}

impl Worker {
    /// The agent runs on its own thread with its own tokio runtime, so neither
    /// client's event loop ever blocks on the network.
    fn spawn(model: Model) -> Self {
        let (prompts, inbox) = mpsc::channel::<Vec<Message>>();
        let (outbox, updates) = mpsc::channel::<Update>();
        thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let mut agent = Agent::new(model).system("You are a terse coding assistant.");
            while let Ok(mut messages) = inbox.recv() {
                let result = runtime.block_on(agent.run(&mut messages, |event| {
                    if let AiEvent::TextDelta { delta, .. } = event {
                        let _ = outbox.send(Update::Delta(delta.clone()));
                    }
                }));
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

/// The session's stored transcript as model context: consecutive lines of
/// one role form one message. Built from the session itself, so the model
/// sees exactly this chat (also after a restart or a session switch).
// ponytail: sends the whole session every turn; trim old turns when chats
// outgrow the context window.
fn history(body: &[String]) -> Vec<Message> {
    let mut messages: Vec<Message> = Vec::new();
    let mut role = None;
    let mut text = String::new();
    let mut flush = |role: Option<crate::Role>, text: &mut String| {
        let body = text.trim().to_string();
        text.clear();
        match role {
            _ if body.is_empty() => {}
            Some(crate::Role::User) => messages.push(Message::user(body)),
            Some(crate::Role::Assistant) => {
                messages.push(Message::assistant(vec![Content::Text { text: body }]));
            }
            None => {}
        }
    };
    for line in crate::transcript(body) {
        if role != Some(line.role) {
            flush(role, &mut text);
            role = Some(line.role);
        }
        text.push_str(&line.text);
        text.push('\n');
    }
    flush(role, &mut text);
    messages
}

/// `SMASH_MODEL=anthropic:… | openai:… | <deepseek model>`, default deepseek-chat.
fn model() -> (Model, String) {
    match std::env::var("SMASH_MODEL") {
        Ok(spec) if spec.starts_with("anthropic:") => (Model::anthropic(&spec[10..]), spec),
        Ok(spec) if spec.starts_with("openai:") => (Model::openai(&spec[7..]), spec),
        Ok(spec) if !spec.is_empty() => (Model::deepseek(&spec), spec),
        _ => (Model::deepseek("deepseek-chat"), "deepseek-chat".into()),
    }
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

/// Laid-out stored transcript, rebuilt only when it or the width changes.
struct Cache {
    version: u64,
    width: usize,
    laid: Laid,
    has_code: bool,
}

/// What a click on a screen row does.
#[derive(Clone)]
enum Hit {
    Copy(String),
    /// Column ranges that act like pressing a key (status hints).
    Zones(Vec<Zone>),
    /// Prompt row, index into `Chat::input_map`.
    Input(usize),
}

/// A clickable column range and the key it presses.
type Zone = (Range<usize>, Key, Mods);

/// The key a hint like `ctrl+p sessions` or `esc close` names, if any.
/// Instructions (`type to filter`, `click copy`, `↑/↓ move`) are not clickable.
fn binding(hint: &str) -> Option<(Key, Mods)> {
    let combo = hint.split_whitespace().next()?;
    let mut mods = Mods::default();
    let mut key = None;
    for part in combo.split('+') {
        match part {
            "ctrl" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" => mods.alt = true,
            "enter" => key = Some(Key::Enter),
            "esc" => key = Some(Key::Esc),
            "↑" => key = Some(Key::PageUp),
            "↓" => key = Some(Key::PageDown),
            single if mods.ctrl && single.chars().count() == 1 => {
                key = single.chars().next().map(Key::Char)
            }
            _ => return None,
        }
    }
    Some((key?, mods))
}

/// A visible prompt row: text range and byte offset under each column.
struct InputSpot {
    screen_row: usize,
    start: usize,
    end: usize,
    cols: Vec<usize>,
}

/// Prompt rows shown at most; more scroll inside the box.
const INPUT_ROWS: usize = 6;

pub struct Chat {
    client: Client,
    worker: Worker,
    editor: Editor,
    streaming: String,
    cost: f64,
    /// One-shot message on the status row (errors, "copied"); next input clears it.
    notice: Option<String>,
    mode: Mode,
    picker: Picker,
    model: String,
    clock: Clock,
    output_chars: usize,
    /// Bumped on every transcript change so layout runs once per change.
    version: u64,
    cache: Option<Cache>,
    /// Rows scrolled up from the bottom; limits as of the last frame.
    scroll: usize,
    max_scroll: usize,
    viewport: usize,
    /// First visible prompt row when the prompt is taller than the box.
    input_top: usize,
    /// Client has a pointer: code headers show a "copy" target.
    mouse: bool,
    /// Click targets of the last frame, one per screen row.
    hits: Vec<Option<Hit>>,
    input_map: Vec<InputSpot>,
    /// A drag that started in the prompt is selecting text.
    dragging: bool,
    /// A drag that started on the transcript scrollbar.
    scrubbing: bool,
    /// Grid size and transcript length of the last frame (scrollbar hits).
    cols: usize,
    total: usize,
}

impl Chat {
    /// Open the default data dir: documents, session log, extensions, agent.
    pub fn open() -> Self {
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
        crate::highlight::warm();
        let (model, label) = model();
        Self {
            client,
            worker: Worker::spawn(model),
            editor: Editor::default(),
            streaming: String::new(),
            cost: 0.0,
            notice: None,
            mode: Mode::Chat,
            picker: Picker::default(),
            model: label,
            clock: Clock::default(),
            output_chars: 0,
            version: 0,
            cache: None,
            scroll: 0,
            max_scroll: 0,
            viewport: 0,
            input_top: 0,
            mouse: false,
            hits: Vec::new(),
            input_map: Vec::new(),
            dragging: false,
            scrubbing: false,
            cols: 0,
            total: 0,
        }
    }

    /// Pointer clients (gpui) call this once; enables click targets.
    pub fn with_mouse(mut self) -> Self {
        self.mouse = true;
        self.cache = None;
        self
    }

    /// True while an answer is in flight: clients redraw on a timer then.
    pub fn animating(&self) -> bool {
        self.clock.generating()
    }

    /// Show a one-shot message, e.g. when the client's clipboard write failed.
    pub fn notice(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }

    /// Selected prompt text, for the primary selection on Linux.
    pub fn selected_text(&self) -> Option<String> {
        self.editor.selected().map(String::from)
    }

    /// Positive = older content. Wheel and trackpad land here.
    pub fn scroll_by(&mut self, rows: isize) {
        self.scroll = self.scroll.saturating_add_signed(rows).min(self.max_scroll);
    }

    /// Byte offset in the prompt under (row, col) of a visible prompt row.
    fn offset(&self, spot: usize, col: usize) -> usize {
        let spot = &self.input_map[spot];
        match col.checked_sub(2) {
            None => spot.start,
            Some(col) => spot.cols.get(col).copied().unwrap_or(spot.end),
        }
    }

    /// Press at a cell of the last frame. `count` = 1, 2, 3 for single,
    /// double, triple click.
    pub fn click(&mut self, row: usize, col: usize, count: usize, mods: Mods) -> Outcome {
        self.notice = None;
        let hit = self.hits.get(row).cloned().flatten();
        // Status hints work in every mode: clicking one presses its key.
        if let Some(Hit::Zones(zones)) = &hit
            && let Some((_, key, mods)) = zones.iter().find(|(range, ..)| range.contains(&col))
        {
            return self.key(*key, *mods);
        }
        if self.mode == Mode::Picker {
            // Click outside the popup closes it, like Esc.
            self.mode = Mode::Chat;
            return Outcome::Redraw;
        }
        if self.on_scrollbar(row, col) {
            self.scrubbing = true;
            self.scrub(row);
            return Outcome::Redraw;
        }
        match hit {
            Some(Hit::Copy(code)) => return self.copied(code),
            Some(Hit::Input(spot)) => {
                let at = self.offset(spot, col);
                match count {
                    1 => self.editor.move_to(at, mods.shift),
                    2 => self.editor.select_word_at(at),
                    _ => self.editor.select_line_at(at),
                }
                self.dragging = true;
            }
            _ => {}
        }
        Outcome::Redraw
    }

    /// Last column of a transcript row, while the chat is scrollable.
    fn on_scrollbar(&self, row: usize, col: usize) -> bool {
        self.max_scroll > 0 && row < self.viewport && col + 1 >= self.cols
    }

    /// Scroll so the thumb sits under `row`.
    fn scrub(&mut self, row: usize) {
        let top = layout::scroll_top_at(self.total, self.viewport, row);
        self.scroll = self.max_scroll - top.min(self.max_scroll);
    }

    /// Mouse moved with the button held. True when the view changed.
    pub fn drag(&mut self, row: usize, col: usize) -> bool {
        if self.scrubbing {
            let before = self.scroll;
            self.scrub(row.min(self.viewport.saturating_sub(1)));
            return before != self.scroll;
        }
        let (Some(first), Some(last)) = (self.input_map.first(), self.input_map.last()) else {
            return false;
        };
        if !self.dragging {
            return false;
        }
        // Above or below the box: select to the edge row.
        let spot = row.clamp(first.screen_row, last.screen_row) - first.screen_row;
        let at = self.offset(spot, col);
        let before = (self.editor.cursor(), self.editor.selection());
        self.editor.move_to(at, true);
        before != (self.editor.cursor(), self.editor.selection())
    }

    /// Button released. Returns the selection, for the primary clipboard.
    pub fn release(&mut self) -> Option<String> {
        self.scrubbing = false;
        std::mem::take(&mut self.dragging)
            .then(|| self.selected_text())
            .flatten()
    }

    /// Open the picker entry with this match index (from [`Popup::items`]).
    pub fn pick(&mut self, index: usize) {
        self.picker.select(index);
        self.open_selected();
    }

    /// Insert text: into the prompt (replacing a selection), or the picker
    /// filter (one line).
    pub fn paste(&mut self, text: &str) {
        self.notice = None;
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        match self.mode {
            Mode::Picker => {
                for character in text.chars().filter(|c| !c.is_control()) {
                    self.picker.push(character);
                }
            }
            Mode::Chat => self.editor.insert(text.trim_end_matches('\n')),
        }
    }

    /// Drain agent output. Returns true when the screen changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(update) = self.worker.updates.try_recv() {
            changed = true;
            match update {
                Update::Delta(delta) => {
                    self.output_chars += delta.len();
                    self.streaming.push_str(&delta);
                }
                Update::Done { cost } => {
                    // Store exactly one blank line between turns.
                    let text = std::mem::take(&mut self.streaming);
                    let text = text.trim_end();
                    if !text.is_empty() {
                        self.dispatch(Action::Append(format!("{text}\n\n")));
                    }
                    self.clock.finish();
                    self.cost += cost;
                }
                Update::Failed(err) => {
                    self.streaming.clear();
                    self.clock.finish();
                    self.notice = Some(friendly_error(&err));
                }
            }
        }
        changed
    }

    pub fn key(&mut self, key: Key, mods: Mods) -> Outcome {
        self.notice = None;
        if mods.ctrl {
            match key {
                Key::Char('q') => return Outcome::Quit,
                Key::Char('c') => {
                    return match self.selected_text() {
                        Some(text) if self.mode == Mode::Chat => self.copied(text),
                        _ => {
                            self.notice = Some("nothing selected · ctrl+q quits".into());
                            Outcome::Redraw
                        }
                    };
                }
                Key::Char('y') => return self.copy_last_code(),
                Key::Char('p') => {
                    self.open_picker();
                    return Outcome::Redraw;
                }
                _ => {}
            }
        }
        match self.mode {
            Mode::Picker => {
                self.picker_key(key, mods.ctrl);
                Outcome::Redraw
            }
            Mode::Chat => self.chat_key(key, mods),
        }
    }

    /// Every action that changes the transcript goes through here.
    fn dispatch(&mut self, action: Action) {
        self.client.dispatch(action);
        self.version += 1;
    }

    fn copy_last_code(&mut self) -> Outcome {
        let body = self.client.view().transcript.join("\n");
        let Some(code) = crate::markdown::last_code_block(&body) else {
            self.notice = Some("no code block to copy".into());
            return Outcome::Redraw;
        };
        self.copied(code)
    }

    fn copied(&mut self, text: String) -> Outcome {
        let lines = text.lines().count().max(1);
        let what = if lines == 1 {
            format!("{} chars", text.chars().count())
        } else {
            format!("{lines} lines")
        };
        self.notice = Some(format!("✓ copied {what}"));
        Outcome::Copy(text)
    }

    fn open_picker(&mut self) {
        let view = self.client.view();
        let items = view
            .rows
            .iter()
            .map(|row| Item {
                id: row.id,
                label: row.text.clone(),
            })
            .collect();
        self.picker.set_items(items);
        self.picker.clear();
        if let Some(index) = view.rows.iter().position(|row| row.selected) {
            self.picker.select(index);
        }
        self.mode = Mode::Picker;
    }

    fn open_selected(&mut self) {
        if let Some(id) = self.picker.selected() {
            self.dispatch(Action::Select(id));
            self.streaming.clear();
            self.scroll = 0;
        }
        self.mode = Mode::Chat;
    }

    fn new_session(&mut self) {
        self.dispatch(Action::New);
        self.streaming.clear();
        self.editor = Editor::default();
        self.scroll = 0;
    }

    fn picker_key(&mut self, key: Key, ctrl: bool) {
        match key {
            Key::Esc => self.mode = Mode::Chat,
            Key::Up => self.picker.move_by(-1),
            Key::Down => self.picker.move_by(1),
            Key::Backspace => self.picker.backspace(),
            Key::Enter => self.open_selected(),
            Key::Char('n') if ctrl => {
                self.new_session();
                self.mode = Mode::Chat;
            }
            Key::Char(character) if !ctrl => self.picker.push(character),
            _ => {}
        }
    }

    /// Browser-textarea editing; arrows past the first/last line scroll chat.
    fn chat_key(&mut self, key: Key, mods: Mods) -> Outcome {
        let Mods { ctrl, shift, alt } = mods;
        let page = self.viewport.max(1) as isize;
        let editor = &mut self.editor;
        match key {
            Key::Char('n') if ctrl => self.new_session(),
            Key::Char('w') if ctrl => {
                if let Some(id) = self.client.app().selected().map(|doc| doc.id) {
                    self.dispatch(Action::Close(id));
                    self.streaming.clear();
                    self.scroll = 0;
                }
            }
            Key::Char('a') if ctrl => editor.select_all(),
            Key::Char('z' | 'Z') if ctrl && shift => editor.redo(),
            Key::Char('Z') if ctrl => editor.redo(),
            Key::Char('z') if ctrl => editor.undo(),
            Key::Char('x') if ctrl => {
                if let Some(text) = editor.cut() {
                    return self.copied(text);
                }
            }
            // Terminals without the kitty protocol send ctrl+backspace as ^H.
            Key::Char('h') if ctrl => editor.backspace(true),
            Key::Char(character) if !ctrl => editor.insert(character.encode_utf8(&mut [0; 4])),
            Key::Enter if shift || alt => editor.insert("\n"),
            Key::Enter => self.send(),
            Key::Tab => editor.insert("    "),
            Key::Backspace => editor.backspace(ctrl || alt),
            Key::Delete => editor.delete(ctrl || alt),
            Key::Left => editor.left(ctrl || alt, shift),
            Key::Right => editor.right(ctrl || alt, shift),
            Key::Home if ctrl && editor.is_empty() => self.scroll = self.max_scroll,
            Key::End if ctrl && editor.is_empty() => self.scroll = 0,
            Key::Home if ctrl => editor.doc_start(shift),
            Key::End if ctrl => editor.doc_end(shift),
            Key::Home => editor.home(shift),
            Key::End => editor.end(shift),
            Key::Up => {
                if !editor.up(shift) {
                    if shift {
                        editor.doc_start(true);
                    } else {
                        self.scroll_by(1);
                    }
                }
            }
            Key::Down => {
                if !editor.down(shift) {
                    if shift {
                        editor.doc_end(true);
                    } else {
                        self.scroll_by(-1);
                    }
                }
            }
            Key::PageUp => self.scroll_by(page),
            Key::PageDown => self.scroll_by(-page),
            Key::Esc => editor.escape(),
            _ => {}
        }
        Outcome::Redraw
    }

    fn send(&mut self) {
        let prompt = self.editor.text().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        // One answer at a time: a second stream would interleave with the first.
        if self.clock.generating() {
            self.notice = Some("still answering · wait for it to finish".into());
            return;
        }
        self.dispatch(Action::Append(crate::user_turn(&prompt)));
        let messages = history(&self.client.view().transcript);
        if self.worker.prompts.send(messages).is_err() {
            self.notice = Some("agent thread stopped · restart the app".into());
            return;
        }
        self.editor = Editor::default();
        self.streaming.clear();
        self.output_chars = 0;
        self.scroll = 0;
        self.clock.start();
    }

    /// Lay out one screen of `cols` × `rows` cells.
    pub fn frame(&mut self, cols: usize, rows: usize) -> Frame {
        let cols = cols.max(10);
        let view = self.client.view();
        let copy = self.mouse.then_some("copy");
        // One column of air on the right of the transcript.
        let width = cols - 1;
        let fresh = self
            .cache
            .as_ref()
            .is_some_and(|cache| cache.version == self.version && cache.width == width);
        if !fresh {
            self.cache = Some(Cache {
                version: self.version,
                width,
                laid: layout::transcript_rows(&view.transcript, width, copy),
                has_code: view
                    .transcript
                    .iter()
                    .any(|line| line.trim_start().starts_with("```")),
            });
        }
        let Some(cache) = self.cache.as_ref() else {
            unreachable!("cache was just filled");
        };

        // Prompt box: at most INPUT_ROWS, scrolled so the caret stays visible.
        let (input, caret) = layout::input(
            &self.editor,
            "type a message",
            width,
            self.mode == Mode::Chat,
        );
        let shown = input.len().min(INPUT_ROWS);
        let max_top = input.len() - shown;
        self.input_top = self
            .input_top
            .clamp((caret + 1).saturating_sub(shown), caret)
            .min(max_top);
        let input_total = input.len();
        let input: Vec<_> = input.into_iter().skip(self.input_top).take(shown).collect();
        let viewport = rows.saturating_sub(2 + shown);

        // Live tail: the streaming answer or the spinner, after the stored rows.
        let stored = cache.laid.rows.len();
        let mut tail = Laid::default();
        if !self.streaming.is_empty() {
            tail = layout::answer_rows(&self.streaming, width, copy);
        } else if let Some(elapsed) = self.clock.running_secs() {
            let word = spinner_word(elapsed);
            tail.rows
                .push(vec![run(format!("✻ {word}… ({elapsed:.0}s)"), DIM)]);
        }
        let gap = usize::from(stored > 0 && !tail.rows.is_empty());
        let total = stored + gap + tail.rows.len();
        self.viewport = viewport;
        self.max_scroll = total.saturating_sub(viewport);
        self.scroll = self.scroll.min(self.max_scroll);
        // History grows upward from the input: pad the top, never the bottom.
        let end = total - self.scroll;
        let start = end.saturating_sub(viewport);
        let pad = viewport - (end - start);
        let blank = Row::new();
        let content = cache
            .laid
            .rows
            .iter()
            .chain((0..gap).map(|_| &blank))
            .chain(tail.rows.iter());
        let mut screen: Vec<Row> = vec![Row::new(); pad];
        screen.extend(content.skip(start).take(end - start).cloned());
        self.cols = cols;
        self.total = total;
        // Content is laid out one column narrow; the bars take that column.
        let bars = [
            layout::bar(cols - 1, 0, viewport, total, start),
            layout::bar(cols - 1, viewport + 1, shown, input_total, self.input_top),
        ]
        .into_iter()
        .flatten()
        .collect();

        // Click targets, by screen row.
        let mut hits: Vec<Option<Hit>> = vec![None; screen.len()];
        if self.mouse {
            let live = tail
                .copies
                .into_iter()
                .map(|(row, code)| (row + stored + gap, code));
            for (row, code) in cache.laid.copies.iter().cloned().chain(live) {
                if (start..end).contains(&row) {
                    hits[pad + row - start] = Some(Hit::Copy(code));
                }
            }
        }

        screen.push(vec![run("─".repeat(cols), Paint::fg(NIGHT.faint))]);
        hits.push(None);
        self.input_map.clear();
        for (index, row) in input.into_iter().enumerate() {
            self.input_map.push(InputSpot {
                screen_row: screen.len(),
                start: row.start,
                end: row.end,
                cols: row.cols,
            });
            hits.push(Some(Hit::Input(index)));
            screen.push(row.row);
        }
        let (status, zones) = self.status(cols, cache.has_code, &view);
        hits.push(self.mouse.then_some(Hit::Zones(zones)));
        screen.push(status);
        let cursors = hits
            .iter()
            .map(|hit| match hit {
                None => Vec::new(),
                Some(Hit::Input(_)) => vec![(0..cols, Cursor::Text)],
                Some(Hit::Copy(_)) => vec![(0..cols, Cursor::Pointer)],
                Some(Hit::Zones(zones)) => zones
                    .iter()
                    .map(|(range, ..)| (range.clone(), Cursor::Pointer))
                    .collect(),
            })
            .enumerate()
            .map(|(row, mut zones)| {
                if zones.is_empty() && self.max_scroll > 0 && row < viewport {
                    zones.push((cols - 1..cols, Cursor::Pointer));
                }
                zones
            })
            .collect();
        self.hits = hits;
        Frame {
            rows: screen,
            cursors,
            bars,
            popup: (self.mode == Mode::Picker).then(|| {
                let items: Vec<_> = self
                    .picker
                    .visible(PICKER_ROWS)
                    .into_iter()
                    .map(|(index, item)| (index, item.label.clone(), index == self.picker.index()))
                    .collect();
                // Border takes 2 cells; leave air around the box on narrow screens.
                let width = 62.min(cols.saturating_sub(10));
                let title = format!("sessions {}", self.picker.counter());
                popup::picker(title, &self.picker.query, &items, width)
            }),
        }
    }

    /// The status row and its click zones: each hint presses its key, the
    /// session name opens the picker.
    fn status(&self, cols: usize, has_code: bool, view: &smash_core::View) -> (Row, Vec<Zone>) {
        let typing = match self.mode {
            Mode::Picker => self.picker.query_typed(),
            Mode::Chat => !self.editor.is_empty(),
        };
        let session = view
            .rows
            .iter()
            .find(|row| row.selected)
            .map_or("no session", |row| row.text.as_str());
        // Narrow screens keep the key hints; the model name goes first.
        let model = if cols >= 72 {
            format!(" · {}", self.model)
        } else {
            String::new()
        };
        let clock = self.clock.label();
        let clock = if clock.is_empty() {
            clock
        } else {
            format!(" · {clock}")
        };
        let info = format!("│  {session}{model}{clock}");
        let (row, items, right) = if let Some(notice) = &self.notice {
            let paint = if notice.starts_with('✓') {
                ACCENT
            } else {
                Paint::fg(NIGHT.code)
            };
            layout::status_row(notice, paint, &info, cols)
        } else {
            let mut hint = hints(self.mode, typing, self.clock.generating()).to_string();
            if self.editor.selection().is_some() && self.mode == Mode::Chat {
                hint = format!("ctrl+c copy · ctrl+x cut · {hint}");
            }
            if self.max_scroll > 0 && self.mode == Mode::Chat && !typing {
                let arrow = if self.scroll > 0 {
                    "↓ newer"
                } else {
                    "↑ older"
                };
                hint = format!("{arrow} · {hint}");
            }
            // Contextual hints first: they change, so they must survive truncation.
            if has_code && self.mode == Mode::Chat && !typing {
                let copy = if self.mouse {
                    "click copy"
                } else {
                    "ctrl+y copy code"
                };
                hint = format!("{copy} · {hint}");
            }
            layout::status_row(&hint, DIM, &info, cols)
        };
        let mut zones: Vec<Zone> = items
            .into_iter()
            .filter_map(|(range, item)| binding(&item).map(|(key, mods)| (range, key, mods)))
            .collect();
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        zones.push((right, Key::Char('p'), ctrl));
        (row, zones)
    }
}

#[cfg(test)]
mod tests {
    use super::{Key, Mods, binding, history};

    #[test]
    fn history_is_this_session() {
        let body: Vec<String> = format!(
            "{}answer\n\nmore\n\n{}",
            crate::user_turn("hi"),
            crate::user_turn("next")
        )
        .lines()
        .map(String::from)
        .collect();
        let messages: Vec<(smash_ai::Role, String)> = history(&body)
            .into_iter()
            .map(|message| (message.role, message.text()))
            .collect();
        use smash_ai::Role::*;
        assert_eq!(
            messages,
            [
                (User, "hi".into()),
                (Assistant, "answer\n\nmore".into()),
                (User, "next".into()),
            ]
        );
    }

    #[test]
    fn hints_name_their_keys() {
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        assert_eq!(binding("ctrl+p sessions"), Some((Key::Char('p'), ctrl)));
        assert_eq!(binding("esc close"), Some((Key::Esc, Mods::default())));
        assert_eq!(binding("↑ older"), Some((Key::PageUp, Mods::default())));
        for text in [
            "type to filter",
            "click copy",
            "↑/↓ move",
            "✓ copied 3 lines",
            "generating…",
        ] {
            assert_eq!(binding(text), None, "{text}");
        }
    }
}
