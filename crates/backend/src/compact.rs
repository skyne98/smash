//! Context compaction: a long conversation stays under the model's window
//! and the user never sees a context error.
//!
//! - At [`PREPARE`] of the limit a summary of everything so far starts in the
//!   background, on a dedicated summarizer model (the main model if none).
//! - At [`SWAP`] the history becomes one user message (the summary plus the
//!   earlier user messages verbatim) followed by every message since.
//! - An overflow error teaches the real limit (saved per model), so the same
//!   size never overflows twice.
//! - Summarizers are tried in order; when all fail a mechanical digest needs
//!   no model, so compaction itself never fails the run.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use smash_ai::{Client, Content, Context, Effort, Error, Event, Kind, Message, Model, Role, Tool};
use tokio::task::JoinHandle;

/// Start the background summary at this share of the limit...
pub const PREPARE: f64 = 0.6;
/// ...swap it in at this share...
pub const SWAP: f64 = 0.8;
/// ...and aim to land below this share afterwards.
pub const TARGET: f64 = 0.5;
/// Window assumed when the catalog doesn't know the model.
pub const DEFAULT_WINDOW: u64 = 128_000;
/// ponytail: answers degrade long before a 1M window fills (context rot), so
/// the working window is capped; make it a setting if someone needs more.
pub const MAX_WINDOW: u64 = 256_000;
/// Earlier user messages kept verbatim (about 20k tokens, as Codex does).
const KEEP_USER_CHARS: usize = 60_000;
/// Per-message caps when the history is written out for the summarizer.
const CODE_CHARS: usize = 2_000;
const RESULT_CHARS: usize = 3_000;
/// Conservative: code runs about 3 characters per token, prose about 4.
const CHARS_PER_TOKEN: u64 = 3;
/// No byte from a summarizer for this long: try the next one.
const IDLE: Duration = Duration::from_secs(120);
/// Background summaries kept at once (one per open conversation, roughly).
const PENDING: usize = 4;

/// Starts the message that replaces the compacted history.
pub const PREFIX: &str = "[Context compacted] Earlier parts of this conversation were summarized by another model to save space. Their tool output is gone; rerun a command if you need its exact output. Continue the work from where it stands.";

const PROMPT: &str = "You compact a coding agent's conversation so another model can continue it seamlessly. Write the handoff summary itself, no preamble.

Sections, each as short as it can be:
- Goal: what the user wants, in their words where wording matters
- Constraints and preferences the user stated
- Progress: done, in progress, blocked
- Decisions, and approaches tried and rejected with why, so they are not retried
- Files read or changed, and what matters about each
- Exact details still needed: error messages, commands, paths, numbers, snippets
- Next steps

Keep only what the transcript says; never invent.";

/// The conversation's own request shape. Summarizing with exactly this
/// prefix (same model, system, tools and messages, the instruction appended)
/// reads the whole history from the prompt cache instead of paying for it
/// again: on DeepSeek a cached token costs a fiftieth of a fresh one.
#[derive(Clone, Debug, Default)]
pub struct Prefix {
    /// Model key of the conversation; only that summarizer can reuse it.
    pub key: String,
    pub system: String,
    pub tools: Vec<Tool>,
    pub effort: Option<Effort>,
    pub cache_key: Option<String>,
    /// Tokens the messages may take in one request; past it the inline
    /// summary would overflow too, so the written-out path goes first.
    pub room: u64,
}

/// Appended to the cached conversation when the chat model summarizes it.
const INLINE: &str = "Stop here: do not call any tool. The conversation above is being compacted. Write the handoff summary for the model that continues it, as your whole reply.";

/// A model that writes summaries.
#[derive(Clone, Debug)]
pub struct Summarizer {
    /// Shown in the log, e.g. `google/gemini-flash`.
    pub name: String,
    pub model: Model,
    /// Context window in tokens; 0 = unknown.
    pub window: u64,
}

/// A summary and who wrote it (`digest` = no model answered).
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub text: String,
    pub by: String,
}

/// What one compaction did, in estimated tokens.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub before: u64,
    pub after: u64,
    pub by: String,
}

struct Pending {
    /// Messages covered: the summary replaces `messages[..upto]`.
    upto: usize,
    /// Hash of those messages, to find the conversation it belongs to.
    key: u64,
    task: JoinHandle<Summary>,
}

