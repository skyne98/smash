//! Agent loop: stream a turn, run the model's code, repeat until it answers.
//!
//! Tools are code (see [`smash_js`]): the model gets ONE tool, `js`, whose
//! argument is a JavaScript program where read/write/edit/ls/sh are async
//! functions. One model turn can then read five files in parallel, patch two,
//! run the tests and return only the failures, instead of the five-to-ten
//! round trips a one-action-per-call agent needs (Tura's measured win).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use smash_ai::{
    Content, Context, Cost, Effort, Error, Event, Kind, Message, Model, Role, StopReason, Tool,
    Usage,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::compact::{self, Compactor, Summarizer};

/// Tool loop guard: models that keep calling tools get cut off, not looped forever.
const MAX_TURNS: usize = 24;

/// Program output kept in the conversation; anything longer is saved to a
/// file and the model gets its head, tail and the path. Output is never
/// changed after it is sent: rewriting history breaks the prompt cache, and a
/// cached token costs a fiftieth of a fresh one.
const RESULT_CHARS: usize = 6_000;

/// Tries per model call for transient failures (rate limit, overload, network).
const MAX_ATTEMPTS: u32 = 6;
/// Tries when the model answers with nothing at all.
const EMPTY_ATTEMPTS: u32 = 3;
/// Compact-and-retry rounds after context overflow errors in one call; each
/// one lowers the learned limit and compacts harder, the last trims.
const OVERFLOW_ATTEMPTS: u8 = 4;
/// No byte from the provider for this long = stalled (reasoning models stream
/// their thinking, so silence this long is not normal).
const IDLE: Duration = Duration::from_secs(120);

/// IDLE, shortened by `SMASH_IDLE_SECS` (fault tests).
fn idle() -> Duration {
    std::env::var("SMASH_IDLE_SECS")
        .ok()
        .and_then(|secs| secs.parse().ok())
        .map_or(IDLE, Duration::from_secs)
}

pub const SYSTEM: &str = "You are a coding assistant working in the user's project directory.
Answer directly; use code only when the task needs the filesystem or a shell.

You have one tool, `js`: you write the body of an async JavaScript function and it runs at once.
Inside it these async functions exist (paths relative to the project root):
{TOOLS}
Plain JS only: no require/import/fetch/process. Use sh for anything else.

Make every call count. One program should do all work whose inputs you already know:
- independent reads/searches together: await Promise.all([read(a), read(b), sh('rg -n foo src')])
- dependent work in order with await; a thrown error stops the program
- edit, then verify in the same program: const t = await sh('cargo test -q'); if (t.code) return t.out
- return or console.log only what you need to see next, not whole files you won't use
- keep commands fast: never scan whole disks or home directories (du/find over ~ or /);
  bound them (du -sh -d1 dir, find dir -maxdepth 2) and pass {timeout} for anything slow
Give every program a title: 2 to 6 words saying what it does, like a commit subject ('Find the auth middleware', 'Run the failing tests'). The user sees the title, not the code.";

/// Everything the UI shows while a run is in flight.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// A model call starts; output after this belongs to it (dropped on retry).
    Attempt,
    Thinking(String),
    Text(String),
    /// The model is still writing a `js` program: its title once the first
    /// line is in, and how many lines so far.
    Drafting {
        title: Option<String>,
        lines: usize,
    },
    /// A `js` program starts.
    Program {
        title: String,
    },
    /// One tool call inside the running program finished.
    Call(smash_js::Call),
    /// The program finished.
    ProgramDone {
        calls: usize,
        secs: f64,
        error: Option<String>,
    },
    /// A model call failed transiently; the same call repeats after `secs`.
    Retry {
        attempt: u32,
        max: u32,
        secs: f64,
        reason: String,
    },
    /// A model turn ended; usage so far.
    Usage(Usage),
    /// The history is being compacted (the next call waits for it).
    Compacting,
    /// A queued message (see [`Inbox`]) joined the conversation as a user turn.
    Steered(String),
    /// The history was compacted from about `before` to `after` tokens.
    Compacted {
        before: u64,
        after: u64,
        /// Summarizer that wrote the summary (`digest`/`trim` = no model).
        by: String,
    },
}

