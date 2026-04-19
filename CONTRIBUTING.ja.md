# kb-mcp への貢献

コントリビュート検討ありがとうございます。必要最低限の開発情報をここにまとめます。

> **English version**: [CONTRIBUTING.md](./CONTRIBUTING.md)

## 前提

- Rust stable (edition 2024)
- Git
- ONNX モデルキャッシュ用に約 3 GB の空き容量 (`--ignored` テスト実行時のみ)

## ビルドとテスト

```bash
cargo build --release      # release バイナリ: target/release/kb-mcp(.exe)
cargo check --all-targets  # 型検査のみ (高速)
cargo test                 # ユニット + integration テスト (モデル DL 不要)
cargo test -- --ignored    # 実モデル DL 込みの embedding / reranker テスト
```

`cargo test -- --ignored` は初回に ONNX モデルを DL する (BGE-small ~130 MB、BGE-M3 ~2.3 GB、BGE-reranker-v2-m3 ~2.3 GB)。OS 標準キャッシュディレクトリに保存される。ネットワーク都合で DL が失敗する場合は、README の「HuggingFace の TLS 失敗への対処」節を参照。

## コードスタイル

- コミット前に `cargo fmt --all` (CI で強制)
- `cargo clippy --all-targets` が警告を出さないこと (CI で強制)
- 日本語 KB 固有のロジック (CJK トークナイズ、日付書式等) に関する日本語コメントは歓迎、それ以外は英語推奨

## リポジトリ構成

- `src/parser/` — `Parser` trait + `Registry` (形式ごとに impl 1 個)
- `src/indexer.rs` — `walkdir` → パース → 埋め込み → 格納のパイプライン
- `src/db.rs` — SQLite + sqlite-vec + FTS5、`search_hybrid` (RRF、k=60)
- `src/embedder.rs` — `fastembed-rs` ラッパ (embedding + cross-encoder reranker)
- `src/server.rs` — `rmcp::ServerHandler`、6 つの MCP ツール
- `src/transport/` — stdio と Streamable HTTP
- `src/watcher.rs` — `notify-debouncer-full` ベースの増分再インデックス
- `src/schema.rs` — frontmatter スキーマ検証
- `src/quality.rs` / `src/graph.rs` — 品質フィルタ + BFS connection graph

詳細は [docs/ARCHITECTURE.ja.md](./docs/ARCHITECTURE.ja.md) を参照。

## テストの 2 層構造

- **軽量テスト**: 既定の `cargo test`。ネットワーク・モデル DL 不要、秒オーダーで完了
- **ignored テスト** (`#[ignore]`): ネットワーク + ディスクが必要 (モデル DL)。`cargo test -- --ignored` で opt-in、CI では別ジョブに分けるのが望ましい

embedder / reranker が必要なテストを追加するときは `#[ignore]` を付け、何を検証するかコメントで記述する。

## 変更の提出

1. リポジトリを fork し、`main` からブランチを切る
2. 新しい挙動にはテストを追加 (ユニットは inline、integration は `tests/` 配下)
3. `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test` を pass
4. 問題と変更内容を明示した PR を開く (関連 issue があればリンク)

## バグ報告

以下を含めて issue を開いてください:
- 最小再現手順 (コマンド、必要に応じて小さな KB サンプル)
- `kb-mcp --version`
- OS と Rust toolchain バージョン (`rustc --version`)
- 期待する挙動 vs 実際の挙動

## ライセンス

貢献によって、あなたのコントリビュートは本プロジェクトと同じ **MIT OR Apache-2.0** デュアルライセンスで扱われることに同意したものとみなします。[LICENSE-MIT](./LICENSE-MIT) / [LICENSE-APACHE](./LICENSE-APACHE) を参照。
