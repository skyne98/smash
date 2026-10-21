//! Unified LLM client: one context, one event stream, any provider.
//!
//! Thin adapter over [`genai`] (openai, anthropic, gemini, groq, deepseek, xai,
//! ollama, cohere, ...). It exists only to give the app one stable shape:
//! [`Context`] in, [`Event`] stream out, with unified tools, thinking blocks,
//! usage and cost. Providers, SSE parsing and auth stay in genai.
//!
//! ```no_run
//! # async fn run() -> Result<(), smash_ai::Error> {
//! use futures_util::StreamExt;
//! use smash_ai::{Client, Context, Event, Message, Model, Tool};
//!
//! let model = Model::anthropic("claude-sonnet-4-20250514");
//! let mut ctx = Context::new().system("be brief");
//! ctx.messages.push(Message::user("hi"));
//! ctx.tools.push(Tool::new("get_time", "current time", serde_json::json!({"type":"object"})));
//!
//! let mut stream = Client::new().stream(&model, &ctx);
//! while let Some(event) = stream.next().await {
//!     match event? {
//!         Event::TextDelta { delta, .. } => print!("{delta}"),
//!         Event::Done { usage, .. } => println!("\n{usage:?}"),
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::pin::Pin;

use futures_util::{Stream, StreamExt};
pub use genai::adapter::AdapterKind;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, ContentPart, MessageContent, ToolCall,
    ToolResponse,
};
use genai::resolver::{AuthData, Endpoint};
use genai::{ModelIden, ModelSpec, ServiceTarget};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod catalog;
mod faux;

pub use faux::faux;

pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event, Error>> + Send>>;

/// Why a model call failed, sorted by what the caller should do about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 401/403 or no key: the user has to fix their key.
    Auth,
    /// 402 or quota wording: the account is out of credit.
    Billing,
    /// 429: wait and retry.
    RateLimit,
    /// 5xx, 529, "overloaded": the provider is struggling; retry.
    Overloaded,
    /// Connection reset, DNS, TLS, timeout, stream cut: retry.
    Network,
    /// The stream went quiet for too long: retry.
    Stalled,
    /// The conversation no longer fits the model's context window (413, or a
    /// 400 that says so). Retrying only helps after compacting it.
    Overflow,
    /// 400 / 404 / 422: the request itself is wrong (model id, parameters).
    /// Retrying the same request won't help.
    Request,
    /// The provider answered with nothing usable: retry once or twice.
    Empty,
    Cancelled,
    Other,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub kind: Kind,
    pub status: Option<u16>,
    /// Seconds the provider asked us to wait (`retry-after`).
    pub retry_after: Option<f64>,
    /// First line of the provider's message.
    pub message: String,
}

impl Error {
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            retry_after: None,
            message: message.into(),
        }
    }

    /// Worth trying the same request again.
    pub fn retryable(&self) -> bool {
        matches!(
            self.kind,
            Kind::RateLimit | Kind::Overloaded | Kind::Network | Kind::Stalled | Kind::Empty
        )
    }

    fn from_genai(err: &genai::Error) -> Self {
        // Streaming errors carry the HTTP status only in their text
        // ("Status: 429 Too Many Requests"), not in status().
        let status = err.status().map(|status| status.as_u16()).or_else(|| {
            let text = err.to_string();
            let at = text.find("Status: ")? + "Status: ".len();
            text[at..].get(..3)?.parse().ok()
        });
        let retry_after = err.headers().and_then(retry_after);
        let text = err.to_string();
        if std::env::var_os("SMASH_AI_DEBUG").is_some() {
            eprintln!("[smash-ai] provider error (status {status:?}): {text:?}");
        }
        let mut error = classify(status, &text);
        error.retry_after = retry_after;
        error
    }
}

