//! Subagents end to end against a fake provider: spawn from a program, wait,
//! steer a finished one (history kept), persistence across restarts, limits.

mod common;

use std::sync::Arc;
use std::time::Duration;

use smash_ai::Message;
use smash_backend::agents::{DOCS, Spec, Status};
use smash_backend::{Agent, Agents, CancellationToken, Update};

fn spec(url: &str, dir: &std::path::Path) -> Spec {
    let url = url.to_string();
    Spec {
        cwd: dir.to_path_buf(),
        effort: None,
        compactor: Arc::default(),
        summarizers: Vec::new(),
        model: "fake/fake".into(),
        resolve: Arc::new(move |key: &str| (key != "nope/nope").then(|| (common::model(&url), 0))),
        images: dir.join("images"),
    }
}

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("smash-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Children answer from their conversation; the parent spawns, then waits
/// and steers, then answers with what it got.
fn script(body: &str) -> String {
    if body.contains("You are a subagent") {
        return if body.contains("again please") {
            common::text("second report, history had: find the answer")
        } else if body.contains("refine please") {
            common::text("refined report")
        } else {
            common::text("child report")
        };
    }
    if body.contains("GOT:") {
        common::text("parent done")
    } else if body.contains("SPAWNED:") {
        common::js(
            "// wait and steer\nconst r = await agents.wait('a1');\nawait agents.send('a1', 'refine please');\nconst r2 = await agents.wait('a1');\nconst all = await agents.list();\nreturn 'GOT:' + [r.status, r.report, r2.report, all.length].join('|')",
        )
    } else {
        common::js(
            "// delegate\nconst id = await agents.spawn('find the answer', {title: 'Find the answer'});\nreturn 'SPAWNED:' + id",
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_wait_steer_and_resume_after_restart() {
    let url = common::serve(Arc::new(script)).await;
    let dir = temp("agents");
    let (events, mut seen) = tokio::sync::mpsc::unbounded_channel();
    let agents = Arc::new(Agents::new(Some(dir.join("agents")), Some(events)));
    let spec = spec(&url, &dir);
    let parent = Agent::new(common::model(&url)).host(agents.host("s1", spec.clone()), DOCS);
    assert!(parent.system.contains("agents.spawn"));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut messages = vec![Message::user("delegate it")];
    let run = parent
        .run(&mut messages, &tx, &CancellationToken::new())
        .await
        .unwrap();
    drop(tx);
    assert_eq!(run.message.text(), "parent done");
    let results: Vec<String> = messages
        .iter()
        .filter(|m| m.role == smash_ai::Role::Tool)
        .map(Message::text)
        .collect();
    assert!(results[0].contains("SPAWNED:a1"), "{results:?}");
    assert!(
        results[1].contains("GOT:done|child report|refined report|1"),
        "{results:?}"
    );

    // The parent's log names the calls.
    let mut verbs = Vec::new();
    while let Some(update) = rx.recv().await {
        if let Update::Call(call) = update
            && !call.running
        {
            verbs.push(format!("{} {}", call.verb, call.target));
        }
    }
    assert!(
        verbs.contains(&"agents.spawn Find the answer".to_string()),
        "{verbs:?}"
    );
    assert!(
        verbs.contains(&"agents.send a1 ← refine please".to_string()),
        "{verbs:?}"
    );

    // Child updates and status changes reach the UI channel, scoped.
    let mut steered = false;
    let mut statuses = Vec::new();
    while let Ok(event) = seen.try_recv() {
        match event {
            smash_backend::agents::Event::Update {
                scope,
                id,
                update: Update::Steered(text),
            } => {
                assert_eq!((scope.as_str(), id.as_str()), ("s1", "a1"));
                steered |= text == "refine please";
            }
            smash_backend::agents::Event::Changed { info, .. } => statuses.push(info.status),
            _ => {}
        }
    }
    assert!(steered, "the steering message shows up as a child update");
    assert_eq!(
        statuses,
        [Status::Running, Status::Idle, Status::Running, Status::Idle]
    );

    // Restart: a fresh registry finds a1 on disk with its whole history.
    let agents = Arc::new(Agents::new(Some(dir.join("agents")), None));
    let list = agents.list("s1");
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0].status, list[0].report.as_str()),
        (Status::Idle, "refined report")
    );
    assert_eq!(list[0].turns, 2);
    assert_eq!(
        list[0].title, "Find the answer",
        "the parent's name, kept across restarts"
    );
    assert!(
        agents.list("other").is_empty(),
        "sessions don't share subagents"
    );
    agents.send("s1", "a1", "again please", &spec).unwrap();
    let info = agents
        .wait("s1", "a1", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(info.report, "second report, history had: find the answer");
    let saved = std::fs::read_to_string(dir.join("agents/s1/a1.json")).unwrap();
    assert!(
        saved.contains("find the answer")
            && saved.contains("refine please")
            && saved.contains("again please")
    );

    // Errors explain themselves.
    let err = agents.send("s1", "a9", "x", &spec).unwrap_err();
    assert!(err.contains("a9") && err.contains("a1"), "{err}");
    assert!(
        agents
            .spawn("s1", " ", None, false, None, &spec)
            .unwrap_err()
            .contains("needs a task")
    );
    assert!(
        agents
            .spawn("s1", "t", None, false, Some("nope/nope"), &spec)
            .unwrap_err()
            .contains("unknown model")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn running_limits_one_writer_and_stop() {
    // Accepts and never answers: every child stays running until stopped.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let dir = temp("limits");
    let agents = Arc::new(Agents::new(None, None));
    let spec = spec(&url, &dir);

    let writer = agents
        .spawn("s", "change a file", None, true, None, &spec)
        .unwrap();
    assert!(writer.write);
    let err = agents
        .spawn("s", "change another", None, true, None, &spec)
        .unwrap_err();
    assert!(err.contains("one writer"), "{err}");
    for task in ["r1", "r2", "r3"] {
        agents.spawn("s", task, None, false, None, &spec).unwrap();
    }
    let err = agents
        .spawn("s", "r4", None, false, None, &spec)
        .unwrap_err();
    assert!(err.contains("already running"), "{err}");
    assert!(
        agents.spawn("other", "r", None, false, None, &spec).is_ok(),
        "limits are per session"
    );

    let quick = agents
        .wait("s", "a2", Duration::from_millis(50))
        .await
        .unwrap();
    assert_eq!(
        quick.status,
        Status::Running,
        "wait times out, still running"
    );

    for id in ["a1", "a2", "a3", "a4"] {
        agents.stop("s", id).unwrap();
    }
    for id in ["a1", "a2", "a3", "a4"] {
        let info = agents.wait("s", id, Duration::from_secs(10)).await.unwrap();
        assert_eq!(info.status, Status::Stopped, "{id}");
    }
    assert!(
        agents.spawn("s", "after", None, true, None, &spec).is_ok(),
        "slots free again"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_subagent_shares_one_system_prompt() {
    let system = smash_backend::agents::child_system();
    assert!(system.contains("You are a subagent"));
    assert!(
        !system.contains("read-only") && !system.contains("only agent changing"),
        "mode lives in the first message, not the shared prefix"
    );
}
