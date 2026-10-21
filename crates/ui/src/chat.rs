//! The chat controller both clients run. It owns the session client, the agent
//! worker thread, the prompt editor, scroll and the picker. Clients feed it
//! keys, clicks and pastes, and paint the [`Frame`] it returns; nothing here
//! knows about terminals or gpui.

use std::ops::Range;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use smash_ai::catalog::{Catalog, Entry};
use smash_ai::{Content, Effort, Error as AiError, Kind, Message, Model, Usage};
use smash_backend::agents::{
    DOCS as AGENT_DOCS, Event as AgentEvent, Info as AgentInfo, Spec, Status,
};
use smash_backend::{
    Agent, Agents, CancellationToken, Compactor, Files, Inbox, Run, Summarizer,
    Update as AgentUpdate,
};
use smash_client::Client;
use smash_core::Action;
use smash_core::DocId;

use crate::editor::Editor;
use crate::layout::{self, ACCENT, DIM, Laid, Paint, Row, run};
use crate::motion::{self, Key as Anim, Motions, Spring};
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
    /// The first `viewport` rows are the transcript. Pixel clients draw them
    /// `shift` rows lower (0..1, smooth scrolling) with `above` peeking in on
    /// top; grid clients ignore both.
    pub viewport: usize,
    pub shift: f32,
    pub above: Option<Row>,
    /// Per row: column ranges that get a non-default mouse cursor.
    pub cursors: Vec<Vec<(Range<usize>, Cursor)>>,
    /// Scrollbars (transcript, prompt), drawn over the last column.
    pub bars: Vec<layout::Bar>,
    /// Overlay (session picker), drawn centered over the rows.
    pub popup: Option<Popup>,
}

/// What a running subagent is doing, for its live row.
#[derive(Default)]
struct AgentLive {
    started: Option<std::time::Instant>,
    /// Title of its current program (or "thinking").
    program: String,
    calls: Vec<(smash_js::Call, std::time::Instant)>,
}

/// Key hints on the status bar at once.
const HINTS: usize = 4;

/// Calls kept per running subagent (the dock shows the newest).
const AGENT_CALLS: usize = 3;
/// Dock rows per running subagent: its badge row and its newest call.
const DOCK_ROWS: usize = 2;

/// The transcript as the last frame showed it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Seen {
    doc: Option<DocId>,
    width: usize,
    total: usize,
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
    /// `provider/model`, for learned context limits.
    key: String,
    window: u64,
    summarizers: Vec<Summarizer>,
    /// Messages typed while this run works; it reads them between turns.
    inbox: Inbox,
    /// For subagents: which models exist.
    catalog: Catalog,
}

enum Msg {
    Update(AgentUpdate),
    /// A subagent of some session changed or made progress.
    Agent(AgentEvent),
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
    fn spawn(compactor: Arc<Compactor>, agents_dir: PathBuf) -> Self {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (outbox, msgs) = mpsc::channel::<Msg>();
        thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            // Subagents outlive runs: their events flow whether or not a job runs.
            let (events, mut feed) = tokio::sync::mpsc::unbounded_channel();
            let agents = Arc::new(Agents::new(Some(agents_dir), Some(events)));
            let agent_out = outbox.clone();
            runtime.spawn(async move {
                while let Some(event) = feed.recv().await {
                    if agent_out.send(Msg::Agent(event)).is_err() {
                        return;
                    }
                }
            });
            while let Ok(job) = inbox.recv() {
                let Job {
                    doc,
                    mut messages,
                    model,
                    effort,
                    cancel,
                    key,
                    window,
                    summarizers,
                    inbox: steer,
                    catalog,
                } = job;
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let (chat_key, chat_model, chat_window) = (key.clone(), model.clone(), window);
                let spec = Spec {
                    cwd,
                    effort,
                    compactor: compactor.clone(),
                    summarizers: summarizers.clone(),
                    model: key.clone(),
                    resolve: Arc::new(move |wanted: &str| {
                        if wanted == chat_key {
                            return Some((chat_model.clone(), chat_window));
                        }
                        let entry = catalog.find(wanted)?;
                        Some((entry.model()?, entry.context))
                    }),
                };
                let host = agents.host(doc.to_string(), spec);
                let agent = Agent::new(model)
                    .effort(effort)
                    .context(key, window, summarizers, compactor.clone())
                    .inbox(steer)
                    .host(host, AGENT_DOCS)
                    .cache_key(format!("smash-{doc}"));
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
    /// Models that write compaction summaries, tried in order; the chat
    /// model is the last resort. ctrl+k in the model picker toggles one.
    #[serde(default)]
    compaction: Vec<String>,
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
            compaction: Vec::new(),
        })
    }

    fn save(&self, path: &std::path::Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}

/// `DeepSeek V4.1 Flash · deepseek · 1M ctx · $0.15/$0.60 · ✻`, with
/// `⟲N` when it is the Nth compaction model.
fn model_label(entry: &Entry, compaction: &[String]) -> String {
    let order = compaction
        .iter()
        .position(|key| *key == entry.key())
        .map(|at| format!(" · ⟲{}", at + 1))
        .unwrap_or_default();
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
        "{} · {} · {context} ctx{price}{thinks}{order}",
        entry.name, entry.provider
    )
}

/// What a subagent is called on screen: the parent's title for it, else the
/// start of its task (agents saved before titles existed).
fn agent_title(info: &AgentInfo) -> String {
    if info.title.trim().is_empty() {
        one_line(&info.task)
    } else {
        one_line(&info.title)
    }
}

/// First line, at most 60 characters.
fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    let mut out: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        out.push('…');
    }
    out
}

/// Percent of the prompt tokens read from the provider's cache.
fn cached_share(usage: &Usage) -> Option<f64> {
    (usage.input > 0).then(|| 100.0 * usage.cache_read as f64 / usage.input as f64)
}

/// Title of the transcript row while compaction runs.
const COMPACTING: &str = "compacting context…";

/// 182400 → `182k`.
fn tokens(n: u64) -> String {
    match n {
        n if n >= 10_000 => format!("{}k", n / 1_000),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1_000.0),
        n => n.to_string(),
    }
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
    /// A message the user typed while the run worked; it joined the run.
    User(String),
    /// A one-line event (compaction, a subagent finishing): drawn like a
    /// program row, but tool calls never attach to it.
    Note {
        title: String,
        summary: String,
    },
    Program {
        title: String,
        /// Calls in start order, with when each started (for running ones).
        calls: Vec<(smash_js::Call, std::time::Instant)>,
        summary: Option<String>,
    },
    /// A subagent: drawn on its own rail, never like the main answer.
    /// `status`: `12s` while running, else `done · 2 turns` and the like;
    /// `activity` shows under it when it has no call to show.
    Agent {
        id: String,
        title: String,
        status: String,
        activity: String,
        calls: Vec<(smash_js::Call, std::time::Instant)>,
    },
}

