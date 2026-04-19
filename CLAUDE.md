# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## プロジェクト概要

Markdown / テキストナレッジベースに対するセマンティック検索を提供する MCP (Model Context Protocol) サーバ。YAML frontmatter 付き `.md` ファイルを見出し単位でチャンク化し、選択可能な埋め込みモデル (BGE-small-en-v1.5 / BGE-M3) でベクトルを生成、sqlite-vec のベクトル検索と FTS5 全文検索を RRF で融合し、任意で cross-encoder reranker を適用する。stdio transport で Claude Code / Cursor に接続する。feature 20 で Parser trait 抽象を導入し `.txt` も opt-in 対応 (将来 `.pdf` / `.docx` / `.rst` / `.adoc` を拡張可能)。

元は `ai_organization` リポジトリ内のサブディレクトリとして開発されていたが、独立プロジェクト化した。

## ビルド・テスト

```bash
cargo build --release                    # release バイナリ: target/release/kb-mcp(.exe)
cargo check                              # 型検査のみ（高速）
cargo test                               # 軽量テスト（embedding DL 不要なもののみ）
cargo test -- --ignored                  # 実モデル DL を伴う embedding/reranker テスト
                                         # (BGE-small ~130 MB / BGE-M3 ~2.3 GB / BGE-reranker-v2-m3 ~2.3 GB)
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
kb-mcp serve --kb-path ... --model bge-m3 --reranker bge-v2-m3  # + cross-encoder reranking
kb-mcp search "クエリ" --kb-path ... --model bge-m3 --limit 3 --format text  # one-shot CLI 検索 (skill bin 用途)
kb-mcp graph --start deep-dive/mcp/overview.md --kb-path ... --depth 2 --fan-out 5 --dedup-by-path  # Connection Graph BFS
```

`--model` は `bge-small-en-v1.5` (既定、384 次元、英語特化、~130 MB) / `bge-m3` (1024 次元、多言語、~2.3 GB) の 2 択。`index_meta` テーブルに記録された model/dim と runtime が不一致なら起動時に拒否するため、モデル切替時は `--force` で再構築が必要。

`--reranker` (既定 `none`) は RRF 上位候補を cross-encoder で再ランクする。候補数 max(limit*5, 50)、BGE-reranker-v2-m3 (日本語推奨、~2.3 GB) / jina-v2-ml (~1.2 GB) / bge-base (英中、~280 MB) を選択可能。レイテンシは CPU で +300〜700 ms 程度。MCP ツール `search` の `rerank: bool` で per-query override 可能。reranker は index 非依存のため、切替で再インデックス不要。

SQLite DB は `--kb-path` の **親ディレクトリ** に `.kb-mcp.db` として作られる（例: `--kb-path ./knowledge-base` ならプロジェクトルートに `.kb-mcp.db`）。

### 設定ファイル (`kb-mcp.toml`)

バイナリと**同じディレクトリ**に `kb-mcp.toml` を置くと、CLI オプションの既定値として使われる。優先順位は `CLI 引数 > 設定ファイル > ビルトイン既定値`。

```toml
# target/release/kb-mcp.toml 等
kb_path = "/path/to/knowledge-base"
model = "bge-m3"
reranker = "bge-v2-m3"
rerank_by_default = true
fastembed_cache_dir = "/home/you/.cache/huggingface/hub"
# 省略時は ["次の深堀り候補"]。[] で除外無効化。
exclude_headings = ["次の深堀り候補", "参考リンク"]

# feature 13: 既定で有効 (threshold 0.3)。enabled=false で pre-feature-13 の
# 従来挙動 (全件返却) に戻る。CLI は --include-low-quality / --min-quality で
# per-query override、MCP search ツールも同名の optional パラメータで override 可能。
[quality_filter]
enabled = true
threshold = 0.3

# feature 20: index 対象の拡張子レジストリ。セクション省略で pre-feature-20 完全
# 後方互換 (md のみ)。空配列はエラー。現在サポート: "md" / "txt"。
[parsers]
enabled = ["md", "txt"]

# feature 12: ファイルウォッチャー (kb-mcp serve のみ)。既定 on。
# --no-watch / --debounce-ms で per-run override 可能。
[watch]
enabled = true
debounce_ms = 500

# feature 18: MCP トランスポート。省略時 stdio (後方互換)。
# "http" で Streamable HTTP、複数クライアント同時接続可。
# CLI --transport http --port 3100 / --bind 0.0.0.0:3100 で override。
[transport]
kind = "http"

[transport.http]
bind = "127.0.0.1:3100"
```

- 全フィールド optional。テンプレートは `kb-mcp.toml.example` (リポジトリ同梱、`.gitignore` で `kb-mcp.toml` 本体は除外)
- 未知キーはパースエラー (`deny_unknown_fields`)
- `FASTEMBED_CACHE_DIR` は env が設定済みなら env 優先、未設定時のみ config 値を env に注入してから embedder を初期化する

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

公開ツール: `search`, `list_topics`, `get_document`, `get_best_practice`, `rebuild_index`, `get_connection_graph`。詳細は README.md 参照。

## アーキテクチャ（ソース別の責務）

