# kb-mcp

MCP server for semantic search over a Markdown knowledge base.

Parses Markdown files with YAML frontmatter, splits them into heading-based chunks, generates embeddings with a selectable model (BGE-small-en-v1.5 by default, BGE-M3 for multilingual/Japanese knowledge bases), and stores everything in SQLite with sqlite-vec for vector similarity search. Connects to Claude Code, Cursor, or any MCP-compatible client via stdio transport.

## Build

```bash
cargo build --release
```

The binary is produced at `target/release/kb-mcp` (or `kb-mcp.exe` on Windows).

## Optional config file

Any CLI option below can be given a default via `kb-mcp.toml` placed **next to the binary**. CLI arguments always win; the file just removes repetition for a given deployment. Copy `kb-mcp.toml.example` to `kb-mcp.toml` and edit:

```toml
# kb-mcp.toml (sits next to kb-mcp / kb-mcp.exe)
kb_path = "/path/to/knowledge-base"
model = "bge-m3"
reranker = "bge-v2-m3"
rerank_by_default = true
fastembed_cache_dir = "/home/you/.cache/huggingface/hub"
```

With the file in place `kb-mcp serve` / `index` / `status` work without any of those flags. Unknown keys are rejected to catch typos early. `FASTEMBED_CACHE_DIR` from the real environment overrides the file entry.

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

#### Model selection trade-offs

| Aspect | BGE-small-en-v1.5 | BGE-M3 |
|---|---|---|
| First-time download | ~130 MB | ~2.3 GB |
| Embedding dim | 384 | 1024 (index file ~2.6× larger) |
| RAM when loaded | ~500 MB | ~2 GB |
| Index build time | baseline | ~3–10× slower (CPU inference) |
| Japanese precision | poor (English-centric vocab) | strong (multilingual tokenizer + training) |
| English precision | strong | comparable |

Switching cost (existing index → new model):

1. `kb-mcp index --kb-path ... --model <new> --force` runs a full re-embedding (no incremental update possible; `DELETE FROM documents/chunks/vec_chunks` and start over).
2. Every `serve` / `index` call afterwards must pass the same `--model` (or have it set in `kb-mcp.toml`). A mismatch is rejected at startup by the `index_meta` check.

Practical recommendation: pick the model that matches your knowledge base's **primary language** up front. Don't oscillate between models unless you have a concrete precision problem — the full re-embedding is the expensive step.

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

#### When to enable reranking

Rerank trades latency for precision. The right choice depends on usage pattern:

| Scenario | Recommendation |
|---|---|
| Interactive agent flows (the LLM calls `search` 2–5 times per turn) | **Leave off.** +500 ms × N search calls adds up fast; retrieval quality from BGE-M3 + heading-weighted bm25 is usually sufficient. |
| One-shot, precision-critical queries (research, definitive answers) | **Enable.** The latency tax is paid once per turn, and the cross-encoder meaningfully promotes semantically relevant candidates. |
| Mixed usage | Start with `rerank_by_default = false` and let the caller opt in per query via the MCP tool's `rerank: true` parameter. |

Symptoms that suggest you should turn rerank on:

- Top-5 results often miss the obviously right chunk even after query rewording.
- Queries that use synonyms / paraphrases of the indexed wording are failing (e.g. Japanese 「バグ」 vs English "error").
- The agent re-queries multiple times per turn, wasting context by reading wrong hits.

Because rerank is index-independent, you can enable it for a week, measure the quality delta, and disable it if the benefit is not visible — no re-indexing needed.

### Show index status

```bash
kb-mcp status --kb-path /path/to/knowledge-base
```

Prints document and chunk counts from the existing index.

### One-shot search from the command line

For shell scripts or skill bins that just need "search this string in the KB" without standing up an MCP connection:

```bash
kb-mcp search "RAG server comparison" --limit 3 --format text
kb-mcp search "E0382" --category deep-dive --format json | jq '.[] | .path'
kb-mcp search "クエリ最適化" --reranker bge-v2-m3        # optional per-invocation rerank
```

`--format` is `json` (default, an array of `{score, path, title, heading, topic, date, content}`) or `text` (LLM-friendly blocks separated by `---`). All other flags mirror `serve`: `--kb-path`, `--model`, `--reranker`, `--category`, `--topic`, `--limit`. The `kb-mcp.toml` defaults apply exactly as in `serve`/`index`.

Typical skill-bin use: a Claude Code skill places `kb-mcp.exe` + `kb-mcp.toml` in its `bin/`, then a command like `kb-mcp search "{{user_query}}" --format text --limit 3` returns a focused reference excerpt for the LLM to cite.

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

With a multilingual model and reranker enabled:

```json
{
  "mcpServers": {
    "ai-knowledge": {
      "command": "/path/to/kb-mcp",
      "args": [
        "serve",
        "--kb-path", "/path/to/knowledge-base",
        "--model", "bge-m3",
        "--reranker", "bge-v2-m3"
      ],
      "env": {
        "FASTEMBED_CACHE_DIR": "/path/to/.cache/huggingface/hub"
      },
      "type": "stdio"
    }
  }
}
```

For agent workflows, a more conservative alternative: load the reranker but leave it off by default, letting the caller opt in with `rerank: true` on individual `search` calls.

```json
{
  "mcpServers": {
    "ai-knowledge": {
      "command": "/path/to/kb-mcp",
      "args": [
        "serve",
        "--kb-path", "/path/to/knowledge-base",
        "--model", "bge-m3",
        "--reranker", "bge-v2-m3",
        "--rerank-by-default=false"
      ],
      "env": { "FASTEMBED_CACHE_DIR": "/path/to/.cache/huggingface/hub" },
      "type": "stdio"
    }
  }
}
```

Or, if you placed a `kb-mcp.toml` next to the binary with those options set, the `.mcp.json` can shrink to:

```json
{
  "mcpServers": {
    "ai-knowledge": {
      "command": "/path/to/kb-mcp",
      "args": ["serve"],
      "type": "stdio"
    }
  }
}
```

The server will be started automatically when the client connects.

### Working around HuggingFace TLS failures on first download

Some environments (corporate proxies, firewalls with TLS inspection) reject fastembed's native TLS connection to `huggingface.co` with `os error 10054` / "Connection was reset". In that case, pre-download the model via the Python HuggingFace CLI and point `FASTEMBED_CACHE_DIR` at the HF Hub cache:

```bash
# Install once
pip install --user huggingface_hub

# Pre-download BGE-M3 (required ONNX files only)
hf download BAAI/bge-m3 \
    --include 'onnx/*' 'tokenizer*' 'config.json' 'special_tokens_map.json'

# Pre-download BGE-reranker-v2-m3 (for `--reranker bge-v2-m3`)
hf download BAAI/bge-reranker-v2-m3

# Run kb-mcp pointing at the HF cache (HF Hub cache layout is compatible with fastembed)
FASTEMBED_CACHE_DIR=~/.cache/huggingface/hub \
    kb-mcp index --kb-path ./knowledge-base --model bge-m3 --force
```

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
