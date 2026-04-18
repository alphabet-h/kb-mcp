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

# Heading substrings to exclude from chunking. Omit the key for the default
# ["次の深堀り候補"]. An explicit empty array disables exclusion entirely.
exclude_headings = ["次の深堀り候補", "参考リンク"]

# Per-chunk quality filter (feature 13). Enabled by default, threshold 0.3.
# Set `enabled = false` to restore pre-feature-13 behavior (return every chunk).
[quality_filter]
enabled = true
threshold = 0.3
```

With the file in place `kb-mcp serve` / `index` / `status` / `graph` / `search` all work without any of those flags. Unknown keys are rejected to catch typos early. `FASTEMBED_CACHE_DIR` from the real environment overrides the file entry.

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

`--format` is `json` (default, an array of `{score, path, title, heading, topic, date, content}`) or `text` (LLM-friendly blocks separated by `---`). All other flags mirror `serve`: `--kb-path`, `--model`, `--reranker`, `--category`, `--topic`, `--limit`. The quality filter is on by default — pass `--include-low-quality` or `--min-quality 0` to restore pre-feature-13 behavior for a single query. The `kb-mcp.toml` defaults apply exactly as in `serve`/`index`.

Typical skill-bin use: a Claude Code skill places `kb-mcp.exe` + `kb-mcp.toml` in its `bin/`, then a command like `kb-mcp search "{{user_query}}" --format text --limit 3` returns a focused reference excerpt for the LLM to cite.

### Connection graph from a starting document

When you want to find not just a single document but the semantic neighborhood around it (and neighbors of those neighbors), use the `graph` subcommand:

```bash
kb-mcp graph --start deep-dive/mcp/overview.md --depth 2 --fan-out 5
kb-mcp graph --start notes/rag.md --dedup-by-path --format text
kb-mcp graph --start a.md --exclude junk1.md,junk2.md --min-similarity 0.5
```

Flags:

- `--start PATH` — required, relative path to an indexed document.
- `--depth` (default 2, clamped to max 3) — BFS hops.
- `--fan-out` (default 5, clamped to max 20) — neighbors per node per hop. `0` returns only the seed.
- `--min-similarity` (default 0.3) — cosine similarity cut-off. `0.0..=1.0`.
- `--seed-strategy` — `all-chunks` (default) expands from every chunk of the start doc; `centroid` averages them (L2-renormalized) into one virtual seed.
- `--exclude` — comma-separated paths to drop from results. The start path itself is always excluded.
- `--dedup-by-path` — collapse same-path hits so each document appears at most once.
- `--category` / `--topic` — apply category / topic filters to every hop.
- `--format json|text` — same as `search`.

The output is a flat array of nodes with `parent_id` / `depth` / `score` so the consumer can reconstruct the tree if it wants. Good use cases: "give me 30 chunks of related context around this note for the LLM to read", or "walk two hops from this overview to see what topics it touches".

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

### Keeping the index fresh via PostToolUse hook (feature 19)

If you edit the knowledge base from inside a Claude Code session (or run a skill that writes Markdown files), the running MCP server will keep returning stale results until the index is rebuilt. A `PostToolUse` hook in `.claude/settings.json` can re-index automatically after every write. Minimal form:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write|Edit|MultiEdit|Skill",
        "hooks": [
          { "type": "command", "command": "kb-mcp index" }
        ]
      }
    ]
  }
}
```

SHA-256 diffing in `kb-mcp index` makes the second-and-later invocations fast (usually sub-second on small KBs). A richer shell script that inspects the tool payload and only rebuilds when the edited file is under `$KB_PATH` ships with the repo: see [`examples/hooks/`](./examples/hooks/README.md). SQLite runs in WAL mode so the hook can safely run while the MCP server is still up.

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
| `search` | Hybrid search (vector + FTS5 full-text) merged via Reciprocal Rank Fusion, optionally followed by cross-encoder reranking. Returns chunks ranked by relevance. | `query` (required), `limit`, `category`, `topic`, `rerank` (override server default), `min_quality` (override quality filter 0.0-1.0), `include_low_quality` (disable the filter for this query) |
| `list_topics` | List all indexed topics and categories with document counts. | (none) |
| `get_document` | Get the full content and metadata of a document by its relative path. | `path` (e.g. `"deep-dive/mcp/overview.md"`) |
| `get_best_practice` | Get a best-practices document for the given target, optionally extracting a specific h2 section. Path resolution uses `[best_practice].path_templates` from `kb-mcp.toml` (default: `best-practices/{target}/PERFECT.md`), so arbitrary KB layouts are supported. | `target` (e.g. `"claude-code"`), `category` (optional) |
| `rebuild_index` | Rebuild the search index by scanning all Markdown files. | `force` (optional, default false) |
| `get_connection_graph` | BFS-expand semantically related chunks starting from a document path. Returns a flat list of nodes with `parent_id` / `depth` / `score` / `snippet` so the caller can chain context discovery. | `path` (required), `depth` (default 2, max 3), `fan_out` (default 5, max 20), `min_similarity` (default 0.3), `seed_strategy` (`all_chunks` / `centroid`), `dedup_by_path`, `category`, `topic`, `exclude_paths` |

