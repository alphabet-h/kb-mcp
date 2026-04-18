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

---

## 未実装: パフォーマンス・スケーラビリティ

### コンテンツハッシュベースの移動検出
- 現状: ファイルを移動すると旧パス削除 + 新パス追加（embedding 再生成）
- 改善: 同じハッシュを持つ新旧パスがあれば、embedding を再利用してパスだけ更新
- 効果: 大規模なディレクトリ再編成時の再インデックス時間を大幅短縮

---

## 未実装: 機能追加

### ライブ同期 (File watcher 駆動の増分再インデックス)
- 出典: [vault-mcp "Live Sync"](https://github.com/robbiemu/vault-mcp)
- 設計: Rust なら [`notify`](https://crates.io/crates/notify) クレートで SOURCE_DIR を watch → debounce → 該当ファイルのみ再チャンク
- 現状の `backfill_fts` / PostToolUse hook 連携の**補完**として有効（hook では拾えない手動編集に対応）

### 品質ベースチャンクフィルタ
- 出典: [vault-mcp "Quality Scoring"](https://github.com/robbiemu/vault-mcp)
- 設計: 各チャンクに品質スコア（長さ・情報密度・見出し深度・定型語句率など）を付与し、検索時に閾値フィルタ
- 効果: 「次の深堀り候補」等の**定型スタブを自動除外**できる（現在はハードコード除外に頼っている）

### Connection Graph (多段類似ノート探索)
- 出典: [smart-connections-mcp "get_connection_graph"](https://github.com/msdanyg/smart-connections-mcp)
- 設計: 既存の vec0 MATCH を複数回発行するだけで実装可能（コスト低）
- 効果: `[[deep-dive]]` 階層の横断探索、トピック俯瞰用ツールとして有用

### Frontmatter スキーマ検証
- 出典: [basic-memory v0.19.0 "Schema System"](https://github.com/basicmachines-co/basic-memory)
- 設計: frontmatter の構造を推論し、検証・差分表示
- 注意: basic-memory は **AGPL v3** なので**直接コピペ不可**。設計思想のみ借用
- 効果: ai_organization の frontmatter 規約（`title` / `date` / `tags` / etc.）の自動検証に使える

### HTTP/SSE トランスポート
- 現状: stdio のみ（1 クライアント限定）
- 改善: HTTP/SSE トランスポートで複数クライアント同時接続
- rmcp は HTTP トランスポートをサポート済み

### PostToolUse hook 連携
- 外部プロジェクトのスキル実行後に自動で `rebuild_index` を呼ぶ Claude Code hook
- knowledge-base 更新 → インデックス再構築を自動化

### Markdown 以外のファイル対応
- `.txt` / `.rst` / `.adoc` 等のテキストファイルも対象に
- パーサーをプラグイン化して拡張可能にする

---

## 未実装: 汎用化

### チャンキング除外パターンの設定ファイル化
- 現状: `## 次の深堀り候補` がハードコードで除外
- 改善: `.kb-mcp.toml` 等の設定ファイルで除外見出しパターンを指定可能にする
- 例: `exclude_headings = ["次の深堀り候補", "References", "See Also"]`

### `get_best_practice` の汎用化
- 現状: `best-practices/{target}/PERFECT.md` 固定パス構造を前提
- 改善: 任意のディレクトリ構造に対応。設定ファイルで「特定パスの特定ファイルをセクション分割で提供する」マッピングを定義

---

## 未実装: コード品質

### transmute の安全化（レビュー指摘 #4）
- `sqlite3_vec_init` の `transmute` を型安全な方法に変更
- sqlite-vec クレートが直接互換な関数ポインタを公開しているか確認

### CRLF 対応の強化（レビュー指摘 #9）
- Windows 生成の `.md` ファイルで frontmatter 内の `\r` がリークする可能性
- YAML パース前に `yaml_str` を `.replace("\r\n", "\n")` で正規化

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
| Connection Graph | 低 | 中 | ✅ | 中 |
| 品質ベースチャンクフィルタ | 低 | 中 | ✅ | 中 |
| コンテンツハッシュ移動検出 | 低 | 低 (運用時のみ) | ✅ | 低 |
| チャンキング除外設定ファイル化 | 低 | 低 | ✅ | 低 |
| ライブ同期 (`notify` crate) | 中 | 中 | ✅ | 中 |
| Markdown 以外のファイル対応 | 中 | 中 | ✅ | 低 |
| HTTP/SSE トランスポート | 中 | 中 (用途依存) | ✅ | 中 |
| PostToolUse hook 連携 | 低 | 中 | ✅ | 中 |
| `get_best_practice` 汎用化 | 中 | 中 | ✅ | 中 |
| フロントマター スキーマ検証 | 高 | 低〜中 | ✅ | 低 |
| `transmute` 安全化 (保守) | 低 | N/A | ✅ | 低 |
| CRLF 正規化強化 (保守) | 低 | N/A | ✅ | 低 |