/// Kind from the HTTP status, else from the provider's wording.
pub fn classify(status: Option<u16>, text: &str) -> Error {
    let lower = text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| lower.contains(word));
    let overflow = has(&[
        "context length",
        "context_length",
        "context window",
        "context size",
        "maximum context",
        "prompt is too long",
        "input is too long",
        "too many tokens",
        "input token count",
        "reduce the length",
    ]);
    let kind = match status {
        Some(401 | 403) => Kind::Auth,
        Some(402) => Kind::Billing,
        Some(429) if has(&["quota", "insufficient", "billing", "balance", "credit"]) => {
            Kind::Billing
        }
        Some(429) => Kind::RateLimit,
        Some(408) => Kind::Network,
        Some(500..=599) => Kind::Overloaded,
        Some(413) => Kind::Overflow,
        Some(400..=499) | None if overflow => Kind::Overflow,
        Some(400..=499) => Kind::Request,
        _ if has(&[
            "api key",
            "api_key",
            "apikey",
            "no auth",
            "unauthorized",
            "missingkey",
            "authentication",
        ]) =>
        {
            Kind::Auth
        }
        _ if has(&[
            "insufficient balance",
            "insufficient_quota",
            "billing",
            "payment required",
        ]) =>
        {
            Kind::Billing
        }
        _ if has(&["overloaded", "rate limit", "too many requests"]) => Kind::Overloaded,
        _ if has(&[
            "connection",
            "connect",
            "timed out",
            "timeout",
            "dns",
            "tls",
            "reset",
            "broken pipe",
            "eof",
            "network",
            "error decoding",
            "stream",
            "body",
            "hyper",
            "unexpected end",
        ]) =>
        {
            Kind::Network
        }
        _ => Kind::Other,
    };
    Error {
        kind,
        status,
        retry_after: None,
        message: first_line(text),
    }
}

/// The provider's own message: JSON `error.message` when the body has one.
fn first_line(text: &str) -> String {
    let message = text
        .find('{')
        .and_then(|at| serde_json::from_str::<Value>(&text[at..]).ok())
        .and_then(|json| {
            json["error"]["message"]
                .as_str()
                .or_else(|| json["message"].as_str())
                .or_else(|| json["error"].as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| {
            text.lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("unknown error")
                .to_string()
        });
    message.chars().take(200).collect()
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<f64> {
    let get = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    if let Some(ms) = get("retry-after-ms").and_then(|v| v.trim().parse::<f64>().ok()) {
        return Some(ms / 1000.0);
    }
    get("retry-after").and_then(|v| v.trim().parse::<f64>().ok())
}

// ---------------------------------------------------------------- messages

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Content {
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
    /// Set on [`Role::Tool`] messages: the call this result answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![Content::Text { text: text.into() }],
            tool_call_id: None,
        }
    }

    pub fn assistant(content: Vec<Content>) -> Self {
        Self {
            role: Role::Assistant,
            content,
            tool_call_id: None,
        }
    }

    pub fn tool(call_id: impl Into<String>, result: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: vec![Content::Text {
                text: result.into(),
            }],
            tool_call_id: Some(call_id.into()),
        }
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|content| match content {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|content| match content {
            Content::ToolCall {
                id,
                name,
                arguments,
            } => Some((id.as_str(), name.as_str(), arguments)),
            _ => None,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

impl Tool {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Context {
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// None = provider default.
    pub effort: Option<Effort>,
    /// Groups requests that share a prefix, so the provider routes them to
    /// the same prompt cache (OpenAI `prompt_cache_key`). One per session.
    pub cache_key: Option<String>,
}

/// How hard the model thinks. `Off` disables thinking where the provider
/// supports a toggle (DeepSeek, Anthropic); levels map to the provider's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Off,
    Low,
    Medium,
    High,
    Max,
}

impl Effort {
    pub fn label(self) -> &'static str {
        match self {
            Effort::Off => "off",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Max => "max",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "off" | "none" | "zero" | "toggle" => Effort::Off,
            "low" | "minimal" => Effort::Low,
            "medium" => Effort::Medium,
            "high" => Effort::High,
            "max" | "xhigh" => Effort::Max,
            _ => return None,
        })
    }

    fn genai(self) -> genai::chat::ReasoningEffort {
        use genai::chat::ReasoningEffort as G;
        match self {
            Effort::Off => G::Zero,
            Effort::Low => G::Low,
            Effort::Medium => G::Medium,
            Effort::High => G::High,
            Effort::Max => G::Max,
        }
    }
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }
}

// ---------------------------------------------------------------- events

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StopReason {
    #[default]
    EndTurn,
    MaxTokens,
    ToolUse,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// USD, computed from [`Price`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub total: f64,
}

/// USD per million tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Start,
    TextStart {
        index: usize,
    },
    TextDelta {
        index: usize,
        delta: String,
    },
    TextEnd {
        index: usize,
        text: String,
    },
    ThinkingStart {
        index: usize,
    },
    ThinkingDelta {
        index: usize,
        delta: String,
    },
    ThinkingEnd {
        index: usize,
        text: String,
    },
    ToolCallStart {
        index: usize,
    },
    ToolCallDelta {
        index: usize,
        delta: String,
    },
    ToolCallEnd {
        index: usize,
        call: (String, String, Value),
    },
    Done {
        message: Message,
        stop: StopReason,
        usage: Usage,
        cost: Cost,
    },
}