| ファイル | 役割 |
|---|---|
| `src/main.rs` | clap CLI エントリ。`index` / `status` / `serve` / `search` サブコマンドの分岐、`kb-mcp.toml` の読み込みと CLI 引数へのマージ、JSON/text 出力フォーマッタ |
| `src/config.rs` | バイナリ同居 `kb-mcp.toml` のロード。CLI / config / 既定値の優先順位解決、`FASTEMBED_CACHE_DIR` の注入 |
| `src/server.rs` | rmcp `ServerHandler` 実装。5 つの MCP ツールをディスパッチ。`search` は `db.search_hybrid` (vec + FTS5 + RRF) 経由 |
| `src/indexer.rs` | walkdir で対象拡張子 (`Registry::extensions()`) を走査 → Parser trait でパース → embedder.rs で embedding → db.rs で格納。SHA-256 で差分検出。冒頭で `backfill_fts()` を呼び pre-feature-9 DB を自動移行。[feature 12] 増分 API (`reindex_single_file` / `deindex_single_file` / `rename_single_file`) を切り出し watcher から共有 |
| `src/parser/` | [feature 20] Parser trait + Registry。`mod.rs` に Frontmatter/Chunk/ParsedDocument、`markdown.rs` に `.md` 実装、`txt.rs` に `.txt` 実装、`registry.rs` に拡張子ルックアップ |
| `src/markdown.rs` | Parser trait への移行後は `crate::parser::markdown::MarkdownParser` への薄い shim (legacy `parse()` / `parse_with_excludes()` 公開 API を維持) |
| `src/watcher.rs` | [feature 12] `notify-debouncer-full` を tokio channel 越しに受信し、拡張子フィルタ + path filter (`.obsidian/`) を経由して `indexer::{reindex,deindex,rename}_single_file` にディスパッチ。server 並走は `tokio::spawn` |
| `src/transport/` | [feature 18] MCP transport 抽象。`mod.rs` に `Transport` enum + CLI/config 解決、`stdio.rs` は既存 stdio 経路、`http.rs` は rmcp `StreamableHttpService` + axum 0.8 で `/mcp` マウント + `/healthz`。`KbServerShared` を Arc 共有し session factory で軽量生成 |
| `src/schema.rs` | [feature 17] frontmatter スキーマ検証。`kb-mcp-schema.toml` を `kb_path` 直下から読み、required / type / pattern / enum / min/max_length / allow_empty を検証。`kb-mcp validate` CLI から呼ばれ text / json / github 形式でレポート |
| `src/embedder.rs` | fastembed-rs の薄いラッパ。`ModelChoice` で埋め込みモデル (BGE-small-en-v1.5 / BGE-M3) を、`RerankerChoice` + `Reranker` で optional な cross-encoder 再ランクを提供 |
| `src/db.rs` | rusqlite + sqlite-vec + FTS5 (trigram)。`chunks` / `vec_chunks` / `fts_chunks` の schema と CRUD、`search_hybrid` (RRF k=60) を提供 |

### データフロー

```
.md / .txt ファイル群 (Registry::extensions() で filter)
     ↓ walkdir
indexer.rs: 差分検出 (SHA-256 vs .kb-mcp.db の chunks.hash)
     ↓ 変更ありのものだけ
parser/: 拡張子で Parser を選択 → frontmatter/title 抽出 + チャンク化
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

初回実行時に HuggingFace hub cache が作られる (BGE-small: ~130 MB、BGE-M3: ~2.3 GB、BGE-reranker-v2-m3: ~2.3 GB 等)。2 回目以降は再 DL されない。

### TLS 接続エラー時の迂回手順

環境によっては fastembed-rs の内部 TLS (native-tls / reqwest) が HuggingFace に接続失敗 (`os error 10054` "Connection was reset") することがある。その場合は Python の HuggingFace CLI で事前 DL してから `FASTEMBED_CACHE_DIR` でキャッシュを指定する:

```bash
hf download BAAI/bge-m3 --include 'onnx/*' 'tokenizer*' 'config.json' 'special_tokens_map.json'
hf download BAAI/bge-reranker-v2-m3                                          # reranker 用

FASTEMBED_CACHE_DIR=~/.cache/huggingface/hub kb-mcp index --kb-path ... --model bge-m3 --force
```

`hf download` は HF Hub 標準のキャッシュ構造 (`models--<org>--<name>/`) に配置する。fastembed-rs は `FASTEMBED_CACHE_DIR` で指定されたディレクトリを同じ構造で扱うため、そのまま流用可能。

## 主要な依存

- `rmcp` 1.x — MCP サーバフレームワーク (stdio transport)
- `fastembed` 5.x — ONNX ベースの埋め込み / reranker (BGE-small-en-v1.5 / BGE-M3 / BGE-reranker-v2-m3 等)
- `rusqlite` 0.39 (bundled、SQLite 3.50+) — SQLite 静的リンク、FTS5 + trigram + `contentless_delete=1` 利用可
- `sqlite-vec` 0.1 — ベクトル類似検索拡張
- `pulldown-cmark` 0.13 — Markdown パーサ
- `dirs` 6 — OS 標準キャッシュディレクトリ解決

## 改善候補

次期バージョンアップで検討中のアイテムは [TODO.md](./TODO.md) を参照。主な残件:

- HTTP/SSE トランスポート (複数クライアント同時接続)
- ライブ同期 (`notify` クレートによる file watcher 駆動の増分再インデックス)
- Markdown 以外のファイル対応 (.txt / .rst / .adoc)
- Frontmatter スキーマ検証

実装済みの履歴 (feature 7-11, 13-16, 19, 21-24 + evaluator 指摘対応) は `claude-progress.txt` と `features.json` を参照。

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
