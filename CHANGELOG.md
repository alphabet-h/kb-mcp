# Changelog

All notable changes to kb-mcp are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Internal
- Hardened DB transaction protection across the three write paths flagged
  by the 2026-04-29 audit (F-32):
  - `Database::upsert_document` now wraps the UPDATE branch's four
    statements (DELETE vec_chunks / DELETE fts_chunks / DELETE chunks /
    UPDATE documents) in an autocommit-aware tx via
    `Connection::unchecked_transaction()`. A failure on any of the four
    statements no longer leaves dangling vec / FTS rows whose `chunks`
    parent has already been removed.
  - `Database::insert_chunk` likewise wraps its three INSERTs (chunks +
    vec_chunks + fts_chunks) so a partial failure (e.g. embedding-dim
    mismatch on the `vec_chunks` insert) cannot leave a chunk visible to
    one search backend but invisible to the other.
  - `Database::rename_documents_atomic` replaces the manual
    `BEGIN`/`COMMIT`/`ROLLBACK` pair with `unchecked_transaction()` so
    that any `?` early-return path is rolled back by the `Transaction`
    Drop guard rather than relying on an explicit `ROLLBACK` call.
  - `indexer::index_single_disk_entry` now wraps `upsert_document`
    plus the per-chunk `insert_chunk` loop in a single tx via the new
    `Database::begin_transaction()` handle — embedding inference still
    runs *outside* the tx so a long-lived write tx does not block
    concurrent WAL readers. A partial failure mid-loop now rolls the
    whole file back instead of leaving a documents row paired with
    M < N chunks. Two regression tests
    (`test_begin_transaction_rolls_back_partial_writes_on_drop`,
    `test_begin_transaction_commits_on_explicit_commit`) lock down the
    Drop-rollback / commit symmetry.
- Added `proptest` 1 as a dev-dependency and locked the f64 value-range
  invariants of the retrieval-quality metrics: `recall_at_k`,
  `ndcg_at_k`, `reciprocal_rank`, and `chunk_quality_score` are now
  property-tested over randomized inputs to ensure each result is
  finite and in `[0.0, 1.0]`. This is a permanent guard against the
  v0.4.2 nDCG > 1.0 class of regression — any future change that lets
  one of these metrics escape the unit range will fail `cargo test`
  before it can ship.
- Migrated YAML parsing from `serde_yaml` 0.9 (deprecated and
  unmaintained — alias-bomb guards rely on the upstream limits in
  `unsafe-libyaml`) to `serde_yaml_bw` 2 ("YAML support for Serde
  with an emphasis on panic-free parsing"). Frontmatter (`Markdown`
  parser) and golden-YAML loading (`kb-mcp eval`) both move to the
  new crate. The `Value` enum gains a tag field so the only API
  delta is the pattern in the `RawFrontmatter` -> `Frontmatter`
  conversion (`Value::String(s, _)`, `Value::Number(n, _)`).
  Adds a smoke regression test that a YAML alias bomb does not
  panic the parser.

## [0.4.3] - 2026-04-29

### Security
- `get_document` MCP tool now rejects symlinks, restricts the file
  extension to the registered parser set, and caps file size at 1 MiB.
  Closes a pre-existing read primitive whereby a connected MCP client
  could call `get_document {path: ".git/config"}` (or any other
  non-indexed file under `kb_path`, including paths under
  `exclude_dirs`) and have the server return its contents — the prior
  defense was only a `kb_path`-prefix check on the canonicalized path,
  which is necessary but not sufficient because `canonicalize` resolves
  symlinks and the prefix check does not enforce the indexer's own
  scoping (extension whitelist, dir exclusions). The size cap mitigates
  a trivial RAM-OOM where one request reads a multi-GB file into a
  string buffer.

### Fixed
- `kb-mcp eval` becomes more robust against non-finite f64 values:
  - `reciprocal_rank` guards rank==0 → returns `0.0` (was `1.0/0.0
    = inf`, poisoning aggregate MRR; warn-logged when triggered).
  - `format_json` no longer panics on a previous `EvalRun` whose
    serialization fails (e.g. NaN/Inf survived from older history).
- `min_quality` and `min_confidence_ratio` MCP search params now
  reject NaN / ±Inf and fall back to the configured server defaults.
  Previously NaN flowed through `clamp(0.0, 1.0)` unchanged (NaN
  comparisons are all false), silently disabling the quality filter
  or low-confidence judgment depending on the path.
- `list_topics` MCP tool no longer fragments titles that contain the
  substring `||`. The aggregator now uses `json_group_array(title)`
  instead of `GROUP_CONCAT(title, '||') + .split("||")`.