## Notes

- **Embedding model**: On first run, the selected ONNX model is downloaded to an OS-standard cache directory. Subsequent runs reuse the cached model. Resolution order:
  1. `FASTEMBED_CACHE_DIR` environment variable, if set.
  2. OS cache dir joined with `fastembed` (Linux: `~/.cache/fastembed`, macOS: `~/Library/Caches/fastembed`, Windows: `%LOCALAPPDATA%\fastembed`).
  3. `.fastembed_cache` under the current working directory (final fallback).
- **Index storage**: The SQLite database is stored as `.kb-mcp.db` in the **parent** directory of the `--kb-path` (i.e. the repository root when `--kb-path` points to `knowledge-base/`).
- **Embedding dimensions**: Depends on `--model`. BGE-small-en-v1.5 = 384, BGE-M3 = 1024. The chosen dim is declared on the `vec_chunks` virtual table and recorded in the `index_meta` table; a mismatch at runtime is detected and rejected.
- **Incremental indexing**: Files are tracked by SHA-256 content hash. Only changed files are re-embedded on subsequent `index` runs (unless `--force` is passed). Moving / renaming a file without modifying its content is detected via hash match and handled as a `documents.path` UPDATE — the existing chunks, embeddings, and FTS rows are reused instead of being rebuilt. The rebuild summary reports the number of renames as `renamed` next to `updated` / `deleted`.
- **Hybrid search (FTS5 + vector)**: The `search` tool combines SQLite FTS5 full-text search (trigram tokenizer, works for Japanese/CJK too; `heading` column is weighted 2× `content` in bm25) with the vector search via Reciprocal Rank Fusion (k=60). The returned `score` is the RRF score (higher = better), not a distance. Queries shorter than 3 characters fall back to vector-only (below the trigram minimum).
- **Optional reranking**: With `--reranker <model>` the top candidates are re-scored by a cross-encoder before being returned. When rerank is applied, `score` is the cross-encoder raw score instead of the RRF value. Reranking is index-independent — you can toggle it at server start without re-indexing.
- **Connection graph**: `get_connection_graph` / `kb-mcp graph` do BFS over the vector index starting from a document. No extra index is built; every hop runs a fresh sqlite-vec KNN. Bounded by `depth ≤ 3` / `fan_out ≤ 20` with client-side clamping, so worst-case is ~21 KNN queries per request. Scores are cosine similarity approximated from L2 distance (`1 - d²/2`, clamped to `[0,1]`) assuming unit-normalized embeddings (BGE-small / BGE-M3 are normalized internally).
- **Heading exclusion**: Sections whose heading text contains any of `exclude_headings` (defaults to `["次の深堀り候補"]`) are dropped during chunking. Set `exclude_headings = []` in `kb-mcp.toml` to disable the default. Matching is substring-based (`heading.contains(pattern)`), so short patterns catch suffixed variants (`"## 次の深堀り候補 (案)"` etc.).
- **`get_best_practice` path templates** (feature 16): the file a call like `get_best_practice(target: "claude-code")` reads is resolved through `[best_practice].path_templates` in `kb-mcp.toml`. Each template may use `{target}` as a placeholder. The server tries templates in order and returns the first existing file under `kb_path` (path-traversal attempts are rejected). The default list is `["best-practices/{target}/PERFECT.md"]` so legacy setups keep working; add entries like `"docs/{target}.md"` to support different KB layouts without changing the tool call site.
- **Per-chunk quality filter** (feature 13, **enabled by default** with threshold `0.3`): each indexed chunk gets a `quality_score` computed from three signals — length (< 30 chars → -0.6), boilerplate-only content (TBD / TODO / 詳細は後述 / etc. → -0.5), poor structure (single line < 80 chars → -0.3). Chunks scoring below the threshold are hidden from `search`, `kb-mcp search`, and `get_connection_graph`. Seed chunks of `get_connection_graph` are exempt. Disable the filter with `[quality_filter] enabled = false` in `kb-mcp.toml`, or opt out per-query with `--include-low-quality` (CLI) / `include_low_quality: true` (MCP). Override the threshold with `--min-quality 0.5` / `min_quality: 0.5`. Upgrading an existing index: the next `kb-mcp index` run transparently adds the `quality_score` column (ALTER TABLE) and backfills scores once (idempotent).