/// Lives across runs: background summaries in flight and learned limits.
#[derive(Default)]
pub struct Compactor {
    pending: Mutex<Vec<Pending>>,
    limits: Mutex<HashMap<String, u64>>,
    path: Option<PathBuf>,
}

impl Compactor {
    /// Learned limits persist in `path` (JSON: model key → tokens).
    pub fn new(path: Option<PathBuf>) -> Self {
        let limits = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            pending: Mutex::default(),
            limits: Mutex::new(limits),
            path,
        }
    }

    /// Working limit for model `key`: its window, capped, lowered to what
    /// overflowed before.
    pub fn limit(&self, key: &str, window: u64) -> u64 {
        let window = if window == 0 { DEFAULT_WINDOW } else { window }.min(MAX_WINDOW);
        let learned = self
            .limits
            .lock()
            .ok()
            .and_then(|limits| limits.get(key).copied());
        learned.map_or(window, |learned| learned.min(window))
    }

    /// `key` overflowed at about `sent` tokens: remember the limit the
    /// provider named, else 90 % of what was sent.
    pub fn learn(&self, key: &str, error: &Error, sent: u64) {
        let limit = named_limit(&error.message)
            .unwrap_or(sent * 9 / 10)
            .max(4_096);
        let Ok(mut limits) = self.limits.lock() else {
            return;
        };
        let entry = limits.entry(key.to_string()).or_insert(limit);
        *entry = (*entry).min(limit);
        if let Some(path) = &self.path
            && let Ok(text) = serde_json::to_string_pretty(&*limits)
        {
            let _ = std::fs::write(path, text);
        }
    }

    /// Start summarizing `messages` in the background, unless a summary of a
    /// prefix of them is already on its way. Must be called right before a
    /// model call, so the messages after the snapshot start with its reply.
    pub fn prepare(
        &self,
        messages: &[Message],
        summarizers: &[Summarizer],
        client: &Client,
        prefix: Option<&Prefix>,
    ) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        if messages.is_empty() || pending.iter().any(|p| covers(p, messages)) {
            return;
        }
        let snapshot = messages.to_vec();
        let (summarizers, client) = (summarizers.to_vec(), client.clone());
        let prefix = prefix.cloned();
        let fallback = digest(messages);
        let task = tokio::spawn(async move {
            summarize_any(&client, &summarizers, &snapshot, prefix.as_ref())
                .await
                .unwrap_or(fallback)
        });
        pending.push(Pending {
            upto: messages.len(),
            key: hash(messages),
            task,
        });
        if pending.len() > PENDING {
            pending.remove(0).task.abort();
        }
    }

    /// The background summary for a prefix of `messages`, waiting for it if
    /// it is still being written.
    async fn take(&self, messages: &[Message]) -> Option<(usize, Summary)> {
        let found = {
            let mut pending = self.pending.lock().ok()?;
            let at = pending.iter().position(|p| covers(p, messages))?;
            pending.remove(at)
        };
        Some((found.upto, found.task.await.ok()?))
    }

    /// Shrink `messages` below `target` tokens (`fixed` = the system prompt's
    /// share), escalating from `level`: 0 the background summary, 1 a fresh
    /// summary of everything, 2 every message cut to fit. Never fails.
    pub async fn shrink(
        &self,
        messages: &mut Vec<Message>,
        target: u64,
        fixed: u64,
        mut level: u8,
        summarizers: &[Summarizer],
        client: &Client,
        prefix: Option<&Prefix>,
    ) -> Outcome {
        let before = estimate(messages) + fixed;
        let mut by = String::new();
        // Earlier user messages may fill at most a third of the target.
        let keep =
            KEEP_USER_CHARS.min((target.saturating_sub(fixed) * CHARS_PER_TOKEN / 3) as usize);
        loop {
            match level {
                0 => {
                    if let Some((upto, summary)) = self.take(messages).await {
                        *messages = replace(messages, upto, &summary.text, keep);
                        by = summary.by;
                    }
                }
                1 => {
                    let summary = match summarize_any(client, summarizers, messages, prefix).await {
                        Some(summary) => summary,
                        None => digest(messages),
                    };
                    *messages = replace(messages, messages.len(), &summary.text, keep);
                    by = summary.by;
                }
                _ => {
                    trim(messages, target.saturating_sub(fixed));
                    if by.is_empty() {
                        by = "trim".into();
                    }
                    break;
                }
            }
            if estimate(messages) + fixed < target {
                break;
            }
            level += 1;
        }
        Outcome {
            before,
            after: estimate(messages) + fixed,
            by,
        }
    }
}

