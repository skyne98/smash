//! Cache probe: a few user turns against DeepSeek, printing prompt-cache
//! hits per model call. A miss beyond the new tokens means the prefix changed.
//!
//!   DEEPSEEK_API_KEY=... cargo run -p smash-backend --example cache
//!   ANTHROPIC_API_KEY=... cargo run -p smash-backend --example cache -- anthropic claude-haiku-4-5

use smash_ai::{Message, Model};
use smash_backend::{Agent, CancellationToken, Update};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let model = match (args.next().as_deref(), args.next()) {
        (Some("anthropic"), Some(id)) => Model::anthropic(id),
        (Some("openai"), Some(id)) => Model::openai(id),
        _ => Model::deepseek("deepseek-flash"),
    };
    let agent = Agent::new(model).cache_key("smash-cache-probe");
    let turns = [
        "Read Cargo.toml with one js call, then say which crates exist in one sentence.",
        "Now read crates/core/src/lib.rs with one js call and say what App does in one sentence.",
        "Now run wc -l on crates/*/src/lib.rs with one js call and name the biggest file.",
        "Last: with one js call, grep which Cargo.toml files depend on smash-ai, and list them.",
    ];
    let mut messages = Vec::new();
    let mut call = 0;
    for prompt in turns {
        messages.push(Message::user(prompt));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let printer = tokio::spawn(async move {
            let mut last = smash_ai::Usage::default();
            let mut rows = Vec::new();
            while let Some(update) = rx.recv().await {
                if let Update::Usage(total) = update {
                    let input = total.input - last.input;
                    let hit = total.cache_read - last.cache_read;
                    let written = total.cache_write - last.cache_write;
                    rows.push((input, hit, written));
                    last = total;
                }
            }
            rows
        });
        agent
            .run(&mut messages, &tx, &CancellationToken::new())
            .await
            .expect("run");
        drop(tx);
        for (input, hit, written) in printer.await.expect("printer") {
            call += 1;
            println!(
                "call {call}: prompt {input:>6}  cached {hit:>6}  written {written:>6}  ({:.0}%)",
                100.0 * hit as f64 / input.max(1) as f64
            );
        }
        println!("-- end of user turn");
    }
}
