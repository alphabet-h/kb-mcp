# Architecture

Source-level structure and data flow of kb-mcp, for contributors extending or modifying the codebase.

> **日本語版**: [ARCHITECTURE.ja.md](./ARCHITECTURE.ja.md)

## Source layout

| File | Responsibility |
|---|---|
| `src/main.rs` | clap CLI entry. Dispatches `index` / `status` / `serve` / `search` / `graph` / `validate` subcommands. Loads `kb-mcp.toml` and merges with CLI args. JSON / text output formatting. |
| `src/config.rs` | Loads the binary-adjacent `kb-mcp.toml`. Resolves `CLI > config > default` precedence. Injects `FASTEMBED_CACHE_DIR` env when the config sets it and the env is unset. |
| `src/server.rs` | `rmcp::ServerHandler` impl. Dispatches six MCP tools. `search` routes to `db.search_hybrid`. |
| `src/indexer.rs` | `walkdir`-based file scan using `Registry::extensions()`. Parses via the Parser trait, embeds, stores. SHA-256 content-hash diff detection. Incremental APIs (`reindex_single_file` / `deindex_single_file` / `rename_single_file`) shared with the file watcher. |
| `src/parser/` | Parser trait + Registry. `mod.rs` (Frontmatter / Chunk / ParsedDocument), `markdown.rs`, `txt.rs`, `registry.rs` (extension lookup). |
| `src/markdown.rs` | Thin shim over `crate::parser::markdown::MarkdownParser`, retained for legacy `parse()` / `parse_with_excludes()` callers. |
| `src/watcher.rs` | `notify-debouncer-full` bridged to a tokio channel. Filters by extension and path, then dispatches to `indexer::{reindex,deindex,rename}_single_file`. Runs alongside the MCP server via `tokio::spawn`. |
| `src/transport/` | MCP transport abstraction. `mod.rs` (Transport enum + CLI/config resolution), `stdio.rs` (stdio), `http.rs` (rmcp `StreamableHttpService` + axum, mounts `/mcp` and `/healthz`). `KbServerShared` is `Arc`-shared through a session factory so each connection gets a lightweight handle. |
| `src/schema.rs` | Frontmatter schema validation. Reads `kb-mcp-schema.toml` under `kb_path`, enforces `required` / `type` / `pattern` / `enum` / `min_length` / `max_length` / `allow_empty`. Invoked by the `kb-mcp validate` CLI which reports in text / JSON / GitHub-annotation formats. |
| `src/embedder.rs` | Thin wrapper over `fastembed-rs`. `ModelChoice` selects the embedding model (BGE-small-en-v1.5 / BGE-M3). `RerankerChoice` + `Reranker` provide optional cross-encoder reranking. |
| `src/db.rs` | `rusqlite` + `sqlite-vec` + FTS5 (trigram). Manages the `chunks` / `vec_chunks` / `fts_chunks` schemas and CRUD. Exposes `search_hybrid` (Reciprocal Rank Fusion, `k = 60`). |
| `src/quality.rs` | Per-chunk quality scoring (length / boilerplate / structure signals). |
| `src/graph.rs` | Connection graph BFS over the vector index, for the `get_connection_graph` MCP tool and the `kb-mcp graph` CLI. |

## Data flow

```
.md / .txt files (filtered by Registry::extensions())
     │
     ▼ walkdir
indexer.rs: SHA-256 content-hash diff vs the chunks.hash column
     │
     ▼ changed files only
parser/: dispatch by extension → extract frontmatter + title + chunk
     │
     ▼
embedder.rs: embedding via fastembed
              (BGE-small-en-v1.5 → 384 dim, BGE-M3 → 1024 dim)
     │
     ▼
db.rs: UPSERT into chunks (metadata)
       + vec_chunks (embedding)
       + fts_chunks (FTS5 trigram)
```

At query time the `search` tool runs a hybrid:

- query → embedder → `vec_chunks MATCH` (top-N)
- query → sanitize → `fts_chunks MATCH` + bm25 (top-N) — heading weighted 2×
- Reciprocal Rank Fusion on the Rust side (`k = 60`) → top-`limit` returned
- (optional) cross-encoder reranker re-scores the top candidates before return

## Embedding cache resolution

`embedder.rs::resolve_cache_dir()` picks in order:

1. `FASTEMBED_CACHE_DIR` env var (highest priority)
2. OS-standard cache directory joined with `fastembed`:
   - Linux: `~/.cache/fastembed`
   - macOS: `~/Library/Caches/fastembed`
   - Windows: `%LOCALAPPDATA%\fastembed`
3. `.fastembed_cache/` under CWD (final fallback)

First run downloads the chosen ONNX model to a HuggingFace-hub-compatible cache layout (BGE-small: ~130 MB, BGE-M3: ~2.3 GB, BGE-reranker-v2-m3: ~2.3 GB). Subsequent runs reuse the cache without re-downloading.

If `fastembed-rs`'s native TLS to HuggingFace fails (corporate proxies / TLS inspection), see the README's "Working around HuggingFace TLS failures" section for a `huggingface_hub` CLI workaround.

## Key dependencies

- **`rmcp`** 1.x — MCP server framework (stdio + Streamable HTTP transports)
- **`fastembed`** 5.x — ONNX-based embeddings / rerankers
- **`rusqlite`** 0.39 with `bundled` — statically linked SQLite 3.50+; FTS5 with trigram tokenizer and `contentless_delete = 1` enabled
- **`sqlite-vec`** 0.1 — vector similarity search extension
- **`pulldown-cmark`** 0.13 — Markdown parser
- **`notify`** 8 + **`notify-debouncer-full`** 0.6 — file watcher with debouncing
- **`axum`** 0.8 — HTTP server for the Streamable HTTP transport
- **`dirs`** 6 — OS-standard cache directory resolution
