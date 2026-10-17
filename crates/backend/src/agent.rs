//! Agent loop: streams a turn, runs the tools the model asked for, repeats
//! until it stops asking. Provider and protocol live in [`smash_ai`].

use std::collections::HashMap;
use std::path::Path;

use futures_util::StreamExt;
use serde_json::{Value, json};
use smash_ai::{Context, Cost, Error, Event, Message, Model, StopReason, Tool, Usage};

/// Tool loop guard: models that keep calling tools get cut off, not looped forever.
const MAX_TURNS: usize = 16;

type ToolFn = Box<dyn Fn(&Value) -> Result<String, String> + Send + Sync>;

pub struct Agent {
    client: smash_ai::Client,
    pub model: Model,
    pub system: Option<String>,
    pub max_tokens: Option<u32>,
    tools: HashMap<String, (Tool, ToolFn)>,
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

impl Agent {
    pub fn new(model: Model) -> Self {
        Self {
            client: smash_ai::Client::new(),
            model,
            system: None,
            max_tokens: None,
            tools: HashMap::new(),
        }
    }

    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Registers a tool and its handler. The handler returns the tool result
    /// text, or an error string that is fed back to the model.
    pub fn tool(
        mut self,
        tool: Tool,
        run: impl Fn(&Value) -> Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        self.tools.insert(tool.name.clone(), (tool, Box::new(run)));
        self
    }

    /// `read_file` and `list_dir`, read-only.
    ///
    /// ponytail: no path sandbox — these tools see whatever the process sees.
    /// Add a root when agents run on untrusted input.
    pub fn builtin_tools(self) -> Self {
        self.tool(
            Tool::new(
                "read_file",
                "Read a UTF-8 file from disk.",
                json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
            ),
            |args| {
                let path = args["path"].as_str().ok_or("missing path")?;
                std::fs::read_to_string(path).map_err(|err| err.to_string())
            },
        )
        .tool(
            Tool::new(
                "list_dir",
                "List a directory, one entry per line.",
                json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
            ),
            |args| {
                let path = Path::new(args["path"].as_str().ok_or("missing path")?);
                let mut names: Vec<String> = std::fs::read_dir(path)
                    .map_err(|err| err.to_string())?
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                Ok(names.join("\n"))
            },
        )
    }

    /// Runs the loop, appending every message (assistant and tool) to `messages`.
    /// Streaming events go to `on_event` as they arrive.
    pub async fn run(
        &mut self,
        messages: &mut Vec<Message>,
        mut on_event: impl FnMut(&Event),
    ) -> Result<Run, Error> {
        let tools: Vec<Tool> = self.tools.values().map(|(tool, _)| tool.clone()).collect();
        let mut usage = Usage::default();
        let mut cost = Cost::default();

        for turns in 1..=MAX_TURNS {
            let context = Context {
                system: self.system.clone(),
                messages: messages.clone(),
                tools: tools.clone(),
                max_tokens: self.max_tokens,
                temperature: None,
            };

            let mut stream = self.client.stream(&self.model, &context);
            let mut finished = None;
            while let Some(event) = stream.next().await {
                let event = event?;
                if let Event::Done {
                    message,
                    stop,
                    usage: turn_usage,
                    cost: turn_cost,
                } = &event
                {
                    usage.input += turn_usage.input;
                    usage.output += turn_usage.output;
                    usage.cache_read += turn_usage.cache_read;
                    usage.cache_write += turn_usage.cache_write;
                    cost.input += turn_cost.input;
                    cost.output += turn_cost.output;
                    cost.total += turn_cost.total;
                    finished = Some((message.clone(), *stop));
                }
                on_event(&event);
            }

            let Some((message, stop)) = finished else {
                return Err(Error::Provider("stream ended without a done event".into()));
            };
            messages.push(message.clone());

            if stop != StopReason::ToolUse {
                return Ok(Run {
                    message,
                    stop,
                    usage,
                    cost,
                    turns,
                });
            }

            let mut calls = 0;
            for (id, name, arguments) in message.tool_calls() {
                calls += 1;
                messages.push(Message::tool(id, self.call_tool(name, arguments)));
            }
            if calls == 0 {
                return Ok(Run {
                    message,
                    stop: StopReason::EndTurn,
                    usage,
                    cost,
                    turns,
                });
            }
        }

        Err(Error::Provider(format!(
            "tool loop exceeded {MAX_TURNS} turns"
        )))
    }

    fn call_tool(&self, name: &str, arguments: &Value) -> String {
        match self.tools.get(name) {
            Some((_, run)) => run(arguments).unwrap_or_else(|err| format!("error: {err}")),
            None => format!("error: unknown tool '{name}'"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_tools_read_and_list() {
        let dir = std::env::temp_dir().join(format!("smash-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello").unwrap();

        let agent = Agent::new(Model::openai("unused")).builtin_tools();
        assert_eq!(
            agent.call_tool("read_file", &json!({ "path": dir.join("a.txt") })),
            "hello"
        );
        assert_eq!(
            agent.call_tool("list_dir", &json!({ "path": dir })),
            "a.txt"
        );
        assert_eq!(
            agent.call_tool("nope", &json!({})),
            "error: unknown tool 'nope'"
        );
        assert_eq!(
            agent
                .call_tool("read_file", &json!({ "path": dir.join("missing") }))
                .starts_with("error: "),
            true
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
