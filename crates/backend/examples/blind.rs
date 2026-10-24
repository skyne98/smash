//! What a text-only model does with an image: metadata only, live.
//!
//!   DEEPSEEK_API_KEY=... cargo run -p smash-backend --example blind -- image.png

use smash_ai::{Message, image};
use smash_backend::{Agent, CancellationToken};

#[tokio::main]
async fn main() {
    let path = std::env::args().nth(1).expect("an image path");
    let cache = smash_backend::Files::default_dir().join("models.json");
    let catalog = smash_ai::catalog::Catalog::load(&cache).expect("models.json (run smash once)");
    let entry = catalog
        .find("deepseek/deepseek-v4-pro")
        .expect("deepseek-v4-pro in the catalog");
    let model = entry.model().expect("callable");
    println!("{} sees images: {}", entry.key(), model.sees_images());
    let store = std::env::temp_dir().join("smash-blind");
    let bytes = std::fs::read(&path).expect("readable");
    let shot = image::prepare_kept(&bytes, "clipboard", &store).expect("an image");
    let mut messages: Vec<Message> = vec![image::user_message(
        "What do you know about the image I attached? Two short sentences, no tools.",
        vec![shot],
    )];
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    match Agent::new(model)
        .run(&mut messages, &tx, &CancellationToken::new())
        .await
    {
        Ok(run) => println!("{}\n--- usage {:?}", run.message.text(), run.usage),
        Err(err) => println!("--- error {:?}: {}", err.kind, err.message),
    }
    let _ = std::fs::remove_dir_all(store);
}