### Documentation
- `examples/deployments/{personal,nas-shared,intranet-http}/.mcp.json`
  now set `"alwaysLoad": true` on the kb-mcp server entry. This is a
  Claude Code v2.1.121+ option that forces kb-mcp's tools to be present
  at initial load instead of going through the tool-search shortlist —
  appropriate for the "search anytime" RAG use case. Other MCP clients
  (Cursor, etc.) ignore the field. Each recipe README (en+ja) gains a
  note covering when to keep it on vs drop it (initial-startup latency
  trade-off, especially relevant for NAS-mounted KBs).
- Audit-driven docs cleanup (en+ja):
  - Fixed broken `serve` example code block in both READMEs
    (line continuation collapsed onto one line, fence didn't close).
  - `kb-mcp search --format json` examples now use `jq '.results[]'`
    against the v0.3.0+ wrapper shape instead of the obsolete `jq '.[]'`
    pattern; section description aligned with the wrapper documentation.
  - Removed six dead anchor links (`#...feature-NN`) left over from the
    v0.1.0 internal-marker stripping campaign.
  - Removed remaining internal feature markers (`F18-11`, `feature 26`,
    `Pre-feature-17`, `feature-26`) from `kb-mcp.toml.example`,
    `README.md`, `docs/ARCHITECTURE.md` (en+ja).
  - `examples/deployments/intranet-http/`: cache directory comment in
    `kb-mcp.toml` corrected (the systemd unit does not create or chown
    `/var/cache/fastembed`); README setup adds an explicit step to
    `install -d -o kbmcp -g kbmcp /var/cache/fastembed` before first run.
  - `kb-mcp index` description now lists the full default `exclude_dirs`
    set instead of just `.obsidian/`.
  - `kb-mcp validate --strict` documented as a no-op accepted for
    forward compatibility.
  - Fixed redundant "by default ... (the default behavior)" stutter in
    en+ja `index` description.

## [0.4.2] - 2026-04-27

### Fixed
- `kb-mcp eval` no longer reports `nDCG@k > 1.0`. The previous DCG loop
  iterated `top` and counted any hit that matched at least one expected
  entry, which over-counted gains when several chunks of the same doc
  (e.g. different headings under one path-only `expected`) appeared in
  top-k. The fix iterates `expected` and uses each entry's first matching
  rank exactly once, restoring the standard `[0, 1]` value range. Recall
  and MRR were not affected. Existing `.kb-mcp-eval-history.json` files
  still load, but historic `nDCG@k` values are not comparable across the
  fix boundary — re-run `kb-mcp eval` to establish a fresh baseline.

## [0.4.1] - 2026-04-26

### Internal
- Added `cargo-dist` 0.31 setup for cross-platform binary releases. From
  this release onwards, GitHub Releases include prebuilt archives for
  Linux x86_64 / aarch64, macOS aarch64 (Apple Silicon), and Windows
  x86_64, plus per-archive SHA-256 sums and a global `sha256.sum`.
  ONNX Runtime and SQLite are statically linked, so the archives ship a
  single binary with no extra DLLs. Intel Mac (`x86_64-apple-darwin`)
  is **not** shipped because `ort-sys` has no prebuilt for that target —
  build from source if needed.
- Linux binaries require **glibc 2.38+** (Ubuntu 24.04+ / Debian 13+ /
  RHEL 9.5+). The `ort-sys` prebuilt references `__isoc23_*` symbols
  introduced in that release.
- Windows binaries link against the dynamic UCRT (ucrtbase.dll /
  vcruntime140.dll, shipped with Windows 10+); cargo-dist's default
  `msvc-crt-static = true` is overridden because `libcmt` conflicts
  with `ort-sys`'s prebuilt.
- README en+ja gain an `Install` section describing the prebuilt
  archives; the existing `cargo build --release` instructions are
  demoted to a `Build from source` subsection.

## [0.4.0] - 2026-04-26

### Added
- `--config <PATH>` global CLI flag for selecting an arbitrary `kb-mcp.toml`.
  `~` is expanded on all platforms. Missing path errors fast (no fallback).
- Discovery now checks `./kb-mcp.toml` (CWD) first, then walks up to 19
  `.git` ancestor levels for a project-root `kb-mcp.toml`, before falling
  back to the legacy binary-side location.

### Changed
- `kb_mcp::config: loaded config source=...` is logged to stderr at startup
  so the active config file is observable. `tracing-subscriber` now uses
  the `env-filter` feature so `RUST_LOG` is honored (default = `info`).

### Compatibility
- Fully back-compat: the binary-side `kb-mcp.toml` (`<exe-dir>/kb-mcp.toml`)
  is still picked up when no higher-priority source is present.

### Internal
- `.githooks/pre-push` enforces `cargo fmt --check` before push so a
  forgotten `cargo fmt` cannot reach CI. Opt-in once via
  `git config core.hooksPath .githooks` (see CONTRIBUTING.md).

## [0.3.0] - 2026-04-26

### Added

- `search` tool now returns `match_spans` (byte offsets) for ASCII queries,
  helping clients quote source text accurately. See `docs/citations.md`.
- `search` tool gained new filters: `path_globs` (glob with `!`-prefixed
  excludes), `tags_any` (OR), `tags_all` (AND), `date_from` / `date_to`
  (lex comparison; date-missing chunks excluded strictly). See `docs/filters.md`.
- `search` response includes a `low_confidence` flag based on a rank-based
  ratio (`top1.score / mean(top-N.score) < min_confidence_ratio`). The threshold
  defaults to `1.5` and can be configured via `[search].min_confidence_ratio`
  in `kb-mcp.toml` or via `--min-confidence-ratio` / `min_confidence_ratio` per
  query.
- `tags` field is now included in each `SearchHit`.
- CLI `kb-mcp search` accepts `--path-glob`, `--tag-any`, `--tag-all`,
  `--date-from`, `--date-to`, `--min-confidence-ratio`.
- `[search]` section in `kb-mcp.toml`.

### Changed (BREAKING)

- The `search` MCP tool now returns a wrapper object
  `{ results, low_confidence, filter_applied }` instead of a raw array of hits.
  Clients that parse the response as `Vec<SearchHit>` directly must be updated.
  CLI `kb-mcp search --format json` follows the same wrapper format.
- Internal `db::search_hybrid` / `db::search_hybrid_candidates` /
  `db::search_vec_candidates` / `db::search_fts_candidates` /
  `db::search_similar` now take a `&SearchFilters<'_>` instead of separate
  `category` / `topic` / `min_quality` arguments. Library consumers (rare
  outside this repo) must migrate.

## [0.2.0] - 2026-04-24

### Added

- `kb-mcp eval` subcommand for retrieval quality evaluation (opt-in power-user feature).
  Runs a golden query set through `search_hybrid` and reports recall@k / MRR / nDCG@k.
  Shows diffs against the previous run. Details: `docs/eval.md` / `docs/eval.ja.md`.

### Internal

- CI (GitHub Actions) upgraded to `actions/checkout@v5` to clear Node.js 20 deprecation warnings

## [0.1.0] - 2026-04-20

First public release. An MCP server providing semantic hybrid search (sqlite-vec + FTS5 via Reciprocal Rank Fusion, with optional cross-encoder reranking) over a Markdown / plain-text knowledge base. Supports stdio and Streamable HTTP transports, includes a live-sync file watcher, and ships with optional frontmatter schema validation via the `kb-mcp validate` CLI.

### Added

- Dual-licensed under **MIT OR Apache-2.0** ([`LICENSE-MIT`](./LICENSE-MIT), [`LICENSE-APACHE`](./LICENSE-APACHE))
- `docs/ARCHITECTURE.md` / `docs/ARCHITECTURE.ja.md` describing source layout, data flow, embedding cache resolution, and key dependencies
- `CONTRIBUTING.md` / `CONTRIBUTING.ja.md` with build / test / code-style instructions
- Bilingual `README.md` (English primary) and `README.ja.md` (Japanese) with cross-links
- `.mcp.json.example` template alongside `.gitignore`'d user-local `.mcp.json`
- `exclude_dirs` config key for directory-level exclusion during indexing (defaults to `.obsidian`, `.git`, `node_modules`, `target`, `.vscode`, `.idea`)
- `Cargo.toml` metadata (description / license / repository / keywords / categories) for crates.io publishing

### Changed

- `exclude_headings` default neutralized from `["次の深堀り候補"]` to `[]` (opt-in by populating the key in `kb-mcp.toml`)
- `get_best_practice` MCP tool is now **opt-in**: requires `[best_practice].path_templates` in `kb-mcp.toml`; otherwise returns a `not configured` error
- `.obsidian/` skip is no longer hardcoded — it is now part of the configurable `exclude_dirs` default list

### Documentation

- Stripped internal feature tracking markers (`[feature N]`, `pre-feature-N`, `F12-N`, etc.) from all public docs and source comments
- Split `CLAUDE.md` into a slim public version and a private `CLAUDE.local.md` (gitignored) for harness-kit / project-history notes
- `README` feature-number references removed in favor of behavior-based descriptions

### Internal

- 207 unit / integration tests + 5 validate-CLI tests pass
- `cargo fmt` / `cargo clippy --all-targets` clean
- Personal dev artifacts moved to `.dev/` (excluded via `.git/info/exclude`)

[Unreleased]: https://github.com/alphabet-h/kb-mcp/compare/v0.4.3...HEAD
[0.4.3]: https://github.com/alphabet-h/kb-mcp/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/alphabet-h/kb-mcp/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/alphabet-h/kb-mcp/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/alphabet-h/kb-mcp/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/alphabet-h/kb-mcp/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/alphabet-h/kb-mcp/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/alphabet-h/kb-mcp/releases/tag/v0.1.0
