# Smash Project Guidelines

## File Size Limit

All files in `crates/smash-shell/src/` must stay under 500 lines of code. If a file approaches this limit, extract functionality into a new module.

## Code Architecture

- Components should be generic and reusable. Use traits (`TextProvider`, `Copyable`, `Selectable`) for shared behavior.
- Keep selection/copy logic in `smash-shell` — not in application code like `cookbook.rs`.

## Roadmap / Planned Features

1. **Manager-based workflow** — a manager LLM orchestrates multiple worker LLMs, dispatching requests and reviewing their work before committing. Requires two or more concurrent chat sessions within a single session.
2. **Multi-LLM provider support** — abstracted provider interface so different LLM backends (OpenAI, Anthropic, local, etc.) can be swapped in without changing application code.
3. **Hash-based line addressing & editing** — address source lines by content hash rather than absolute line numbers, so edits survive re-indentation and minor reshuffling.