fn covers(pending: &Pending, messages: &[Message]) -> bool {
    pending.upto <= messages.len() && hash(&messages[..pending.upto]) == pending.key
}

fn hash(messages: &[Message]) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(messages)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

/// Rough tokens of `messages` (characters / 3: errs high, which only means
/// compacting a little early).
pub fn estimate(messages: &[Message]) -> u64 {
    let chars: usize = messages
        .iter()
        .flat_map(|message| &message.content)
        .map(|content| match content {
            Content::Text { text } | Content::Thinking { text } => text.len(),
            Content::ToolCall { arguments, .. } => arguments.to_string().len(),
            // In characters, so the division below gives its token estimate.
            Content::Image(image) => (image.tokens() * CHARS_PER_TOKEN) as usize,
        })
        .sum();
    chars as u64 / CHARS_PER_TOKEN + 4 * messages.len() as u64
}

/// Rough tokens of a plain string.
pub fn estimate_text(text: &str) -> u64 {
    text.len() as u64 / CHARS_PER_TOKEN
}

/// The limit an overflow error names: its smallest number of 4096 or more
/// ("maximum context length is 131072 tokens, you requested 140000").
fn named_limit(message: &str) -> Option<u64> {
    message
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|digits| digits.parse::<u64>().ok())
        .filter(|&n| n >= 4_096)
        .min()
}

/// The compacted history: one user message with the summary and the earlier
/// user messages verbatim, then everything after `upto` unchanged.
pub fn replace(messages: &[Message], upto: usize, summary: &str, keep: usize) -> Vec<Message> {
    let (old, tail) = messages.split_at(upto.min(messages.len()));
    // Newest first until the budget runs out, then back in order.
    let mut kept = Vec::new();
    let mut left = keep;
    for message in old.iter().rev().filter(|m| m.role == Role::User) {
        let text = message.text();
        if left == 0 || text.starts_with(PREFIX) {
            continue;
        }
        let text = if text.chars().count() > left {
            clip(&text, left)
        } else {
            text
        };
        left = left.saturating_sub(text.chars().count());
        kept.push(text);
    }
    kept.reverse();
    let mut head = String::from(PREFIX);
    if !kept.is_empty() {
        head.push_str("\n\nEarlier user messages, verbatim:\n");
        for text in kept {
            head.push_str(&format!("<user>\n{}\n</user>\n", text.trim()));
        }
    }
    head.push_str("\nSummary:\n");
    head.push_str(summary.trim());
    let mut out = vec![Message::user(head)];
    // A result whose call was summarized away would be orphaned.
    let mut tail = tail.iter().skip_while(|m| m.role == Role::Tool).cloned();
    match tail.next() {
        // Two user messages in a row: strict providers want them as one.
        Some(next) if next.role == Role::User => {
            out[0].content.push(Content::Text {
                text: "\n\n".into(),
            });
            out[0].content.extend(next.content);
        }
        Some(next) => out.push(next),
        None => {}
    }
    out.extend(tail);
    out
}

/// The history written out for a summarizer, one block per message.
fn blocks(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| match message.role {
            Role::User => {
                let mut text = message.text();
                for (number, image) in smash_ai::image::numbered(std::slice::from_ref(message)) {
                    text.push_str(&format!(
                        "\n{} (image file: {})",
                        image.label(number),
                        image.path.display()
                    ));
                }
                format!("[user]\n{text}\n")
            }
            Role::Tool => format!("[tool result]\n{}\n", clip(&message.text(), RESULT_CHARS)),
            Role::Assistant => {
                let mut out = String::from("[assistant]\n");
                for content in &message.content {
                    match content {
                        Content::Text { text } => {
                            out.push_str(text);
                            out.push('\n');
                        }
                        Content::Thinking { .. } | Content::Image(_) => {}
                        Content::ToolCall {
                            name, arguments, ..
                        } => out.push_str(&format!(
                            "ran {name}:\n{}\n",
                            clip(&code_of(arguments), CODE_CHARS)
                        )),
                    }
                }
                out
            }
        })
        .collect()
}

fn code_of(arguments: &Value) -> String {
    arguments
        .get("code")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| arguments.to_string())
}

