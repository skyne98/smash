//! Print the first streamed text deltas of N runs, delta boundaries marked │.
//!   DEEPSEEK_API_KEY=... cargo run -q -p smash-backend --example deltas -- "question" 3
use smash_ai::{Message, Model};
use smash_backend::{Agent, Update};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let question = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "what are you".into());
    let runs: usize = std::env::args()
        .nth(2)
        .and_then(|n| n.parse().ok())
        .unwrap_or(3);
    for n in 0..runs {
        let agent = Agent::new(Model::deepseek("deepseek-flash"))
            .system("You are a terse coding assistant.");
        let mut messages = vec![Message::user(question.clone())];
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        agent.run(&mut messages, &tx, &Default::default()).await?;
        drop(tx);
        let mut text = String::new();
        let mut log = Vec::new();
        while let Some(update) = rx.recv().await {
            if let Update::Text(delta) = update {
                text.push_str(&delta);
                log.push(format!("{delta:?}"));
            }
        }
        println!("=== run {n}: {}", text.lines().next().unwrap_or(""));
        println!(
            "{}",
            log.iter().take(12).cloned().collect::<Vec<_>>().join(" │ ")
        );
    }
    Ok(())
}
