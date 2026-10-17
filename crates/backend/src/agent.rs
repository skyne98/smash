//! Agent loop: stream a turn, run the model's code, repeat until it answers.
//!
//! Tools are code (see [`smash_js`]): the model gets ONE tool, `js`, whose
//! argument is a JavaScript program where read/write/edit/ls/sh are async
//! functions. One model turn can then read five files in parallel, patch two,
//! run the tests and return only the failures, instead of the five-to-ten
//! round trips a one-action-per-call agent needs (Tura's measured win).

use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use smash_ai::{
    Content, Context, Cost, Effort, Error, Event, Kind, Message, Model, Role, StopReason, Tool,
    Usage,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

/// Tool loop guard: models that keep calling tools get cut off, not looped forever.
const MAX_TURNS: usize = 24;

/// Tool results older than this many turns are cut to a short head when sent
/// again; the model already acted on them (Tura's context compaction, minimal).
const FRESH_TURNS: usize = 2;
const STALE_RESULT: usize = 600;

/// Tries per model call for transient failures (rate limit, overload, network).
const MAX_ATTEMPTS: u32 = 6;
/// Tries when the model answers with nothing at all.
const EMPTY_ATTEMPTS: u32 = 3;
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
Start the program with a // comment naming what it does.";

/// Everything the UI shows while a run is in flight.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// A model call starts; output after this belongs to it (dropped on retry).
    Attempt,
    Thinking(String),
    Text(String),
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
}

pub struct Agent {
    client: smash_ai::Client,
    pub model: Model,
    pub system: String,
    pub max_tokens: Option<u32>,
    pub effort: Option<Effort>,
    pub cwd: PathBuf,
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
            "properties": { "code": { "type": "string", "description": "Body of an async function." } },
            "required": ["code"],
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
        }
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

        for turns in 1..=MAX_TURNS {
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
            if calls.is_empty() {
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
        }

        Err(Error::new(
            Kind::Other,
            format!(
                "stopped after {MAX_TURNS} tool rounds without a final answer · say \"continue\" to keep going"
            ),
        ))
    }

    /// One model turn, retried on transient failures with backoff (honoring
    /// the provider's retry-after) and on empty replies.
    async fn call(
        &self,
        messages: &[Message],
        updates: &UnboundedSender<Update>,
        cancel: &CancellationToken,
    ) -> Result<Reply, Error> {
        let context = Context {
            system: Some(self.system.clone()),
            messages: compact(messages),
            tools: vec![js_tool()],
            max_tokens: self.max_tokens,
            temperature: None,
            effort: self.effort,
        };
        let mut attempt = 0;
        loop {
            attempt += 1;
            send(updates, Update::Attempt);
            let error = match self.attempt(&context, updates, cancel).await {
                Ok(reply) if !empty(&reply.message) => return Ok(reply),
                Ok(_) => Error::new(Kind::Empty, "the model returned an empty reply"),
                Err(error) => error,
            };
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
                title: smash_js::title(&code),
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
        let run = smash_js::run(&code, &self.cwd, smash_js::Limits::default(), log);
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
        match note {
            Some(note) => format!("{note}\n{}", outcome.text),
            None => outcome.text,
        }
    }
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

fn send(updates: &UnboundedSender<Update>, update: Update) {
    let _ = updates.send(update);
}

/// The history as sent: tool results more than FRESH_TURNS assistant turns
/// old keep only their head. The stored messages stay intact.
fn compact(messages: &[Message]) -> Vec<Message> {
    let mut turns_after = vec![0usize; messages.len()];
    let mut seen = 0;
    for (index, message) in messages.iter().enumerate().rev() {
        turns_after[index] = seen;
        if message.role == Role::Assistant {
            seen += 1;
        }
    }
    messages
        .iter()
        .zip(turns_after)
        .map(|(message, after)| {
            if message.role != Role::Tool || after < FRESH_TURNS {
                return message.clone();
            }
            let text = message.text();
            if text.chars().count() <= STALE_RESULT {
                return message.clone();
            }
            let head: String = text.chars().take(STALE_RESULT).collect();
            let mut message = message.clone();
            message.content = vec![Content::Text {
                text: format!("{head}\n…[older output trimmed; rerun if needed]"),
            }];
            message
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_tool_results_shrink_new_ones_stay() {
        let long = "x".repeat(2000);
        let assistant = || Message::assistant(vec![Content::Text { text: "a".into() }]);
        let messages = vec![
            Message::user("go"),
            assistant(),
            Message::tool("1", long.clone()),
            assistant(),
            assistant(),
            Message::tool("2", long.clone()),
        ];
        let sent = compact(&messages);
        assert!(sent[2].text().len() < 700, "old result trimmed");
        assert_eq!(sent[5].text(), long, "fresh result intact");
        assert_eq!(messages[2].text(), long, "stored history untouched");
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