/// Head and tail of `text` in `max` characters.
fn clip(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max * 2 / 3).collect();
    let tail: String = text.chars().skip(count - max / 3).collect();
    format!("{head}\n…[{} characters cut]…\n{tail}", count - max)
}

/// The no-model fallback: the last answer and the titles of the programs run.
fn digest(messages: &[Message]) -> Summary {
    let last = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant && !m.text().trim().is_empty())
        .map(|m| clip(&m.text(), 4_000))
        .unwrap_or_default();
    let programs: Vec<String> = messages
        .iter()
        .flat_map(Message::tool_calls)
        .map(|(_, name, arguments)| {
            crate::agent::program_title(name, arguments, &code_of(arguments))
        })
        .collect();
    let mut text = String::from("(No summarizer answered; this is a mechanical digest.)\n");
    if !last.is_empty() {
        text.push_str(&format!("\nLast assistant message:\n{last}\n"));
    }
    if !programs.is_empty() {
        text.push_str("\nPrograms run, oldest first:\n");
        let skip = programs.len().saturating_sub(40);
        for title in &programs[skip..] {
            text.push_str(&format!("- {title}\n"));
        }
    }
    Summary {
        text,
        by: "digest".into(),
    }
}

/// Last resort: cut every message so the history fits `target` tokens, then
/// drop the oldest messages after the first while it still does not.
fn trim(messages: &mut Vec<Message>, target: u64) {
    // 80 %: clip markers and per-message overhead count too.
    let budget = (target * CHARS_PER_TOKEN * 4 / 5) as usize;
    let each = (budget / messages.len().max(1)).max(200);
    for content in messages.iter_mut().flat_map(|m| m.content.iter_mut()) {
        match content {
            Content::Text { text } | Content::Thinking { text } if text.chars().count() > each => {
                *text = clip(text, each);
            }
            Content::ToolCall { arguments, .. } if arguments.to_string().len() > each => {
                *arguments = serde_json::json!({ "code": clip(&code_of(arguments), each) });
            }
            _ => {}
        }
    }
    while estimate(messages) >= target && messages.len() > 2 {
        messages.remove(1);
        // Results of a dropped call would be orphaned.
        while messages.len() > 1 && messages[1].role == Role::Tool {
            messages.remove(1);
        }
    }
}

/// The first summary any summarizer manages, in order.
async fn summarize_any(
    client: &Client,
    summarizers: &[Summarizer],
    messages: &[Message],
    prefix: Option<&Prefix>,
) -> Option<Summary> {
    let blocks = blocks(messages);
    for summarizer in summarizers {
        // The conversation's own model reads it from the cache.
        let fits = |prefix: &&Prefix| estimate(messages) < prefix.room;
        if let Some(prefix) = prefix.filter(|p| p.key == summarizer.name).filter(fits) {
            match inline(client, &summarizer.model, prefix, messages).await {
                Ok(text) if !text.trim().is_empty() => {
                    return Some(Summary {
                        text,
                        by: summarizer.name.clone(),
                    });
                }
                Ok(_) => log(&summarizer.name, "empty inline summary"),
                Err(error) => log(&summarizer.name, &error.message),
            }
        }
        match summarize(client, summarizer, &blocks).await {
            Ok(text) if !text.trim().is_empty() => {
                return Some(Summary {
                    text,
                    by: summarizer.name.clone(),
                });
            }
            Ok(_) => log(&summarizer.name, "empty summary"),
            Err(error) => log(&summarizer.name, &error.message),
        }
    }
    None
}

fn log(name: &str, what: &str) {
    if std::env::var_os("SMASH_AI_DEBUG").is_some() {
        eprintln!("[smash] summarizer {name} failed: {what}");
    }
}

/// The summary as one more turn of the cached conversation. Fails (and the
/// caller falls back to a written-out history) when the history no longer
/// fits the model or the model calls a tool instead.
async fn inline(
    client: &Client,
    model: &Model,
    prefix: &Prefix,
    messages: &[Message],
) -> Result<String, Error> {
    let context = Context {
        system: Some(prefix.system.clone()),
        messages: inline_messages(messages),
        tools: prefix.tools.clone(),
        max_tokens: None,
        temperature: None,
        effort: prefix.effort,
        cache_key: prefix.cache_key.clone(),
    };
    complete(client, model, &context).await
}

