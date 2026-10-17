//! JSON benchmark at session scale: owned vs borrowed, copy vs in-place.
//!   cargo run -q --release -p smash-ext --example json-bench
//!
//! Payload is a realistic session transcript: messages with text, thinking and
//! tool-call parts, string dominated, no escapes (so an in-place parse never
//! has to rewrite bytes).

use std::time::Instant;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Msg {
    id: u64,
    role: String,
    content: Vec<Part>,
}

#[derive(Serialize, Deserialize)]
enum Part {
    Text { text: String },
    Thinking { text: String },
    ToolCall { name: String, arguments: String },
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct BMsg<'a> {
    id: u64,
    role: &'a str,
    content: Vec<BPart<'a>>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
enum BPart<'a> {
    Text { text: &'a str },
    Thinking { text: &'a str },
    ToolCall { name: &'a str, arguments: &'a str },
}

fn session(messages: usize) -> Vec<Msg> {
    let body =
        "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor ".repeat(6);
    (0..messages as u64)
        .map(|id| Msg {
            id,
            role: if id % 2 == 0 {
                "user".into()
            } else {
                "assistant".into()
            },
            content: vec![
                Part::Text {
                    text: format!("{body}/{id}"),
                },
                Part::Thinking {
                    text: format!("thinking about {id}: {body}"),
                },
                Part::ToolCall {
                    name: "read_file".into(),
                    // Escape-free on purpose: a borrowed &str cannot point into
                    // JSON that needs unescaping.
                    arguments: format!("read_file path=/tmp/{id}"),
                },
            ],
        })
        .collect()
}

fn bench(mut parse: impl FnMut(&[u8]), bytes: &[u8], name: &str) {
    let iters = (300_000_000 / bytes.len().max(1)).clamp(3, 200_000);
    for _ in 0..iters.div_ceil(10) {
        parse(bytes);
    }
    let start = Instant::now();
    for _ in 0..iters {
        parse(bytes);
    }
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "{name:34} {:9.2} ms/op {:8.1} MB/s",
        elapsed * 1e3 / iters as f64,
        bytes.len() as f64 * iters as f64 / elapsed / 1e6
    );
}

fn main() {
    for messages in [1usize, 10, 100, 2_000, 10_000] {
        let bytes = serde_json::to_vec(&session(messages)).unwrap();
        println!(
            "--- {messages} messages, {:.1} MB ---",
            bytes.len() as f64 / 1e6
        );

        bench(
            |input| {
                let _: Vec<Msg> = serde_json::from_slice(input).unwrap();
            },
            &bytes,
            "serde_json owned",
        );
        bench(
            |input| {
                let _: Vec<Msg> = sonic_rs::from_slice(input).unwrap();
            },
            &bytes,
            "sonic_rs owned",
        );
        let mut scratch = vec![0u8; bytes.len() + 64];
        bench(
            |input| {
                scratch[..input.len()].copy_from_slice(input);
                let _: Vec<Msg> =
                    simd_json::serde::from_slice(&mut scratch[..input.len()]).unwrap();
            },
            &bytes,
            "simd_json owned + copy",
        );

        bench(
            |input| {
                let _: Vec<BMsg> = serde_json::from_slice(input).unwrap();
            },
            &bytes,
            "serde_json borrowed",
        );
        bench(
            |input| {
                let _: Vec<BMsg> = sonic_rs::from_slice(input).unwrap();
            },
            &bytes,
            "sonic_rs borrowed",
        );
        let mut scratch = vec![0u8; bytes.len() + 64];
        bench(
            |input| {
                scratch[..input.len()].copy_from_slice(input);
                let _: Vec<BMsg> =
                    simd_json::serde::from_slice(&mut scratch[..input.len()]).unwrap();
            },
            &bytes,
            "simd_json borrowed + copy",
        );

        // No copy: the payload has no escapes, so an in-place parse leaves the
        // buffer unchanged and can be repeated.
        let mut owned = vec![0u8; bytes.len() + 64];
        owned[..bytes.len()].copy_from_slice(&bytes);
        bench(
            |_| {
                let _: Vec<BMsg> = simd_json::serde::from_slice(&mut owned[..bytes.len()]).unwrap();
            },
            &bytes,
            "simd_json borrowed in place",
        );

        bench(
            |input| {
                let mut copy = vec![0u8; input.len() + 64];
                copy[..input.len()].copy_from_slice(input);
                std::hint::black_box(&copy);
            },
            &bytes,
            "memcpy baseline",
        );
        println!();
    }
}