pub struct Agent {
    client: smash_ai::Client,
    pub model: Model,
    pub system: String,
    pub max_tokens: Option<u32>,
    pub effort: Option<Effort>,
    pub cwd: PathBuf,
    /// Key for learned limits (`provider/model`).
    pub key: String,
    /// The model's context window in tokens; 0 = unknown.
    pub window: u64,
    /// Models that write compaction summaries, tried in order.
    pub summarizers: Vec<Summarizer>,
    /// Shared across runs: background summaries and learned limits.
    pub compactor: Arc<Compactor>,
    /// Messages to steer the run: each becomes a user turn between model
    /// turns, and keeps the run going if it was about to end.
    pub inbox: Inbox,
    pub max_turns: usize,
    /// Extra functions for the `js` tool (subagents).
    pub host: Option<Arc<dyn smash_js::Host>>,
    /// `write`/`edit` refuse.
    pub read_only: bool,
    /// Groups this conversation's requests on one provider cache (one per
    /// session; its subagents share it, their prefix is the same).
    pub cache_key: Option<String>,
}

/// Steering queue shared between an [`Agent`] run and whoever steers it.
pub type Inbox = Arc<std::sync::Mutex<Vec<String>>>;

/// Everything queued in `inbox`, oldest first.
pub fn drain(inbox: &Inbox) -> Vec<String> {
    inbox
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

/// Outcome of [`Agent::run`], summed over every turn.
#[derive(Clone, Debug)]
pub struct Run {
    pub message: Message,
    pub stop: StopReason,
    pub usage: Usage,
    pub cost: Cost,
    pub turns: usize,
}

fn js_tool() -> Tool {
    Tool::new(
        "js",
        "Run an async JavaScript program with read/write/edit/ls/sh. Returns console output and the return value.",
        json!({
            "type": "object",
            // title first: models emit keys in schema order, so the UI can
            // show the name while the code is still streaming.
            "properties": {
                "title": { "type": "string", "description": "What this program does, 2-6 words, e.g. \"Find the auth middleware\". Shown to the user." },
                "code": { "type": "string", "description": "Body of an async function." },
            },
            "required": ["title", "code"],
        }),
    )
}

fn cancelled() -> Error {
    Error::new(Kind::Cancelled, "cancelled")
}

/// One finished model call.
struct Reply {
    message: Message,
    stop: StopReason,
    usage: Usage,
    cost: Cost,
}

impl Agent {
    pub fn new(model: Model) -> Self {
        Self {
            client: smash_ai::Client::new(),
            model,
            system: SYSTEM.replace("{TOOLS}", smash_js::TOOLS),
            max_tokens: None,
            effort: None,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            key: String::new(),
            window: 0,
            summarizers: Vec::new(),
            compactor: Arc::default(),
            inbox: Inbox::default(),
            max_turns: MAX_TURNS,
            host: None,
            read_only: false,
            cache_key: None,
        }
    }

    /// Route this conversation's requests to one prompt cache.
    pub fn cache_key(mut self, key: impl Into<String>) -> Self {
        self.cache_key = Some(key.into());
        self
    }

    /// Steer this agent through `inbox`.
    pub fn inbox(mut self, inbox: Inbox) -> Self {
        self.inbox = inbox;
        self
    }

    /// Extra `js` functions; `docs` lists them for the model.
    pub fn host(mut self, host: Arc<dyn smash_js::Host>, docs: &str) -> Self {
        self.host = Some(host);
        self.system.push_str("\n\n");
        self.system.push_str(docs);
        self
    }

    /// Queued steering messages become user turns; true if there were any.
    fn steer(&self, messages: &mut Vec<Message>, updates: &UnboundedSender<Update>) -> bool {
        let queued = drain(&self.inbox);
        for text in &queued {
            send(updates, Update::Steered(text.clone()));
        }
        if queued.is_empty() {
            return false;
        }
        // Consecutive user turns merge: strict providers reject two in a row.
        let text = queued.join("\n\n");
        match messages.last_mut() {
            Some(last) if last.role == Role::User => {
                last.content.push(Content::Text {
                    text: format!("\n\n{text}"),
                });
            }
            _ => messages.push(Message::user(text)),
        }
        true
    }

    /// Context handling: limits are learned under `key`, `window` is the
    /// model's context size, `summarizers` write compaction summaries.
    pub fn context(
        mut self,
        key: impl Into<String>,
        window: u64,
        summarizers: Vec<Summarizer>,
        compactor: Arc<Compactor>,
    ) -> Self {
        self.key = key.into();
        self.window = window;
        self.summarizers = summarizers;
        self.compactor = compactor;
        self
    }

    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = system.into();
        self
    }

    pub fn effort(mut self, effort: Option<Effort>) -> Self {
        self.effort = effort;
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    /// Runs the loop, appending every message (assistant and tool) to
    /// `messages`. On any error `messages` stays a valid conversation (every
    /// tool call answered), so the same run can simply be continued.
    pub async fn run(
        &self,
        messages: &mut Vec<Message>,
        updates: &UnboundedSender<Update>,
        cancel: &CancellationToken,
    ) -> Result<Run, Error> {
        let mut usage = Usage::default();
        let mut cost = Cost::default();

        // Messages queued before the run started go first.
        self.steer(messages, updates);
        for turns in 1..=self.max_turns {
            let reply = self.call(messages, updates, cancel).await?;
            usage.input += reply.usage.input;
            usage.output += reply.usage.output;
            usage.cache_read += reply.usage.cache_read;
            usage.cache_write += reply.usage.cache_write;
            cost.input += reply.cost.input;
            cost.output += reply.cost.output;
            cost.total += reply.cost.total;
            send(updates, Update::Usage(usage));

            let message = reply.message;
            messages.push(message.clone());
            let calls: Vec<(String, String, Value)> = message
                .tool_calls()
                .map(|(id, name, arguments)| (id.to_string(), name.to_string(), arguments.clone()))
                .collect();
            // A steering message that arrived meanwhile keeps the run going.
            if calls.is_empty() && !self.steer(messages, updates) {
                return Ok(Run {
                    message,
                    stop: reply.stop,
                    usage,
                    cost,
                    turns,
                });
            }
            // Every call gets an answer, even after a stop, so the history stays valid.
            for (id, name, arguments) in calls {
                let result = if cancel.is_cancelled() {
                    "error: cancelled by the user before this call ran".to_string()
                } else {
                    self.program(&name, &arguments, updates, cancel).await
                };
                messages.push(Message::tool(id, result));
            }
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            self.steer(messages, updates);
        }

        Err(Error::new(
            Kind::Other,
            format!(
                "stopped after {} tool rounds without a final answer · say \"continue\" to keep going",
                self.max_turns
            ),
        ))
    }

    /// What one model call sends.
    fn request(&self, messages: &[Message]) -> Context {
        Context {
            system: Some(self.system.clone()),
            messages: messages.to_vec(),
            tools: vec![js_tool()],
            max_tokens: self.max_tokens,
            temperature: None,
            effort: self.effort,
            cache_key: self.cache_key.clone(),
        }
    }

    /// Tokens every call carries besides the messages: system prompt, tool
    /// schema, and room for the reply.
    fn fixed(&self) -> u64 {
        let reply = self
            .max_tokens
            .map_or_else(|| (self.limit() / 8).min(8_192), u64::from);
        compact::estimate_text(&self.system) + 300 + reply
    }

    fn limit(&self) -> u64 {
        self.compactor.limit(&self.key, self.window)
    }

    /// This conversation's request shape, for summaries that reuse its cache.
    fn prefix(&self) -> compact::Prefix {
        compact::Prefix {
            key: self.key.clone(),
            system: self.system.clone(),
            tools: vec![js_tool()],
            effort: self.effort,
            cache_key: self.cache_key.clone(),
            room: self.limit().saturating_sub(self.fixed()),
        }
    }

    /// Keep `messages` under the limit before a call: start a background
    /// summary past PREPARE, swap it in past SWAP.
    async fn fit(&self, messages: &mut Vec<Message>, updates: &UnboundedSender<Update>) {
        let limit = self.limit() as f64;
        let used = (compact::estimate(messages) + self.fixed()) as f64;
        if used >= compact::SWAP * limit {
            self.shrink(messages, updates, 0).await;
        } else if used >= compact::PREPARE * limit {
            self.compactor.prepare(
                messages,
                &self.summarizers,
                &self.client,
                Some(&self.prefix()),
            );
        }
    }

    /// Compact `messages` to TARGET of the limit, starting at `level`.
    async fn shrink(
        &self,
        messages: &mut Vec<Message>,
        updates: &UnboundedSender<Update>,
        level: u8,
    ) {
        send(updates, Update::Compacting);
        let target = (compact::TARGET * self.limit() as f64) as u64;
        let outcome = self
            .compactor
            .shrink(
                messages,
                target,
                self.fixed(),
                level,
                &self.summarizers,
                &self.client,
                Some(&self.prefix()),
            )
            .await;
        send(
            updates,
            Update::Compacted {
                before: outcome.before,
                after: outcome.after,
                by: outcome.by,
            },
        );
    }

    /// One model turn, retried on transient failures with backoff (honoring
    /// the provider's retry-after) and on empty replies. A context overflow
    /// never reaches the caller while compaction can still help: the limit is
    /// learned, the history compacted, and the call sent again.
    async fn call(
        &self,
        messages: &mut Vec<Message>,
        updates: &UnboundedSender<Update>,
        cancel: &CancellationToken,
    ) -> Result<Reply, Error> {
        self.fit(messages, updates).await;
        let mut context = self.request(messages);
        let mut attempt = 0;
        let mut overflows = 0;
        loop {
            attempt += 1;
            send(updates, Update::Attempt);
            let error = match self.attempt(&context, updates, cancel).await {
                Ok(reply) if !empty(&reply.message) => return Ok(reply),
                Ok(_) => Error::new(Kind::Empty, "the model returned an empty reply"),
                Err(error) => error,
            };
            if error.kind == Kind::Overflow && overflows < OVERFLOW_ATTEMPTS {
                let sent = compact::estimate(&context.messages) + self.fixed();
                self.compactor.learn(&self.key, &error, sent);
                // First the background summary if one exists, then harder.
                self.shrink(messages, updates, overflows.min(2)).await;
                overflows += 1;
                attempt -= 1;
                context = self.request(messages);
                continue;
            }
            let limit = if error.kind == Kind::Empty {
                EMPTY_ATTEMPTS
            } else {
                MAX_ATTEMPTS
            };
            if !error.retryable() || attempt >= limit {
                return Err(error);
            }
            let wait = backoff(&error, attempt);
            send(
                updates,
                Update::Retry {
                    attempt: attempt + 1,
                    max: limit,
                    secs: wait.as_secs_f64(),
                    reason: reason(&error),
                },
            );
            tokio::select! {
                () = cancel.cancelled() => return Err(cancelled()),
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// Stream one model call. A stall (no data for IDLE) or a stream that
    /// ends before its done event is a retryable error.
    async fn attempt(
        &self,
        context: &Context,
        updates: &UnboundedSender<Update>,
        cancel: &CancellationToken,
    ) -> Result<Reply, Error> {
        let mut stream = self.client.stream(&self.model, context);
        let idle = idle();
        // Tool-call arguments as they stream, for the "writing" row.
        let mut drafts: Vec<String> = Vec::new();
        let mut drafted = (None, 0);
        loop {
            let next = tokio::select! {
                () = cancel.cancelled() => return Err(cancelled()),
                next = tokio::time::timeout(idle, stream.next()) => next,
            };
            let event = match next {
                Err(_) => {
                    return Err(Error::new(
                        Kind::Stalled,
                        format!("no data from the provider for {}s", idle.as_secs()),
                    ));
                }
                Ok(None) => {
                    return Err(Error::new(Kind::Network, "the response stream ended early"));
                }
                Ok(Some(event)) => event?,
            };
            match event {
                Event::ThinkingDelta { delta, .. } => send(updates, Update::Thinking(delta)),
                Event::TextDelta { delta, .. } => send(updates, Update::Text(delta)),
                Event::ToolCallDelta { index, delta } => {
                    if drafts.len() <= index {
                        drafts.resize(index + 1, String::new());
                    }
                    drafts[index].push_str(&delta);
                    let code = draft_field(&drafts[index], "code");
                    let lines = code.lines().count().max(1);
                    // The model's own title; a leading comment until it comes.
                    let title = Some(draft_field(&drafts[index], "title"))
                        .filter(|title| !title.trim().is_empty())
                        .or_else(|| {
                            code.contains('\n')
                                .then(|| smash_js::title(&code))
                                .flatten()
                        });
                    // One update per new line, not per token.
                    if (title.clone(), lines) != drafted {
                        drafted = (title.clone(), lines);
                        send(updates, Update::Drafting { title, lines });
                    }
                }
                Event::Done {
                    message,
                    stop,
                    usage,
                    cost,
                } => {
                    return Ok(Reply {
                        message,
                        stop,
                        usage,
                        cost,
                    });
                }
                _ => {}
            }
        }
    }

    /// Run one tool call. Anything the model sends is accepted when its
    /// intent is clear; otherwise the error says exactly how to call.
    async fn program(
        &self,
        name: &str,
        arguments: &Value,
        updates: &UnboundedSender<Update>,
        cancel: &CancellationToken,
    ) -> String {
        let (code, note) = match code_for(name, arguments) {
            Ok(found) => found,
            Err(error) => {
                send(
                    updates,
                    Update::Program {
                        title: format!("{name} (rejected)"),
                    },
                );
                send(
                    updates,
                    Update::ProgramDone {
                        calls: 0,
                        secs: 0.0,
                        error: Some(first_line(&error)),
                    },
                );
                return format!("error: {error}");
            }
        };
        send(
            updates,
            Update::Program {
                title: program_title(name, arguments, &code),
            },
        );
        let (log, mut calls) = tokio::sync::mpsc::unbounded_channel();
        let forward = {
            let updates = updates.clone();
            async move {
                while let Some(call) = calls.recv().await {
                    send(&updates, Update::Call(call));
                }
            }
        };
        let limits = smash_js::Limits {
            read_only: self.read_only,
            ..smash_js::Limits::default()
        };
        let run = smash_js::run_with(&code, &self.cwd, limits, log, self.host.clone());
        let started = std::time::Instant::now();
        let outcome = tokio::select! {
            () = cancel.cancelled() => {
                send(updates, Update::ProgramDone {
                    calls: 0,
                    secs: started.elapsed().as_secs_f64(),
                    error: Some("stopped by the user".into()),
                });
                return "error: cancelled by the user while the program ran; calls that finished kept their effects".into();
            }
            (outcome, ()) = async { tokio::join!(run, forward) } => outcome,
        };
        send(
            updates,
            Update::ProgramDone {
                calls: outcome.calls,
                secs: outcome.elapsed.as_secs_f64(),
                error: outcome.error.as_deref().map(first_line),
            },
        );
        let text = spill(&outcome.text, &self.spill_dir());
        match note {
            Some(note) => format!("{note}\n{text}"),
            None => text,
        }
    }

    /// Where long program output is saved: `$TMPDIR/smash-output`.
    fn spill_dir(&self) -> PathBuf {
        std::env::temp_dir().join("smash-output")
    }
}

/// `text` as the model keeps it for the rest of the conversation: whole when
/// short, else head and tail plus the path of a file with all of it.
fn spill(text: &str, dir: &std::path::Path) -> String {
    let count = text.chars().count();
    if count <= RESULT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(RESULT_CHARS * 2 / 3).collect();
    let tail: String = text.chars().skip(count - RESULT_CHARS / 3).collect();
    let mut hasher = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(text, &mut hasher);
    let path = dir.join(format!("{:016x}.txt", std::hash::Hasher::finish(&hasher)));
    let saved = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, text));
    let whole = match saved {
        Ok(()) => format!(
            "full output ({count} characters) in {}; read it with read()",
            path.display()
        ),
        Err(_) => format!("{} characters cut", count - RESULT_CHARS),
    };
    format!("{head}\n…[{whole}]…\n{tail}")
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(160)
        .collect()
}

/// Nothing to show and nothing to do: no text and no tool call.
fn empty(message: &Message) -> bool {
    message.content.iter().all(|content| match content {
        Content::Text { text } => text.trim().is_empty(),
        Content::Thinking { .. } => true,
        Content::ToolCall { .. } => false,
    })
}

/// 1, 2, 4, 8, 16 s (+ up to 25 % jitter), or what the provider asked for,
/// capped at a minute.
fn backoff(error: &Error, attempt: u32) -> Duration {
    if let Some(secs) = error.retry_after {
        return Duration::from_secs_f64(secs.clamp(0.5, 60.0));
    }
    let base = 2f64.powi(attempt.saturating_sub(1) as i32).min(30.0);
    let jitter = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis())
        .unwrap_or(0)
        % 250) as f64
        / 1000.0;
    Duration::from_secs_f64(base * (1.0 + jitter))
}