// ---------------------------------------------------------------- model

#[derive(Clone, Debug)]
pub struct Model {
    spec: ModelSpec,
    pub price: Option<Price>,
    /// OpenAI's own API: it takes `prompt_cache_key` (compatible servers
    /// may reject unknown fields, so they never get it).
    cache_key: bool,
    /// Send earlier turns' reasoning back. DeepSeek thinking mode requires it
    /// once tools are in the request and ignores it otherwise; Anthropic and
    /// OpenAI must not get unsigned reasoning, so it is off for them.
    pub(crate) reasoning_passback: bool,
}

impl Model {
    /// Any genai adapter; the id is whatever that provider calls the model.
    pub fn any(adapter: AdapterKind, id: impl Into<String>) -> Self {
        Self {
            spec: ModelSpec::from_iden(ModelIden::new(adapter, id.into())),
            price: None,
            cache_key: matches!(adapter, AdapterKind::OpenAI | AdapterKind::OpenAIResp),
            reasoning_passback: false,
        }
    }

    pub fn openai(id: impl Into<String>) -> Self {
        Self::any(AdapterKind::OpenAI, id)
    }

    pub fn anthropic(id: impl Into<String>) -> Self {
        Self::any(AdapterKind::Anthropic, id)
    }

    pub fn gemini(id: impl Into<String>) -> Self {
        Self::any(AdapterKind::Gemini, id)
    }

    pub fn ollama(id: impl Into<String>) -> Self {
        Self::any(AdapterKind::Ollama, id)
    }

    pub fn deepseek(id: impl Into<String>) -> Self {
        Self {
            reasoning_passback: true,
            ..Self::any(AdapterKind::DeepSeek, id)
        }
    }

    /// A fully specified endpoint, auth and adapter.
    pub fn from_target(target: ServiceTarget) -> Self {
        Self {
            spec: target.into(),
            price: None,
            reasoning_passback: false,
            cache_key: false,
        }
    }

    /// OpenAI-compatible gateway at `base_url`, key read from `key_env`.
    pub fn compatible(
        base_url: impl Into<String>,
        key_env: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        let target = ServiceTarget {
            endpoint: Endpoint::from_owned(base_url.into()),
            auth: AuthData::from_env(key_env.into()),
            model: ModelIden::new(AdapterKind::OpenAI, id.into()),
        };
        Self {
            spec: target.into(),
            price: None,
            reasoning_passback: false,
            cache_key: false,
        }
    }

    pub fn price(mut self, price: Price) -> Self {
        self.price = Some(price);
        self
    }

    fn cost(&self, usage: Usage) -> Cost {
        let Some(price) = self.price else {
            return Cost::default();
        };
        let per_mtok = |tokens: u64, usd: f64| tokens as f64 * usd / 1_000_000.0;
        let input = per_mtok(usage.input, price.input)
            + per_mtok(usage.cache_read, price.cache_read)
            + per_mtok(usage.cache_write, price.cache_write);
        let output = per_mtok(usage.output, price.output);
        Cost {
            input,
            output,
            total: input + output,
        }
    }
}

// ---------------------------------------------------------------- client

