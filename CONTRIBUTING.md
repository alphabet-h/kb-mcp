# Contributing to kb-mcp

Thanks for considering a contribution! This document covers the essentials of working on kb-mcp.

> **日本語版**: [CONTRIBUTING.ja.md](./CONTRIBUTING.ja.md)

## Prerequisites

- Rust stable (edition 2024)
- Git
- ~3 GB of disk space for ONNX model caches when running ignored tests

## First-time setup

After cloning, opt in to the repository's git hooks once:

```bash
git config core.hooksPath .githooks
```

This activates `.githooks/pre-push`, which runs `cargo fmt --all -- --check` before every push so a missed `cargo fmt` cannot reach CI. The hook is shared with the rest of the team — see [`.githooks/pre-push`](./.githooks/pre-push). To bypass it in an emergency, append `--no-verify` to the push.

## Build and test

```bash
cargo build --release      # Release binary at target/release/kb-mcp(.exe)
cargo check --all-targets  # Quick type check
cargo test                 # Unit + integration tests (no model download)
cargo test -- --ignored    # Includes embedding / reranker tests that download models
```

`cargo test -- --ignored` downloads ONNX models on first run (BGE-small ~130 MB, BGE-M3 ~2.3 GB, BGE-reranker-v2-m3 ~2.3 GB). The models are cached per OS conventions — see the README's "Working around HuggingFace TLS failures" section if your network blocks the download.

## Code style

- `cargo fmt --all` before committing (enforced in CI)
- `cargo clippy --all-targets` must produce no warnings (enforced in CI)
- Japanese comments are welcome for Japanese-KB-specific logic (CJK tokenization, date formats, etc.); English otherwise

## Repository layout

- `src/parser/` — `Parser` trait + `Registry` (one impl per file format)
- `src/indexer.rs` — `walkdir` → parse → embed → store pipeline
- `src/db.rs` — SQLite + sqlite-vec + FTS5 storage, `search_hybrid` (RRF, k=60)
- `src/embedder.rs` — `fastembed-rs` wrapper (embeddings + cross-encoder rerankers)
- `src/server.rs` — `rmcp::ServerHandler` with six MCP tools
- `src/transport/` — stdio and Streamable HTTP transports
- `src/watcher.rs` — `notify-debouncer-full`-based incremental reindex
- `src/schema.rs` — frontmatter schema validation
- `src/quality.rs` / `src/graph.rs` — quality filter + BFS connection graph

See [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) for a detailed walkthrough.

## Test layering

- **Light tests**: default `cargo test`. No network, no model download, runs in seconds.
- **Ignored tests** (`#[ignore]`): require network + disk for model downloads. Opt in via `cargo test -- --ignored` or run in CI with a separate job.

When adding behavior that needs the embedder or reranker, mark the test `#[ignore]` and add a comment explaining what it exercises.

## Submitting changes

1. Fork the repo and branch from `main`
2. Add tests for new behavior (unit tests inline, integration tests under `tests/`)
3. `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
4. Open a PR describing the problem and the change; link any related issues

## Reporting bugs

Include:
- A minimal reproduction (commands, small sample KB if relevant)
- `kb-mcp --version`
- Operating system and Rust toolchain version (`rustc --version`)
- Expected vs observed behavior

## License

By contributing, you agree that your contributions are dual-licensed under **MIT OR Apache-2.0**, matching the project. See [LICENSE-MIT](./LICENSE-MIT) and [LICENSE-APACHE](./LICENSE-APACHE).