fn reason(error: &Error) -> String {
    let status = error
        .status
        .map(|code| format!(" ({code})"))
        .unwrap_or_default();
    match error.kind {
        Kind::RateLimit => format!("rate limited{status}"),
        Kind::Overloaded => format!("provider overloaded{status}"),
        Kind::Network => "network error".into(),
        Kind::Stalled => "provider went silent".into(),
        Kind::Empty => "empty reply".into(),
        _ => error.message.clone(),
    }
}

/// The JS program for a tool call. Models sometimes call the functions as
/// tools (`sh`, `read_file`), rename the argument, or send the code as a
/// bare string; all of that runs, with a note so they learn the real shape.
fn code_for(name: &str, args: &Value) -> Result<(String, Option<String>), String> {
    let text = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| args.get(*key).and_then(Value::as_str))
            .map(str::to_string)
    };
    let quote = |value: &str| serde_json::to_string(value).unwrap_or_default();
    let hint = "The only tool is js: call it with {\"code\": \"...\"} and use read/write/edit/ls/sh inside, e.g. return await read('Cargo.toml')";
    let note = |as_js: &str| {
        Some(format!(
            "(note: there is only the js tool; your {name} call ran as: {as_js})"
        ))
    };
    let need = |what: &str| format!("{name} needs {what}. {hint}");
    match name {
        "js" | "javascript" | "code" | "run" | "eval" | "script" => {
            if let Some(code) = args.as_str() {
                return Ok((code.to_string(), None));
            }
            if args.is_null() {
                return Err("the js call's arguments were not valid JSON, most likely cut off at the output limit. Send a shorter program; write big files in several smaller calls.".into());
            }
            let code = text(&["code", "script", "source", "program", "js", "input", "body"])
                .ok_or_else(|| {
                    "js needs {\"code\": \"<body of an async function>\"}".to_string()
                })?;
            Ok((code, None))
        }
        "sh" | "bash" | "shell" | "exec" | "run_command" | "terminal" | "command" => {
            let cmd = text(&["cmd", "command", "script", "code", "input"])
                .ok_or_else(|| need("{\"cmd\": \"...\"}"))?;
            let code = format!("return await sh({})", quote(&cmd));
            Ok((code.clone(), note(&code)))
        }
        "read" | "read_file" | "cat" | "view" => {
            let path = text(&["path", "file", "file_path", "filename"])
                .ok_or_else(|| need("{\"path\": \"...\"}"))?;
            let code = format!("return await read({})", quote(&path));
            Ok((code.clone(), note(&code)))
        }
        "ls" | "list_dir" | "list" | "list_files" => {
            let path = text(&["path", "dir", "directory"]).unwrap_or_else(|| ".".into());
            let code = format!("return await ls({})", quote(&path));
            Ok((code.clone(), note(&code)))
        }
        "write" | "write_file" | "create_file" => {
            let path = text(&["path", "file", "file_path"])
                .ok_or_else(|| need("{\"path\", \"content\"}"))?;
            let content = text(&["content", "text", "contents", "data"])
                .ok_or_else(|| need("{\"path\", \"content\"}"))?;
            let code = format!(
                "await write({}, {}); return 'written'",
                quote(&path),
                quote(&content)
            );
            Ok((code, note(&format!("await write({}, …)", quote(&path)))))
        }
        "edit" | "edit_file" | "replace" | "str_replace" => {
            let path = text(&["path", "file", "file_path"])
                .ok_or_else(|| need("{\"path\", \"old\", \"new\"}"))?;
            let old = text(&["old", "old_text", "old_string", "old_str", "search"])
                .ok_or_else(|| need("{\"path\", \"old\", \"new\"}"))?;
            let new =
                text(&["new", "new_text", "new_string", "new_str", "replace"]).unwrap_or_default();
            let code = format!(
                "await edit({}, {}, {}); return 'edited'",
                quote(&path),
                quote(&old),
                quote(&new)
            );
            Ok((code, note(&format!("await edit({}, …)", quote(&path)))))
        }
        other => Err(format!("unknown tool '{other}'. {hint}")),
    }
}

