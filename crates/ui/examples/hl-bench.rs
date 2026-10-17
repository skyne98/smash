//! cargo run --release -p smash-ui --example hl-bench
fn main() {
    let code = include_str!("../src/markdown.rs");
    smash_ui::highlight::highlight("rust", "fn warm() {}");
    let start = std::time::Instant::now();
    let lines = smash_ui::highlight::highlight("rust", code);
    let took = start.elapsed();
    println!(
        "{} lines, {} KB in {:?} ({:.1} MB/s)",
        lines.len(),
        code.len() / 1024,
        took,
        code.len() as f64 / took.as_secs_f64() / 1e6
    );
}
