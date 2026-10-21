//! Context overflow end to end, against an in-process fake provider: the
//! first overflow is absorbed (learn, compact, resend) and the same size never
//! overflows again.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

mod common;

use smash_ai::{Message, Role};
use smash_backend::compact::PREFIX;
use smash_backend::{Agent, CancellationToken, Compactor, Summarizer, Update};

/// The fake model's real limit, in tokens, and the request size that breaks it.
const LIMIT: u64 = 16_384;
const BREAKS_AT: usize = (LIMIT * 3) as usize;

#[derive(Default)]
struct Seen {
    overflows: AtomicUsize,
    answers: AtomicUsize,
    summaries: AtomicUsize,
    /// Summaries written as one more turn of the cached conversation.
    inline: AtomicUsize,
}

/// Summarizer requests (their system prompt says "You compact") get a short
/// summary; others overflow past BREAKS_AT and answer otherwise.
async fn serve(seen: Arc<Seen>) -> String {
    common::serve(Arc::new(move |body: &str| {
        if body.contains("being compacted") && body.len() <= BREAKS_AT {
            seen.inline.fetch_add(1, Ordering::SeqCst);
            common::text("Goal: answer. Progress: log read.")
        } else if body.contains("You compact") {
            seen.summaries.fetch_add(1, Ordering::SeqCst);
            common::text("Goal: answer the question. Progress: data read, nothing pending.")
        } else if body.len() > BREAKS_AT {
            seen.overflows.fetch_add(1, Ordering::SeqCst);
            common::error(
                400,
                &format!("This model's maximum context length is {LIMIT} tokens. However, your messages resulted in more tokens."),
            )
        } else {
            seen.answers.fetch_add(1, Ordering::SeqCst);
            common::text("final answer")
        }
    }))
    .await
}

async fn run(
    agent: &Agent,
    messages: &mut Vec<Message>,
) -> (Result<smash_backend::Run, smash_ai::Error>, Vec<Update>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = agent.run(messages, &tx, &CancellationToken::new()).await;
    drop(tx);
    let mut updates = Vec::new();
    while let Some(update) = rx.recv().await {
        updates.push(update);
    }
    (result, updates)
}

#[tokio::test(flavor = "multi_thread")]
async fn overflow_is_absorbed_once_and_never_repeats() {
    let seen = Arc::new(Seen::default());
    let url = serve(seen.clone()).await;
    let model = || common::model(&url);
    let dir = std::env::temp_dir().join(format!("smash-overflow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let limits = dir.join("limits.json");
    let compactor = Arc::new(Compactor::new(Some(limits.clone())));
    let summarizer = Summarizer {
        name: "fake/summarizer".into(),
        model: model(),
        window: 1_000_000,
    };
    // The catalog claims a big window; the provider knows better.
    let agent =
        Agent::new(model()).context("fake/fake", 200_000, vec![summarizer], compactor.clone());

    let mut messages = vec![
        Message::user(format!(
            "here is the log:\n{}",
            "line of log output\n".repeat(6_000)
        )),
        Message::assistant(vec![smash_ai::Content::Text {
            text: "read it".into(),
        }]),
        Message::user("what failed?"),
    ];
    let (result, updates) = run(&agent, &mut messages).await;
    let answer = result.expect("the overflow never reaches the caller");
    assert_eq!(answer.message.text(), "final answer");
    assert_eq!(
        seen.overflows.load(Ordering::SeqCst),
        1,
        "one overflow, absorbed"
    );
    assert_eq!(seen.summaries.load(Ordering::SeqCst), 1);
    assert!(
        updates
            .iter()
            .any(|u| matches!(u, Update::Compacted { by, .. } if by == "fake/summarizer")),
        "the compaction is visible: {updates:?}"
    );
    assert!(messages[0].text().starts_with(PREFIX));
    assert!(
        messages[0].text().contains("what failed?"),
        "user's words kept"
    );
    assert_eq!(messages.last().unwrap().role, Role::Assistant);
    assert_eq!(
        compactor.limit("fake/fake", 200_000),
        LIMIT,
        "limit learned"
    );
    assert!(
        std::fs::read_to_string(&limits).unwrap().contains("16384"),
        "and saved"
    );

    // Same size again, even after a restart: compacted before sending, no error.
    let compactor = Arc::new(Compactor::new(Some(limits.clone())));
    let summarizer = Summarizer {
        name: "fake/summarizer".into(),
        model: model(),
        window: 1_000_000,
    };
    let agent = Agent::new(model()).context("fake/fake", 200_000, vec![summarizer], compactor);
    messages.push(Message::user(format!(
        "another log:\n{}",
        "more output here\n".repeat(6_000)
    )));
    let (result, _) = run(&agent, &mut messages).await;
    assert_eq!(result.unwrap().message.text(), "final answer");
    assert_eq!(
        seen.overflows.load(Ordering::SeqCst),
        1,
        "never overflows twice"
    );
    assert_eq!(seen.answers.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn dead_summarizers_still_compact() {
    let seen = Arc::new(Seen::default());
    let url = serve(seen.clone()).await;
    let model = common::model(&url);
    // Nothing listens here: every summary attempt fails.
    let dead = Summarizer {
        name: "dead/model".into(),
        model: common::model("http://127.0.0.1:9/v1/"),
        window: 100_000,
    };
    let agent = Agent::new(model).context("fake/fake2", 0, vec![dead], Arc::default());
    let mut messages = vec![Message::user("x".repeat(80_000))];
    let (result, updates) = run(&agent, &mut messages).await;
    assert_eq!(result.unwrap().message.text(), "final answer");
    assert!(
        updates
            .iter()
            .any(|u| matches!(u, Update::Compacted { by, .. } if by == "digest" || by == "trim")),
        "{updates:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_chat_model_compacts_inside_its_own_cached_conversation() {
    let seen = Arc::new(Seen::default());
    let url = serve(seen.clone()).await;
    let model = || common::model(&url);
    // The chat model is the summarizer: same key, same model.
    let summarizer = Summarizer {
        name: "fake/inline".into(),
        model: model(),
        window: LIMIT,
    };
    let agent = Agent::new(model()).context("fake/inline", LIMIT, vec![summarizer], Arc::default());
    // About 85 % of the window: past SWAP, still fits the provider.
    let mut messages = vec![
        Message::user(format!("log:\n{}", "a line of log output\n".repeat(1_700))),
        Message::assistant(vec![smash_ai::Content::Text {
            text: "read".into(),
        }]),
        Message::user("what failed?"),
    ];
    let (result, updates) = run(&agent, &mut messages).await;
    assert_eq!(result.unwrap().message.text(), "final answer");
    assert_eq!(
        seen.inline.load(Ordering::SeqCst),
        1,
        "summarized in the cached conversation"
    );
    assert_eq!(
        seen.summaries.load(Ordering::SeqCst),
        0,
        "no written-out copy paid for"
    );
    assert_eq!(seen.overflows.load(Ordering::SeqCst), 0);
    assert!(
        updates
            .iter()
            .any(|u| matches!(u, Update::Compacted { by, .. } if by == "fake/inline")),
        "{updates:?}"
    );
}