/// The `key` string of partial tool-call JSON, decoded as far as it goes:
/// `{"code": "// read config\nconst a = await re` → `// read config⏎const a = await re`.
fn draft_field(partial: &str, key: &str) -> String {
    let quoted = format!("\"{key}\"");
    let Some(at) = partial.find(&quoted) else {
        return String::new();
    };
    let rest = partial[at + quoted.len()..].trim_start();
    let Some(rest) = rest.strip_prefix(':') else {
        return String::new();
    };
    let Some(body) = rest.trim_start().strip_prefix('"') else {
        return String::new();
    };
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => {}
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(c);
                    }
                }
                Some(other) => out.push(other),
                None => break,
            },
            c => out.push(c),
        }
    }
    out
}

/// What the log calls a program: the model's `title`, else a leading
/// `//` comment, else what was called. Never a line of code.
pub fn program_title(name: &str, arguments: &Value, code: &str) -> String {
    arguments
        .get("title")
        .and_then(Value::as_str)
        .map(|title| title.lines().next().unwrap_or_default().trim())
        .filter(|title| !title.is_empty())
        .map(|title| title.chars().take(80).collect())
        .or_else(|| smash_js::title(code))
        .unwrap_or_else(|| match name {
            "js" => "program".into(),
            other => format!("{other} call"),
        })
}

