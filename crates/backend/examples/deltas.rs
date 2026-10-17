//! Print every streamed event, delta boundaries marked with │, N runs.
//!   DEEPSEEK_API_KEY=... cargo run -q -p smash-backend --example deltas -- "question" 3
use smash_ai::{Event, Message, Model};
use smash_backend::Agent;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let question = std::env::args().nth(1).unwrap_or_else(|| "what are you".into());
    let runs: usize = std::env::args().nth(2).and_then(|n| n.parse().ok()).unwrap_or(3);
    for n in 0..runs {
        let mut agent = Agent::new(Model::deepseek("deepseek-chat")).system("You are a terse coding assistant.");
        let mut messages = vec![Message::user(question.clone())];
        let mut text = String::new();
        let mut log = Vec::new();
        agent
            .run(&mut messages, |event| match event {
                Event::TextDelta { delta, .. } => {
                    text.push_str(delta);
                    log.push(format!("{delta:?}"));
                }
                Event::Done { stop, .. } => log.push(format!("<done {stop:?}>")),
                Event::Start | Event::TextStart { .. } | Event::TextEnd { .. } => {}
                other => log.push(format!("<{other:?}>").chars().take(80).collect()),
            })
            .await?;
        println!("=== run {n}: {}", text.lines().next().unwrap_or(""));
        println!("{}", log.iter().take(12).cloned().collect::<Vec<_>>().join(" │ "));
    }
    Ok(())
}
