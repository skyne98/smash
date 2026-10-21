//! Persistent subagents the model starts and steers from its `js` programs:
//! `agents.spawn/send/wait/list/stop`.
//!
//! A subagent is an [`Agent`] run with a fresh context (only its task) on a
//! background task, so it outlives the program and the turn that started it.
//! Its conversation is kept (and saved), so `send` steers a running one
//! between its turns and wakes a finished one with its history intact, even
//! after a restart. Read-only unless asked, never nested, a few at a time.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use smash_ai::{Effort, Message, Model};
use tokio::sync::{mpsc::UnboundedSender, watch};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, Inbox, SYSTEM, Update, drain};
use crate::compact::{Compactor, Summarizer};

/// Subagents running at once per session.
pub const MAX_RUNNING: usize = 4;
/// Tool rounds per subagent run before it has to report.
const CHILD_TURNS: usize = 16;
/// `wait` gives up after this long by default (the program's own limit is
/// 10 minutes).
const WAIT: Duration = Duration::from_secs(300);
/// Report characters returned to the parent.
const REPORT: usize = 8_000;

/// For the parent's system prompt.
pub const DOCS: &str = "Subagents (each one a separate model run with a fresh context that sees only its task):
agents.spawn(task, {title, write?, model?}) -> id  start one in the background; it outlives this program.
  title: 2-6 words the user sees, like 'Map the crates'; task: the full instructions
agents.send(id, message) -> status              steer it between its turns; wakes a finished one, which keeps its history
agents.wait(id | [ids], {timeout?}) -> {id, status, report, turns, cost}  until it finishes; timeout ms (default 300000)
agents.list() -> [{id, task, status, turns}]    this session's subagents, including ones from earlier turns
agents.stop(id)
They are read-only unless {write: true} (one writer at a time), at most 4 run at once, and cannot spawn their own.
Delegate work whose raw output you don't need: searching, reading logs, mapping code, fresh-eyes review of a diff.
Give the full task: they know nothing of this conversation. Implementation that depends on this conversation you do yourself.
Spawn several, do other work, then wait; keep using an earlier agent with send instead of re-explaining to a new one.";

const CHILD: &str = "\n\nYou are a subagent. Another agent gave you the task in the first message and sees only your final message, never your tool output. Work until the task is done, then end with a concise report: findings with file:line, exact commands and errors that matter, what you changed, open questions. Later messages from the other agent may refine the task; answer each with an updated report.";
const READ_ONLY: &str = "\nYou are read-only: write and edit refuse. Do not change files through sh either; report what to change.";
const WRITER: &str = "\nYou may change files. You are the only agent changing files right now.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    /// Finished a run; `send` wakes it.
    Idle,
    Failed,
    Stopped,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Idle => "done",
            Status::Failed => "failed",
            Status::Stopped => "stopped",
        }
    }
}

/// What the UI and `wait` see of one subagent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub id: String,
    /// What the user sees: the parent's 2-6 word name for it.
    #[serde(default)]
    pub title: String,
    pub task: String,
    pub status: Status,
    /// Last run's final message, or its error.
    pub report: String,
    pub turns: usize,
    pub cost: f64,
    pub model: String,
    pub write: bool,
}

/// Saved form: the info plus the full conversation.
#[derive(Serialize, Deserialize)]
struct Saved {
    info: Info,
    messages: Vec<Message>,
}

/// Something happened to a subagent of session `scope`.
#[derive(Clone, Debug)]
pub enum Event {
    Update {
        scope: String,
        id: String,
        update: Update,
    },
    Changed {
        scope: String,
        info: Info,
        /// Cost of the run that just ended (0 when it started).
        cost: f64,
    },
}

/// How subagents run: built by the frontend per parent run.
#[derive(Clone)]
pub struct Spec {
    pub cwd: PathBuf,
    pub effort: Option<Effort>,
    pub compactor: Arc<Compactor>,
    pub summarizers: Vec<Summarizer>,
    /// Model key used when `spawn` names none.
    pub model: String,
    /// Model key → model and its context window (0 = unknown).
    #[allow(clippy::type_complexity)]
    pub resolve: Arc<dyn Fn(&str) -> Option<(Model, u64)> + Send + Sync>,
}

