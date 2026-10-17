//! Agent loop: stream a turn, run the model's code, repeat until it answers.
//!
//! Tools are code (see [`smash_js`]): the model gets ONE tool, `js`, whose
//! argument is a JavaScript program where read/write/edit/ls/sh are async
//! functions. One model turn can then read five files in parallel, patch two,
//! run the tests and return only the failures, instead of the five-to-ten
//! round trips a one-action-per-call agent needs (Tura's measured win).

use std::path::PathBuf;

use futures_util::StreamExt;
use serde_json::{Value, json};
use smash_ai::{Content, Context, Cost, Effort, Error, Event, Message, Model, Role, StopReason, Tool, Usage};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

/// Tool loop guard: models that keep calling tools get cut off, not looped forever.
const MAX_TURNS: usize = 24;

/// Tool results older than this many turns are cut to a short head when sent
/// again; the model already acted on them (Tura's context compaction, minimal).
const FRESH_TURNS: usize = 2;
const STALE_RESULT: usize = 600;

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
Start the program with a // comment naming what it does.";

/// Everything the UI shows while a run is in flight.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    Thinking(String),
    Text(String),
    /// The model started writing a `js` program (title known once complete).
    Program { title: String },
    /// One tool call inside the running program finished.
    Call(smash_js::Call),
    /// The program finished.
    ProgramDone { calls: usize, secs: f64, error: Option<String> },
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

    /// Runs the loop, appending every message (assistant and tool) to `messages`.
    /// Progress goes to `updates`; `cancel` stops it between and inside turns.
    pub async fn run(
        &self,
        messages: &mut Vec<Message>,
        updates: &UnboundedSender<Update>,
        cancel: &CancellationToken,
    ) -> Result<Run, Error> {
        let mut usage = Usage::default();
        let mut cost = Cost::default();

        for turns in 1..=MAX_TURNS {
            let context = Context {
                system: Some(self.system.clone()),
                messages: compact(messages),
                tools: vec![js_tool()],
                max_tokens: self.max_tokens,
                temperature: None,
                effort: self.effort,
            };

            let mut stream = self.client.stream(&self.model, &context);
            let mut finished = None;
            loop {
                let event = tokio::select! {
                    () = cancel.cancelled() => return Err(Error::Provider("cancelled".into())),
                    event = stream.next() => event,
                };
                let Some(event) = event else { break };
                match event? {
                    Event::ThinkingDelta { delta, .. } => send(updates, Update::Thinking(delta)),
                    Event::TextDelta { delta, .. } => send(updates, Update::Text(delta)),
                    Event::ToolCallEnd { call: (_, _, arguments), .. } => {
                        let code = arguments["code"].as_str().unwrap_or_default();
                        send(updates, Update::Program { title: smash_js::title(code) });
                    }
                    Event::Done { message, stop, usage: turn, cost: turn_cost } => {
                        usage.input += turn.input;
                        usage.output += turn.output;
                        usage.cache_read += turn.cache_read;
                        usage.cache_write += turn.cache_write;
                        cost.input += turn_cost.input;
                        cost.output += turn_cost.output;
                        cost.total += turn_cost.total;
                        send(updates, Update::Usage(usage));
                        finished = Some((message, stop));
                    }
                    _ => {}
                }
            }

            let Some((message, stop)) = finished else {
                return Err(Error::Provider("stream ended without a done event".into()));
            };
            messages.push(message.clone());
            let calls: Vec<(String, Value)> = message
                .tool_calls()
                .map(|(id, _, arguments)| (id.to_string(), arguments.clone()))
                .collect();
            if stop != StopReason::ToolUse || calls.is_empty() {
                return Ok(Run { message, stop, usage, cost, turns });
            }
            for (id, arguments) in calls {
                let result = self.program(&arguments, updates, cancel).await;
                messages.push(Message::tool(id, result));
            }
        }

        Err(Error::Provider(format!("tool loop exceeded {MAX_TURNS} turns")))
    }

    async fn program(&self, arguments: &Value, updates: &UnboundedSender<Update>, cancel: &CancellationToken) -> String {
        let Some(code) = arguments["code"].as_str() else {
            return "error: js needs {\"code\": \"...\"}".into();
        };
        let (log, mut calls) = tokio::sync::mpsc::unbounded_channel();
        let forward = {
            let updates = updates.clone();
            async move {
                while let Some(call) = calls.recv().await {
                    send(&updates, Update::Call(call));
                }
            }
        };
        let run = smash_js::run(code, &self.cwd, smash_js::Limits::default(), log);
        let started = std::time::Instant::now();
        let outcome = tokio::select! {
            () = cancel.cancelled() => {
                send(updates, Update::ProgramDone {
                    calls: 0,
                    secs: started.elapsed().as_secs_f64(),
                    error: Some("stopped by the user".into()),
                });
                return "error: cancelled by the user".into();
            }
            (outcome, ()) = async { tokio::join!(run, forward) } => outcome,
        };
        send(updates, Update::ProgramDone {
            calls: outcome.calls,
            secs: outcome.elapsed.as_secs_f64(),
            error: outcome.error.clone(),
        });
        outcome.text
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
    fn system_prompt_lists_the_tools() {
        let agent = Agent::new(Model::openai("unused"));
        assert!(agent.system.contains("edit(path, old, new"));
        assert!(!agent.system.contains("{TOOLS}"));
    }
}