fn send(updates: &UnboundedSender<Update>, update: Update) {
    let _ = updates.send(update);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_output_spills_to_a_file_and_short_output_stays() {
        let dir = std::env::temp_dir().join(format!("smash-spill-{}", std::process::id()));
        assert_eq!(spill("short", &dir), "short");
        let long = format!("HEAD{}TAIL", "x".repeat(20_000));
        let kept = spill(&long, &dir);
        assert!(kept.len() < RESULT_CHARS + 300, "{}", kept.len());
        assert!(kept.starts_with("HEAD") && kept.ends_with("TAIL"));
        let path = kept
            .split(" in ")
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), long, "nothing lost");
        assert_eq!(
            spill(&long, &dir),
            kept,
            "same output, same text: cache-stable"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tool_call_shenanigans_are_accepted_or_explained() {
        let ok = |name: &str, args: Value| code_for(name, &args).map(|(code, _)| code);
        assert_eq!(ok("js", json!({"code": "return 1"})).unwrap(), "return 1");
        assert_eq!(
            ok("js", json!("return 2")).unwrap(),
            "return 2",
            "bare string"
        );
        assert_eq!(
            ok("js", json!({"script": "return 3"})).unwrap(),
            "return 3",
            "renamed field"
        );
        assert_eq!(
            ok("bash", json!({"command": "ls -la"})).unwrap(),
            "return await sh(\"ls -la\")"
        );
        assert_eq!(
            ok("read_file", json!({"file_path": "a.rs"})).unwrap(),
            "return await read(\"a.rs\")"
        );
        assert!(
            ok("js", Value::Null).unwrap_err().contains("cut off"),
            "truncated arguments"
        );
        assert!(
            ok("browse", json!({}))
                .unwrap_err()
                .contains("only tool is js")
        );
        assert!(
            code_for("bash", &json!({"command": "x"}))
                .unwrap()
                .1
                .unwrap()
                .contains("only the js tool")
        );
    }

    #[test]
    fn drafts_decode_partial_json() {
        assert_eq!(draft_field("", "code"), "");
        assert_eq!(draft_field("{\"co", "code"), "");
        assert_eq!(
            draft_field("{\"code\": \"// read config", "code"),
            "// read config"
        );
        // JSON escapes: newline, quote, backslash, unicode.
        let json = r#"{"code":"// read config\nlet a = \"q\" + '\\' + '\u00e9'"#;
        assert_eq!(
            draft_field(json, "code"),
            "// read config\nlet a = \"q\" + '\\' + 'é'"
        );
        assert_eq!(
            draft_field(r#"{"code": "a\nb", "x": 1}"#, "code"),
            "a\nb",
            "stops at the closing quote"
        );
        assert_eq!(
            draft_field(r#"{"code": "a\"#, "code"),
            "a",
            "dangling escape"
        );
    }

    #[test]
    fn programs_are_named_by_the_model() {
        let code = "const [c, t] = await Promise.all([";
        assert_eq!(
            program_title(
                "js",
                &json!({"title": "Map the crates", "code": code}),
                code
            ),
            "Map the crates"
        );
        assert_eq!(
            program_title("js", &json!({"code": code}), code),
            "program",
            "never a line of code"
        );
        assert_eq!(
            program_title(
                "js",
                &json!({"title": "  ", "code": "// list files\nx"}),
                "// list files\nx"
            ),
            "list files"
        );
        assert_eq!(
            program_title("bash", &json!({"command": "ls"}), "return await sh(\"ls\")"),
            "bash call"
        );
        assert_eq!(
            draft_field(r#"{"title": "Run the tes"#, "title"),
            "Run the tes",
            "the title shows while it streams"
        );
        let tool = js_tool();
        assert_eq!(tool.parameters["required"], json!(["title", "code"]));
    }

    #[test]
    fn retry_policy() {
        let mut error = Error::new(Kind::RateLimit, "slow down");
        assert!(error.retryable());
        error.retry_after = Some(7.0);
        assert_eq!(backoff(&error, 1), Duration::from_secs(7));
        let fresh = Error::new(Kind::Overloaded, "x");
        assert!(backoff(&fresh, 1) < Duration::from_millis(1300));
        assert!(backoff(&fresh, 4) >= Duration::from_secs(8));
        for kind in [Kind::Auth, Kind::Billing, Kind::Request, Kind::Cancelled] {
            assert!(!Error::new(kind, "x").retryable(), "{kind:?}");
        }
        assert!(empty(&Message::assistant(vec![Content::Thinking {
            text: "hm".into()
        }])));
    }

    #[test]
    fn system_prompt_lists_the_tools() {
        let agent = Agent::new(Model::openai("unused"));
        assert!(agent.system.contains("edit(path, old, new"));
        assert!(!agent.system.contains("{TOOLS}"));
    }
}