#[derive(Clone)]
pub struct Client {
    inner: genai::Client,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// Install rustls' ring provider once, before any HTTPS client exists.
/// reqwest is built without a provider of its own (see Cargo.toml).
pub fn tls() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Err only if another provider is already installed: fine either way.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl Client {
    /// ponytail: panics only if TLS init fails, which is fatal anyway.
    pub fn new() -> Self {
        tls();
        Self {
            inner: genai::Client::new().expect("genai client init"),
        }
    }

    /// Streams one assistant turn. Errors arrive in-band as `Err`.
    pub fn stream(&self, model: &Model, ctx: &Context) -> EventStream {
        let client = self.inner.clone();
        let spec = model.spec.clone();
        let price = model.price;
        let request = request(ctx, model.reasoning_passback);
        let mut options = ChatOptions::default()
            .with_capture_usage(true)
            .with_capture_content(true)
            .with_capture_tool_calls(true)
            .with_capture_reasoning_content(true);
        if let Some(effort) = ctx.effort {
            options = options.with_reasoning_effort(effort.genai());
        }
        if model.cache_key
            && let Some(key) = &ctx.cache_key
        {
            options = options.with_prompt_cache_key(key.clone());
        }
        if let Some(trace) = Trace::from_env() {
            options = options.with_raw_frame_sink(trace);
        }

        Box::pin(async_stream::stream! {
            let response = match client.exec_chat_stream(spec, request, Some(&options)).await {
                Ok(response) => response,
                Err(err) => {
                    yield Err(Error::from_genai(&err));
                    return;
                }
            };
            let mut stream = Box::pin(response.stream);

            let mut accum = Accum::default();
            while let Some(item) = stream.next().await {
                match item {
                    Ok(event) => for event in accum.on(event) { yield Ok(event) },
                    Err(err) => {
                        yield Err(Error::from_genai(&err));
                        return;
                    }
                }
            }
            for event in accum.finish(price) {
                yield Ok(event);
            }
        })
    }

    /// Drains [`Client::stream`] into the final assistant message.
    pub async fn complete(&self, model: &Model, ctx: &Context) -> Result<Message, Error> {
        let mut stream = self.stream(model, ctx);
        while let Some(event) = stream.next().await {
            if let Event::Done { message, .. } = event? {
                return Ok(message);
            }
        }
        Err(Error::new(
            Kind::Network,
            "stream ended without a done event",
        ))
    }
}

/// `SMASH_TRACE=<file>`: append every raw provider frame as JSON lines, so a
/// garbled answer can be pinned on the wire (provider) or on us.
struct Trace(std::sync::Mutex<std::fs::File>);

impl Trace {
    fn from_env() -> Option<Self> {
        let path = std::env::var_os("SMASH_TRACE")?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(Self(std::sync::Mutex::new(file)))
    }

    fn write(&self, line: serde_json::Value) {
        use std::io::Write;
        if let Ok(mut file) = self.0.lock() {
            let _ = writeln!(file, "{line}");
        }
    }
}

impl genai::chat::ChatFrameSink for Trace {
    fn on_frame(&self, ctx: &genai::chat::FrameCtx, frame: genai::chat::RawFrameRef<'_>) {
        self.write(serde_json::json!({
            "pid": std::process::id(),
            "stream": ctx.stream_id,
            "i": frame.index,
            "us": frame.elapsed_us,
            "data": frame.data,
        }));
    }

