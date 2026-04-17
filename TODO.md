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