struct State {
    info: Info,
    messages: Vec<Message>,
    cancel: CancellationToken,
}

struct Child {
    state: Mutex<State>,
    inbox: Inbox,
    /// Bumped whenever the status changes; `wait` watches it.
    changed: watch::Sender<u64>,
}

impl Child {
    fn info(&self) -> Info {
        self.state
            .lock()
            .map(|s| s.info.clone())
            .unwrap_or_else(|p| p.into_inner().info.clone())
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }
}

/// Every session's subagents. Lives as long as the app.
pub struct Agents {
    /// `dir/<scope>/<id>.json`; None = memory only.
    dir: Option<PathBuf>,
    scopes: Mutex<HashMap<String, Vec<Arc<Child>>>>,
    events: Option<UnboundedSender<Event>>,
}

impl Agents {
    pub fn new(dir: Option<PathBuf>, events: Option<UnboundedSender<Event>>) -> Self {
        Self {
            dir,
            scopes: Mutex::default(),
            events,
        }
    }

    /// The `js` host for one parent run in session `scope`.
    pub fn host(self: &Arc<Self>, scope: impl Into<String>, spec: Spec) -> Arc<dyn smash_js::Host> {
        Arc::new(Tools {
            agents: self.clone(),
            scope: scope.into(),
            spec,
        })
    }

    /// Subagents of `scope`, loading saved ones on first use.
    fn children(&self, scope: &str) -> Vec<Arc<Child>> {
        let Ok(mut scopes) = self.scopes.lock() else {
            return Vec::new();
        };
        scopes
            .entry(scope.to_string())
            .or_insert_with(|| self.load(scope))
            .clone()
    }

    pub fn list(&self, scope: &str) -> Vec<Info> {
        self.children(scope)
            .iter()
            .map(|child| child.info())
            .collect()
    }

    fn find(&self, scope: &str, id: &str) -> Result<Arc<Child>, String> {
        let children = self.children(scope);
        children
            .iter()
            .find(|child| child.info().id == id)
            .cloned()
            .ok_or_else(|| {
                let known: Vec<String> = children.iter().map(|c| c.info().id).collect();
                format!("no subagent {id:?} (known: {})", known.join(", "))
            })
    }

    fn load(&self, scope: &str) -> Vec<Arc<Child>> {
        let Some(dir) = &self.dir else {
            return Vec::new();
        };
        let mut saved: Vec<Saved> = std::fs::read_dir(dir.join(scope))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .filter_map(|text| serde_json::from_str::<Saved>(&text).ok())
            .collect();
        saved.sort_by_key(|saved| number(&saved.info.id));
        saved
            .into_iter()
            .map(|mut saved| {
                // It was running when the app quit; its run died with it.
                if saved.info.status == Status::Running {
                    saved.info.status = Status::Stopped;
                    saved.info.report = "stopped when the app quit; send() continues it".into();
                }
                Arc::new(Child {
                    state: Mutex::new(State {
                        info: saved.info,
                        messages: saved.messages,
                        cancel: CancellationToken::new(),
                    }),
                    inbox: Inbox::default(),
                    changed: watch::channel(0).0,
                })
            })
            .collect()
    }

    fn save(&self, scope: &str, child: &Child) {
        let Some(dir) = &self.dir else {
            return;
        };
        let Ok(state) = child.state.lock() else {
            return;
        };
        let dir = dir.join(scope);
        let saved = Saved {
            info: state.info.clone(),
            messages: state.messages.clone(),
        };
        // ponytail: whole file rewritten per change; append-only log if
        // subagent histories grow large.
        if std::fs::create_dir_all(&dir).is_ok()
            && let Ok(text) = serde_json::to_string(&saved)
        {
            let _ = std::fs::write(dir.join(format!("{}.json", saved.info.id)), text);
        }
    }

    fn emit(&self, event: Event) {
        if let Some(events) = &self.events {
            let _ = events.send(event);
        }
    }

