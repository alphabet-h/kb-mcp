# Changelog

All notable changes to kb-mcp are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

_(nothing yet)_

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

[Unreleased]: https://github.com/alphabet-h/kb-mcp/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/alphabet-h/kb-mcp/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/alphabet-h/kb-mcp/releases/tag/v0.1.0
