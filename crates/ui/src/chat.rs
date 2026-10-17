//! The chat controller both clients run. It owns the session client, the agent
//! worker thread, the prompt editor, scroll and the picker. Clients feed it
//! keys, clicks and pastes, and paint the [`Frame`] it returns; nothing here
//! knows about terminals or gpui.

use std::ops::Range;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use std::collections::HashMap;
use std::path::PathBuf;

use smash_ai::catalog::{Catalog, Entry};
use smash_ai::{Content, Effort, Error as AiError, Kind, Message, Model, Usage};
use smash_backend::{Agent, CancellationToken, Files, Run, Update as AgentUpdate};
use smash_client::Client;
use smash_core::Action;
use smash_core::DocId;

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

/// One prompt for the agent thread.
struct Job {
    doc: DocId,
    messages: Vec<Message>,
    model: Model,
    effort: Option<Effort>,
    cancel: CancellationToken,
}

enum Msg {
    Update(AgentUpdate),
    /// The run ended; the messages are the full conversation after it.
    Done {
        doc: DocId,
        result: Result<Run, AiError>,
        messages: Vec<Message>,
    },
}

struct Worker {
    jobs: Sender<Job>,
    msgs: Receiver<Msg>,
}

impl Worker {
    /// The agent runs on its own thread with its own tokio runtime, so neither
    /// client's event loop ever blocks on the network or a tool.
    fn spawn() -> Self {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (outbox, msgs) = mpsc::channel::<Msg>();
        thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            while let Ok(job) = inbox.recv() {
                let Job {
                    doc,
                    mut messages,
                    model,
                    effort,
                    cancel,
                } = job;
                let agent = Agent::new(model).effort(effort);
                let (result, messages) = runtime.block_on(async {
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let run = async move {
                        let result = agent.run(&mut messages, &tx, &cancel).await;
                        (result, messages)
                    };
                    let forward = async {
                        while let Some(update) = rx.recv().await {
                            let _ = outbox.send(Msg::Update(update));
                        }
                    };
                    let (done, ()) = tokio::join!(run, forward);
                    done
                });
                if outbox
                    .send(Msg::Done {
                        doc,
                        result,
                        messages,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self { jobs, msgs }
    }
}

/// models.dev list: the disk cache at once, a fresh copy when it is stale.
fn catalog_feed(cache: PathBuf) -> Receiver<Catalog> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Some(catalog) = Catalog::load(&cache) {
            let _ = tx.send(catalog);
        }
        if Catalog::stale(&cache)
            && let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            && let Ok(catalog) = runtime.block_on(Catalog::refresh(&cache))
        {
            let _ = tx.send(catalog);
        }
    });
    rx
}

/// What the user picked, kept across restarts.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Settings {
    /// `provider/model` (models.dev ids).
    model: String,
    effort: Option<Effort>,
    thinking: bool,
}

impl Settings {
    fn load(path: &std::path::Path) -> Self {
        let saved = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Settings>(&text).ok());
        saved.unwrap_or_else(|| Settings {
            // `deepseek-chat` is a legacy name for deepseek-flash with thinking
            // off, which garbles ~half its answers (measured 2026-10).
            model: std::env::var("SMASH_MODEL")
                .unwrap_or_else(|_| "deepseek/deepseek-flash".into()),
            effort: None,
            thinking: true,
        })
    }

    fn save(&self, path: &std::path::Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}

/// `DeepSeek V4.1 Flash · deepseek · 1M ctx · $0.15/$0.60 · ✻`
fn model_label(entry: &Entry) -> String {
    let context = match entry.context {
        n if n >= 1_000_000 => format!("{}M", n / 1_000_000),
        n if n >= 1_000 => format!("{}k", n / 1_000),
        n => n.to_string(),
    };
    let price = entry
        .price
        .map(|price| format!(" · ${:.2}/${:.2}", price.input, price.output))
        .unwrap_or_default();
    let thinks = if entry.efforts.is_empty() {
        ""
    } else {
        " · ✻"
    };
    format!(
        "{} · {} · {context} ctx{price}{thinks}",
        entry.name, entry.provider
    )
}

