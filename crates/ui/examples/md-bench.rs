//! Markdown parse speed: cargo run -q --release -p smash-ui --example md-bench

use std::time::Instant;

fn document(repeats: usize) -> String {
    let mut source = String::with_capacity(repeats * 400);
    for index in 0..repeats {
        source.push_str(&format!(
            "## Section {index}\n\nSome **bold**, *italic*, `inline code` and a [link](https://example.com/{index}).\n\n- first item\n- second item with `code`\n  - nested item\n\n```rust\nfn f{index}() -> u32 {{ {index} }}\n```\n\n| a | b |\n|---|---|\n| {index} | x |\n\n"
        ));
    }
    source
}

fn main() {
    for repeats in [10usize, 100, 1000] {
        let source = document(repeats);
        let bytes = source.len();
        for _ in 0..5 {
            std::hint::black_box(smash_ui::markdown::markdown(&source));
        }
        let runs = 50;
        let start = Instant::now();
        let mut blocks = 0;
        for _ in 0..runs {
            blocks = smash_ui::markdown::markdown(&source).len();
        }
        let elapsed = start.elapsed().as_secs_f64() / runs as f64;
        println!(
            "{:>6} KB  {:>8.2} us  {:>7.1} MB/s  {blocks} blocks",
            bytes / 1024,
            elapsed * 1e6,
            bytes as f64 / elapsed / 1e6
        );
    }
}
