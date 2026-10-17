//! The chat controller both clients run. It owns the session client, the agent
//! worker thread, input, scroll and the picker. Clients feed it [`Key`]s,
//! clicks and pastes, and paint the [`Frame`] it returns; nothing here knows
//! about terminals or gpui.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use smash_ai::{Event as AiEvent, Message, Model};
use smash_backend::{Agent, Files};
use smash_client::Client;
use smash_core::Action;
use unicode_width::UnicodeWidthStr;

use crate::layout::{self, ACCENT, DIM, Laid, Paint, Row, run};
use crate::{Clock, Item, Mode, NIGHT, Picker, hints, spinner_word};

/// Keys after the client has translated its native event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Esc,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// What the client must do after an input.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Redraw,
    Quit,
    /// Put this text on the system clipboard (each client has its own way).
    Copy(String),
}

/// One screen, top to bottom: transcript, rule, input, status. Every row is
/// at most `cols` cells wide and there are exactly `rows` of them.
pub struct Frame {
    pub rows: Vec<Row>,
    /// Per row: does a click there do something (pointer cursor)?
    pub clickable: Vec<bool>,
    /// Session picker, drawn as an overlay when open.
    pub picker: Option<PickerView>,
}

pub struct PickerView {
    pub title: String,
    /// Typed filter; empty means show the placeholder.
    pub query: String,
    /// (match index for [`Chat::pick`], label, selected), visible window only.
    pub items: Vec<(usize, String, bool)>,
}

pub const PICKER_ROWS: usize = 5;

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
    /// The agent runs on its own thread with its own tokio runtime, so neither
    /// client's event loop ever blocks on the network.
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
    /// Status row: clicks at or right of this column open the session picker.
    Status(usize),
}

pub struct Chat {
    client: Client,
    worker: Worker,
    input: String,
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
    /// Client has a pointer: code headers show a "copy" target.
    mouse: bool,
    /// Click targets of the last frame, one per screen row.
    hits: Vec<Option<Hit>>,
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
            input: String::new(),
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
            mouse: false,
            hits: Vec::new(),
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

    /// Positive = older content. Wheel and trackpad land here.
    pub fn scroll_by(&mut self, rows: isize) {
        self.scroll = self.scroll.saturating_add_signed(rows).min(self.max_scroll);
    }

    /// Click at a cell of the last frame.
    pub fn click(&mut self, row: usize, col: usize) -> Outcome {
        self.notice = None;
        if self.mode == Mode::Picker {
            // Click outside the popup closes it, like Esc.
            self.mode = Mode::Chat;
            return Outcome::Redraw;
        }
        match self.hits.get(row).cloned().flatten() {
            Some(Hit::Copy(code)) => self.copied(code),
            Some(Hit::Status(from)) if col >= from => {
                self.open_picker();
                Outcome::Redraw
            }
            _ => Outcome::Redraw,
        }
    }

    /// Open the picker entry with this match index (from [`PickerView::items`]).
    pub fn pick(&mut self, index: usize) {
        self.picker.select(index);
        self.open_selected();
    }

