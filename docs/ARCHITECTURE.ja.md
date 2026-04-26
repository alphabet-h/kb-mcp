# アーキテクチャ

kb-mcp のソース構造とデータフロー。コードを拡張・修正するコントリビュータ向け。

> **English version**: [ARCHITECTURE.md](./ARCHITECTURE.md)

## ソース別の責務

| ファイル | 役割 |
|---|---|
| `src/main.rs` | clap CLI エントリ。`index` / `status` / `serve` / `search` / `graph` / `validate` / `eval` サブコマンドのディスパッチ。`kb-mcp.toml` 読み込みと CLI 引数へのマージ。JSON / text 出力フォーマッタ |
| `src/config.rs` | 4 階層の `kb-mcp.toml` 探索 (`--config` フラグ → CWD → `.git` 祖先 (最大 20 階層) → バイナリ隣 legacy)。`Config::discover()` が `ConfigSource` enum を返し、`main.rs` が起動時に tracing で出す。`CLI > 設定ファイル > 既定値` の優先順位を解決。config が設定していて env 未設定の場合のみ `FASTEMBED_CACHE_DIR` を env に注入 |
| `src/server.rs` | rmcp `ServerHandler` 実装。6 つの MCP ツールをディスパッチ。`search` は `db.search_hybrid` 経由で結果を `SearchResponse` ラッパ (`low_confidence` / `match_spans` / `filter_applied`) に包んで返す (feature-26、v0.3.0 で BREAKING) |
| `src/indexer.rs` | walkdir で `Registry::extensions()` の拡張子を走査。Parser trait でパース → embedder で embedding → db に格納。SHA-256 content-hash による差分検出。watcher と共有する増分 API (`reindex_single_file` / `deindex_single_file` / `rename_single_file`) |
| `src/parser/` | Parser trait + Registry。`mod.rs` (Frontmatter / Chunk / ParsedDocument)、`markdown.rs`、`txt.rs`、`registry.rs` (拡張子ルックアップ) |
| `src/markdown.rs` | `crate::parser::markdown::MarkdownParser` への薄い shim。legacy `parse()` / `parse_with_excludes()` 公開 API を維持 |
| `src/watcher.rs` | `notify-debouncer-full` を tokio channel 越しに受信。拡張子 + path でフィルタして `indexer::{reindex,deindex,rename}_single_file` にディスパッチ。MCP サーバと並走 (`tokio::spawn`) |
| `src/transport/` | MCP transport 抽象。`mod.rs` (Transport enum + CLI/config 解決)、`stdio.rs` (stdio)、`http.rs` (rmcp `StreamableHttpService` + axum、`/mcp` と `/healthz` をマウント)。`KbServerShared` を Arc 共有し session factory で接続ごとに軽量ハンドルを生成 |
| `src/schema.rs` | Frontmatter スキーマ検証。`kb_path` 直下の `kb-mcp-schema.toml` を読み、`required` / `type` / `pattern` / `enum` / `min_length` / `max_length` / `allow_empty` を検証。`kb-mcp validate` CLI から呼ばれ、text / JSON / GitHub annotation 形式で報告 |
| `src/embedder.rs` | `fastembed-rs` の薄いラッパ。`ModelChoice` で embedding モデル (BGE-small-en-v1.5 / BGE-M3) を選択。`RerankerChoice` + `Reranker` で optional な cross-encoder 再ランク |
| `src/db.rs` | `rusqlite` + `sqlite-vec` + FTS5 (trigram)。`chunks` / `vec_chunks` / `fts_chunks` スキーマと CRUD を管理。`search_hybrid` (Reciprocal Rank Fusion、`k = 60`) を提供。`SearchFilters` 構造体でフィルタ引数 (path glob / tags / date range / min_quality) を集約、`MatchSpan` でバイトオフセット引用を表現 (feature-26) |
| `src/quality.rs` | チャンク単位の品質スコアリング (長さ / 定型語 / 構造シグナル) |
| `src/graph.rs` | ベクトルインデックス上での Connection Graph BFS。`get_connection_graph` MCP ツールと `kb-mcp graph` CLI から利用 |
| `src/eval.rs` | `kb-mcp eval` CLI 用のリトリーバル品質評価 (opt-in)。Golden YAML を parse し、各クエリを `db.search_hybrid` で実行、recall@k / MRR / nDCG@k を計算。`<kb_path>/.kb-mcp-eval-history.json` を読み書きして前回との差分を表示。`serve` / `search` / `index` の挙動は一切変えない |

## データフロー

```
.md / .txt ファイル (Registry::extensions() でフィルタ)
     │
     ▼ walkdir
indexer.rs: SHA-256 content-hash を chunks.hash と比較
     │
     ▼ 変更ありのファイルのみ
parser/: 拡張子で Parser を選択 → frontmatter + title 抽出 + チャンク化
     │
     ▼
embedder.rs: fastembed で embedding 生成
              (BGE-small-en-v1.5 → 384 次元、BGE-M3 → 1024 次元)
     │
     ▼
db.rs: chunks (メタデータ) + vec_chunks (embedding)
       + fts_chunks (FTS5 trigram) に UPSERT
```

検索時、`search` ツールはハイブリッド検索を実行する:

- query → embedder → `vec_chunks MATCH` (top-N)
- query → sanitize → `fts_chunks MATCH` + bm25 (top-N) — 見出しに 2 倍の重み
- Rust 側で Reciprocal Rank Fusion (`k = 60`) → top-`limit` を返却
- (任意) cross-encoder reranker が上位候補を再スコアリングして返却

## Embedding キャッシュの解決

`embedder.rs::resolve_cache_dir()` が以下の順で解決する:

1. `FASTEMBED_CACHE_DIR` 環境変数 (最優先)
2. OS 標準キャッシュディレクトリ + `fastembed`:
   - Linux: `~/.cache/fastembed`
   - macOS: `~/Library/Caches/fastembed`
   - Windows: `%LOCALAPPDATA%\fastembed`
3. CWD 直下の `.fastembed_cache/` (最終フォールバック)

初回実行時、選択した ONNX モデルが HuggingFace hub 互換のキャッシュ構造で DL される (BGE-small: 約 130 MB、BGE-M3: 約 2.3 GB、BGE-reranker-v2-m3: 約 2.3 GB)。2 回目以降は再 DL されない。

`fastembed-rs` の native TLS が HuggingFace への接続に失敗する場合 (企業プロキシや TLS inspection の影響) は、README の「HuggingFace の TLS 失敗への対処」節を参照して `huggingface_hub` CLI で迂回する。

## 主要な依存

- **`rmcp`** 1.x — MCP サーバフレームワーク (stdio + Streamable HTTP トランスポート)
- **`fastembed`** 5.x — ONNX ベースの embedding / reranker
- **`rusqlite`** 0.39 with `bundled` — 静的リンク SQLite 3.50+、FTS5 + trigram tokenizer + `contentless_delete = 1`
- **`sqlite-vec`** 0.1 — ベクトル類似検索拡張
- **`pulldown-cmark`** 0.13 — Markdown パーサ
- **`notify`** 8 + **`notify-debouncer-full`** 0.6 — debounce 付きファイルウォッチャ
- **`axum`** 0.8 — Streamable HTTP トランスポートの HTTP サーバ
- **`dirs`** 6 — OS 標準キャッシュディレクトリ解決
