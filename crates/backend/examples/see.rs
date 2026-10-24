//! One image question against a real provider.
//!
//!   DEEPSEEK_API_KEY=... cargo run -p smash-backend --example see -- path/to/image.png "question"
//!   ANTHROPIC_API_KEY=... cargo run -p smash-backend --example see -- img.png "q" anthropic claude-haiku-4-5

use smash_ai::{Model, image};
use smash_backend::{Agent, CancellationToken, Update};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("an image path");
    let question = args
        .next()
        .unwrap_or_else(|| "What is in this image? Two sentences.".into());
    let model = match (args.next().as_deref(), args.next()) {
        (Some("anthropic"), Some(id)) => Model::anthropic(id),
        _ => Model::deepseek("deepseek-flash"),
    };
    let store = std::env::temp_dir().join("smash-see");
    // SEE_PASTE=1: treat the file as pasted bytes (no path the model knows).
    let prepared = if std::env::var_os("SEE_PASTE").is_some() {
        std::fs::read(&path)
            .map_err(|err| err.to_string())
            .and_then(|bytes| image::prepare_kept(&bytes, "clipboard", &store))
    } else {
        image::prepare_file(std::path::Path::new(&path), &store)
    };
    let shot = match prepared {
        Ok(shot) => shot,
        Err(why) => return eprintln!("cannot use {path}: {why}"),
    };
    println!(
        "{} · ~{} tokens · {}",
        shot.label(1),
        shot.tokens(),
        shot.path.display()
    );
    let mut messages = vec![image::user_message(&question, vec![shot])];
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let printer = tokio::spawn(async move {
        while let Some(update) = rx.recv().await {
            if let Update::Text(delta) = update {
                print!("{delta}");
            }
        }
        println!();
    });
    let run = Agent::new(model)
        .run(&mut messages, &tx, &CancellationToken::new())
        .await;
    drop(tx);
    let _ = printer.await;
    match run {
        Ok(run) => println!("--- usage {:?} cost ${:.5}", run.usage, run.cost.total),
        Err(err) => println!("--- error {:?}: {}", err.kind, err.message),
    }
}