    pub fn spawn(
        self: &Arc<Self>,
        scope: &str,
        task: &str,
        title: Option<&str>,
        write: bool,
        model: Option<&str>,
        spec: &Spec,
    ) -> Result<Info, String> {
        if task.trim().is_empty() {
            return Err(
                "spawn needs a task: agents.spawn('what to do, with all context it needs')".into(),
            );
        }
        let model = model.unwrap_or(&spec.model).to_string();
        if (spec.resolve)(&model).is_none() {
            return Err(format!("unknown model {model:?}"));
        }
        let child = {
            let mut scopes = self.scopes.lock().map_err(|_| "registry poisoned")?;
            let children = scopes
                .entry(scope.to_string())
                .or_insert_with(|| self.load(scope));
            let running: Vec<Info> = children
                .iter()
                .map(|c| c.info())
                .filter(|info| info.status == Status::Running)
                .collect();
            if running.len() >= MAX_RUNNING {
                return Err(format!(
                    "{MAX_RUNNING} subagents are already running ({}); wait for one first",
                    running
                        .iter()
                        .map(|i| i.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if write && let Some(writer) = running.iter().find(|info| info.write) {
                return Err(format!(
                    "{} is already changing files; one writer at a time (wait for it, or spawn read-only)",
                    writer.id
                ));
            }
            let next = children
                .iter()
                .map(|c| number(&c.info().id))
                .max()
                .unwrap_or(0)
                + 1;
            let child = Arc::new(Child {
                state: Mutex::new(State {
                    info: Info {
                        id: format!("a{next}"),
                        title: title
                            .map(str::trim)
                            .filter(|title| !title.is_empty())
                            .map_or_else(
                                || default_title(task),
                                |title| title.chars().take(60).collect(),
                            ),
                        task: task.to_string(),
                        status: Status::Running,
                        report: String::new(),
                        turns: 0,
                        cost: 0.0,
                        model,
                        write,
                    },
                    messages: vec![Message::user(task)],
                    cancel: CancellationToken::new(),
                }),
                inbox: Inbox::default(),
                changed: watch::channel(0).0,
            });
            children.push(child.clone());
            child
        };
        let info = child.info();
        self.save(scope, &child);
        self.emit(Event::Changed {
            scope: scope.to_string(),
            info: info.clone(),
            cost: 0.0,
        });
        self.clone().start(scope.to_string(), child, spec.clone());
        Ok(info)
    }

    /// Queue `message` for subagent `id`; wakes it if it is not running.
    pub fn send(
        self: &Arc<Self>,
        scope: &str,
        id: &str,
        message: &str,
        spec: &Spec,
    ) -> Result<Status, String> {
        let child = self.find(scope, id)?;
        let wake =
            {
                let mut state = child.state.lock().map_err(|_| "poisoned")?;
                if let Ok(mut inbox) = child.inbox.lock() {
                    inbox.push(message.to_string());
                }
                let wake = state.info.status != Status::Running;
                if wake {
                    if state.info.write {
                        let others = self.children(scope);
                        if let Some(writer) = others.iter().map(|c| c.info()).find(|i| {
                            i.id != state.info.id && i.write && i.status == Status::Running
                        }) {
                            drain(&child.inbox);
                            return Err(format!(
                                "{} is changing files; one writer at a time",
                                writer.id
                            ));
                        }
                    }
                    state.info.status = Status::Running;
                    state.cancel = CancellationToken::new();
                }
                wake
            };
        if wake {
            child.bump();
            self.emit(Event::Changed {
                scope: scope.to_string(),
                info: child.info(),
                cost: 0.0,
            });
            self.clone().start(scope.to_string(), child, spec.clone());
            return Ok(Status::Running);
        }
        Ok(Status::Running)
    }

    pub fn stop(&self, scope: &str, id: &str) -> Result<Info, String> {
        let child = self.find(scope, id)?;
        if let Ok(state) = child.state.lock() {
            state.cancel.cancel();
        }
        Ok(child.info())
    }

    /// Until `id` is not running, or `timeout` passes (then its info as is).
    pub async fn wait(&self, scope: &str, id: &str, timeout: Duration) -> Result<Info, String> {
        let child = self.find(scope, id)?;
        let mut changed = child.changed.subscribe();
        let done = async {
            loop {
                if child.info().status != Status::Running {
                    return;
                }
                if changed.changed().await.is_err() {
                    return;
                }
            }
        };
        let _ = tokio::time::timeout(timeout, done).await;
        Ok(child.info())
    }

    /// Drive `child` on a blocking thread: its `js` programs (QuickJS) are
    /// not `Send`, so it can't be a plain task. Tasks it spawns (background
    /// summaries) still land on the shared runtime.
    fn start(self: Arc<Self>, scope: String, child: Arc<Child>, spec: Spec) {
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || handle.block_on(self.drive(scope, child, spec)));
    }

    /// Run `child` until it has nothing left to do.
    async fn drive(self: Arc<Self>, scope: String, child: Arc<Child>, spec: Spec) {
        loop {
            let (mut messages, info, cancel) = match child.state.lock() {
                Ok(state) => (
                    state.messages.clone(),
                    state.info.clone(),
                    state.cancel.clone(),
                ),
                Err(_) => return,
            };
            let agent = match (spec.resolve)(&info.model) {
                Some((model, window)) => {
                    let mut agent = Agent::new(model)
                        .effort(spec.effort)
                        .cwd(spec.cwd.clone())
                        .context(
                            info.model.clone(),
                            window,
                            spec.summarizers.clone(),
                            spec.compactor.clone(),
                        )
                        .inbox(child.inbox.clone());
                    agent.system = format!(
                        "{}{CHILD}{}",
                        SYSTEM.replace("{TOOLS}", smash_js::TOOLS),
                        if info.write { WRITER } else { READ_ONLY }
                    );
                    agent.read_only = !info.write;
                    agent.max_turns = CHILD_TURNS;
                    agent
                }
                None => {
                    self.finish(
                        &scope,
                        &child,
                        messages,
                        Status::Failed,
                        format!("unknown model {}", info.model),
                        0,
                        0.0,
                    );
                    return;
                }
            };
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let forward = {
                let agents = self.clone();
                let (scope, id) = (scope.clone(), info.id.clone());
                tokio::spawn(async move {
                    while let Some(update) = rx.recv().await {
                        agents.emit(Event::Update {
                            scope: scope.clone(),
                            id: id.clone(),
                            update,
                        });
                    }
                })
            };
            let result = agent.run(&mut messages, &tx, &cancel).await;
            drop(tx);
            let _ = forward.await;
            let (status, report, turns, cost) = match result {
                Ok(run) => (Status::Idle, run.message.text(), run.turns, run.cost.total),
                Err(error) if error.kind == smash_ai::Kind::Cancelled => {
                    (Status::Stopped, "stopped".to_string(), 0, 0.0)
                }
                Err(error) => (Status::Failed, format!("error: {}", error.message), 0, 0.0),
            };
            // A message that arrived after its last turn: keep going.
            let again = {
                let Ok(mut state) = child.state.lock() else {
                    return;
                };
                state.messages = messages.clone();
                let queued = child.inbox.lock().map(|q| !q.is_empty()).unwrap_or(false);
                queued && status != Status::Stopped
            };
            if again {
                continue;
            }
            self.finish(&scope, &child, messages, status, report, turns, cost);
            return;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        scope: &str,
        child: &Child,
        messages: Vec<Message>,
        status: Status,
        report: String,
        turns: usize,
        cost: f64,
    ) {
        if let Ok(mut state) = child.state.lock() {
            state.messages = messages;
            state.info.status = status;
            state.info.report = report;
            state.info.turns += turns;
            state.info.cost += cost;
        }
        if status == Status::Stopped {
            drain(&child.inbox);
        }
        self.save(scope, child);
        child.bump();
        self.emit(Event::Changed {
            scope: scope.to_string(),
            info: child.info(),
            cost,
        });
    }
}

/// A title from the task when the parent gave none: its first sentence,
/// at most 48 characters.
fn default_title(task: &str) -> String {
    let first = task
        .split(['\n', '.', ':'])
        .map(str::trim)
        .find(|part| !part.is_empty())
        .unwrap_or("subagent");
    if first.chars().count() <= 48 {
        return first.to_string();
    }
    let cut: String = first.chars().take(47).collect();
    format!("{}…", cut.trim_end())
}

/// `a12` → 12.
fn number(id: &str) -> u64 {
    id.trim_start_matches('a').parse().unwrap_or(0)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}\n…[report cut; agents.send(id, 'shorter please') asks again]")
}

fn public(info: &Info) -> Value {
    json!({
        "id": info.id,
        "task": info.task,
        "status": info.status.label(),
        "report": clip(&info.report, REPORT),
        "turns": info.turns,
        "cost": (info.cost * 10_000.0).round() / 10_000.0,
        "write": info.write,
    })
}

/// The functions of one parent run.
struct Tools {
    agents: Arc<Agents>,
    scope: String,
    spec: Spec,
}

impl smash_js::Host for Tools {
    fn names(&self) -> &[&'static str] {
        &[
            "agents.spawn",
            "agents.send",
            "agents.wait",
            "agents.list",
            "agents.stop",
        ]
    }

    fn target(&self, name: &str, args: &[Value]) -> String {
        let text = |at: usize| args.get(at).and_then(Value::as_str).unwrap_or_default();
        match name {
            "agents.send" => format!("{} ← {}", text(0), text(1)),
            "agents.wait" => match args.first() {
                Some(Value::Array(ids)) => ids
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => text(0).to_string(),
            },
            "agents.list" => String::new(),
            // The name the parent gave it, else the start of its task.
            "agents.spawn" => args
                .get(1)
                .and_then(|opts| opts["title"].as_str())
                .filter(|title| !title.trim().is_empty())
                .map_or_else(|| default_title(text(0)), String::from),
            _ => text(0).to_string(),
        }
    }

    fn call<'a>(&'a self, name: &'a str, args: Vec<Value>) -> smash_js::HostFuture<'a> {
        Box::pin(async move {
            let text = |at: usize| {
                args.get(at)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let opts = args.get(1).cloned().unwrap_or(Value::Null);
            let (agents, scope) = (&self.agents, self.scope.as_str());
            match name {
                "agents.spawn" => {
                    let task = match args.first() {
                        Some(Value::Object(o)) => o
                            .get("task")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        _ => text(0),
                    };
                    let write = opts["write"].as_bool().unwrap_or(false);
                    let info = agents.spawn(
                        scope,
                        &task,
                        opts["title"].as_str(),
                        write,
                        opts["model"].as_str(),
                        &self.spec,
                    )?;
                    let mode = if write { " · writes" } else { "" };
                    Ok((Value::String(info.id.clone()), format!("{}{mode}", info.id)))
                }
                "agents.send" => {
                    let status = agents.send(scope, &text(0), &text(1), &self.spec)?;
                    Ok((Value::String(status.label().into()), "queued".into()))
                }
                "agents.wait" => {
                    let timeout = opts["timeout"].as_u64().map_or(WAIT, Duration::from_millis);
                    match args.first() {
                        Some(Value::Array(ids)) => {
                            let ids: Vec<String> = ids
                                .iter()
                                .filter_map(Value::as_str)
                                .map(String::from)
                                .collect();
                            let all = futures_util::future::join_all(
                                ids.iter().map(|id| agents.wait(scope, id, timeout)),
                            )
                            .await;
                            let infos: Vec<Info> = all.into_iter().collect::<Result<_, _>>()?;
                            let line = infos
                                .iter()
                                .map(|i| format!("{} {}", i.id, i.status.label()))
                                .collect::<Vec<_>>()
                                .join(" · ");
                            Ok((Value::Array(infos.iter().map(public).collect()), line))
                        }
                        _ => {
                            let info = agents.wait(scope, &text(0), timeout).await?;
                            let line = format!("{} · {} turns", info.status.label(), info.turns);
                            Ok((public(&info), line))
                        }
                    }
                }
                "agents.list" => {
                    let infos = agents.list(scope);
                    let line = format!("{} subagents", infos.len());
                    let list = infos
                        .iter()
                        .map(|i| json!({"id": i.id, "task": i.task, "status": i.status.label(), "turns": i.turns}))
                        .collect();
                    Ok((Value::Array(list), line))
                }
                "agents.stop" => {
                    let info = agents.stop(scope, &text(0))?;
                    Ok((Value::String(info.id), "stopping".into()))
                }
                other => Err(format!("unknown function {other}")),
            }
        })
    }
}
