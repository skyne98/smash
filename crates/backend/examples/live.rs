//! One live turn against a real provider. Prints deltas as they stream.
//!
//!   DEEPSEEK_API_KEY=... cargo run -p smash-backend --example live -- "your question"

use smash_ai::{Event, Message, Model};
use smash_backend::Agent;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let question = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Say hi in three words.".into());
    let model = Model::deepseek("deepseek-chat");

    let mut agent = Agent::new(model)
        .system("You are terse. Answer in one sentence unless asked to show work.")
        .builtin_tools();

    let mut messages = vec![Message::user(question)];
    let run = agent
        .run(&mut messages, |event| {
            if let Event::TextDelta { delta, .. } = event {
                print!("{delta}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
        })
        .await?;

    println!(
        "\n---\nstop={:?} turns={} usage={:?} cost=${:.6}",
        run.stop, run.turns, run.usage, run.cost.total
    );
    Ok(())
}