/// A model without a catalog entry yet (offline first start): the provider
/// from the key's prefix, DeepSeek otherwise.
fn fallback_model(key: &str) -> Model {
    // Any OpenAI-compatible endpoint: SMASH_MODEL=http://host:port/v1/|model
    // with the key in SMASH_KEY (local servers, fault tests).
    if let Some((base, id)) = key.split_once('|')
        && base.starts_with("http")
    {
        return Model::compatible(base, "SMASH_KEY", id);
    }
    match key.split_once(['/', ':']) {
        Some(("anthropic", id)) => Model::anthropic(id),
        Some(("openai", id)) => Model::openai(id),
        Some((_, id)) => Model::deepseek(id),
        None => Model::deepseek(key),
    }
}

/// A live answer, in arrival order.
#[derive(Clone, Debug)]
enum Segment {
    Thinking(String),
    Text(String),
    Program {
        title: String,
        /// Calls in start order, with when each started (for running ones).
        calls: Vec<(smash_js::Call, std::time::Instant)>,
        summary: Option<String>,
    },
}

/// Segments in the stored-body format; the same parser and renderer draw
/// live runs and saved ones.
fn serialize(segments: &[Segment]) -> String {
    let field = |text: &str| text.replace(['\t', '\n'], " ");
    let mut out = String::new();
    for segment in segments {
        match segment {
            Segment::Thinking(text) => {
                for line in text.trim().lines() {
                    out.push_str(&format!("{}{line}\n", crate::THINK));
                }
            }
            Segment::Text(text) => {
                let text = text.trim_matches('\n');
                if !text.trim().is_empty() {
                    out.push_str(text);
                    out.push('\n');
                }
            }
            Segment::Program {
                title,
                calls,
                summary,
            } => {
                let summary = summary.as_deref().unwrap_or_default();
                out.push_str(&format!(
                    "{}{}\t{}\n",
                    crate::PROGRAM,
                    field(title),
                    field(summary)
                ));
                for (call, started) in calls {
                    let (mark, info) = match (call.running, call.ok) {
                        (true, _) => ("…", format!("running {:.0}s", started.elapsed().as_secs_f64())),
                        (false, true) => ("✓", call.info.clone()),
                        (false, false) => ("✗", call.info.clone()),
                    };
                    out.push_str(&format!(
                        "{}{mark}\t{}\t{}\t{}\n",
                        crate::CALL,
                        field(&call.verb),
                        field(&call.target),
                        field(&info)
                    ));
                }
            }
        }
    }
    out
}

/// The session's stored transcript as model context (used when the exact
/// conversation is not in memory, e.g. after a restart). Answer text and
/// reasoning come back; tool logs stay out (they are summaries, not results).
// ponytail: sends the whole session every turn; trim old turns when chats
// outgrow the context window.
fn history(body: &[String]) -> Vec<Message> {
    use crate::Role;
    let mut messages: Vec<Message> = Vec::new();
    let (mut thinking, mut text) = (String::new(), String::new());
    let flush = |messages: &mut Vec<Message>, thinking: &mut String, text: &mut String| {
        let mut content = Vec::new();
        if !thinking.trim().is_empty() {
            content.push(Content::Thinking {
                text: thinking.trim().to_string(),
            });
        }
        if !text.trim().is_empty() {
            content.push(Content::Text {
                text: text.trim().to_string(),
            });
        }
        if !content.is_empty() {
            messages.push(Message::assistant(content));
        }
        thinking.clear();
        text.clear();
    };
    let mut user = String::new();
    for line in crate::transcript(body) {
        match line.role {
            Role::User => {
                flush(&mut messages, &mut thinking, &mut text);
                user.push_str(&line.text);
                user.push('\n');
                continue;
            }
            Role::Thinking => {
                thinking.push_str(&line.text);
                thinking.push('\n');
            }
            Role::Assistant => {
                text.push_str(&line.text);
                text.push('\n');
            }
            // Logs and failure notes are for the user, not model context.
            Role::Program | Role::Call | Role::Fail => {}
        }
        if !user.is_empty() {
            messages.push(Message::user(user.trim().to_string()));
            user.clear();
        }
    }
    flush(&mut messages, &mut thinking, &mut text);
    if !user.trim().is_empty() {
        messages.push(Message::user(user.trim().to_string()));
    }
    messages
}

