//! Context overflow end to end, against an in-process fake provider: the
//! first overflow is absorbed (learn, compact, resend) and the same size never
//! overflows again.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use smash_ai::{Message, Model, Role};
use smash_backend::compact::PREFIX;
use smash_backend::{Agent, CancellationToken, Compactor, Summarizer, Update};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The fake model's real limit, in tokens, and the request size that breaks it.
const LIMIT: u64 = 16_384;
const BREAKS_AT: usize = (LIMIT * 3) as usize;

#[derive(Default)]
struct Seen {
    overflows: AtomicUsize,
    answers: AtomicUsize,
    summaries: AtomicUsize,
}

fn sse(text: &str) -> String {
    let chunk = |delta: serde_json::Value, finish: serde_json::Value| json!({"id": "x", "model": "fake", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
    let usage = json!({"id": "x", "model": "fake", "choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}});
    let mut body = String::new();
    for event in [
        chunk(json!({"role": "assistant", "content": text}), json!(null)),
        chunk(json!({}), json!("stop")),
        usage,
    ] {
        body.push_str(&format!("data: {event}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn overflow() -> String {
    let body = json!({"error": {"message": format!("This model's maximum context length is {LIMIT} tokens. However, your messages resulted in more tokens."), "type": "invalid_request_error"}}).to_string();
    format!(
        "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Summarizer requests (their system prompt says "You compact") get a short
/// summary; others overflow past BREAKS_AT and answer otherwise.
async fn serve(seen: Arc<Seen>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buf = [0u8; 65_536];
                let body = loop {
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw);
                    let Some(end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let length = text[..end]
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if raw.len() >= end + 4 + length {
                        break String::from_utf8_lossy(&raw[end + 4..end + 4 + length]).to_string();
                    }
                };
                let reply = if body.contains("You compact") {
                    seen.summaries.fetch_add(1, Ordering::SeqCst);
                    sse("Goal: answer the question. Progress: data read, nothing pending.")
                } else if body.len() > BREAKS_AT {
                    seen.overflows.fetch_add(1, Ordering::SeqCst);
                    overflow()
                } else {
                    seen.answers.fetch_add(1, Ordering::SeqCst);
                    sse("final answer")
                };
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
}

async fn run(
    agent: &Agent,
    messages: &mut Vec<Message>,
) -> (Result<smash_backend::Run, smash_ai::Error>, Vec<Update>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = agent.run(messages, &tx, &CancellationToken::new()).await;
    drop(tx);
    let mut updates = Vec::new();
    while let Some(update) = rx.recv().await {
        updates.push(update);
    }
    (result, updates)
}

#[tokio::test(flavor = "multi_thread")]
async fn overflow_is_absorbed_once_and_never_repeats() {
    // SAFETY: set before any thread reads it; only this test uses the variable.
    unsafe { std::env::set_var("FAKE_OVERFLOW_KEY", "x") };
    let seen = Arc::new(Seen::default());
    let url = serve(seen.clone()).await;
    let model = || Model::compatible(url.clone(), "FAKE_OVERFLOW_KEY", "fake");
    let dir = std::env::temp_dir().join(format!("smash-overflow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let limits = dir.join("limits.json");
    let compactor = Arc::new(Compactor::new(Some(limits.clone())));
    let summarizer = Summarizer {
        name: "fake/summarizer".into(),
        model: model(),
        window: 1_000_000,
    };
    // The catalog claims a big window; the provider knows better.
    let agent =
        Agent::new(model()).context("fake/fake", 200_000, vec![summarizer], compactor.clone());

    let mut messages = vec![
        Message::user(format!(
            "here is the log:\n{}",
            "line of log output\n".repeat(6_000)
        )),
        Message::assistant(vec![smash_ai::Content::Text {
            text: "read it".into(),
        }]),
        Message::user("what failed?"),
    ];
    let (result, updates) = run(&agent, &mut messages).await;
    let answer = result.expect("the overflow never reaches the caller");
    assert_eq!(answer.message.text(), "final answer");
    assert_eq!(
        seen.overflows.load(Ordering::SeqCst),
        1,
        "one overflow, absorbed"
    );
    assert_eq!(seen.summaries.load(Ordering::SeqCst), 1);
    assert!(
        updates
            .iter()
            .any(|u| matches!(u, Update::Compacted { by, .. } if by == "fake/summarizer")),
        "the compaction is visible: {updates:?}"
    );
    assert!(messages[0].text().starts_with(PREFIX));
    assert!(
        messages[0].text().contains("what failed?"),
        "user's words kept"
    );
    assert_eq!(messages.last().unwrap().role, Role::Assistant);
    assert_eq!(
        compactor.limit("fake/fake", 200_000),
        LIMIT,
        "limit learned"
    );
    assert!(
        std::fs::read_to_string(&limits).unwrap().contains("16384"),
        "and saved"
    );

    // Same size again, even after a restart: compacted before sending, no error.
    let compactor = Arc::new(Compactor::new(Some(limits.clone())));
    let summarizer = Summarizer {
        name: "fake/summarizer".into(),
        model: model(),
        window: 1_000_000,
    };
    let agent = Agent::new(model()).context("fake/fake", 200_000, vec![summarizer], compactor);
    messages.push(Message::user(format!(
        "another log:\n{}",
        "more output here\n".repeat(6_000)
    )));
    let (result, _) = run(&agent, &mut messages).await;
    assert_eq!(result.unwrap().message.text(), "final answer");
    assert_eq!(
        seen.overflows.load(Ordering::SeqCst),
        1,
        "never overflows twice"
    );
    assert_eq!(seen.answers.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn dead_summarizers_still_compact() {
    unsafe { std::env::set_var("FAKE_OVERFLOW_KEY", "x") };
    let seen = Arc::new(Seen::default());
    let url = serve(seen.clone()).await;
    let model = Model::compatible(url, "FAKE_OVERFLOW_KEY", "fake");
    // Nothing listens here: every summary attempt fails.
    let dead = Summarizer {
        name: "dead/model".into(),
        model: Model::compatible("http://127.0.0.1:9/v1/", "FAKE_OVERFLOW_KEY", "dead"),
        window: 100_000,
    };
    let agent = Agent::new(model).context("fake/fake2", 0, vec![dead], Arc::default());
    let mut messages = vec![Message::user("x".repeat(80_000))];
    let (result, updates) = run(&agent, &mut messages).await;
    assert_eq!(result.unwrap().message.text(), "final answer");
    assert!(
        updates
            .iter()
            .any(|u| matches!(u, Update::Compacted { by, .. } if by == "digest" || by == "trim")),
        "{updates:?}"
    );
}
