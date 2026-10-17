//! One live run against a real provider, printing thinking, text and tool calls.
//!
//!   DEEPSEEK_API_KEY=... cargo run -p smash-backend --example live -- "your question" [effort]

use smash_ai::{Effort, Message, Model};
use smash_backend::{Agent, Update};
use std::io::Write;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let question = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Say hi in three words.".into());
    let effort = std::env::args()
        .nth(2)
        .and_then(|text| Effort::parse(&text));
    let agent = Agent::new(Model::deepseek("deepseek-flash")).effort(effort);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let printer = tokio::spawn(async move {
        let mut thinking = false;
        while let Some(update) = rx.recv().await {
            match update {
                Update::Thinking(delta) => {
                    if !thinking {
                        print!("\x1b[2m✻ ");
                        thinking = true;
                    }
                    print!("{delta}");
                }
                Update::Text(delta) => {
                    if thinking {
                        print!("\x1b[0m\n");
                        thinking = false;
                    }
                    print!("{delta}");
                }
                Update::Program { title } => println!("\n▸ js  {title}"),
                Update::Call(call) if call.running => {}
                Update::Call(call) => println!(
                    "  {} {:<5} {}  {}",
                    if call.ok { "✓" } else { "✗" },
                    call.verb,
                    call.target,
                    call.info
                ),
                Update::ProgramDone { calls, secs, error } => {
                    println!(
                        "  └ {calls} calls · {secs:.1}s{}",
                        error.map(|e| format!(" · {e}")).unwrap_or_default()
                    )
                }
                Update::Retry {
                    attempt,
                    max,
                    secs,
                    reason,
                } => {
                    println!("\n↻ {reason} · retry {attempt}/{max} in {secs:.0}s")
                }
                Update::Usage(_) | Update::Attempt => {}
            }
            let _ = std::io::stdout().flush();
        }
    });

    let mut messages = vec![Message::user(question)];
    let run = agent.run(&mut messages, &tx, &Default::default()).await?;
    drop(tx);
    printer.await?;
    println!(
        "\x1b[0m\n---\nstop={:?} turns={} usage={:?}",
        run.stop, run.turns, run.usage
    );
    Ok(())
}
