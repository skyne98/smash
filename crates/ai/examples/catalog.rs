//! Parse the real models.dev list and time it.
//!   cargo run -q --release -p smash-ai --example catalog -- /tmp/models.json
fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/models.json".into());
    let json = std::fs::read_to_string(&path).expect("models.dev json");
    let start = std::time::Instant::now();
    let catalog = smash_ai::catalog::Catalog::parse(&json).expect("parse");
    let took = start.elapsed();
    let available = catalog.available();
    println!(
        "{} usable models ({} KB) parsed in {took:?}; {} have a key set",
        catalog.entries.len(),
        json.len() / 1024,
        available.len()
    );
    for entry in available.iter().take(8) {
        println!(
            "  {:<40} ctx {:>8}  efforts {:?}  price {:?}",
            entry.key(),
            entry.context,
            entry.efforts,
            entry.price.map(|p| (p.input, p.output))
        );
    }
}