    fn on_error(&self, ctx: &genai::chat::FrameCtx, err: &genai::Error) {
        self.write(serde_json::json!({
            "pid": std::process::id(),
            "stream": ctx.stream_id,
            "error": err.to_string(),
        }));
    }
}

fn request(ctx: &Context, reasoning_passback: bool) -> ChatRequest {
    let mut messages = Vec::new();
    // Two prompt-cache breakpoints, for providers that need them marked
    // (Anthropic; OpenAI's newest models): the system prompt, which never
    // changes, and the last message, which caches the whole conversation so
    // the next call (the same messages plus a few) reads it back. Providers
    // that cache on their own (DeepSeek, Gemini, older OpenAI) ignore them.
    let mark = || genai::chat::MessageOptions::from(genai::chat::CacheControl::Ephemeral);
    if let Some(system) = &ctx.system {
        messages.push(
            ChatMessage::system(MessageContent::from_text(system.as_str())).with_options(mark()),
        );
    }
    for message in &ctx.messages {
        let converted = match message.role {
            Role::User => ChatMessage::user(MessageContent::from_text(message.text())),
            Role::Tool => {
                ChatMessage::tool(MessageContent::from_tool_responses(vec![ToolResponse {
                    call_id: message.tool_call_id.clone().unwrap_or_default(),
                    fn_name: None,
                    content: message.text(),
                }]))
            }
            Role::Assistant => ChatMessage::assistant(MessageContent::from_parts(
                message
                    .content
                    .iter()
                    .filter_map(|content| content_part(content, reasoning_passback))
                    .collect::<Vec<_>>(),
            )),
        };
        messages.push(converted);
    }
    if !ctx.messages.is_empty()
        && let Some(last) = messages.pop()
    {
        messages.push(last.with_options(mark()));
    }

    if std::env::var_os("SMASH_AI_DEBUG").is_some() {
        eprintln!(
            "[smash-ai] request messages: {}",
            serde_json::to_string(&messages).unwrap_or_default()
        );
    }
    let mut request = ChatRequest::new(messages);
    for tool in &ctx.tools {
        let mut converted =
            genai::chat::Tool::new(genai::chat::ToolName::Custom(tool.name.clone()));
        converted.description = Some(tool.description.clone());
        converted.schema = Some(tool.parameters.clone());
        request = request.append_tool(converted);
    }
    request
}

fn content_part(content: &Content, reasoning_passback: bool) -> Option<ContentPart> {
    match content {
        Content::Text { text } => Some(ContentPart::Text(text.clone())),
        // genai hoists this into the assistant's `reasoning_content` field.
        Content::Thinking { text } if reasoning_passback => {
            Some(ContentPart::ReasoningContent(text.clone()))
        }
        Content::Thinking { .. } => None,
        Content::ToolCall {
            id,
            name,
            arguments,
        } => Some(ContentPart::ToolCall(ToolCall {
            call_id: id.clone(),
            fn_name: name.clone(),
            fn_arguments: arguments.clone(),
            thought_signatures: None,
        })),
    }
}

// ---------------------------------------------------------------- accumulation

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    args: String,
}

#[derive(Default)]
struct Accum {
    text: String,
    text_open: bool,
    thinking: String,
    thinking_open: bool,
    calls: Vec<Call>,
    usage: Usage,
    stop: StopReason,
}