/// `messages` unchanged, then the instruction: as a new user turn, or added
/// to a trailing user message (strict providers reject two in a row).
fn inline_messages(messages: &[Message]) -> Vec<Message> {
    let mut out = messages.to_vec();
    match out.last_mut() {
        Some(last) if last.role == Role::User => last.content.push(Content::Text {
            text: format!("\n\n{INLINE}"),
        }),
        _ => out.push(Message::user(INLINE)),
    }
    out
}

/// Summarize `blocks` with one model. A history bigger than half its window
/// goes in chunks, each folded into the summary so far.
async fn summarize(
    client: &Client,
    summarizer: &Summarizer,
    blocks: &[String],
) -> Result<String, Error> {
    let window = if summarizer.window == 0 {
        DEFAULT_WINDOW
    } else {
        summarizer.window
    };
    let budget = (window / 2 * CHARS_PER_TOKEN) as usize;
    let mut summary: Option<String> = None;
    for chunk in chunks(blocks, budget) {
        let mut text = String::new();
        if let Some(so_far) = &summary {
            text.push_str(&format!(
                "Summary of the conversation so far:\n{so_far}\n\nThe conversation continues; fold it into one updated summary:\n\n"
            ));
        }
        text.push_str(&chunk);
        let context = Context::new().system(PROMPT).message(Message::user(text));
        summary = Some(complete(client, &summarizer.model, &context).await?);
    }
    Ok(summary.unwrap_or_default())
}

/// Blocks grouped into chunks of at most `budget` characters; a block bigger
/// than that is clipped.
fn chunks(blocks: &[String], budget: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    for block in blocks {
        let block = clip(block, budget);
        let current = out.last_mut().expect("never empty");
        if !current.is_empty() && current.len() + block.len() > budget {
            out.push(block);
        } else {
            current.push_str(&block);
        }
    }
    out
}

