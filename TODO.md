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

### ライブ同期 (File watcher 駆動の増分再インデックス)
- 出典: [vault-mcp "Live Sync"](https://github.com/robbiemu/vault-mcp)
- 設計: Rust なら [`notify`](https://crates.io/crates/notify) クレートで SOURCE_DIR を watch → debounce → 該当ファイルのみ再チャンク
- 現状の `backfill_fts` / PostToolUse hook 連携の**補完**として有効（hook では拾えない手動編集に対応）

### Frontmatter スキーマ検証
- 出典: [basic-memory v0.19.0 "Schema System"](https://github.com/basicmachines-co/basic-memory)
- 設計: frontmatter の構造を推論し、検証・差分表示
- 注意: basic-memory は **AGPL v3** なので**直接コピペ不可**。設計思想のみ借用
- 効果: ai_organization の frontmatter 規約（`title` / `date` / `tags` / etc.）の自動検証に使える

### HTTP/SSE トランスポート
- 現状: stdio のみ（1 クライアント限定）
- 改善: HTTP/SSE トランスポートで複数クライアント同時接続
- rmcp は HTTP トランスポートをサポート済み

### Markdown 以外のファイル対応
- `.txt` / `.rst` / `.adoc` 等のテキストファイルも対象に
- パーサーをプラグイン化して拡張可能にする

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
| ライブ同期 (`notify` crate) | 中 | 中 | ✅ | 中 |
| Markdown 以外のファイル対応 | 中 | 中 | ✅ | 低 |
| HTTP/SSE トランスポート | 中 | 中 (用途依存) | ✅ | 中 |
| フロントマター スキーマ検証 | 高 | 低〜中 | ✅ | 低 |
