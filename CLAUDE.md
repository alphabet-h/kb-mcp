# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## プロジェクト概要

Markdown ナレッジベースに対するセマンティック検索を提供する MCP (Model Context Protocol) サーバ。YAML frontmatter 付き `.md` ファイルを見出し単位でチャンク化し、BGE-small-en-v1.5 で embedding を生成、sqlite-vec のベクトル類似検索と組み合わせて stdio transport で Claude Code / Cursor に接続する。

元は `ai_organization` リポジトリ内のサブディレクトリとして開発されていたが、独立プロジェクト化した。

## ビルド・テスト

```bash
cargo build --release                    # release バイナリ: target/release/kb-mcp(.exe)
cargo check                              # 型検査のみ（高速）
cargo test                               # 軽量テスト（embedding DL 不要なもののみ）
cargo test -- --ignored                  # 実モデル DL を伴う embedding テスト（~128 MB キャッシュ必要）
```

Windows では `kb-mcp.exe` になる。ONNX runtime (`ort-sys`) は静的リンクされるため、**追加の DLL は不要**。SQLite も `rusqlite` の `bundled` feature で同梱。

## 実行

```bash
kb-mcp index --kb-path /path/to/knowledge-base           # 初回 index（BGE-small-en-v1.5 で 384 次元）
kb-mcp index --kb-path /path/to/knowledge-base --force   # 同一モデルで全件再生成
kb-mcp index --kb-path ... --model bge-m3 --force        # BGE-M3 (1024 次元、多言語) へ切替
kb-mcp status --kb-path /path/to/knowledge-base          # docs/chunks 数の確認
kb-mcp serve --kb-path /path/to/knowledge-base           # MCP サーバ起動（stdio、デフォルトモデル）
kb-mcp serve --kb-path ... --model bge-m3                # BGE-M3 でサーバ起動（index 済が前提）
```

`--model` は `bge-small-en-v1.5` (既定、384 次元、英語特化、~130 MB) / `bge-m3` (1024 次元、多言語、~2.3 GB) の 2 択。`index_meta` テーブルに記録された model/dim と runtime が不一致なら起動時に拒否するため、モデル切替時は `--force` で再構築が必要。

SQLite DB は `--kb-path` の **親ディレクトリ** に `.kb-mcp.db` として作られる（例: `--kb-path ./knowledge-base` ならプロジェクトルートに `.kb-mcp.db`）。

## MCP クライアントとの接続

呼び出し側プロジェクトのルートに `.mcp.json` を置く:

```json
{
  "mcpServers": {
    "ai-knowledge": {
      "type": "stdio",
      "command": "/path/to/kb-mcp.exe",
      "args": ["serve", "--kb-path", "/path/to/knowledge-base"]
    }
  }
}
```

公開ツール: `search`, `list_topics`, `get_document`, `get_best_practice`, `rebuild_index`。詳細は README.md 参照。

## アーキテクチャ（ソース別の責務）

| ファイル | 役割 |
|---|---|
| `src/main.rs` | clap CLI エントリ。`index` / `status` / `serve` サブコマンドの分岐 |
| `src/server.rs` | rmcp `ServerHandler` 実装。5 つの MCP ツールをディスパッチ。`search` は `db.search_hybrid` (vec + FTS5 + RRF) 経由 |
| `src/indexer.rs` | walkdir で `.md` を走査 → markdown.rs でパース → embedder.rs で embedding → db.rs で格納。SHA-256 で差分検出。冒頭で `backfill_fts()` を呼び pre-feature-9 DB を自動移行 |
| `src/markdown.rs` | pulldown-cmark で Markdown をパース、frontmatter 抽出、見出し単位でチャンク分割 |
| `src/embedder.rs` | fastembed-rs の薄いラッパ。`ModelChoice` enum で BGE-small-en-v1.5 (384 次元) / BGE-M3 (1024 次元) を切替 |
| `src/db.rs` | rusqlite + sqlite-vec + FTS5 (trigram)。`chunks` / `vec_chunks` / `fts_chunks` の schema と CRUD、`search_hybrid` (RRF k=60) を提供 |

### データフロー

```
.md ファイル群
     ↓ walkdir
indexer.rs: 差分検出 (SHA-256 vs .kb-mcp.db の chunks.hash)
     ↓ 変更ありのものだけ
markdown.rs: frontmatter 抽出 + 見出しでチャンク化
     ↓
embedder.rs: fastembed でベクトル生成 (BGE-small-en-v1.5 で 384 次元、BGE-M3 で 1024 次元)
     ↓
db.rs: chunks (メタ) + vec_chunks (embedding) + fts_chunks (FTS5 trigram) に UPSERT
```

検索時はハイブリッド:
- query → embedder → vec_chunks MATCH (top-N)
- query → sanitize → fts_chunks MATCH + bm25 (top-N)
- Rust 側で RRF (k=60) マージ → top-limit を返す

## Embedding モデルのキャッシュ

`embedder.rs::resolve_cache_dir()` が以下の順でキャッシュディレクトリを決める:

1. `FASTEMBED_CACHE_DIR` 環境変数（最優先）
2. OS 標準キャッシュディレクトリ + `fastembed`
   - Linux: `~/.cache/fastembed`
   - macOS: `~/Library/Caches/fastembed`
   - Windows: `%LOCALAPPDATA%\fastembed`
3. `.fastembed_cache/`（CWD 直下、最終フォールバック）

初回実行時に ~128 MB の HuggingFace hub cache が作られる（model.onnx 127 MB + tokenizer.json 711 KB 等）。2 回目以降は再 DL されない。

## 主要な依存

- `rmcp` 1.x — MCP サーバフレームワーク (stdio transport)
- `fastembed` 5.x — ONNX ベースの embedding (BGE-small-en-v1.5)
- `rusqlite` 0.39 (bundled) — SQLite 静的リンク
- `sqlite-vec` 0.1 — ベクトル類似検索拡張
- `pulldown-cmark` 0.13 — Markdown パーサ
- `dirs` 6 — OS 標準キャッシュディレクトリ解決

## 改善候補

次期バージョンアップで検討中のアイテムは [TODO.md](./TODO.md) を参照。主なもの:

- 多言語 embedding モデル (BGE-M3) 対応
- ハイブリッド検索 (vec + FTS5)
- Reranking
- HTTP/SSE トランスポート
- CRLF 正規化の強化 / `transmute` の型安全化

## 運用の細則

- **Cargo.lock はコミットする**（バイナリクレート）
- **`.kb-mcp.db` はクライアントプロジェクト側の責務**。本リポジトリでは生成しない
- **テストは 2 層構造**: 通常 `cargo test` では `#[ignore]` の embedding 実行テストはスキップされる。CI 等で検証したければ `-- --ignored` を付ける

## 開発ワークフロー（harness-kit）

- `features.json` でタスク管理。実装完了した機能は status を `"passing"` に変更
- テストの削除・編集は禁止（機能が壊れる原因になる）
- 各機能の実装後は `evaluator` エージェントで品質チェック
- 進捗は `claude-progress.txt` に逐次記録（append-only）
- 新セッション開始時: progress 確認 → features.json 確認 → 次タスク特定
