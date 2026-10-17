//! Drive the agent against a local fake provider (see /tmp/fakeprov.js).
//!   FAKE=http://127.0.0.1:18765/v1/ cargo run -q -p smash-backend --example fault -- 5
use smash_ai::{Message, Model};
use smash_backend::{Agent, CancellationToken, Update};
use std::time::Instant;

#[tokio::main]
async fn main() {
    let base = std::env::var("FAKE").unwrap_or_else(|_| "http://127.0.0.1:18765/v1/".into());
    // Run with FAKE_KEY=anything in the environment.
    let model = Model::compatible(base, "FAKE_KEY", "fake");
    let agent = Agent::new(model);
    let mut messages = vec![Message::user("hi")];
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let started = Instant::now();
    let printer = tokio::spawn(async move {
        while let Some(update) = rx.recv().await {
            match update {
                Update::Retry { attempt, max, secs, reason } => {
                    println!("{:>5.1}s RETRY {attempt}/{max} in {secs:.1}s: {reason}", started.elapsed().as_secs_f64())
                }
                Update::Program { title } => println!("{:>5.1}s PROGRAM {title}", started.elapsed().as_secs_f64()),
                Update::Call(call) => println!("        call {} {} {}", call.verb, call.target, call.info),
                Update::Text(t) => println!("{:>5.1}s TEXT {t:?}", started.elapsed().as_secs_f64()),
                _ => {}
            }
        }
    });
    let result = agent.run(&mut messages, &tx, &CancellationToken::new()).await;
    drop(tx);
    let _ = printer.await;
    match result {
        Ok(run) => println!("{:>5.1}s OK turns={} answer={:?}", started.elapsed().as_secs_f64(), run.turns, run.message.text()),
        Err(err) => println!("{:>5.1}s ERR kind={:?} status={:?} retry_after={:?} msg={}", started.elapsed().as_secs_f64(), err.kind, err.status, err.retry_after, err.message),
    }
    let tools: Vec<String> = messages.iter().filter(|m| m.role == smash_ai::Role::Tool).map(|m| m.text()).collect();
    println!("tool results sent back: {tools:?}");
}
