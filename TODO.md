# kb-mcp — 次期バージョンアップ候補

このファイルは将来の改善候補をまとめたもの。優先度順ではなく、思いついた順に追記する。実装済みの記録と未実装の候補を分けて管理する。最新の実装履歴は `claude-progress.txt` と `features.json` を参照。

---

## ✅ 実装済み (2026-04-18)

| 項目 | commit | 備考 |
|---|---|---|
| `index_meta` によるモデル/次元整合性チェック (feature 8) | `ccc9c8d` | 不整合検出時は `--force` 誘導エラー |
| 多言語 Embedding (BGE-M3) 対応 + `--model` CLI (feature 7) | `23478d7` | 1024 次元、`vec_chunks` を dim 可変に |
| FTS5 + ベクトル検索の RRF ハイブリッド (feature 9) | `10edf23` | trigram tokenizer、日本語対応、`contentless_delete=1` |
| evaluator 指摘 #1-4 反映 (SQL 重複解消、filter over-fetch 等) | `8af254b` | |
| Cross-encoder reranker + FTS bm25 heading 重み (feature 10, evaluator #5) | `9f4b092` | BGE-reranker-v2-m3 等、`--reranker` CLI |
| モデルごとの batch size 明示 (evaluator #6) | `7604e37` | BGE-M3 で 32、BGE-small で 256 |
| バイナリ同居 `kb-mcp.toml` で CLI 既定値 (feature 23) | `23bb24d` | deny_unknown_fields、相対パスは config dir 基準 |
| CLI `search` サブコマンド (feature 24) | — | skill bin / シェル用途の one-shot 検索 |
| YAML frontmatter CRLF 正規化 (feature 22) | `a3a321f` | yaml_raw の \\r を LF に置換してから parse |
| `sqlite3_vec_init` の `transmute` 除去 (feature 21) | `ba59ae2` | 自前 extern + `#[link(kind="static")]` で ABI 整合 |
| チャンキング除外見出しの設定ファイル化 (feature 14) | `190b358` | `exclude_headings` キー、3 状態セマンティクス (None/[]/custom) |
| Connection Graph ツール (feature 15) | `29fde54`, `709c48c` | `get_connection_graph` MCP + `graph` CLI、BFS + on-the-fly KNN |
| 品質ベースチャンクフィルタ (feature 13) | `a43bbb0` | 長さ/定型語/構造の 3 シグナルで 0-1 スコア、`[quality_filter]` + `--min-quality` / `--include-low-quality`、graph にも透過適用 |
| PostToolUse hook 連携 (feature 19) | - | `examples/hooks/` に `.claude/settings.json` スニペット + path-filter 付き shell script を同梱、README で案内 |
| `get_best_practice` の汎用化 (feature 16) | - | `[best_practice].path_templates` で `{target}` プレースホルダ列を定義し、先頭から順にファイル解決。既定は legacy の `best-practices/{target}/PERFECT.md` |
| content_hash ベースの移動検出 (feature 11) | - | index 冒頭で disk/DB の path-hash を突き合わせ、同 hash ペアを rename_document で path だけ UPDATE (embedding 再利用)。IndexResult.renamed にカウント |

---

## 未実装: 機能追加

### ~~ライブ同期 (File watcher 駆動の増分再インデックス)~~ ✅ 実装済 (feature 12)
- 実装コミット: `docs/plans/feature-12-live-sync.md` 参照
- notify + notify-debouncer-full、`src/watcher.rs`、既定 on、`--no-watch` / `--debounce-ms`
- 将来拡張: F12-8 frontmatter-only skip、F12-9 self-heal (未実装)

### Frontmatter スキーマ検証
- 出典: [basic-memory v0.19.0 "Schema System"](https://github.com/basicmachines-co/basic-memory)
- 設計: frontmatter の構造を推論し、検証・差分表示
- 注意: basic-memory は **AGPL v3** なので**直接コピペ不可**。設計思想のみ借用
- 効果: ai_organization の frontmatter 規約（`title` / `date` / `tags` / etc.）の自動検証に使える

### ~~HTTP/SSE トランスポート~~ ✅ 実装済 (feature 18)
- 実装コミット: `docs/plans/feature-18-http-transport.md` 参照
- rmcp 1.x `transport-streamable-http-server` + axum 0.8
- `/mcp` mount + `/healthz` + 既定 bind=127.0.0.1:3100
- 拡張 (F18-11〜F18-16) は別 feature として後回し: Bearer token / CORS / TLS / メトリクス / mount path 設定 / rate limit

### Markdown 以外のファイル対応
- 仕様書: [docs/plans/feature-20-non-markdown.md](./docs/plans/feature-20-non-markdown.md)
- **MVP スコープ (決定済み 2026-04-19)**: `.md` + `.txt` の 2 種
- Parser trait 抽象 (`src/parser/`) を入れて将来の拡張子追加に備える
- feature 12 (watcher) は `Registry::extensions()` を流用

### 将来候補: 追加フォーマット対応 (feature 20 の拡張)
MVP 完了後、需要と実装コストを見ながら以下を追加検討:

| 形式 | 優先度 | 備考 |
|---|---|---|
| `.pdf` | 中〜高 | 企業/学術で需要大。`pdf-extract` crate 依存、text layer 抽出のみ (OCR 非対応) |
| `.docx` | 中 | 企業文書。`docx-rs` / `dotext` 等 |
| `.rst` | 中 | Python エコシステム限定 (Sphinx / Read the Docs)。自前パーサで 200 LOC |
| `.adoc` | 低 | 技術ライター community 限定 (Fedora / Kubernetes docs)。自前パーサで 180 LOC |
| `.org` / `.typ` / `.wiki` / `.tex` | 低 | trait で受けられるが需要見極め中 |

恒久非スコープ: `pandoc` / `asciidoctor` 外部プロセス呼び出し (階層 C 違反)

---

## 借用判断の基本原則

kb-mcp は「**ドキュメントがあるディレクトリに 1 バイナリを置くだけで簡易 RAG 化**」できる点が本質的な価値提案 (階層 C)。ai_organization の `knowledge-base/deep-dive/mcp-rag-servers/overview.md` の比較分析を参照。借用判断は以下の基準で行う:

- ✅ **採用可**: SQLite 標準機能 / fastembed-rs 既存 API / Rust クレートで完結する → 単体バイナリの性質を維持できる
- ❌ **不採用**: 外部 LLM 呼び出し（agentic rerank 等）/ クラウドサービス依存 → ランタイム依存を増やし、階層 B / A へ転落する

機能の有無ではなく「**単体バイナリ・ランタイム不要の value prop を壊さないか**」が採否の一次基準である。

### 借用しない候補 (記録のため)

| 項目 | 出典 | 不採用理由 |
|---|---|---|
| 書き込み系ツール (create/edit/delete/rename) | markdown-vault-mcp, basic-memory, obsidian-mcp-tools | ai_organization は「スキル経由で書く」ポリシー。MCP が書込みを担うとスキルと責務重複 |
| Agentic rerank (LLM 呼び出し) | vault-mcp agentic モード | ランタイム依存を増やす。現状の fastembed TextRerank で階層 C を維持 |
| クラウド同期 | basic-memory | 階層 A へ転落 |

---

## 優先度マトリクス (未実装分)

「単体バイナリ維持」列は**ランタイム依存を増やさず実装できるか**の判断。✅ = OK、❌ = value prop を壊す。

| 項目 | 実装コスト | ユーザ体感向上 | 単体バイナリ維持 | 優先度 |
|---|---|---|:-:|---|
| ~~Markdown 以外のファイル対応 (.md + .txt MVP)~~ ✅ 実装済 (feature 20) | 低〜中 | 中 | ✅ | 完了 |
| ~~ライブ同期 (`notify` crate)~~ ✅ 実装済 (feature 12) | 中 | 中 | ✅ | 完了 |
| ~~HTTP/SSE トランスポート~~ ✅ 実装済 (feature 18) | 中 | 中 (用途依存) | ✅ | 完了 |
| 追加フォーマット対応 (`.pdf` / `.docx` 等) | 中〜高 | 大 (特に `.pdf`) | ✅ | 将来検討 |
| HTTP 認証 / TLS / CORS / メトリクス (F18-11〜F18-16) | 中〜高 | 公開用途で必須 | ✅ | 必要になったら |
| watcher frontmatter-only skip (F12-8) | 低 | 低 (BGE-M3 の再 embed 回避) | ✅ | 低 |
| watcher self-heal (F12-9) | 低 | 低 | ✅ | 低 |
| フロントマター スキーマ検証 | 高 | 低〜中 | ✅ | 低 |
