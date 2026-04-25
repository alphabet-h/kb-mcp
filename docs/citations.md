# Citations

The `search` MCP tool returns `match_spans` for each hit, indicating where the
query terms matched within the chunk's `content`. This helps Claude / clients
quote source text accurately and reduces hallucination.

> **日本語版**: [citations.ja.md](./citations.ja.md)

## Output shape

```jsonc
{
  "results": [
    {
      "score": 0.0327,
      "path": "docs/foo.md",
      "content": "Use tokio::spawn for async tasks.",
      "match_spans": [
        {"start": 4,  "end": 9 },   // "tokio"
        {"start": 11, "end": 16}    // "spawn"
      ],
      // ... other fields
    }
  ]
}
```

## `match_spans` semantics

| Value | Meaning |
|---|---|
| `null` (key omitted) | Match spans not computed (current scope: query contains non-ASCII) |
| `[]` (empty array) | Computed, but no match was found |
| `[{...}, ...]` | Computed; one or more matches |

## Byte offsets

`start` / `end` are **byte offsets** into the chunk's `content` string. `kb-mcp`
guarantees that both indices fall on UTF-8 codepoint boundaries, so clients can
safely slice:

```typescript
const snippet = content.slice(span.start, span.end);
```

In Rust:

```rust
let snippet = content.get(span.start..span.end).unwrap_or("");
```

If you ever observe a span that breaks codepoint boundaries, please file a bug.

## What gets matched

`match_spans` are computed by:

1. Splitting the query on whitespace into terms.
2. Lower-casing both query and content (ASCII fold only).
3. Searching for each term as a substring (case-insensitive) in `content`.
4. Reporting all match positions, sorted by start byte, deduped.

## Non-ASCII queries

When the query contains **any non-ASCII character** (e.g., Japanese, emoji),
`match_spans` is omitted from the JSON output entirely (key not present).

This is a deliberate MVP limitation. Substring matching on non-ASCII text would
miss the granularity that the FTS5 trigram tokenizer provides on the search
side, leading to confusing results. A future feature will use FTS5's `snippet()`
function for precise span extraction across all languages.

## Empty results

When `results: []` is returned, `match_spans` simply isn't relevant (there's no
chunk to point into). The `low_confidence` flag should be checked for the
"no relevant content" signal.

## Related

- `docs/filters.md` — narrowing search results
- `README.md` — full search tool reference