/// One line for the status bar: what happened and what to do, in words.
fn friendly_error(err: &AiError, model: &str, key_env: Option<&str>) -> String {
    // "deepseek/deepseek-flash" → deepseek; "http://host:1234/v1/|m" → host:1234.
    let provider = match model.split_once("://") {
        Some((_, rest)) => rest.split('/').next().unwrap_or(rest),
        None => model.split('/').next().unwrap_or(model),
    };
    let key = key_env.map(String::from).unwrap_or_else(|| {
        if model.contains("://") {
            "SMASH_KEY".into()
        } else {
            format!("{}_API_KEY", provider.to_uppercase().replace('-', "_"))
        }
    });
    let detail: String = err.message.chars().take(90).collect();
    match err.kind {
        Kind::Auth => {
            format!("{provider} rejected the API key · check {key}, restart · ctrl+l other model")
        }
        Kind::Billing => {
            format!("{provider} account is out of credit · top up, or ctrl+l other model")
        }
        Kind::RateLimit => {
            format!("{provider} still rate limiting after retries · wait a minute, enter retries")
        }
        Kind::Overloaded => {
            format!("{provider} is overloaded right now · enter retries · ctrl+l other model")
        }
        Kind::Network | Kind::Stalled => {
            "network trouble talking to the model · enter retries".into()
        }
        Kind::Empty => {
            "the model kept answering with nothing · enter retries · ctrl+l other model".into()
        }
        Kind::Request if err.status == Some(404) || detail.contains("model") => {
            format!("{provider} doesn't accept this model · ctrl+l pick another · {detail}")
        }
        Kind::Request
            if detail.contains("context")
                || detail.contains("too long")
                || err.status == Some(413) =>
        {
            "the chat is too long for this model · ctrl+n new chat, or ctrl+l a bigger model".into()
        }
        Kind::Request => format!("{provider} refused the request · {detail}"),
        Kind::Cancelled => "stopped".into(),
        Kind::Other => format!("{detail} · enter retries"),
    }
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

/// Which list the picker popup is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    Sessions,
    Models,
}

