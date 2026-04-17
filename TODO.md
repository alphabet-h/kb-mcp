# kb-mcp — 次期バージョンアップ候補

このファイルは将来の改善候補をまとめたもの。優先度順ではなく、思いついた順に追記する。

---

## 汎用化

### 多言語 Embedding モデル対応
- 現状: BGE-small-en-v1.5（英語特化、384 次元）
- 改善: `--model bge-m3` 等のフラグで多言語モデル（BAAI/bge-m3）に切り替え可能にする
- 理由: 日本語のみの文書では検索精度が落ちる可能性がある。fastembed-rs は BGE-M3 を含む 30+ モデルに対応済み
- 影響: embedding 次元が変わるため、DB スキーマの `float[384]` が動的になる必要がある。`index_meta` テーブルのモデル名と次元数で管理

### チャンキング除外パターンの設定ファイル化
- 現状: `## 次の深堀り候補` がハードコードで除外
- 改善: `.kb-mcp.toml` 等の設定ファイルで除外見出しパターンを指定可能にする
- 例: `exclude_headings = ["次の深堀り候補", "References", "See Also"]`

### `get_best_practice` の汎用化
- 現状: `best-practices/{target}/PERFECT.md` 固定パス構造を前提
- 改善: 任意のディレクトリ構造に対応。設定ファイルで「特定パスの特定ファイルをセクション分割で提供する」マッピングを定義

---

## パフォーマンス・スケーラビリティ

### コンテンツハッシュベースの移動検出
- 現状: ファイルを移動すると旧パス削除 + 新パス追加（embedding 再生成）
- 改善: 同じハッシュを持つ新旧パスがあれば、embedding を再利用してパスだけ更新
- 効果: 大規模なディレクトリ再編成時の再インデックス時間を大幅短縮

### ハイブリッド検索（ベクトル + キーワード）
- SQLite FTS5 によるキーワード検索を追加し、sqlite-vec のベクトル検索と RRF で統合
- 完全一致が必要なケース（エラーコード、コマンド名等）で精度向上

### Reranking
- fastembed-rs の reranker モデル（BGE-reranker 等）で search 結果を再ランク
- 精度の大幅改善が見込めるが、レイテンシとのトレードオフ

---

## 機能追加

### HTTP/SSE トランスポート
- 現状: stdio のみ（1 クライアント限定）
- 改善: HTTP/SSE トランスポートで複数クライアント同時接続
- rmcp は HTTP トランスポートをサポート済み

### PostToolUse hook 連携
- このプロジェクトのスキル（ai-news-collector / deep-dive / pbd-keeper）実行後に自動で `rebuild_index` を呼ぶ Claude Code hook
- knowledge-base 更新 → インデックス再構築を自動化

### Markdown 以外のファイル対応
- `.txt` / `.rst` / `.adoc` 等のテキストファイルも対象に
- パーサーをプラグイン化して拡張可能にする

---

## コード品質

### transmute の安全化（レビュー指摘 #4）
- `sqlite3_vec_init` の `transmute` を型安全な方法に変更
- sqlite-vec クレートが直接互換な関数ポインタを公開しているか確認

### CRLF 対応の強化（レビュー指摘 #9）
- Windows 生成の `.md` ファイルで frontmatter 内の `\r` がリークする可能性
- YAML パース前に `yaml_str` を `.replace("\r\n", "\n")` で正規化

---

## 既存実装からの借用・参考（2026-04-18 追加）

2026 時点で Markdown + MCP + セマンティック検索の領域には実装が乱立している。
ai_organization 側に網羅的な比較ノート `knowledge-base/deep-dive/mcp-rag-servers/overview.md` を整理済み（同プロジェクトを運用する環境ならローカル参照可）。
以下は **kb-mcp に借用 or 参考にすべき設計要素** を既存実装別に抽出したもの。単体で読めるよう出典と概略を全てインラインで記載する。

### 既出項目に対する参照先（上記セクションの具体化）

