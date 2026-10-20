//! In-process fake OpenAI-compatible provider for the integration tests.
#![allow(dead_code)]

use std::sync::Arc;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub type Handler = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Key variable every fake model reads.
pub const KEY: &str = "SMASH_FAKE_KEY";

pub fn model(url: &str) -> smash_ai::Model {
    // SAFETY: tests only ever set this one variable, always to the same value.
    unsafe { std::env::set_var(KEY, "x") };
    smash_ai::Model::compatible(url.to_string(), KEY, "fake")
}

fn chunk(delta: serde_json::Value, finish: serde_json::Value) -> serde_json::Value {
    json!({"id": "x", "model": "fake", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

fn stream(events: Vec<serde_json::Value>) -> String {
    let usage = json!({"id": "x", "model": "fake", "choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}});
    let mut body = String::new();
    for event in events.into_iter().chain([usage]) {
        body.push_str(&format!("data: {event}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A plain answer.
pub fn text(text: &str) -> String {
    stream(vec![
        chunk(json!({"role": "assistant", "content": text}), json!(null)),
        chunk(json!({}), json!("stop")),
    ])
}

/// A `js` tool call running `code`.
pub fn js(code: &str) -> String {
    let arguments = json!({ "code": code }).to_string();
    stream(vec![
        chunk(
            json!({"role": "assistant", "tool_calls": [{"index": 0, "id": format!("c{}", code.len()), "type": "function", "function": {"name": "js", "arguments": arguments}}]}),
            json!(null),
        ),
        chunk(json!({}), json!("tool_calls")),
    ])
}

pub fn error(status: u16, message: &str) -> String {
    let body = json!({"error": {"message": message, "type": "invalid_request_error"}}).to_string();
    format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Serve `handler` (request body → raw HTTP response); returns the base URL.
pub async fn serve(handler: Handler) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
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
                let reply = handler(&body);
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
}