pub struct Chat {
    client: Client,
    worker: Worker,
    editor: Editor,
    /// The answer in flight, and the session it belongs to.
    live: Vec<Segment>,
    live_doc: Option<DocId>,
    cancel: Option<CancellationToken>,
    /// Exact conversations (with tool calls and results) since this start,
    /// per session; sessions not here are rebuilt from their stored text.
    conversations: HashMap<DocId, Vec<Message>>,
    usage: Usage,
    cost: f64,
    catalog: Catalog,
    catalogs: Receiver<Catalog>,
    settings: Settings,
    settings_path: PathBuf,
    pick: Pick,
    /// Model keys behind the model picker's items (item id = index).
    model_keys: Vec<String>,
    /// One-shot message on the status row (errors, "copied"); next input clears it.
    notice: Option<String>,
    mode: Mode,
    picker: Picker,
    clock: Clock,
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
    /// A model call failed transiently: (what, when the retry starts).
    retrying: Option<(String, std::time::Instant)>,
    /// The last failed run, kept so Enter on an empty box continues it.
    failed: Option<DocId>,
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
        let dir = Files::default_dir();
        let settings_path = dir.join("settings.json");
        let settings = Settings::load(&settings_path);
        Self {
            client,
            worker: Worker::spawn(),
            live: Vec::new(),
            live_doc: None,
            cancel: None,
            conversations: HashMap::new(),
            usage: Usage::default(),
            catalog: Catalog::default(),
            catalogs: catalog_feed(dir.join("models.json")),
            settings,
            settings_path,
            pick: Pick::Sessions,
            model_keys: Vec::new(),
            editor: Editor::default(),
            cost: 0.0,
            notice: None,
            mode: Mode::Chat,
            picker: Picker::default(),
            clock: Clock::default(),
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
            retrying: None,
            failed: None,
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

    /// Drain agent output and catalog refreshes. True when the screen changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(catalog) = self.catalogs.try_recv() {
            self.catalog = catalog;
            changed = true;
        }
        while let Ok(msg) = self.worker.msgs.try_recv() {
            changed = true;
            match msg {
                Msg::Update(update) => self.apply(update),
                Msg::Done {
                    doc,
                    result,
                    messages,
                } => self.finish(doc, result, messages),
            }
        }
        changed
    }

    fn apply(&mut self, update: AgentUpdate) {
        match update {
            // A retried model call streams again from the start: drop what the
            // failed attempt showed since the last finished program.
            AgentUpdate::Attempt => {
                self.retrying = None;
                while matches!(
                    self.live.last(),
                    Some(Segment::Thinking(_) | Segment::Text(_))
                ) {
                    self.live.pop();
                }
            }
            AgentUpdate::Retry {
                attempt,
                max,
                secs,
                reason,
            } => {
                let at = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
                self.retrying = Some((format!("{reason} · retry {attempt}/{max}"), at));
            }
            AgentUpdate::Thinking(delta) => match self.live.last_mut() {
                Some(Segment::Thinking(text)) => text.push_str(&delta),
                _ => self.live.push(Segment::Thinking(delta)),
            },
            AgentUpdate::Text(delta) => match self.live.last_mut() {
                Some(Segment::Text(text)) => text.push_str(&delta),
                _ => self.live.push(Segment::Text(delta)),
            },
            AgentUpdate::Program { title } => self.live.push(Segment::Program {
                title,
                calls: Vec::new(),
                summary: None,
            }),
            // A call shows up when it starts and is updated in place when it ends,
            // so slow commands are visible while they run.
            AgentUpdate::Call(call) => {
                if let Some(Segment::Program { calls, .. }) = self.live.last_mut() {
                    match calls.iter_mut().find(|(known, _)| known.id == call.id) {
                        Some((known, _)) => *known = call,
                        None => calls.push((call, std::time::Instant::now())),
                    }
                }
            }
            AgentUpdate::ProgramDone {
                calls: count,
                secs,
                error,
            } => {
                if let Some(Segment::Program { summary, calls, .. }) = self.live.last_mut() {
                    // A stopped program reports 0; the log knows what finished.
                    let count = count.max(calls.len());
                    let plural = if count == 1 { "" } else { "s" };
                    let mut text = format!("{count} call{plural} · {secs:.1}s");
                    if let Some(error) = error {
                        text.push_str(&format!(" · error: {error}"));
                    }
                    *summary = Some(text);
                }
            }
            AgentUpdate::Usage(usage) => self.usage = usage,
        }
    }

    /// Store the finished answer in the session it was asked in, even if the
    /// user switched sessions meanwhile.
    fn finish(&mut self, doc: DocId, result: Result<Run, AiError>, messages: Vec<Message>) {
        let mut segments = std::mem::take(&mut self.live);
        // Calls still running when the run ended were killed with it.
        for segment in &mut segments {
            if let Segment::Program { calls, .. } = segment {
                for (call, _) in calls.iter_mut().filter(|(call, _)| call.running) {
                    call.running = false;
                    call.ok = false;
                    call.info = "stopped".into();
                }
            }
        }
        self.live_doc = None;
        self.cancel = None;
        self.retrying = None;
        self.clock.finish();
        let cancelled = matches!(&result, Err(err) if err.kind == Kind::Cancelled);
        let mut body = serialize(&segments);
        match &result {
            Ok(run) => {
                self.cost += run.cost.total;
                self.failed = None;
            }
            Err(_) if cancelled => {
                self.notice = Some("stopped · enter on an empty box continues".into());
                body.push_str("*(stopped)*\n");
                self.failed = Some(doc);
            }
            Err(err) => {
                let key = self.entry().and_then(Entry::key_env).map(String::from);
                let line = friendly_error(err, &self.settings.model, key.as_deref());
                // In the chat too, so it survives the next keypress.
                body.push_str(&format!("{}{line}\n", crate::FAIL));
                self.notice = Some(line);
                self.failed = Some(doc);
            }
        }
        if !body.trim().is_empty() {
            self.append_to(doc, &format!("{body}\n"));
        }
        // The agent leaves `messages` valid even after an error (every tool
        // call answered), so a continue picks up with full tool context.
        self.conversations.insert(doc, messages);
    }

    /// Append to `doc` without leaving the session the user is looking at.
    fn append_to(&mut self, doc: DocId, text: &str) {
        let shown = self.client.app().selected().map(|selected| selected.id);
        if shown != Some(doc) {
            self.dispatch(Action::Select(doc));
        }
        self.dispatch(Action::Append(text.to_string()));
        if let Some(shown) = shown
            && shown != doc
        {
            self.dispatch(Action::Select(shown));
        }
    }

    /// The catalog entry for the chosen model, if the list knows it.
    fn entry(&self) -> Option<&Entry> {
        self.catalog.find(&self.settings.model)
    }

    fn effort(&self) -> Option<Effort> {
        self.settings
            .effort
            .or_else(|| self.entry().and_then(Entry::default_effort))
    }

    fn cycle_effort(&mut self) {
        let next = match self.entry() {
            Some(entry) => entry.next_effort(self.effort()),
            None => Some(match self.effort() {
                Some(Effort::High) => Effort::Max,
                Some(Effort::Max) => Effort::Off,
                Some(Effort::Off) => Effort::Low,
                _ => Effort::High,
            }),
        };
        match next {
            Some(effort) => {
                self.settings.effort = Some(effort);
                self.settings.save(&self.settings_path);
                self.notice = Some(format!("✓ thinking {} · next message", effort.label()));
            }
            None => self.notice = Some("this model has no thinking levels".into()),
        }
    }

    fn toggle_thinking(&mut self) {
        self.settings.thinking = !self.settings.thinking;
        self.settings.save(&self.settings_path);
        self.cache = None;
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
                Key::Char('l') => {
                    self.open_models();
                    return Outcome::Redraw;
                }
                Key::Char('t') => {
                    self.cycle_effort();
                    return Outcome::Redraw;
                }
                Key::Char('o') => {
                    self.toggle_thinking();
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
        self.pick = Pick::Sessions;
        self.mode = Mode::Picker;
    }

    /// Models whose API key is set, from models.dev; type to filter.
    fn open_models(&mut self) {
        // First open right after start: the cached list loads in ~0.1s; wait
        // for it rather than bounce the user.
        if self.catalog.entries.is_empty()
            && let Ok(catalog) = self
                .catalogs
                .recv_timeout(std::time::Duration::from_millis(400))
        {
            self.catalog = catalog;
        }
        let mut entries = self.catalog.available();
        if entries.is_empty() {
            self.notice = Some(if self.catalog.entries.is_empty() {
                "model list still loading (models.dev) · try again in a moment".into()
            } else {
                "no API keys found · export e.g. DEEPSEEK_API_KEY or ANTHROPIC_API_KEY".into()
            });
            return;
        }
        // Current provider first, then by provider and name.
        let current = self
            .settings
            .model
            .split('/')
            .next()
            .unwrap_or_default()
            .to_string();
        entries.sort_by_key(|entry| {
            (
                entry.provider != current,
                entry.provider.clone(),
                entry.name.clone(),
            )
        });
        self.model_keys = entries.iter().map(|entry| entry.key()).collect();
        let items = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| Item {
                id: index as u64,
                label: model_label(entry),
            })
            .collect();
        self.picker.set_items(items);
        self.picker.clear();
        if let Some(index) = self
            .model_keys
            .iter()
            .position(|key| *key == self.settings.model)
        {
            self.picker.select(index);
        }
        self.pick = Pick::Models;
        self.mode = Mode::Picker;
    }

    fn open_selected(&mut self) {
        let Some(id) = self.picker.selected() else {
            self.mode = Mode::Chat;
            return;
        };
        match self.pick {
            Pick::Sessions => {
                self.dispatch(Action::Select(id));
                self.scroll = 0;
            }
            Pick::Models => {
                if let Some(key) = self.model_keys.get(id as usize).cloned() {
                    self.settings.model = key;
                    // Levels differ per model; fall back to the model's default.
                    self.settings.effort = None;
                    self.settings.save(&self.settings_path);
                    let name = self
                        .entry()
                        .map_or(self.settings.model.clone(), |entry| entry.name.clone());
                    self.notice = Some(format!("✓ {name} · next message"));
                }
            }
        }
        self.mode = Mode::Chat;
    }

    fn new_session(&mut self) {
        self.dispatch(Action::New);
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
                    if self.live_doc == Some(id)
                        && let Some(cancel) = &self.cancel
                    {
                        cancel.cancel();
                    }
                    self.conversations.remove(&id);
                    self.dispatch(Action::Close(id));
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
            // Esc stops a running answer first, then clears the box.
            Key::Esc if self.cancel.is_some() && editor.is_empty() => {
                if let Some(cancel) = &self.cancel {
                    cancel.cancel();
                }
            }
            Key::Esc => editor.escape(),
            _ => {}
        }
        Outcome::Redraw
    }

    fn send(&mut self) {
        let prompt = self.editor.text().trim().to_string();
        let viewed = self.client.app().selected().map(|doc| doc.id);
        // Enter on an empty box after a failure or a stop: carry on.
        // Also after a restart: the last answer in the chat is a failure note.
        let ended_badly = self
            .client
            .view()
            .transcript
            .iter()
            .rev()
            .find(|line| !line.trim().is_empty())
            .is_some_and(|line| line.starts_with(crate::FAIL) || line.trim() == "*(stopped)*");
        let resume =
            prompt.is_empty() && ((self.failed.is_some() && self.failed == viewed) || ended_badly);
        if prompt.is_empty() && !resume {
            return;
        }
        // One answer at a time: a second run would interleave with the first.
        if self.clock.generating() {
            self.notice = Some("still answering · esc stops it".into());
            return;
        }
        let Some(doc) = self.client.app().selected().map(|doc| doc.id) else {
            return;
        };
        let mut messages = match self.conversations.get(&doc) {
            Some(messages) => messages.clone(),
            None => history(&self.client.view().transcript),
        };
        let prompt = if resume {
            "continue".to_string()
        } else {
            prompt
        };
        messages.push(Message::user(prompt.clone()));
        self.dispatch(Action::Append(crate::user_turn(&prompt)));
        self.failed = None;
        let model = match self.entry().and_then(Entry::model) {
            Some(model) => model,
            None => fallback_model(&self.settings.model),
        };
        let cancel = CancellationToken::new();
        let job = Job {
            doc,
            messages,
            model,
            effort: self.effort(),
            cancel: cancel.clone(),
        };
        if self.worker.jobs.send(job).is_err() {
            self.notice = Some("agent thread stopped · restart the app".into());
            return;
        }
        self.cancel = Some(cancel);
        self.live_doc = Some(doc);
        self.live.clear();
        self.usage = Usage::default();
        self.editor = Editor::default();
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
                laid: layout::transcript_rows(
                    &view.transcript,
                    width,
                    copy,
                    self.settings.thinking,
                ),
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

        // Live tail: the answer in flight (only in the session that asked),
        // then a spinner row with time, tokens and how to stop.
        let stored = cache.laid.rows.len();
        let mut tail = Laid::default();
        let viewed = self.client.app().selected().map(|doc| doc.id);
        if let Some(elapsed) = self.clock.running_secs()
            && viewed == self.live_doc
        {
            let lines = crate::answer_lines(&serialize(&self.live));
            tail = layout::turn_rows(&lines, width, copy, self.settings.thinking, true);
            if !tail.rows.is_empty() {
                tail.rows.push(Row::new());
            }
            let word = spinner_word(elapsed);
            let tokens = if self.usage.output > 0 {
                format!(" · ↓{} tok", self.usage.output)
            } else {
                String::new()
            };
            // Waiting to retry: say why and how long, in warning colour.
            let waiting = self.retrying.as_ref().and_then(|(why, at)| {
                let left = at
                    .saturating_duration_since(std::time::Instant::now())
                    .as_secs_f64();
                (left > 0.0).then(|| format!("↻ {why} in {left:.0}s "))
            });
            match waiting {
                Some(waiting) => tail.rows.push(vec![
                    run(waiting, Paint::fg(NIGHT.code)),
                    run(format!("({elapsed:.0}s · esc to stop)"), DIM),
                ]),
                None => tail.rows.push(vec![
                    run(format!("✻ {word}… "), ACCENT),
                    run(format!("({elapsed:.0}s{tokens} · esc to stop)"), DIM),
                ]),
            }
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
                let what = match self.pick {
                    Pick::Sessions => "sessions",
                    Pick::Models => "models",
                };
                let title = format!("{what} {}", self.picker.counter());
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
            let name = match self.settings.model.split_once('|') {
                Some((_, id)) => id,
                None => self.settings.model.rsplit('/').next().unwrap_or_default(),
            };
            let effort = self
                .effort()
                .map(|e| format!(" ✻{}", e.label()))
                .unwrap_or_default();
            format!(" · {name}{effort}")
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
            let viewed = view.rows.iter().find(|row| row.selected).map(|row| row.id);
            if self.failed.is_some() && self.failed == viewed && !typing && self.mode == Mode::Chat
            {
                hint = format!("enter continue · {hint}");
            }
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

    fn sample() -> Vec<super::Segment> {
        use super::Segment;
        vec![
            Segment::Thinking("look at the files\nthen answer".into()),
            Segment::Program {
                title: "read config".into(),
                calls: vec![
                    (
                        smash_js::Call {
                            id: 0,
                            running: false,
                            verb: "read".into(),
                            target: "Cargo.toml".into(),
                            info: "36 lines".into(),
                            ok: true,
                        },
                        std::time::Instant::now(),
                    ),
                    (
                        smash_js::Call {
                            id: 1,
                            running: false,
                            verb: "sh".into(),
                            target: "cargo test\t-q".into(),
                            info: "exit 1".into(),
                            ok: false,
                        },
                        std::time::Instant::now(),
                    ),
                ],
                summary: Some("2 calls · 0.4s · error: tests failed".into()),
            },
            Segment::Text("Resolver is **3**.".into()),
        ]
    }

    #[test]
    fn live_segments_round_trip_through_the_body() {
        use crate::Role::*;
        let body = format!(
            "{}{}",
            crate::user_turn("which resolver?"),
            super::serialize(&sample())
        );
        let lines: Vec<String> = body.lines().map(String::from).collect();
        let roles: Vec<crate::Role> = crate::transcript(&lines)
            .into_iter()
            .map(|line| line.role)
            .collect();
        assert_eq!(
            roles,
            [User, Thinking, Thinking, Program, Call, Call, Assistant]
        );
        let call = crate::transcript(&lines)[5].text.clone();
        assert_eq!(
            call, "✗\tsh\tcargo test -q\texit 1",
            "tabs in fields can't break the format"
        );

        let rows = crate::layout::transcript_rows(&lines, 60, None, true).rows;
        let text: Vec<String> = rows
            .iter()
            .map(|row| row.iter().map(|run| run.text.as_str()).collect())
            .collect();
        assert!(
            text.iter()
                .any(|row| row.starts_with("✻ look at the files")),
            "{text:#?}"
        );
        assert!(
            text.iter()
                .any(|row| row.starts_with("⏵ read config") && row.trim_end().ends_with("failed")),
            "{text:#?}"
        );
        assert!(
            text.iter()
                .any(|row| row.contains("✗ sh    cargo test -q")
                    && row.trim_end().ends_with("exit 1")),
            "{text:#?}"
        );
        assert!(rows.iter().all(|row| crate::layout::width(row) <= 60));

        let hidden = crate::layout::transcript_rows(&lines, 60, None, false).rows;
        let hidden: Vec<String> = hidden
            .iter()
            .map(|row| row.iter().map(|run| run.text.as_str()).collect())
            .collect();
        assert!(
            hidden
                .iter()
                .any(|row| row.contains("thought · 6 words · ctrl+o")),
            "{hidden:#?}"
        );
        assert!(!hidden.iter().any(|row| row.contains("look at the files")));
    }

    #[test]
    fn history_is_this_session() {
        let body: Vec<String> = format!(
            "{}{}\n{}",
            crate::user_turn("hi"),
            super::serialize(&sample()),
            crate::user_turn("next")
        )
        .lines()
        .map(String::from)
        .collect();
        let messages = history(&body);
        let roles: Vec<smash_ai::Role> = messages.iter().map(|message| message.role).collect();
        use smash_ai::Role::*;
        assert_eq!(roles, [User, Assistant, User]);
        assert_eq!(
            messages[1].text(),
            "Resolver is **3**.",
            "tool logs are not replayed as text"
        );
        assert!(
            matches!(&messages[1].content[0], smash_ai::Content::Thinking { text } if text.contains("then answer"))
        );
        assert_eq!(messages[2].text(), "next");
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
