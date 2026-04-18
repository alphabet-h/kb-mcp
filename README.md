# kb-mcp

MCP server for semantic search over a Markdown knowledge base.

Parses Markdown files with YAML frontmatter, splits them into heading-based chunks, generates embeddings with a selectable model (BGE-small-en-v1.5 by default, BGE-M3 for multilingual/Japanese knowledge bases), and stores everything in SQLite with sqlite-vec for vector similarity search. Connects to Claude Code, Cursor, or any MCP-compatible client via stdio transport.

## Build

```bash
cargo build --release
```

The binary is produced at `target/release/kb-mcp` (or `kb-mcp.exe` on Windows).

## Usage

### Build / rebuild the search index

```bash
kb-mcp index --kb-path /path/to/knowledge-base
kb-mcp index --kb-path /path/to/knowledge-base --force   # full re-index
kb-mcp index --kb-path /path/to/knowledge-base --model bge-m3 --force  # switch to BGE-M3 (1024 dim, multilingual)
```

Scans all `.md` files under the given directory, skipping `.obsidian/`. Files whose content hash has not changed since the last run are skipped unless `--force` is passed.

`--model` accepts:
- `bge-small-en-v1.5` (default) — 384 dim, English-focused, ~130 MB first download.
- `bge-m3` — 1024 dim, multilingual (100+ languages incl. Japanese), ~2.3 GB first download. Recommended for Japanese-heavy knowledge bases.

Switching models on an existing index requires `--force` (the DB records the model/dim in `index_meta` and rejects mismatched runtimes).

### Start the MCP server

```bash
kb-mcp serve --kb-path /path/to/knowledge-base
kb-mcp serve --kb-path /path/to/knowledge-base --model bge-m3   # must match the indexed model
kb-mcp serve --kb-path ... --model bge-m3 --reranker bge-v2-m3  # + cross-encoder reranking
```

Starts the MCP server on stdio transport. The server exposes 5 tools (see below) and keeps the index in-process for low-latency queries. `--model` must match the model that built the current index, otherwise the server refuses to start with an actionable error message.

`--reranker` (optional, default `none`) enables a cross-encoder re-ranking pass over the top candidates of the hybrid search:

- `none` — disabled (default).
- `bge-v2-m3` — BAAI/bge-reranker-v2-m3 (multilingual 100+, ~2.3 GB first download). Recommended for Japanese knowledge bases.
- `jina-v2-ml` — jinaai/jina-reranker-v2-base-multilingual (multilingual, ~1.2 GB). Lighter alternative.
- `bge-base` — BAAI/bge-reranker-base (English/Chinese only, ~280 MB). Not recommended for Japanese.

Latency cost of rerank is roughly 300–700 ms per query on CPU with `bge-v2-m3` over 50 candidates. `--rerank-by-default` (on by default when `--reranker` is set) controls whether every `search` call uses rerank; the MCP tool takes `rerank: Option<bool>` to override per-query. Switching the reranker does **not** require re-indexing (it is index-independent).

### Show index status

```bash
kb-mcp status --kb-path /path/to/knowledge-base
```

Prints document and chunk counts from the existing index.

## Connecting to Claude Code / Cursor

Add the following to `.mcp.json` in your project root (or the equivalent MCP config for your client):

```json
{
  "mcpServers": {
    "ai-knowledge": {
      "command": "/path/to/kb-mcp",
      "args": ["serve", "--kb-path", "/path/to/knowledge-base"],
      "type": "stdio"
    }
  }
}
```

The server will be started automatically when the client connects.

## Tools

| Tool | Description | Key parameters |
|---|---|---|
| `search` | Hybrid search (vector + FTS5 full-text) merged via Reciprocal Rank Fusion, optionally followed by cross-encoder reranking. Returns chunks ranked by relevance. | `query` (required), `limit`, `category`, `topic`, `rerank` (override server default) |
| `list_topics` | List all indexed topics and categories with document counts. | (none) |
| `get_document` | Get the full content and metadata of a document by its relative path. | `path` (e.g. `"deep-dive/mcp/overview.md"`) |
| `get_best_practice` | Get a PERFECT.md best-practices document, optionally extracting a specific h2 section. | `target` (e.g. `"claude-code"`), `category` (optional) |
| `rebuild_index` | Rebuild the search index by scanning all Markdown files. | `force` (optional, default false) |

## Notes

- **Embedding model**: On first run, the selected ONNX model is downloaded to an OS-standard cache directory. Subsequent runs reuse the cached model. Resolution order:
  1. `FASTEMBED_CACHE_DIR` environment variable, if set.
  2. OS cache dir joined with `fastembed` (Linux: `~/.cache/fastembed`, macOS: `~/Library/Caches/fastembed`, Windows: `%LOCALAPPDATA%\fastembed`).
  3. `.fastembed_cache` under the current working directory (final fallback).
- **Index storage**: The SQLite database is stored as `.kb-mcp.db` in the **parent** directory of the `--kb-path` (i.e. the repository root when `--kb-path` points to `knowledge-base/`).
- **Embedding dimensions**: Depends on `--model`. BGE-small-en-v1.5 = 384, BGE-M3 = 1024. The chosen dim is declared on the `vec_chunks` virtual table and recorded in the `index_meta` table; a mismatch at runtime is detected and rejected.
- **Incremental indexing**: Files are tracked by SHA-256 content hash. Only changed files are re-embedded on subsequent `index` runs (unless `--force` is passed).
- **Hybrid search (FTS5 + vector)**: The `search` tool combines SQLite FTS5 full-text search (trigram tokenizer, works for Japanese/CJK too; `heading` column is weighted 2× `content` in bm25) with the vector search via Reciprocal Rank Fusion (k=60). The returned `score` is the RRF score (higher = better), not a distance. Queries shorter than 3 characters fall back to vector-only (below the trigram minimum).
- **Optional reranking**: With `--reranker <model>` the top candidates are re-scored by a cross-encoder before being returned. When rerank is applied, `score` is the cross-encoder raw score instead of the RRF value. Reranking is index-independent — you can toggle it at server start without re-indexing.
