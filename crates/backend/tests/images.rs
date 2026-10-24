//! Images end to end against a fake provider: sent labelled, and an image the
//! provider refuses becomes a placeholder and the call is resent once.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use smash_ai::{Content, image};
use smash_backend::{Agent, CancellationToken};

fn png() -> Vec<u8> {
    let mut out = Vec::new();
    ::image::DynamicImage::ImageRgb8(::image::RgbImage::new(6, 4))
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            ::image::ImageFormat::Png,
        )
        .unwrap();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn images_are_sent_and_a_refused_one_becomes_a_placeholder() {
    let dir = std::env::temp_dir().join(format!("smash-img-it-{}", std::process::id()));
    let shot = image::prepare(&png(), "shot.png", &dir).unwrap();
    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let url = {
        let (bodies, calls) = (bodies.clone(), calls.clone());
        common::serve(Arc::new(move |body: &str| {
            bodies.lock().unwrap().push(body.to_string());
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                common::error(400, "Invalid image: the model does not support image input")
            } else {
                common::text("I can't see it, but the label says shot.png")
            }
        }))
        .await
    };
    let agent = Agent::new(common::model(&url));
    let mut messages = vec![image::user_message("what is this?", vec![shot])];
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let run = agent
        .run(&mut messages, &tx, &CancellationToken::new())
        .await;
    assert!(run.is_ok(), "{run:?}");
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "refused once, resent once");
    assert!(
        bodies[0].contains("data:image/png;base64,"),
        "image sent: {}",
        &bodies[0][..400.min(bodies[0].len())]
    );
    assert!(bodies[0].contains("[Image #1] shot.png · 6×4"), "labelled");
    assert!(!bodies[1].contains("base64,"), "no image the second time");
    assert!(
        bodies[1].contains("the provider refused it"),
        "placeholder explains"
    );
    assert!(
        matches!(&messages[0].content[1], Content::Text { text } if text.starts_with("[Image #1]"))
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A program reads two images (free), shows one: it arrives labelled after
/// the tool result, and the conversation only ever grows at the end.
#[tokio::test(flavor = "multi_thread")]
async fn programs_show_images_after_their_results() {
    let dir = std::env::temp_dir().join(format!("smash-img-show-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("before.png"), png()).unwrap();
    std::fs::write(dir.join("after.png"), png()).unwrap();
    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let url = {
        let bodies = bodies.clone();
        common::serve(Arc::new(move |body: &str| {
            let mut seen = bodies.lock().unwrap();
            seen.push(body.to_string());
            match seen.len() {
                1 => common::js(
                    "const [a, b] = await Promise.all([read('before.png'), read('after.png')]); await image(b); return [a.width, b.width]",
                ),
                _ => common::text("after.png is 6 pixels wide"),
            }
        }))
        .await
    };
    let agent = Agent::new(common::model(&url))
        .cwd(&dir)
        .images(dir.join("store"));
    let mut messages = vec![smash_ai::Message::user("compare them")];
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run(&mut messages, &tx, &CancellationToken::new())
        .await
        .unwrap();

    use smash_ai::Role::*;
    let roles: Vec<_> = messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [User, Assistant, Tool, User, Assistant],
        "images ride after the results"
    );
    let result = messages[2].text();
    assert!(result.starts_with("[6,6]"), "{result}");
    assert!(
        result.contains("[Image #1] ") && result.contains("after.png · 6×4 (attached below)"),
        "{result}"
    );
    let shown: Vec<_> = image::numbered(&messages)
        .map(|(_, i)| i.name.clone())
        .collect();
    assert_eq!(
        shown.len(),
        1,
        "only the image it showed, not the one it read"
    );
    assert!(shown[0].ends_with("after.png"));
    let second = &bodies.lock().unwrap()[1];
    assert_eq!(second.matches("base64,").count(), 1);
    assert!(second.contains("Images shown by the programs above:"));
    let _ = std::fs::remove_dir_all(dir);
}