/// One non-streaming-style completion: the final text of a streamed call.
async fn complete(client: &Client, model: &Model, context: &Context) -> Result<String, Error> {
    let mut stream = client.stream(model, context);
    loop {
        match tokio::time::timeout(IDLE, stream.next()).await {
            Err(_) => return Err(Error::new(Kind::Stalled, "summarizer went silent")),
            Ok(None) => return Err(Error::new(Kind::Network, "summary stream ended early")),
            Ok(Some(event)) => {
                if let Event::Done { message, .. } = event? {
                    return Ok(message.text());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(text: &str) -> Message {
        Message::assistant(vec![Content::Text { text: text.into() }])
    }

    #[test]
    fn replacement_keeps_user_words_and_the_tail() {
        let messages = vec![
            Message::user("fix the login bug"),
            assistant("looking"),
            Message::tool("1", "x".repeat(5_000)),
            Message::user("use the v2 api"),
            assistant("done half"),
            Message::user("now the tests"),
        ];
        let out = replace(&messages, 5, "S", KEEP_USER_CHARS);
        assert_eq!(out.len(), 1, "trailing user message folds into the head");
        let head = out[0].text();
        assert!(head.starts_with(PREFIX));
        assert!(head.contains("fix the login bug") && head.contains("use the v2 api"));
        assert!(head.contains("Summary:\nS") && head.ends_with("now the tests"));
        assert!(!head.contains("xxxx"), "tool output is summarized away");

        let again = replace(&[out[0].clone(), assistant("ok")], 1, "S2", KEEP_USER_CHARS);
        assert_eq!(
            again[0].text().matches(PREFIX).count(),
            1,
            "old head not nested"
        );
        assert_eq!(again[1].role, Role::Assistant);

        let orphan = replace(&messages, 2, "S", KEEP_USER_CHARS);
        assert!(
            orphan.iter().all(|m| m.role != Role::Tool),
            "orphaned result dropped"
        );
        assert!(orphan[0].text().ends_with("use the v2 api"));
        assert_eq!(orphan[1].role, Role::Assistant);
    }

    #[test]
    fn limits_are_learned_and_capped() {
        let compactor = Compactor::default();
        assert_eq!(compactor.limit("m", 0), DEFAULT_WINDOW);
        assert_eq!(compactor.limit("m", 1_000_000), MAX_WINDOW);
        let error = Error::new(
            Kind::Overflow,
            "This model's maximum context length is 131072 tokens. However, you requested 140000 tokens",
        );
        compactor.learn("m", &error, 999_999);
        assert_eq!(compactor.limit("m", 1_000_000), 131_072);
        compactor.learn("m", &Error::new(Kind::Overflow, "too long"), 100_000);
        assert_eq!(
            compactor.limit("m", 1_000_000),
            90_000,
            "never raised, only lowered"
        );
        assert_eq!(compactor.limit("other", 200_000), 200_000);
    }

    #[test]
    fn inline_summary_keeps_the_prefix_byte_for_byte() {
        let history = vec![Message::user("go"), Message::tool("1", "out")];
        let sent = inline_messages(&history);
        assert_eq!(sent[..2], history[..], "the cached part is untouched");
        assert_eq!(sent[2].text(), INLINE);
        let trailing = vec![Message::user("a"), Message::user("b")];
        let sent = inline_messages(&trailing);
        assert_eq!(sent.len(), 2, "no two user turns in a row");
        assert!(sent[1].text().starts_with("b") && sent[1].text().ends_with(INLINE));
    }

    #[test]
    fn chunks_and_digest() {
        let blocks: Vec<String> = (0..10).map(|i| format!("{i}{}", "a".repeat(99))).collect();
        let chunked = chunks(&blocks, 250);
        assert_eq!(chunked.len(), 5);
        assert!(chunked.iter().all(|c| c.len() <= 250));
        let huge = chunks(&["b".repeat(10_000)], 300);
        assert!(huge[0].chars().count() < 400, "oversized block clipped");

        let call = Message::assistant(vec![Content::ToolCall {
            id: "1".into(),
            name: "js".into(),
            arguments: serde_json::json!({"code": "// read the auth files\nawait read('a')"}),
        }]);
        let summary = digest(&[Message::user("go"), call, assistant("auth uses JWT")]);
        assert_eq!(summary.by, "digest");
        assert!(summary.text.contains("read the auth files") && summary.text.contains("JWT"));
    }

    #[tokio::test]
    async fn background_summary_is_swapped_in_and_newer_messages_stay() {
        let compactor = Compactor::default();
        let client = Client::new();
        let mut messages = vec![
            Message::user("start"),
            assistant(&"old work ".repeat(5_000)),
        ];
        compactor.prepare(&messages, &[], &client, None);
        compactor.prepare(&messages, &[], &client, None);
        assert_eq!(
            compactor.pending.lock().unwrap().len(),
            1,
            "one summary per prefix"
        );
        messages.push(Message::user("newer question"));
        messages.push(assistant("newer answer"));
        compactor.prepare(&messages, &[], &client, None);
        assert_eq!(
            compactor.pending.lock().unwrap().len(),
            1,
            "a prefix is already covered"
        );

        let outcome = compactor
            .shrink(&mut messages, 100_000, 0, 0, &[], &client, None)
            .await;
        assert_eq!(outcome.by, "digest");
        assert!(compactor.pending.lock().unwrap().is_empty(), "taken");
        assert_eq!(
            messages.len(),
            2,
            "head (with the next user message folded in) + answer"
        );
        assert!(messages[0].text().contains("newer question"));
        assert_eq!(messages[1].text(), "newer answer");

        // A different conversation never picks up someone else's summary.
        compactor.prepare(&[Message::user("other chat")], &[], &client, None);
        assert!(
            compactor
                .take(&[Message::user("third chat")])
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn shrink_always_lands_under_target() {
        // No summarizer can answer (none configured): digest, then trim.
        let compactor = Compactor::default();
        let mut messages = vec![
            Message::user("y".repeat(300_000)),
            assistant(&"z".repeat(300_000)),
        ];
        let outcome = compactor
            .shrink(&mut messages, 10_000, 500, 0, &[], &Client::new(), None)
            .await;
        assert!(outcome.before > 100_000);
        assert!(outcome.after < 10_000, "{outcome:?}");
        assert_eq!(outcome.by, "digest");
        assert_eq!(messages[0].role, Role::User);

        // Hundreds of messages: clipping alone can't fit, old ones go.
        let mut many: Vec<Message> = (0..600)
            .flat_map(|i| {
                [
                    Message::user(format!("q{i} {}", "w".repeat(900))),
                    assistant(&"a".repeat(900)),
                ]
            })
            .collect();
        let outcome = compactor
            .shrink(&mut many, 5_000, 0, 2, &[], &Client::new(), None)
            .await;
        assert!(outcome.after < 5_000, "{outcome:?}");
        assert!(
            many.last().unwrap().text().starts_with("aaa"),
            "newest kept"
        );
    }
}