fn field(text: &str) -> String {
    text.replace(['\t', '\n'], " ")
}

/// One call line of the stored body.
fn call_line(call: &smash_js::Call, started: std::time::Instant) -> String {
    let (mark, info) = match (call.running, call.ok) {
        (true, _) => (
            "…",
            format!("running {:.0}s", started.elapsed().as_secs_f64()),
        ),
        (false, true) => ("✓", call.info.clone()),
        (false, false) => ("✗", call.info.clone()),
    };
    format!(
        "{}{mark}\t{}\t{}\t{}\n",
        crate::CALL,
        field(&call.verb),
        field(&call.target),
        field(&info)
    )
}

/// Segments in the stored-body format; the same parser and renderer draw
/// live runs and saved ones.
fn serialize(segments: &[Segment]) -> String {
    let mut out = String::new();
    for segment in segments {
        match segment {
            Segment::Agent {
                id,
                title,
                status,
                activity,
                calls,
            } => {
                out.push_str(&format!(
                    "{}{}\t{}\t{}\t{}\n",
                    crate::AGENT,
                    field(id),
                    field(title),
                    field(status),
                    field(activity)
                ));
                for (call, started) in calls {
                    out.push_str(&call_line(call, *started));
                }
            }
            Segment::Thinking(text) => {
                for line in text.trim().lines() {
                    out.push_str(&format!("{}{line}\n", crate::THINK));
                }
            }
            Segment::User(text) => out.push_str(&crate::user_turn(text)),
            Segment::Note { title, summary } => out.push_str(&format!(
                "{}{}\t{}\n",
                crate::PROGRAM,
                field(title),
                field(summary)
            )),
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
                    out.push_str(&call_line(call, *started));
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
            Role::Program | Role::Call | Role::Fail | Role::Agent => {}
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
        // Compaction absorbs overflows; reaching here means even the trimmed
        // history didn't fit (a tiny window or a huge system prompt).
        Kind::Overflow => {
            "this model's window is too small even after compacting · ctrl+l a bigger model".into()
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
    /// Learned context limits (shared with the worker), for the ctx meter.
    compactor: Arc<Compactor>,
    /// Share of the working context limit each session's conversation fills.
    context: HashMap<DocId, f64>,
    /// Steering queue of the run in flight.
    steer: Option<Inbox>,
    /// Every animated value (see `motion`), and what the last frame showed,
    /// to tell which state changed.
    motions: Motions,
    seen: Option<Seen>,
    seen_notice: Option<String>,
    seen_pick: Option<(usize, String)>,
    /// The popup as last drawn open, shown while it closes.
    last_popup: Option<Popup>,
    /// The client draws in pixels: scroll by fractions of a row.
    pixels: bool,
    /// Latest state of each session's subagents, and what each did last.
    agents: HashMap<DocId, Vec<AgentInfo>>,
    agent_live: HashMap<(DocId, String), AgentLive>,
    editor: Editor,
    /// The answer in flight, and the session it belongs to.
    live: Vec<Segment>,
    live_doc: Option<DocId>,
    cancel: Option<CancellationToken>,
    /// The data directory (conversations live under it).
    data_dir: PathBuf,
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
        Self::open_in(Files::default_dir())
    }

    /// [`Chat::open`] on another data directory (tests).
    pub fn open_in(dir: PathBuf) -> Self {
        let root = dir.clone();
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
        let dir = root;
        let settings_path = dir.join("settings.json");
        let settings = Settings::load(&settings_path);
        let compactor = Arc::new(Compactor::new(Some(dir.join("limits.json"))));
        Self {
            client,
            worker: Worker::spawn(compactor.clone(), dir.join("agents")),
            compactor,
            context: HashMap::new(),
            steer: None,
            motions: Motions::default(),
            seen: None,
            seen_notice: None,
            seen_pick: None,
            last_popup: None,
            pixels: false,
            agents: HashMap::new(),
            agent_live: HashMap::new(),
            live: Vec::new(),
            live_doc: None,
            cancel: None,
            conversations: HashMap::new(),
            data_dir: dir.clone(),
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

    /// `SMASH_PROMPT`: send this prompt at startup (screenshots, demos).
    pub fn with_startup_prompt(mut self) -> Self {
        if let Ok(prompt) = std::env::var("SMASH_PROMPT")
            && !prompt.trim().is_empty()
        {
            self.paste(&prompt);
            self.send();
        }
        self
    }

    /// Pointer clients (gpui) call this once; enables click targets.
    pub fn with_mouse(mut self) -> Self {
        self.mouse = true;
        self.cache = None;
        self
    }

    /// Pixel clients call this: the transcript scrolls by fractions of a row.
    pub fn with_pixels(mut self) -> Self {
        self.pixels = true;
        self
    }

    /// Snap instead of animating (tests, or a user who asked for it).
    pub fn with_reduced_motion(mut self, reduced: bool) -> Self {
        self.motions.reduced = reduced;
        self
    }

    /// True while something moves or an answer is in flight: clients redraw
    /// every [`Chat::frame_interval`] then.
    pub fn animating(&self) -> bool {
        self.clock.generating() || self.motions.active()
    }

    /// How soon the next frame is due: one display frame while something
    /// moves, a slow tick otherwise.
    pub fn frame_interval(&self) -> std::time::Duration {
        if self.motions.active() {
            motion::FAST
        } else {
            motion::SLOW
        }
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
                Msg::Agent(event) => self.agent_event(event),
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
                ) || self.drafting()
                {
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
            // The program as the model writes it: one row, updated per line.
            AgentUpdate::Drafting { title, lines } => {
                let row = Segment::Program {
                    title: title.unwrap_or_else(|| "writing a program".into()),
                    calls: Vec::new(),
                    summary: Some(format!(
                        "writing · {lines} line{}",
                        if lines == 1 { "" } else { "s" }
                    )),
                };
                if self.drafting() {
                    self.live.pop();
                }
                self.live.push(row);
            }
            AgentUpdate::Program { title } => {
                if self.drafting() {
                    self.live.pop();
                }
                self.live.push(Segment::Program {
                    title,
                    calls: Vec::new(),
                    summary: None,
                });
            }
            // A call shows up when it starts and is updated in place when it ends,
            // so slow commands are visible while they run.
            AgentUpdate::Call(call) => {
                if let Some(Segment::Program { calls, .. }) = self.program_mut() {
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
                if let Some(Segment::Program { summary, calls, .. }) = self.program_mut() {
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
            AgentUpdate::Steered(text) => self.live.push(Segment::User(text)),
            AgentUpdate::Compacting => self.live.push(Segment::Note {
                title: COMPACTING.into(),
                summary: "summarizing".into(),
            }),
            AgentUpdate::Compacted { before, after, by } => {
                let note = Segment::Note {
                    title: format!(
                        "context compacted · {} → {} tokens",
                        tokens(before),
                        tokens(after)
                    ),
                    summary: format!("by {by}"),
                };
                match self
                    .live
                    .iter_mut()
                    .rev()
                    .find(|s| matches!(s, Segment::Note { title, .. } if title == COMPACTING))
                {
                    Some(segment) => *segment = note,
                    None => self.live.push(note),
                }
            }
        }
    }

    fn agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Update { scope, id, update } => {
                let Ok(doc) = scope.parse::<DocId>() else {
                    return;
                };
                let live = self.agent_live.entry((doc, id)).or_default();
                match update {
                    AgentUpdate::Program { title } => {
                        live.program = title;
                        live.calls.clear();
                    }
                    AgentUpdate::Drafting { title, .. } => {
                        live.program = title.unwrap_or_else(|| "writing a program".into());
                        live.calls.clear();
                    }
                    AgentUpdate::Call(call) => {
                        match live.calls.iter_mut().find(|(known, _)| known.id == call.id) {
                            Some((known, _)) => *known = call,
                            None => live.calls.push((call, std::time::Instant::now())),
                        }
                    }
                    AgentUpdate::Thinking(_) => live.program = "thinking".into(),
                    AgentUpdate::Compacting => live.program = COMPACTING.into(),
                    _ => {}
                }
            }
            AgentEvent::Changed { scope, info, cost } => {
                let Ok(doc) = scope.parse::<DocId>() else {
                    return;
                };
                self.cost += cost;
                let list = self.agents.entry(doc).or_default();
                match list.iter_mut().find(|known| known.id == info.id) {
                    Some(known) => *known = info.clone(),
                    None => list.push(info.clone()),
                }
                let key = (doc, info.id.clone());
                if info.status == Status::Running {
                    let live = self.agent_live.entry(key).or_default();
                    live.started = Some(std::time::Instant::now());
                    return;
                }
                self.agent_live.remove(&key);
                // Its outcome stays in that session's transcript: inside the
                // live answer when that is the one in flight, so order is true.
                let price = if info.cost > 0.0 {
                    format!(" · ${:.4}", info.cost)
                } else {
                    String::new()
                };
                let turns = if info.turns == 1 { "turn" } else { "turns" };
                let row = Segment::Agent {
                    id: info.id.clone(),
                    title: agent_title(&info),
                    status: format!("{} · {} {turns}{price}", info.status.label(), info.turns),
                    activity: String::new(),
                    calls: Vec::new(),
                };
                if self.live_doc == Some(doc) {
                    self.live.push(row);
                } else {
                    self.append_to(doc, &serialize(&[row]));
                }
            }
        }
    }

    /// The program row calls belong to: the newest one, skipping notes.
    fn program_mut(&mut self) -> Option<&mut Segment> {
        self.live
            .iter_mut()
            .rev()
            .find(|segment| matches!(segment, Segment::Program { .. }))
    }

    /// The last live row is a program still being written.
    fn drafting(&self) -> bool {
        matches!(
            self.live.last(),
            Some(Segment::Program { summary: Some(summary), calls, .. })
                if calls.is_empty() && summary.starts_with("writing")
        )
    }

    /// The dock: exactly DOCK_ROWS rows per running subagent of `doc`.
    fn dock(&self, doc: DocId, width: usize) -> Vec<Row> {
        let mut rows = Vec::new();
        for segment in self.agent_rows(doc) {
            let Segment::Agent {
                id,
                title,
                status,
                activity,
                mut calls,
            } = segment
            else {
                continue;
            };
            // Only the newest call (else what it is doing): exactly
            // DOCK_ROWS rows per agent, so the dock never changes height
            // while agents work.
            let skip = calls.len().saturating_sub(1);
            calls.drain(..skip);
            let lines = crate::answer_lines(&serialize(&[Segment::Agent {
                id,
                title,
                status,
                activity,
                calls,
            }]));
            let mut laid = layout::turn_rows(&lines, width, None, false, true).rows;
            laid.resize(DOCK_ROWS, Row::new());
            rows.extend(laid);
        }
        // Air between the transcript and the dock.
        if !rows.is_empty() {
            rows.insert(0, Row::new());
        }
        rows
    }

    /// Running subagents of `doc` as live rows: each with its current
    /// program and its last few calls, updated as they run.
    fn agent_rows(&self, doc: DocId) -> Vec<Segment> {
        let Some(list) = self.agents.get(&doc) else {
            return Vec::new();
        };
        list.iter()
            .filter(|info| info.status == Status::Running)
            .map(|info| {
                let live = self.agent_live.get(&(doc, info.id.clone()));
                let secs = live
                    .and_then(|live| live.started)
                    .map_or(0.0, |started| started.elapsed().as_secs_f64());
                let doing = live
                    .map(|live| live.program.as_str())
                    .filter(|program| !program.is_empty())
                    .unwrap_or("starting…");
                let calls = live
                    .map(|live| {
                        let skip = live.calls.len().saturating_sub(AGENT_CALLS);
                        live.calls[skip..].to_vec()
                    })
                    .unwrap_or_default();
                Segment::Agent {
                    id: info.id.clone(),
                    title: agent_title(info),
                    status: format!("{secs:.0}s"),
                    activity: one_line(doing),
                    calls,
                }
            })
            .collect()
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
        // Typed after the run's last turn: it never reached the model.
        let late = self
            .steer
            .take()
            .map(|inbox| smash_backend::drain(&inbox))
            .unwrap_or_default();
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
        let limit = self.compactor.limit(
            &self.settings.model,
            self.entry().map_or(0, |entry| entry.context),
        );
        self.context.insert(
            doc,
            smash_backend::compact::estimate(&messages) as f64 / limit as f64,
        );
        // The agent leaves `messages` valid even after an error (every tool
        // call answered), so a continue picks up with full tool context.
        self.save_conversation(doc, &messages);
        self.conversations.insert(doc, messages);
        if !late.is_empty() && !cancelled && self.client.app().selected().map(|d| d.id) == Some(doc)
        {
            // Whatever the user is typing now waits behind it.
            let draft = self.editor.text().to_string();
            self.editor = Editor::default();
            self.editor.insert(&late.join("\n\n"));
            self.send();
            self.editor.insert(&draft);
        }
    }

    fn conversation_path(&self, doc: DocId) -> PathBuf {
        self.data_dir
            .join("conversations")
            .join(format!("{doc}.json"))
    }

    /// Save `doc`'s exact conversation. ponytail: rewritten whole after each
    /// run; an append-only log if conversations get large.
    fn save_conversation(&self, doc: DocId, messages: &[Message]) {
        let path = self.conversation_path(doc);
        if let Some(dir) = path.parent()
            && std::fs::create_dir_all(dir).is_ok()
            && let Ok(text) = serde_json::to_string(messages)
        {
            let _ = std::fs::write(path, text);
        }
    }

    fn load_conversation(&self, doc: DocId) -> Option<Vec<Message>> {
        let text = std::fs::read_to_string(self.conversation_path(doc)).ok()?;
        serde_json::from_str(&text).ok()
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

    /// The configured compaction models that resolve, then the chat model.
    fn summarizers(&self, chat: Model) -> Vec<Summarizer> {
        let mut out: Vec<Summarizer> = self
            .settings
            .compaction
            .iter()
            .filter(|key| **key != self.settings.model)
            .filter_map(|key| {
                let entry = self.catalog.find(key)?;
                Some(Summarizer {
                    name: key.clone(),
                    model: entry.model()?,
                    window: entry.context,
                })
            })
            .collect();
        out.push(Summarizer {
            name: self.settings.model.clone(),
            model: chat,
            window: self.entry().map_or(0, |entry| entry.context),
        });
        out
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
                label: model_label(entry, &self.settings.compaction),
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

    /// Add the selected model to the compaction models, or take it out.
    fn toggle_compaction(&mut self) {
        let Some(id) = self.picker.selected() else {
            return;
        };
        let Some(key) = self.model_keys.get(id as usize).cloned() else {
            return;
        };
        let list = &mut self.settings.compaction;
        let added = match list.iter().position(|known| *known == key) {
            Some(at) => {
                list.remove(at);
                false
            }
            None => {
                list.push(key.clone());
                true
            }
        };
        self.settings.save(&self.settings_path);
        // Order numbers shift for every model in the list.
        for (index, known) in self.model_keys.iter().enumerate() {
            if let Some(entry) = self.catalog.find(known) {
                self.picker
                    .relabel(index as u64, model_label(entry, &self.settings.compaction));
            }
        }
        let name = self
            .catalog
            .find(&key)
            .map_or(key.clone(), |entry| entry.name.clone());
        self.notice = Some(if added {
            format!(
                "✓ {name} compacts context (⟲{})",
                self.settings.compaction.len()
            )
        } else {
            format!("✓ {name} no longer compacts")
        });
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
            Key::Char('k') if ctrl && self.pick == Pick::Models => self.toggle_compaction(),
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
                    let _ = std::fs::remove_file(self.conversation_path(id));
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
        // One run at a time. Typed into the session that is answering, the
        // message steers it: the agent reads it after its current step.
        if self.clock.generating() {
            match &self.steer {
                Some(inbox) if self.live_doc == viewed && !prompt.is_empty() => {
                    if let Ok(mut queue) = inbox.lock() {
                        queue.push(prompt);
                    }
                    self.editor = Editor::default();
                    self.notice = Some("✓ queued · the agent reads it after this step".into());
                }
                _ => self.notice = Some("still answering · esc stops it".into()),
            }
            return;
        }
        let Some(doc) = self.client.app().selected().map(|doc| doc.id) else {
            return;
        };
        // The exact conversation (tool calls and results included) keeps the
        // provider's prompt cache warm across restarts; the transcript text
        // is the fallback for sessions from before conversations were saved.
        let mut messages = match self.conversations.get(&doc) {
            Some(messages) => messages.clone(),
            None => self
                .load_conversation(doc)
                .unwrap_or_else(|| history(&self.client.view().transcript)),
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
        let inbox = Inbox::default();
        let job = Job {
            doc,
            messages,
            model: model.clone(),
            effort: self.effort(),
            cancel: cancel.clone(),
            key: self.settings.model.clone(),
            window: self.entry().map_or(0, |entry| entry.context),
            summarizers: self.summarizers(model),
            inbox: inbox.clone(),
            catalog: self.catalog.clone(),
        };
        if self.worker.jobs.send(job).is_err() {
            self.notice = Some("agent thread stopped · restart the app".into());
            return;
        }
        self.cancel = Some(cancel);
        self.steer = Some(inbox);
        self.live_doc = Some(doc);
        self.live.clear();
        self.usage = Usage::default();
        self.editor = Editor::default();
        self.scroll = 0;
        self.clock.start();
    }

    /// Lay out one screen of `cols` × `rows` cells.
    pub fn frame(&mut self, cols: usize, rows: usize) -> Frame {
        self.frame_at(cols, rows, std::time::Instant::now())
    }

    /// [`Chat::frame`] at a given time (tests drive the clock).
    pub fn frame_at(&mut self, cols: usize, rows: usize, now: std::time::Instant) -> Frame {
        self.motions.tick(now);
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
        // Out of self for the frame, so animation state can change meanwhile.
        let Some(cache) = self.cache.take() else {
            unreachable!("cache was just filled");
        };

        // Prompt box: at most INPUT_ROWS, scrolled so the caret stays visible.
        let (input, caret) = layout::input(
            &self.editor,
            "type a message",
            width,
            self.mode == Mode::Chat,
        );
        // The box grows and shrinks a row at a time instead of jumping.
        let wanted = input.len().min(INPUT_ROWS);
        let height = self
            .motions
            .follow(Anim::Input, wanted as f64, Spring::GLIDE);
        let shown = (height.round() as usize).clamp(1, wanted.max(1));
        let max_top = input.len() - shown;
        self.input_top = self
            .input_top
            .clamp((caret + 1).saturating_sub(shown), caret)
            .min(max_top);
        let input_total = input.len();
        let input: Vec<_> = input.into_iter().skip(self.input_top).take(shown).collect();
        // Running subagents get a dock above the prompt with a fixed height
        // per agent (badge + newest call), so their progress never shifts
        // the transcript; its size only changes when one starts or ends.
        let viewed_doc = self.client.app().selected().map(|doc| doc.id);
        let dock = viewed_doc
            .map(|doc| self.dock(doc, width))
            .unwrap_or_default();
        let dock_target = dock.len().min(rows / 3);
        let dock_height = self
            .motions
            .follow(Anim::Dock, dock_target as f64, Spring::GLIDE);
        let dock_shown = (dock_height.round() as usize).min(dock.len());
        let viewport = rows.saturating_sub(2 + shown + dock_shown);

        // Live tail: the answer in flight (only in the session that asked),
        // then a spinner row with time, tokens and how to stop.
        let stored = cache.laid.rows.len();
        let mut tail = Laid::default();
        let viewed = self.client.app().selected().map(|doc| doc.id);
        let answering = self.clock.running_secs().is_some() && viewed == self.live_doc;
        if answering {
            let lines = crate::answer_lines(&serialize(&self.live));
            tail = layout::turn_rows(&lines, width, copy, self.settings.thinking, true);
            if !tail.rows.is_empty() {
                tail.rows.push(Row::new());
            }
        }
        if let Some(elapsed) = self.clock.running_secs()
            && viewed == self.live_doc
        {
            let word = spinner_word(elapsed);
            let tokens = if self.usage.output > 0 {
                format!(" · ↓{} tok", self.usage.output)
            } else {
                String::new()
            };
            // How much of what this run sent came from the prompt cache.
            let tokens = match cached_share(&self.usage) {
                Some(share) => format!("{tokens} · {share:.0}% cached"),
                None => tokens,
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
        // Shown scroll glides to `self.scroll`; grid clients round it to rows.
        let offset = self.animate_transcript(viewed, width, total);
        let whole = if self.pixels {
            offset.floor()
        } else {
            offset.round()
        } as usize;
        let shift = if self.pixels {
            (offset - offset.floor()) as f32
        } else {
            0.0
        };
        // History grows upward from the input: pad the top, never the bottom.
        let end = total - whole.min(total);
        let start = end.saturating_sub(viewport);
        let pad = viewport - (end - start);
        let blank = Row::new();
        let content: Vec<&Row> = cache
            .laid
            .rows
            .iter()
            .chain((0..gap).map(|_| &blank))
            .chain(tail.rows.iter())
            .collect();
        let whole_fade = self.motions.value(&Anim::Content).unwrap_or(1.0);
        let faded = |index: usize| {
            let mut row = content[index].clone();
            let t = self.motions.value(&Anim::Row(index)).unwrap_or(1.0) * whole_fade;
            motion::fade(&mut row, NIGHT.bg, t);
            row
        };
        let mut screen: Vec<Row> = vec![Row::new(); pad];
        screen.extend((start..end).map(faded));
        let above = (shift > 0.0 && start > 0).then(|| faded(start - 1));

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

        // The dock unfolds from its top as it grows (oldest agent first).
        for row in dock.into_iter().take(dock_shown) {
            screen.push(row);
            hits.push(None);
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
        self.cache = Some(cache);
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
        let popup = self.animate_popup(cols);
        Frame {
            rows: screen,
            viewport,
            shift,
            above,
            cursors,
            bars,
            popup,
        }
    }

    /// The picker as data (None when closed).
    fn picker_popup(&self, cols: usize) -> Option<Popup> {
        (self.mode == Mode::Picker).then(|| {
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
        })
    }

    /// Shown scroll offset (rows from the bottom), animating every change of
    /// what the transcript shows: scrolling glides, new rows fade in, a
    /// switched session or a relayout fades in as a whole.
    fn animate_transcript(&mut self, doc: Option<DocId>, width: usize, total: usize) -> f64 {
        let now = Seen { doc, width, total };
        let before = self.seen.replace(now);
        match before {
            // Another session or width: a different picture, fade it in.
            Some(before) if before.doc != doc || before.width != width => {
                self.motions.forget_rows();
                self.motions.start(Anim::Content, 0.0, 1.0, Spring::FADE);
                self.motions.snap(Anim::Scroll, self.scroll as f64);
            }
            // Rows arrived (or went): new ones fade in. Scrolled up, the view
            // keeps its place (`scroll` grows with the content) without a jump.
            Some(before) if before.total != total => {
                if total > before.total {
                    for row in before.total..total {
                        self.motions.start(Anim::Row(row), 0.0, 1.0, Spring::FADE);
                    }
                }
                if self.scroll > 0 {
                    let grown = total as isize - before.total as isize;
                    self.scroll = self
                        .scroll
                        .saturating_add_signed(grown)
                        .min(self.max_scroll);
                    self.motions.nudge(Anim::Scroll, grown as f64);
                }
            }
            _ => {}
        }
        self.motions
            .follow(Anim::Scroll, self.scroll as f64, Spring::GLIDE)
            .clamp(0.0, self.max_scroll as f64)
    }

    /// The popup with its opening (or closing) animation applied: rows
    /// unfold top-down, the highlight fades onto the selected item.
    fn animate_popup(&mut self, cols: usize) -> Option<Popup> {
        let current = self.picker_popup(cols);
        let open = f64::from(u8::from(current.is_some()));
        let t = self.motions.follow(Anim::Popup, open, Spring::POP);
        if let Some(popup) = &current {
            if self.motions.value(&Anim::Popup) == Some(open)
                && open > 0.0
                && self.last_popup.is_none()
            {
                // First frame open: start from closed.
                self.motions.start(Anim::Popup, 0.0, 1.0, Spring::POP);
            }
            self.last_popup = Some(popup.clone());
        }
        let t = self.motions.value(&Anim::Popup).unwrap_or(t);
        let mut popup = match current {
            Some(popup) => popup,
            None if t > 0.02 => self.last_popup.clone()?,
            None => {
                self.last_popup = None;
                return None;
            }
        };
        // The highlight follows the selection: a fresh one fades in.
        let pick = (self.picker.index(), self.picker.query.clone());
        if self.mode == Mode::Picker && self.seen_pick.as_ref() != Some(&pick) {
            self.seen_pick = Some(pick);
            self.motions.start(Anim::Pick, 0.0, 1.0, Spring::FADE);
        }
        let lit = self.motions.value(&Anim::Pick).unwrap_or(1.0);
        for row in popup.rows.iter_mut() {
            if row.iter().any(|run| run.paint.bg == Some(NIGHT.surface)) {
                motion::tint(row, NIGHT.bg, lit);
            }
        }
        let fade = t.clamp(0.0, 1.0);
        let keep = motion::reveal(popup.rows.len(), t);
        popup.rows.truncate(keep);
        popup.items.truncate(keep);
        if popup.caret.is_some_and(|(row, _)| row >= keep) {
            popup.caret = None;
        }
        for row in popup.rows.iter_mut() {
            motion::fade(row, NIGHT.bg, fade);
        }
        popup.open = t;
        Some(popup)
    }

    /// The status row and its click zones: each hint presses its key, the
    /// session name opens the picker.
    fn status(&mut self, cols: usize, has_code: bool, view: &smash_core::View) -> (Row, Vec<Zone>) {
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
        // Elapsed time only while answering; the spinner row shows it too,
        // so the bar stays short.
        let clock = String::new();
        let viewed = view.rows.iter().find(|row| row.selected).map(|row| row.id);
        // The meter counts to a new value instead of jumping.
        let context = match viewed.and_then(|doc| self.context.get(&doc).copied()) {
            Some(share) => {
                let shown = self.motions.follow(Anim::Context, share, Spring::COUNT);
                format!(" · ctx {:.0}%", shown * 100.0)
            }
            None => String::new(),
        };
        let info = format!("{session}{model}{context}{clock}");
        // A new notice fades in where the hints were.
        if self.notice != self.seen_notice {
            self.seen_notice = self.notice.clone();
            self.motions.start(Anim::Notice, 0.0, 1.0, Spring::FADE);
        }
        let (row, items, right) = if let Some(notice) = self.notice.clone() {
            let paint = if notice.starts_with('✓') {
                ACCENT
            } else {
                Paint::fg(NIGHT.code)
            };
            let (mut row, items, right) = layout::status_row(&notice, paint, &info, cols);
            let t = self.motions.value(&Anim::Notice).unwrap_or(1.0);
            if let Some(first) = row.first_mut() {
                motion::fade(std::slice::from_mut(first), NIGHT.bg, t);
            }
            (row, items, right)
        } else {
            // Only what matters now: the situational hint first, then the
            // mode's keys, at most HINTS in all.
            let viewed = view.rows.iter().find(|row| row.selected).map(|row| row.id);
            let chat = self.mode == Mode::Chat;
            let mut items: Vec<&str> = Vec::new();
            if self.mode == Mode::Picker && self.pick == Pick::Models {
                items.push("ctrl+k compaction model ⟲");
            }
            if chat && self.failed.is_some() && self.failed == viewed && !typing {
                items.push("enter continue");
            }
            if chat && self.editor.selection().is_some() {
                items.push("ctrl+c copy");
            }
            if chat && self.scroll > 0 && !typing {
                items.push("ctrl+end newest");
            }
            items.extend(hints(self.mode, typing, self.clock.generating()).split(" · "));
            items.truncate(HINTS);
            let _ = has_code;
            layout::status_row(&items.join(" · "), DIM, &info, cols)
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

/// Every state change animates: driven through the real `Chat::frame` on a
/// fake clock, the way both clients call it.
#[cfg(test)]
mod motion_tests {
    use std::time::{Duration, Instant};

    use super::{Chat, Frame, Key, Mods};
    use crate::NIGHT;
    use smash_core::Action;

    const COLS: usize = 60;
    const ROWS: usize = 16;
    const FRAME: Duration = Duration::from_millis(16);

    fn chat() -> (Chat, std::path::PathBuf) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("smash-motion-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (Chat::open_in(dir.clone()).with_reduced_motion(false), dir)
    }

    /// Body of an answer with `n` numbered lines.
    fn lines(n: usize) -> String {
        (0..n).map(|i| format!("line {i}\n\n")).collect()
    }

    fn text(frame: &Frame) -> Vec<String> {
        frame
            .rows
            .iter()
            .map(|row| row.iter().map(|run| run.text.as_str()).collect())
            .collect()
    }

    /// Frames until nothing moves; returns them all.
    fn settle(chat: &mut Chat, t: &mut Instant, max: usize) -> Vec<Frame> {
        let mut frames = Vec::new();
        for _ in 0..max {
            *t += FRAME;
            frames.push(chat.frame_at(COLS, ROWS, *t));
            if !chat.motions.active() {
                return frames;
            }
        }
        panic!("still moving after {max} frames");
    }

    fn row_with<'a>(frame: &'a Frame, needle: &str) -> Option<&'a crate::layout::Row> {
        frame.rows.iter().find(|row| {
            row.iter()
                .map(|run| run.text.as_str())
                .collect::<String>()
                .contains(needle)
        })
    }

    #[test]
    fn the_program_shows_while_it_is_written_then_runs_in_place() {
        use smash_backend::Update;
        let (mut chat, dir) = chat();
        chat.apply(Update::Drafting {
            title: None,
            lines: 1,
        });
        chat.apply(Update::Drafting {
            title: Some("read config".into()),
            lines: 3,
        });
        assert_eq!(chat.live.len(), 1, "one row, updated in place");
        assert!(chat.drafting());
        chat.apply(Update::Program {
            title: "read config".into(),
        });
        assert_eq!(chat.live.len(), 1, "the draft becomes the program");
        assert!(!chat.drafting());
        chat.apply(Update::Compacting);
        chat.apply(Update::Call(smash_js::Call {
            id: 0,
            running: true,
            verb: "sh".into(),
            target: "ls".into(),
            info: String::new(),
            ok: true,
        }));
        let super::Segment::Program { calls, .. } = &chat.live[0] else {
            panic!("program first");
        };
        assert_eq!(
            calls.len(),
            1,
            "calls skip the note and land on the program"
        );
        // A retry drops a half-written draft with the rest of the attempt.
        chat.apply(Update::Drafting {
            title: None,
            lines: 2,
        });
        chat.apply(Update::Attempt);
        assert!(!chat.drafting());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn agent(id: &str, status: smash_backend::agents::Status) -> super::AgentInfo {
        super::AgentInfo {
            id: id.into(),
            title: format!("Title of {id}"),
            task: format!("task of {id}"),
            status,
            report: String::new(),
            turns: 0,
            cost: 0.0,
            model: "m".into(),
            write: false,
        }
    }

    fn call(id: usize, running: bool) -> smash_backend::Update {
        smash_backend::Update::Call(smash_js::Call {
            id,
            running,
            verb: "sh".into(),
            target: format!("step {id}"),
            info: if running { String::new() } else { "ok".into() },
            ok: true,
        })
    }

    #[test]
    fn subagents_live_in_a_fixed_dock_that_never_moves_the_transcript() {
        use smash_backend::agents::{Event, Status};
        let (mut chat, dir) = chat();
        chat.dispatch(Action::Append(lines(40)));
        let doc = chat.client.app().selected().unwrap().id;
        let scope = doc.to_string();
        let changed = |id: &str, status| Event::Changed {
            scope: scope.clone(),
            info: agent(id, status),
            cost: 0.0,
        };
        let mut t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        settle(&mut chat, &mut t, 60);
        chat.agent_event(changed("a1", Status::Running));
        chat.agent_event(changed("a2", Status::Running));
        let frames = settle(&mut chat, &mut t, 60);
        let docked = frames.last().unwrap();
        let dock_rows = ROWS - 2 - docked.viewport - 1;
        assert_eq!(dock_rows, 5, "air + two agents, two rows each");
        let transcript = text(docked)[..docked.viewport].to_vec();
        let dock = text(docked)[docked.viewport + 1..docked.viewport + 5].to_vec();
        assert!(dock[0].starts_with("◆ Title of a1"), "{dock:?}");
        assert!(dock[0].trim_end().ends_with("a1 · 0s"), "{dock:?}");
        assert!(
            dock[1].starts_with("│ starting"),
            "activity on the rail: {dock:?}"
        );
        assert!(
            dock.iter().all(|row| row.trim() != "│"),
            "no empty rail rows: {dock:?}"
        );

        // Dozens of calls: the dock keeps its height, the transcript stands still.
        for step in 0..30 {
            for id in ["a1", "a2"] {
                for running in [true, false] {
                    chat.agent_event(Event::Update {
                        scope: scope.clone(),
                        id: id.into(),
                        update: call(step, running),
                    });
                }
            }
            t += FRAME;
            let frame = chat.frame_at(COLS, ROWS, t);
            let rows = text(&frame);
            assert_eq!(frame.viewport, docked.viewport, "step {step}: height fixed");
            assert_eq!(rows[..frame.viewport], transcript[..], "step {step}: still");
            let newest = format!("step {step}");
            assert!(rows[frame.viewport + 2].contains(&newest), "{rows:?}");
        }

        // One finishes: it leaves the dock (gliding) and gets a row in the chat.
        chat.agent_event(changed("a1", Status::Idle));
        let frames = settle(&mut chat, &mut t, 60);
        let after = frames.last().unwrap();
        assert_eq!(ROWS - 2 - after.viewport - 1, 3, "air + one agent left");
        let all = text(after).join("\n");
        assert!(all.contains("◆ Title of a1"), "{all}");
        assert!(all.contains("a1 · done · 0 turns"), "{all}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exact_conversations_survive_a_restart() {
        use smash_ai::{Content, Message};
        let (mut chat, dir) = chat();
        let doc = chat.client.app().selected().unwrap().id;
        let exact = vec![
            Message::user("go"),
            Message::assistant(vec![Content::ToolCall {
                id: "c1".into(),
                name: "js".into(),
                arguments: serde_json::json!({"title": "List", "code": "return 1"}),
            }]),
            Message::tool("c1", "1"),
            Message::assistant(vec![Content::Text {
                text: "done".into(),
            }]),
        ];
        chat.save_conversation(doc, &exact);
        drop(chat);
        let reopened = Chat::open_in(dir.clone());
        assert_eq!(
            reopened.load_conversation(doc),
            Some(exact),
            "tool calls and results kept"
        );
        assert_eq!(reopened.load_conversation(doc + 99), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn idle_screen_does_not_animate() {
        let (mut chat, dir) = chat();
        let mut t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        for _ in 0..5 {
            t += FRAME;
            chat.frame_at(COLS, ROWS, t);
        }
        assert!(!chat.animating(), "nothing changed, nothing moves");
        assert_eq!(chat.frame_interval(), crate::motion::SLOW);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn new_rows_fade_in_quickly() {
        let (mut chat, dir) = chat();
        let mut t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        chat.dispatch(Action::Append("fresh answer\n".into()));
        t += FRAME;
        let first = chat.frame_at(COLS, ROWS, t);
        let paint = row_with(&first, "fresh answer").expect("row shown at once")[0].paint;
        assert_eq!(paint.fg, NIGHT.bg, "starts invisible: {paint:?}");
        assert!(chat.animating());
        assert_eq!(chat.frame_interval(), crate::motion::FAST);
        let frames = settle(&mut chat, &mut t, 40);
        assert!(
            frames.len() * 16 < 400,
            "settles in {} ms",
            frames.len() * 16
        );
        let colours: Vec<u32> = frames
            .iter()
            .filter_map(|frame| row_with(frame, "fresh answer").map(|row| row[0].paint.fg))
            .collect();
        let brightness = |c: u32| (c >> 16) + ((c >> 8) & 0xff) + (c & 0xff);
        assert!(
            colours
                .windows(2)
                .all(|w| brightness(w[1]) >= brightness(w[0])),
            "{colours:x?}"
        );
        assert_ne!(*colours.last().unwrap(), NIGHT.bg, "ends visible");
        assert!(
            brightness(colours[2]) > brightness(NIGHT.bg),
            "visible by frame 3"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scrolling_glides_and_lands_on_whole_rows() {
        let (mut chat, dir) = chat();
        chat.dispatch(Action::Append(lines(60)));
        let mut t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        settle(&mut chat, &mut t, 60);
        let before = text(&chat.frame_at(COLS, ROWS, t));
        chat.scroll_by(10);
        t += FRAME;
        let first = text(&chat.frame_at(COLS, ROWS, t));
        assert_ne!(first, before, "the first frame already moves");
        let frames = settle(&mut chat, &mut t, 60);
        let end = text(frames.last().unwrap());
        assert!(
            frames.len() > 3,
            "it glides, it doesn't jump ({} frames)",
            frames.len()
        );
        // Same as a direct jump.
        let (mut snap, dir2) = chat_like(&before);
        assert_eq!(end, snap(10), "lands where an instant scroll would");
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    /// A reduced-motion chat with the same content, scrolled by `rows`.
    fn chat_like(_: &[String]) -> (impl FnMut(isize) -> Vec<String>, std::path::PathBuf) {
        let (chat, dir) = chat();
        let mut chat = chat.with_reduced_motion(true);
        chat.dispatch(Action::Append(lines(60)));
        let t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        (
            move |rows| {
                chat.scroll_by(rows);
                text(&chat.frame_at(COLS, ROWS, t))
            },
            dir,
        )
    }

    #[test]
    fn pixel_clients_get_fractions_grid_clients_whole_rows() {
        let (chat_px, dir) = chat();
        let mut chat_px = chat_px.with_pixels();
        let (mut grid, dir2) = chat();
        let mut t = Instant::now();
        for chat in [&mut chat_px, &mut grid] {
            chat.dispatch(Action::Append(lines(60)));
            chat.frame_at(COLS, ROWS, t);
        }
        settle(&mut chat_px, &mut t.clone(), 60);
        settle(&mut grid, &mut t.clone(), 60);
        chat_px.scroll_by(5);
        grid.scroll_by(5);
        t += FRAME * 4;
        let mut fractions = 0;
        for _ in 0..20 {
            t += FRAME;
            let a = chat_px.frame_at(COLS, ROWS, t);
            let b = grid.frame_at(COLS, ROWS, t);
            assert!((0.0..1.0).contains(&a.shift));
            fractions += usize::from(a.shift > 0.0);
            assert_eq!(b.shift, 0.0);
            assert!(b.above.is_none());
            if a.shift > 0.0 {
                assert!(a.above.is_some(), "the row above peeks in");
            }
        }
        assert!(fractions > 0, "pixel scrolling moved by fractions");
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    #[test]
    fn retargeting_mid_scroll_is_continuous() {
        let (chat, dir) = chat();
        let mut chat = chat.with_pixels();
        chat.dispatch(Action::Append(lines(80)));
        let mut t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        settle(&mut chat, &mut t, 60);
        chat.scroll_by(20);
        let mut offsets = Vec::new();
        for frame in 0..30 {
            if frame == 4 {
                chat.scroll_by(-20);
            }
            t += FRAME;
            chat.frame_at(COLS, ROWS, t);
            offsets.push(chat.motions.value(&super::Anim::Scroll).unwrap());
        }
        let jumps: Vec<f64> = offsets.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        let fastest_before = jumps[..3].iter().cloned().fold(0.0, f64::max);
        let fastest_after = jumps[3..].iter().cloned().fold(0.0, f64::max);
        assert!(
            fastest_after <= fastest_before,
            "turning adds no jump: {offsets:?}"
        );
        assert!(
            offsets[4] > offsets[3],
            "still moving up right after the turn"
        );
        assert!(*offsets.last().unwrap() < 0.01, "returns to the bottom");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn new_content_while_scrolled_up_keeps_the_view_still() {
        let (mut chat, dir) = chat();
        chat.dispatch(Action::Append(lines(60)));
        let mut t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        settle(&mut chat, &mut t, 60);
        chat.scroll_by(12);
        settle(&mut chat, &mut t, 60);
        let before = text(&chat.frame_at(COLS, ROWS, t));
        chat.dispatch(Action::Append("a new answer below\n".into()));
        t += FRAME;
        let after = text(&chat.frame_at(COLS, ROWS, t));
        assert_eq!(
            after[..ROWS - 3],
            before[..ROWS - 3],
            "reading position unchanged"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn switching_sessions_fades_the_transcript_in() {
        let (mut chat, dir) = chat();
        chat.dispatch(Action::Append("first session text\n".into()));
        let mut t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        settle(&mut chat, &mut t, 60);
        chat.key(
            Key::Char('n'),
            Mods {
                ctrl: true,
                ..Mods::default()
            },
        );
        chat.dispatch(Action::Append("second session text\n".into()));
        t += FRAME;
        let first = chat.frame_at(COLS, ROWS, t);
        let row = row_with(&first, "second session text").expect("shown");
        assert_eq!(row[0].paint.fg, NIGHT.bg, "fades in from the background");
        let frames = settle(&mut chat, &mut t, 60);
        let last = row_with(frames.last().unwrap(), "second session text").unwrap();
        assert_ne!(last[0].paint.fg, NIGHT.bg);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn popup_unfolds_with_a_pop_and_folds_away() {
        let (mut chat, dir) = chat();
        for _ in 0..3 {
            chat.dispatch(Action::New);
        }
        let mut t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        settle(&mut chat, &mut t, 60);
        chat.key(
            Key::Char('p'),
            Mods {
                ctrl: true,
                ..Mods::default()
            },
        );
        t += FRAME;
        let first = chat
            .frame_at(COLS, ROWS, t)
            .popup
            .expect("visible from the first frame");
        assert!(first.open < 0.2, "starts nearly closed: {}", first.open);
        let full = super::PICKER_ROWS.min(4) + 1;
        assert!(
            first.rows.len() < full,
            "rows unfold ({} of {full})",
            first.rows.len()
        );
        let frames = settle(&mut chat, &mut t, 60);
        let opens: Vec<f64> = frames
            .iter()
            .map(|f| f.popup.as_ref().unwrap().open)
            .collect();
        let peak = opens.iter().cloned().fold(0.0, f64::max);
        assert!(peak > 1.01 && peak < 1.1, "a small overshoot: {peak}");
        assert!(frames.len() * 16 < 450, "{} ms", frames.len() * 16);
        let open = frames.last().unwrap().popup.as_ref().unwrap();
        assert_eq!(open.open, 1.0);
        assert!(open.rows.len() >= 4, "all rows once open");

        chat.key(Key::Esc, Mods::default());
        t += FRAME;
        let closing = chat
            .frame_at(COLS, ROWS, t)
            .popup
            .expect("still drawn while closing");
        assert!(closing.open > 0.5);
        let frames = settle(&mut chat, &mut t, 60);
        assert!(frames.last().unwrap().popup.is_none(), "gone once closed");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn moving_the_selection_fades_the_highlight_onto_the_new_item() {
        let (mut chat, dir) = chat();
        for _ in 0..3 {
            chat.dispatch(Action::New);
        }
        let mut t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        chat.key(
            Key::Char('p'),
            Mods {
                ctrl: true,
                ..Mods::default()
            },
        );
        settle(&mut chat, &mut t, 60);
        let before = chat.picker.index();
        chat.key(Key::Up, Mods::default());
        if chat.picker.index() == before {
            chat.key(Key::Down, Mods::default());
        }
        assert_ne!(chat.picker.index(), before, "the selection moved");
        t += FRAME;
        let popup = chat.frame_at(COLS, ROWS, t).popup.unwrap();
        let band = |popup: &crate::popup::Popup| {
            popup
                .rows
                .iter()
                .find(|row| row[0].text.starts_with('▌'))
                .and_then(|row| row[1].paint.bg)
        };
        assert_eq!(
            band(&popup),
            Some(NIGHT.bg),
            "fresh highlight starts at the background"
        );
        let frames = settle(&mut chat, &mut t, 60);
        assert_eq!(
            band(frames.last().unwrap().popup.as_ref().unwrap()),
            Some(NIGHT.surface)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn notices_fade_in_and_the_context_meter_counts() {
        let (mut chat, dir) = chat();
        let mut t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        chat.notice("✓ saved");
        t += FRAME;
        let status = chat.frame_at(COLS, ROWS, t).rows.last().unwrap().clone();
        assert!(status[0].text.contains("saved"));
        assert_eq!(status[0].paint.fg, NIGHT.bg, "notice starts invisible");
        let frames = settle(&mut chat, &mut t, 60);
        assert_eq!(
            frames.last().unwrap().rows.last().unwrap()[0].paint.fg,
            crate::layout::ACCENT.fg
        );

        let doc = chat.client.app().selected().unwrap().id;
        chat.context.insert(doc, 0.2);
        chat.notice = None;
        t += FRAME;
        chat.frame_at(COLS, ROWS, t);
        settle(&mut chat, &mut t, 60);
        chat.context.insert(doc, 0.8);
        let mut shown = Vec::new();
        for _ in 0..40 {
            t += FRAME;
            let frame = chat.frame_at(COLS, ROWS, t);
            let status: String = frame
                .rows
                .last()
                .unwrap()
                .iter()
                .map(|r| r.text.as_str())
                .collect();
            let at = status.find("ctx ").expect(&status) + 4;
            let percent: u32 = status[at..].split('%').next().unwrap().parse().unwrap();
            shown.push(percent);
        }
        assert_eq!(
            shown[0], 20,
            "the change starts from the old value: {shown:?}"
        );
        assert!(shown[2] > 20 && shown[2] < 80, "counts through: {shown:?}");
        assert!(shown.windows(2).all(|w| w[1] >= w[0]), "{shown:?}");
        assert_eq!(*shown.last().unwrap(), 80);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_prompt_box_grows_a_row_at_a_time() {
        let (mut chat, dir) = chat();
        let mut t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        chat.paste("one\ntwo\nthree\nfour\nfive");
        let mut heights = Vec::new();
        for _ in 0..30 {
            t += FRAME;
            let frame = chat.frame_at(COLS, ROWS, t);
            heights.push(ROWS - 2 - frame.viewport);
        }
        assert_eq!(heights[0], 1, "starts at its old height: {heights:?}");
        assert!(
            heights.windows(2).all(|w| w[1] >= w[0] && w[1] - w[0] <= 2),
            "{heights:?}"
        );
        assert_eq!(*heights.last().unwrap(), 5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reduced_motion_shows_every_end_state_at_once() {
        let (chat, dir) = chat();
        let mut chat = chat.with_reduced_motion(true);
        chat.dispatch(Action::Append(lines(40)));
        let t = Instant::now();
        chat.frame_at(COLS, ROWS, t);
        chat.dispatch(Action::Append("new row\n".into()));
        chat.key(
            Key::Char('p'),
            Mods {
                ctrl: true,
                ..Mods::default()
            },
        );
        chat.notice("✓ hi");
        let frame = chat.frame_at(COLS, ROWS, t);
        assert!(!chat.motions.active());
        assert_eq!(frame.popup.unwrap().open, 1.0);
        assert_ne!(frame.rows.last().unwrap()[0].paint.fg, NIGHT.bg);
        let _ = std::fs::remove_dir_all(dir);
    }
}