impl Accum {
    fn on(&mut self, event: ChatStreamEvent) -> Vec<Event> {
        match event {
            ChatStreamEvent::Start => vec![Event::Start],
            ChatStreamEvent::Chunk(chunk) => {
                let mut events = Vec::new();
                if !self.text_open {
                    self.text_open = true;
                    events.push(Event::TextStart { index: 0 });
                }
                self.text.push_str(&chunk.content);
                events.push(Event::TextDelta {
                    index: 0,
                    delta: chunk.content,
                });
                events
            }
            ChatStreamEvent::ReasoningChunk(chunk) => {
                let mut events = Vec::new();
                if !self.thinking_open {
                    self.thinking_open = true;
                    events.push(Event::ThinkingStart { index: 1 });
                }
                self.thinking.push_str(&chunk.content);
                events.push(Event::ThinkingDelta {
                    index: 1,
                    delta: chunk.content,
                });
                events
            }
            ChatStreamEvent::ToolCallChunk(chunk) => {
                let call = chunk.tool_call;
                // Providers differ: OpenAI streams fragments, DeepSeek streams the
                // arguments so far. Treat a full-prefix payload as a replacement
                // and anything else as an append.
                let raw = match call.fn_arguments {
                    Value::String(text) => text,
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                let mut events = Vec::new();
                let position = match self
                    .calls
                    .iter()
                    .position(|existing| existing.id == call.call_id)
                {
                    Some(position) => position,
                    None => {
                        self.calls.push(Call {
                            id: call.call_id.clone(),
                            name: call.fn_name.clone(),
                            args: String::new(),
                        });
                        events.push(Event::ToolCallStart {
                            index: 2 + self.calls.len() - 1,
                        });
                        self.calls.len() - 1
                    }
                };
                let existing = &mut self.calls[position];
                if !call.fn_name.is_empty() {
                    existing.name = call.fn_name;
                }
                // The delta is only the new tail, whichever way the provider sends it.
                let delta = if raw.starts_with(existing.args.as_str()) {
                    let tail = raw[existing.args.len()..].to_string();
                    existing.args = raw;
                    tail
                } else {
                    existing.args.push_str(&raw);
                    raw
                };
                if !delta.is_empty() {
                    events.push(Event::ToolCallDelta {
                        index: 2 + position,
                        delta,
                    });
                }
                events
            }
            ChatStreamEvent::End(end) => {
                self.usage = usage(end.captured_usage);
                if let Some(reason) = end.captured_stop_reason {
                    self.stop = stop(reason);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn finish(&mut self, price: Option<Price>) -> Vec<Event> {
        let mut events = Vec::new();
        let mut content = Vec::new();

        if self.text_open {
            events.push(Event::TextEnd {
                index: 0,
                text: self.text.clone(),
            });
        }
        if !self.text.is_empty() {
            content.push(Content::Text {
                text: std::mem::take(&mut self.text),
            });
        }
        if self.thinking_open {
            events.push(Event::ThinkingEnd {
                index: 1,
                text: self.thinking.clone(),
            });
        }
        if !self.thinking.is_empty() {
            content.push(Content::Thinking {
                text: std::mem::take(&mut self.thinking),
            });
        }
        for (position, call) in std::mem::take(&mut self.calls).into_iter().enumerate() {
            let arguments: Value = serde_json::from_str(&call.args).unwrap_or(Value::Null);
            events.push(Event::ToolCallEnd {
                index: 2 + position,
                call: (call.id.clone(), call.name.clone(), arguments),
            });
            content.push(Content::ToolCall {
                id: call.id,
                name: call.name,
                arguments: serde_json::from_str(&call.args).unwrap_or(Value::Null),
            });
        }

        let cost = Model {
            spec: ModelSpec::from_name(""),
            price,
            reasoning_passback: false,
            cache_key: false,
        }
        .cost(self.usage);
        events.push(Event::Done {
            message: Message {
                role: Role::Assistant,
                content,
                tool_call_id: None,
            },
            stop: self.stop,
            usage: self.usage,
            cost,
        });
        events
    }
}

fn usage(usage: Option<genai::chat::Usage>) -> Usage {
    let Some(usage) = usage else {
        return Usage::default();
    };
    let to_u64 = |value: Option<i32>| value.unwrap_or(0).max(0) as u64;
    Usage {
        input: to_u64(usage.prompt_tokens),
        output: to_u64(usage.completion_tokens),
        cache_read: to_u64(
            usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens),
        ),
        cache_write: to_u64(
            usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cache_creation_tokens),
        ),
    }
}

fn stop(reason: genai::chat::StopReason) -> StopReason {
    use genai::chat::StopReason as Genai;
    match reason {
        Genai::Completed(_) => StopReason::EndTurn,
        Genai::MaxTokens(_) => StopReason::MaxTokens,
        Genai::ToolCall(_) => StopReason::ToolUse,
        _ => StopReason::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_breakpoints_mark_the_system_prompt_and_the_last_message() {
        let marked = |message: &ChatMessage| {
            message
                .options
                .as_ref()
                .and_then(|options| options.cache_control.as_ref())
                .is_some()
        };
        let mut ctx = Context::new().system("sys");
        ctx.messages = vec![
            Message::user("a"),
            Message::assistant(vec![Content::Text { text: "b".into() }]),
            Message::user("c"),
        ];
        let built = request(&ctx, false);
        let flags: Vec<bool> = built.messages.iter().map(marked).collect();
        assert_eq!(
            flags,
            [true, false, false, true],
            "system + last only (Anthropic allows 4)"
        );
        let empty = request(&Context::new().system("sys"), false);
        assert_eq!(empty.messages.len(), 1);
    }

    #[test]
    fn errors_are_classified() {
        let body = |status: u16, msg: &str| {
            format!(
                "Web stream error.\nCause: HTTP error.\nStatus: {status} X\nBody: {{\"error\":{{\"message\":\"{msg}\"}}}}"
            )
        };
        let kind = |status, msg: &str| classify(Some(status), &body(status, msg)).kind;
        assert_eq!(kind(429, "Rate limit reached"), Kind::RateLimit);
        assert_eq!(kind(429, "You exceeded your current quota"), Kind::Billing);
        assert_eq!(kind(529, "overloaded"), Kind::Overloaded);
        assert_eq!(kind(401, "bad key"), Kind::Auth);
        assert_eq!(kind(400, "maximum context length"), Kind::Overflow);
        assert_eq!(
            kind(400, "prompt is too long: 210000 tokens > 200000 maximum"),
            Kind::Overflow
        );
        assert_eq!(kind(413, "payload too large"), Kind::Overflow);
        assert_eq!(kind(400, "unknown parameter"), Kind::Request);
        assert_eq!(
            classify(None, "error sending request: connection reset").kind,
            Kind::Network
        );
        assert_eq!(
            classify(Some(400), &body(400, "too long")).message,
            "too long",
            "provider's own message"
        );
    }
    use genai::chat::{StreamChunk, StreamEnd, ToolChunk};

    #[test]
    fn reasoning_goes_back_only_when_asked() {
        let ctx = Context {
            messages: vec![Message::assistant(vec![
                Content::Thinking {
                    text: "plan".into(),
                },
                Content::Text {
                    text: "answer".into(),
                },
            ])],
            ..Context::default()
        };
        let reasoning = |passback| {
            request(&ctx, passback).messages[0]
                .content
                .parts()
                .iter()
                .any(|part| matches!(part, ContentPart::ReasoningContent(text) if text == "plan"))
        };
        assert!(reasoning(true), "deepseek gets its reasoning back");
        assert!(
            !reasoning(false),
            "other providers never see unsigned reasoning"
        );
    }

    #[test]
    fn cumulative_tool_call_chunks_merge_into_one_call() {
        let mut accum = Accum::default();
        let mut events = Vec::new();
        for args in ["", "{\"path", "{\"path\": \".\"}"] {
            events.extend(accum.on(ChatStreamEvent::ToolCallChunk(ToolChunk {
                tool_call: ToolCall {
                    call_id: "call_1".into(),
                    fn_name: "list_dir".into(),
                    fn_arguments: Value::String(args.into()),
                    thought_signatures: None,
                },
            })));
        }
        events.extend(accum.finish(None));

        let message = events
            .iter()
            .find_map(|event| match event {
                Event::Done { message, .. } => Some(message),
                _ => None,
            })
            .expect("done event");
        let calls: Vec<_> = message.tool_calls().collect();
        assert_eq!(calls.len(), 1, "one tool result per call id");
        assert_eq!(calls[0].2, &serde_json::json!({ "path": "." }));
        let count = |f: fn(&Event) -> bool| events.iter().filter(|e| f(e)).count();
        assert_eq!(
            count(|e| matches!(e, Event::ToolCallStart { .. })),
            1,
            "one start per call"
        );
        assert_eq!(
            count(|e| matches!(e, Event::ToolCallEnd { .. })),
            1,
            "one end per call"
        );
        let deltas: String = events
            .iter()
            .filter_map(|e| match e {
                Event::ToolCallDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, "{\"path\": \".\"}", "deltas are only new tails");
    }

    #[test]
    fn stream_events_assemble_into_one_assistant_message() {
        let mut accum = Accum::default();
        let mut events = Vec::new();
        events.extend(accum.on(ChatStreamEvent::Start));
        for text in ["Hel", "lo"] {
            events.extend(accum.on(ChatStreamEvent::Chunk(StreamChunk {
                content: text.into(),
            })));
        }
        events.extend(accum.on(ChatStreamEvent::ReasoningChunk(StreamChunk {
            content: "hmm".into(),
        })));
        events.extend(accum.on(ChatStreamEvent::ToolCallChunk(ToolChunk {
            tool_call: ToolCall {
                call_id: "call_1".into(),
                fn_name: "get_time".into(),
                fn_arguments: serde_json::json!({ "tz": "UTC" }),
                thought_signatures: None,
            },
        })));
        events.extend(accum.on(ChatStreamEvent::End(StreamEnd {
            captured_stop_reason: Some(genai::chat::StopReason::ToolCall("tool_calls".into())),
            ..Default::default()
        })));
        events.extend(accum.finish(Some(Price {
            input: 3.0,
            output: 15.0,
            ..Price::default()
        })));

        let (message, stop, cost) = events
            .iter()
            .find_map(|event| match event {
                Event::Done {
                    message,
                    stop,
                    cost,
                    ..
                } => Some((message, stop, cost)),
                _ => None,
            })
            .expect("done event");

        assert_eq!(message.text(), "Hello");
        assert_eq!(*stop, StopReason::ToolUse);
        let calls: Vec<_> = message.tool_calls().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, "get_time");
        assert_eq!(calls[0].2, &serde_json::json!({ "tz": "UTC" }));
        assert_eq!(cost.total, 0.0);
        assert!(matches!(events[0], Event::Start));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::ToolCallEnd { index: 2, .. }))
        );
    }
}
