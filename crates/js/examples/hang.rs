//! Repro: a program whose last statement is a slow sh call, timed.
use std::time::{Duration, Instant};
#[tokio::main]
async fn main() {
    let code = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "await Promise.all([sh('ls'), sh('sleep 1')]); return 'done'".into());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<smash_js::Call>();
    let start = Instant::now();
    let watch = tokio::spawn(async move {
        while let Some(call) = rx.recv().await {
            if call.running {
                println!("{:>6.2}s start {} {}", start.elapsed().as_secs_f64(), call.verb, call.target);
                continue;
            }
            println!(
                "{:>6.2}s call {} {} {}",
                start.elapsed().as_secs_f64(),
                call.verb,
                call.target,
                call.info
            );
        }
    });
    let outcome = smash_js::run(
        &code,
        std::path::Path::new("."),
        smash_js::Limits {
            timeout: Duration::from_secs(20),
            ..Default::default()
        },
        tx,
    )
    .await;
    println!(
        "{:>6.2}s done calls={} error={:?}\n{}",
        start.elapsed().as_secs_f64(),
        outcome.calls,
        outcome.error,
        outcome.text
    );
    let _ = watch.await;
}