- **ハイブリッド検索（FTS5 + ベクトル）**: [markdown-vault-mcp](https://github.com/pvliesdonk/markdown-vault-mcp) が SQLite FTS5 + BM25 + porter stemming をベースに Reciprocal Rank Fusion で融合する実装を公開。[basic-memory v0.19.0](https://github.com/basicmachines-co/basic-memory) も同方針で追加。Python 実装だが SQLite スキーマ設計は Rust に翻案しやすい
- **Reranking**: [vault-mcp](https://github.com/robbiemu/vault-mcp) の "agentic / static" モード切替が参考になる。agentic=LLM 再ランク、static=決定論的。fastembed-rs の `TextRerank` で BGE-reranker-base を使うなら追加モデル DL ~100 MB
- **多言語 embedding**: [MCP-Markdown-RAG のロードマップ](https://github.com/Zackriya-Solutions/MCP-Markdown-RAG) に "BGEM3-large 対応" と明記。[markdown-vault-mcp](https://github.com/pvliesdonk/markdown-vault-mcp) は FastEmbed / Ollama / OpenAI を**実行時切替**で実装済みなので設計を参考にできる

### 新規追加候補

#### ライブ同期（File watcher 駆動の増分再インデックス）
- 出典: [vault-mcp "Live Sync"](https://github.com/robbiemu/vault-mcp) — "Automatically re-indexes files when they change on disk"
- 設計: Rust なら [`notify`](https://crates.io/crates/notify) クレートで SOURCE_DIR を watch → debounce → 該当ファイルのみ再チャンク
- 現状の PostToolUse hook 連携の**補完**として有効（hook では拾えない手動編集に対応）

#### 品質ベースチャンクフィルタ
- 出典: [vault-mcp "Quality Scoring"](https://github.com/robbiemu/vault-mcp) — "Filters document chunks based on content quality"
- 設計: 各チャンクに品質スコア（長さ・情報密度・見出し深度・定型語句率など）を付与し、検索時に閾値フィルタ
- 効果: 「次の深堀り候補」等の**定型スタブを自動除外**できる（現在はハードコード除外に頼っている）

#### Connection Graph（多段類似ノート探索）
- 出典: [smart-connections-mcp "get_connection_graph"](https://github.com/msdanyg/smart-connections-mcp) — 深さ・閾値指定の BFS で類似ノード連鎖を返す
- 設計: 既存の vec0 MATCH を複数回発行するだけで実装可能（コスト低）
- 効果: `[[deep-dive]]` 階層の横断探索、トピック俯瞰用ツールとして有用

#### Frontmatter スキーマ検証
- 出典: [basic-memory v0.19.0 "Schema System"](https://github.com/basicmachines-co/basic-memory) — `schema_infer` / `schema_validate` / `schema_diff`
- 設計: frontmatter の構造を推論し、検証・差分表示
- 注意: basic-memory は **AGPL v3** なので**直接コピペ不可**。設計思想のみ借用
- 効果: ai_organization の frontmatter 規約（`title` / `date` / `tags` / etc.）の自動検証に使える

### 借用しない候補（記録のため）

#### 書き込み系ツール（create/edit/delete/rename）
- 出典: [markdown-vault-mcp "Write operations"](https://github.com/pvliesdonk/markdown-vault-mcp), [basic-memory](https://github.com/basicmachines-co/basic-memory), [obsidian-mcp-tools](https://github.com/jacksteamdev/obsidian-mcp-tools)
- **不採用理由**: ai_organization は「スキル経由で書く」ポリシー。MCP サーバが書き込みを担うとスキルとの責務が重複する
- **例外**: 単純な「フラット Markdown 管理」を外部利用者が望む場合のみ別 feature flag で提供検討

### 優先度マトリクス

| 項目 | 実装コスト | ユーザ体感向上 | 優先度 |
|---|---|---|---|
| FTS5 ハイブリッド検索 | 中 | 高 | **高** |
| Reranking | 低 | 中〜高 | 高 |
| 多言語 embedding (BGE-M3) | 中 | 高（日本語で顕著） | 高 |
| 品質ベースチャンクフィルタ | 低 | 中 | 中 |
| ライブ同期 | 中 | 中 | 中 |
| Connection Graph | 低 | 中 | 中 |
| スキーマ検証 | 高 | 低〜中 | 低 |
| 書き込み系ツール | 中 | N/A | **不採用** |
