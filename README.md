# kb-mcp

MCP server for semantic search over a Markdown knowledge base.

Parses Markdown files with YAML frontmatter, splits them into heading-based chunks, generates embeddings with BGE-small-en-v1.5, and stores everything in SQLite with sqlite-vec for vector similarity search. Connects to Claude Code, Cursor, or any MCP-compatible client via stdio transport.

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
```

Scans all `.md` files under the given directory, skipping `.obsidian/`. Files whose content hash has not changed since the last run are skipped unless `--force` is passed.

### Start the MCP server

```bash
kb-mcp serve --kb-path /path/to/knowledge-base
```

Starts the MCP server on stdio transport. The server exposes 5 tools (see below) and keeps the index in-process for low-latency queries.

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
| `search` | Semantic search over the knowledge base. Returns chunks ranked by relevance. | `query` (required), `limit`, `category`, `topic` |
| `list_topics` | List all indexed topics and categories with document counts. | (none) |
| `get_document` | Get the full content and metadata of a document by its relative path. | `path` (e.g. `"deep-dive/mcp/overview.md"`) |
| `get_best_practice` | Get a PERFECT.md best-practices document, optionally extracting a specific h2 section. | `target` (e.g. `"claude-code"`), `category` (optional) |
| `rebuild_index` | Rebuild the search index by scanning all Markdown files. | `force` (optional, default false) |

## Notes

- **Embedding model**: On first run, the BGE-small-en-v1.5 ONNX model (~23 MB) is downloaded to an OS-standard cache directory. Subsequent runs reuse the cached model. Resolution order:
  1. `FASTEMBED_CACHE_DIR` environment variable, if set.
  2. OS cache dir joined with `fastembed` (Linux: `~/.cache/fastembed`, macOS: `~/Library/Caches/fastembed`, Windows: `%LOCALAPPDATA%\fastembed`).
  3. `.fastembed_cache` under the current working directory (final fallback).
- **Index storage**: The SQLite database is stored as `.kb-mcp.db` in the **parent** directory of the `--kb-path` (i.e. the repository root when `--kb-path` points to `knowledge-base/`).
- **Embedding dimensions**: 384 (float32), matching the vec0 virtual table schema.
- **Incremental indexing**: Files are tracked by SHA-256 content hash. Only changed files are re-embedded on subsequent `index` runs.