    /// Insert text: into the input, or the picker filter (one line).
    pub fn paste(&mut self, text: &str) {
        self.notice = None;
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        match self.mode {
            Mode::Picker => {
                for character in text.chars().filter(|c| !c.is_control()) {
                    self.picker.push(character);
                }
            }
            Mode::Chat => self.input.push_str(text.trim_end_matches('\n')),
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

    pub fn key(&mut self, key: Key, ctrl: bool) -> Outcome {
        self.notice = None;
        match (key, ctrl) {
            (Key::Char('q' | 'c'), true) => return Outcome::Quit,
            (Key::Char('y'), true) => return self.copy_last_code(),
            (Key::Char('p'), true) => self.open_picker(),
            _ => match self.mode {
                Mode::Picker => self.picker_key(key, ctrl),
                Mode::Chat => self.chat_key(key, ctrl),
            },
        }
        Outcome::Redraw
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

    fn copied(&mut self, code: String) -> Outcome {
        let lines = code.lines().count();
        let plural = if lines == 1 { "" } else { "s" };
        self.notice = Some(format!("✓ copied {lines} line{plural}"));
        Outcome::Copy(code)
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
        self.input.clear();
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

    fn chat_key(&mut self, key: Key, ctrl: bool) {
        let page = self.viewport.max(1) as isize;
        match key {
            Key::Char('n') if ctrl => self.new_session(),
            Key::Char('w') if ctrl => {
                if let Some(id) = self.client.app().selected().map(|doc| doc.id) {
                    self.dispatch(Action::Close(id));
                    self.streaming.clear();
                    self.scroll = 0;
                }
            }
            Key::Up => self.scroll_by(1),
            Key::Down => self.scroll_by(-1),
            Key::PageUp => self.scroll_by(page),
            Key::PageDown => self.scroll_by(-page),
            Key::Home if ctrl => self.scroll = self.max_scroll,
            Key::End if ctrl => self.scroll = 0,
            Key::Esc => self.input.clear(),
            Key::Enter => self.send(),
            Key::Backspace => {
                self.input.pop();
            }
            Key::Char(character) if !ctrl => self.input.push(character),
            _ => {}
        }
    }

    fn send(&mut self) {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        self.dispatch(Action::Append(crate::user_turn(&prompt)));
        if self.worker.prompts.send(prompt).is_err() {
            self.notice = Some("agent thread stopped · restart the app".into());
            return;
        }
        self.input.clear();
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

        let mut input = layout::input_rows(&self.input, "type a message", width);
        let keep = input.len().clamp(1, 6);
        input.drain(..input.len() - keep);
        let viewport = rows.saturating_sub(2 + input.len());

        // Live tail: the streaming answer or the spinner, after the stored rows.
        let stored = cache.laid.rows.len();
        let mut tail = Laid::default();
        if !self.streaming.is_empty() {
            tail = layout::answer_rows(&self.streaming, width, copy);
        } else if let Some(elapsed) = self.clock.running_secs() {
            let word = spinner_word(elapsed);
            tail.rows.push(vec![run(format!("✻ {word}… ({elapsed:.0}s)"), DIM)]);
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
        screen.extend(input);
        let (status, right) = self.status(cols, cache.has_code, &view);
        hits.resize(screen.len(), None);
        hits.push(self.mouse.then_some(Hit::Status(right)));
        screen.push(status);
        let clickable = hits.iter().map(Option::is_some).collect();
        self.hits = hits;
        Frame {
            rows: screen,
            clickable,
            picker: (self.mode == Mode::Picker).then(|| PickerView {
                title: format!("sessions {}", self.picker.counter()),
                query: self.picker.query.clone(),
                items: self
                    .picker
                    .visible(PICKER_ROWS)
                    .into_iter()
                    .map(|(index, item)| (index, item.label.clone(), index == self.picker.index()))
                    .collect(),
            }),
        }
    }

    /// The status row and the column where its right part (session) starts.
    fn status(&self, cols: usize, has_code: bool, view: &smash_core::View) -> (Row, usize) {
        let typing = match self.mode {
            Mode::Picker => self.picker.query_typed(),
            Mode::Chat => !self.input.is_empty(),
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
        let right = format!("│  {session}{model}{clock}");
        let from = cols.saturating_sub(right.width());
        if let Some(notice) = &self.notice {
            let paint = if notice.starts_with('✓') {
                ACCENT
            } else {
                Paint::fg(NIGHT.code)
            };
            return (layout::status_row(notice, paint, &right, cols), from);
        }
        let mut hint = hints(self.mode, typing, self.clock.generating()).to_string();
        if self.max_scroll > 0 && self.mode == Mode::Chat && !typing {
            let arrow = if self.scroll > 0 { "↓ newer" } else { "↑ older" };
            hint = format!("{arrow} · {hint}");
        }
        // Contextual hints first: they change, so they must survive truncation.
        if has_code && self.mode == Mode::Chat && !typing {
            let copy = if self.mouse { "click copy" } else { "ctrl+y copy code" };
            hint = format!("{copy} · {hint}");
        }
        (layout::status_row(&hint, DIM, &right, cols), from)
    }
}
