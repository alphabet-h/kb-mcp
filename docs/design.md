# kb-mcp: ナレッジベース MCP サーバー設計書

> **作成日**: 2026-04-17
> **最終更新**: 2026-04-18 (feature 7-10 + evaluator #1-6 反映)
> **ステータス**: Implemented
> **対象**: Markdown ナレッジベースを外部プロジェクトから検索可能にする独立 MCP サーバー

---

## 1. 背景と目的

### 問題

`ai_organization` リポジトリは 100〜500 の Markdown ファイル（deep-dive / ai-news / tech-watch / best-practices）を蓄積している。外部プロジェクトからこの知識を参照する際、ファイルを走査して必要な情報を探す必要があり、コンテキストウィンドウを圧迫する。

### 解決策

knowledge-base を **読み取り専用の MCP サーバー**として公開する。セマンティック検索 + 全文検索 + (任意で) cross-encoder 再ランクで必要な情報だけを返し、外部プロジェクトのコンテキスト消費を最小化する。

### 非目標

- ナレッジベースへの書き込み機能（既存スキルの管理フローを維持するため提供しない）
- 他リポジトリ / 他ユーザーとの共有（個人利用が主）

---

## 2. ユーザーとユースケース

### 利用者

1. **自分の他の Claude Code プロジェクト** — Web アプリ開発中に AI 技術情報を参照
2. **MCP 対応の任意のクライアント** — Cursor / Gemini CLI 等

### 主要ユースケース

| # | シナリオ | 使うツール |
|---|---|---|
| 1 | 「ChromaDB のハイブリッド検索の設定方法は？」 | `search` |
| 2 | 「Claude Code の hooks のベストプラクティスは？」 | `get_best_practice` |
| 3 | 「今どんなトピックの調査があるか一覧で見たい」 | `list_topics` |
| 4 | 「deep-dive/chromadb/overview.md を全文読みたい」 | `get_document` |
| 5 | 「スキル実行後にインデックスを更新したい」 | `rebuild_index` |

---

## 3. アーキテクチャ

### 全体像

```
外部プロジェクト (Claude Code / Cursor / Gemini CLI)
    │
    │ MCP (stdio)
    ▼
┌─────────────────────────────────────────────┐
│  kb-mcp (Rust シングルバイナリ)              │
│                                             │
│  ┌──────────┐    ┌───────────────────────┐  │
│  │  rmcp    │    │  fastembed-rs         │  │
│  │  (MCP    │    │  ・TextEmbedding       │  │
│  │   SDK)   │    │    (BGE-small / M3)   │  │
│  │          │    │  ・TextRerank          │  │
│  │          │    │    (optional BGE v2m3) │  │
│  └────┬─────┘    └──────┬────────────────┘  │
│       │                 │                   │
│  ┌────┴─────────────────┴────────────────┐  │
│  │  SQLite (rusqlite bundled, 3.50+)     │  │
│  │  ・chunks テーブル                     │  │
│  │  ・vec_chunks (sqlite-vec, 動的 dim)   │  │
│  │  ・fts_chunks (FTS5 trigram,           │  │
│  │    contentless_delete=1)              │  │
│  │  ・index_meta (model/dim 整合性)       │  │
│  └───────────────────────────────────────┘  │
│       ▲                                     │
│  knowledge-base/**/*.md (読み取り)           │
└─────────────────────────────────────────────┘
```

### 技術スタック

| コンポーネント | クレート | バージョン | 役割 |
|---|---|---|---|
| MCP サーバー | `rmcp` | 1.x | stdio トランスポート、ツール定義マクロ |
| Embedding | `fastembed` | 5.x | BGE-small-en-v1.5 (384) / BGE-M3 (1024)、ONNX Runtime |
| Reranker | `fastembed` TextRerank | 5.x | BGE-reranker-v2-m3 / jina-v2-ml / BGE-reranker-base、optional |
| ベクトル検索 | `sqlite-vec` | 0.1 | SQLite 拡張、ベクトル近傍探索 |
| 全文検索 | SQLite FTS5 (bundled) | — | trigram tokenizer + bm25 column weight |
| SQLite | `rusqlite` | 0.39 | `bundled` feature で SQLite 3.50+ を静的リンク |
| Markdown パース | `pulldown-cmark` | 0.13 | frontmatter 抽出、見出しベースチャンキング |
| YAML パース | `serde_yaml` | — | frontmatter の YAML 解析 |
| 非同期ランタイム | `tokio` | ^1 | rmcp 依存 |
| CLI 引数 | `clap` | ^4 | `--kb-path` / `--model` / `--reranker` / `--force` 等 |

### シングルバイナリ戦略 (階層 C 維持)

- `rusqlite` の `bundled` feature で SQLite を静的リンク（OS の libsqlite に依存しない）
- `sqlite-vec` は C ソースを `cc` クレートでビルドし、`rusqlite` にロード
- `fastembed` は ONNX Runtime を静的リンク (ort-sys)。追加 DLL 不要
- 埋め込み / reranker モデルは初回実行時に HuggingFace Hub から自動 DL。`FASTEMBED_CACHE_DIR` でキャッシュ先を指定可能
- 最終バイナリ: 約 **30 MB** (Windows release, ONNX Runtime 含む)
- **判断基準**: 新機能追加時も「単体バイナリ・ランタイム不要」を維持できるもののみ採用する (詳細は `TODO.md` 冒頭)

---

## 4. データモデル

### SQLite スキーマ

```sql
-- メタデータ (model / dim の整合性チェック用)
-- init() で最初に作る (vec_chunks の dim を決めるため)
CREATE TABLE IF NOT EXISTS index_meta (
    key   TEXT PRIMARY KEY,
    value TEXT
);
-- 主要 key: 'embedding_model' / 'embedding_dim'

-- ドキュメントメタデータ
CREATE TABLE IF NOT EXISTS documents (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    path         TEXT UNIQUE NOT NULL,     -- knowledge-base/ からの相対パス
    title        TEXT,                     -- frontmatter の title
    topic        TEXT,                     -- ディレクトリ名（例: chromadb, claude-code）
    category     TEXT,                     -- ai-news / deep-dive / tech-watch / best-practices
    depth        TEXT,                     -- overview / getting-started / features 等
    tags         TEXT,                     -- JSON 配列 '["chromadb","rag"]'
    date         TEXT,                     -- frontmatter の date (YYYY-MM-DD)
    content_hash TEXT NOT NULL,            -- SHA-256。差分検出用
    last_indexed TEXT NOT NULL             -- ISO 8601
);

-- テキストチャンク (content はここで保持)
CREATE TABLE IF NOT EXISTS chunks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    document_id  INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    chunk_index  INTEGER NOT NULL,         -- ドキュメント内の順序
    heading      TEXT,                     -- 属する見出しテキスト
    content      TEXT NOT NULL,            -- チャンク本文
    token_count  INTEGER                   -- 概算トークン数
);

-- ベクトル仮想テーブル (dim は index_meta から決定、遅延生成)
-- CREATE VIRTUAL TABLE vec_chunks USING vec0(
--     chunk_id INTEGER PRIMARY KEY,
--     embedding float[{dim}]   -- BGE-small なら 384、BGE-M3 なら 1024
-- );

-- FTS5 仮想テーブル (contentless, trigram)
-- rowid = chunks.id で統一
CREATE VIRTUAL TABLE fts_chunks USING fts5(
    heading,
    content,
    content='',
    contentless_delete=1,         -- SQLite 3.43+
    tokenize = "trigram remove_diacritics 1 case_sensitive 0"
);
```

### 次元の動的決定 (feature 7/8)

1. `Database::init()` で `index_meta` / `documents` / `chunks` / `fts_chunks` を作成するが `vec_chunks` は作らない
2. `verify_embedding_meta(model, dim)` が呼ばれたタイミングで meta を読み、必要なら `vec_chunks` を `ensure_vec_chunks_table(dim)` で作成
3. runtime の model/dim と meta の不一致を検出して `--force --model <name>` 誘導エラー
4. pre-feature-8 DB (chunks 既存 + meta 空) は初回起動で自動的に `(bge-small-en-v1.5, 384)` を記録

### チャンキング戦略

1. **見出し単位分割**: Markdown の `## ` / `### ` を区切りとしてチャンクを生成
2. **最大長制限**: 1 チャンクが 512 トークンを超える場合、段落単位で再分割
3. **最小長フィルタ**: 短すぎるチャンクは前のチャンクにマージ
4. **除外ルール**: `## 次の深堀り候補` セクションはインデックス対象外 (今後設定化予定)
5. **見出しの継承**: 各チャンクの `heading` フィールドに属する h2 / h3 見出しを保持

### コンテンツハッシュによる差分検出

`rebuild_index` 実行時:
1. `knowledge-base/` 配下の全 `.md` ファイルを走査
2. 各ファイルの SHA-256 を計算し、`documents.content_hash` と比較
3. ハッシュが一致 → スキップ、不一致 → 該当ドキュメントのチャンク + embedding + FTS 行を再生成
4. DB にあるがファイルシステムにないドキュメント → 削除
5. 起動時に `backfill_fts()` を呼び、pre-feature-9 DB の FTS 行を非破壊的に埋める

---

## 5. 検索パイプライン

```
query text
     │
     ├─→ sanitize_fts_query (3 文字未満は vec-only にフォールバック)
     │        ↓
     │        FTS5 MATCH + bm25(fts_chunks, 2.0, 1.0)  ← heading 重み付き
     │        ↓ top-N (max(limit*5, 50))
     │        (filter: 選択的 filter 時は 10x over-fetch)
     │
     └─→ embedder.embed_single
              ↓
              vec_chunks MATCH + k = ?
              ↓ top-N (same)
              (filter: 同上)

     ↓ RRF マージ (k=60): 1/(60 + rank + 1) を chunk_id ごとに加算

     上位 limit 件 (OR 候補をそのまま次段へ)

  [optional] reranker.rerank_candidates(query, candidates, limit)
              ↓ cross-encoder でスコアを付けて降順ソート
              ↓ 上位 limit 件
```

- `score` は RRF モードなら RRF スコア (大きいほど良い)、rerank モードなら cross-encoder raw score (モデル依存、通常 0〜1)
- filter (category / topic) は両側それぞれで JOIN 経由で適用し、Rust 側で補足フィルタ + `FILTER_OVERFETCH_FACTOR=10` の over-fetch で欠落を防ぐ

---

## 6. ツール定義 (MCP)

### `search`

ハイブリッド検索 (vec + FTS + 任意で rerank)。

```
入力:
  query: string          # 検索クエリ (自然言語)
  limit: u32 = 5         # 返却件数
  category: string?      # "deep-dive" / "ai-news" / ...
  topic: string?         # "chromadb" / "claude-code" / ...
  rerank: bool?          # サーバ既定を上書き (reranker が有効時のみ意味を持つ)

出力:
  [
    {
      score: f32,            # RRF or cross-encoder スコア (rerank 時に意味が変わる)
      path: string,
      title: string?,
      heading: string?,
      content: string,
      topic: string?,
      date: string?
    }
  ]
```

### `list_topics`

```
入力: なし
出力: [{category, topic, file_count, last_updated, titles: [string]}]
```

### `get_document`

```
入力: { path: string }
出力: { path, title, date, topic, tags: [string], content: string }
```

### `get_best_practice`

```
入力: { target: string, category: string? }
出力: { target, category?: string, content: string }
```

`best-practices/{target}/PERFECT.md` を読み、category 省略時は h2 見出し一覧付きで全文を、指定時は該当 h2 セクションだけを返す。

### `rebuild_index`

```
入力: { force: bool = false }
出力: { total_documents, updated, deleted, total_chunks, duration_ms }
```

---

## 7. CLI インターフェース

```bash
# インデックス構築
kb-mcp index --kb-path ./kb [--force] [--model bge-small-en-v1.5|bge-m3]

# モデル切替 (既存 DB が異なるモデルの場合は --force が必須)
kb-mcp index --kb-path ./kb --model bge-m3 --force

# ステータス
kb-mcp status --kb-path ./kb

# MCP サーバ起動
kb-mcp serve --kb-path ./kb \
    [--model bge-small-en-v1.5|bge-m3] \
    [--reranker none|bge-v2-m3|jina-v2-ml|bge-base] \
    [--rerank-by-default]
```

`serve` は stdio で MCP プロトコルを喋る。

---

## 8. ファイル配置

```
kb-mcp/
├── Cargo.toml
├── build.rs                      # sqlite-vec の C ソースビルド
├── CLAUDE.md                     # Claude Code 向け運用ガイド
├── README.md                     # 英語向けユーザードキュメント
├── features.json / claude-progress.txt  # harness-kit の状態ファイル
├── docs/
│   ├── design.md                 # このファイル
│   └── plan.md                   # 初期実装計画 (履歴)
├── TODO.md                       # 未実装候補 + 借用判断基準
├── src/
│   ├── main.rs                   # clap CLI エントリ + サブコマンド分岐
│   ├── server.rs                 # rmcp ServerHandler + 5 ツール + rerank 分岐
│   ├── indexer.rs                # walkdir + SHA-256 差分 + backfill_fts
│   ├── markdown.rs               # frontmatter + 見出しチャンキング
│   ├── embedder.rs               # ModelChoice / Embedder / RerankerChoice / Reranker
│   └── db.rs                     # SQLite スキーマ + CRUD + search_hybrid + bm25 重み
└── tests/fixtures/               # テスト用 MD
```

外部プロジェクト側:
```
.mcp.json                         # MCP クライアント設定 (kb-mcp へのポインタ)
.kb-mcp.db(-shm, -wal)            # SQLite DB (gitignore)
```

---

## 9. エラーハンドリング

| エラーケース | 対処 |
|---|---|
| DB ファイルが存在しない | `status` は「no index」警告、`serve`/`index` は新規作成 |
| `--kb-path` が無効 | 起動時にエラーメッセージを stderr に出力して終了 |
| 埋め込みモデル未ダウンロード | 初回の `index` / `serve` 時に自動 DL (進捗表示)。HF 側の TLS 切断時は CLAUDE.md の迂回手順を参照 |
| model/dim ミスマッチ | `--force --model <name>` を示すエラー終了 (DL より前に判定) |
| FTS5 クエリに予約構文 | `sanitize_fts_query` で全体をダブルクォート化してリテラル扱い。3 文字未満は vec-only フォールバック |
| ファイル読み取り権限 | 該当ファイルをスキップし、ログに記録 |
| sqlite-vec ロード失敗 | 起動時にエラー |

---

## 10. テスト戦略

| レベル | 対象 | 方法 |
|---|---|---|
| ユニット | `markdown.rs` | fixture MD → チャンク配列検証 |
| ユニット | `db.rs` | インメモリ SQLite で CRUD / 検索 / FTS / 整合性チェック |
| ユニット | `embedder.rs` ModelChoice / RerankerChoice | 値テーブルと Default の invariant |
| 統合 (ignored) | 実モデル DL を伴う embedding / rerank | `cargo test -- --ignored` |
| 手動 | 実データ (ai_organization/.kb-mcp.db) | `index` / `serve` / search を手動で叩く |

2026-04-18 時点で 43 件 (4 ignored)。

---

## 11. 決定ログ

| 決定 | 選択肢 | 選択 | 理由 |
|---|---|---|---|
| 言語 | Rust / TypeScript / Python | **Rust** | シングルバイナリ要件 |
| MCP SDK | rmcp / TypeScript SDK | **rmcp** | Rust 一本に統一 |
| ベクトル検索 | ChromaDB / Qdrant / LanceDB / sqlite-vec | **sqlite-vec** | 100〜500 ファイル規模、組み込み、依存最小 |
| 全文検索 | FTS5 trigram / unicode61 / ICU | **FTS5 trigram** | CJK (日本語) 対応。ICU は bundled で使えない |
| Embedding | fastembed-rs / EmbedAnything / API (OpenAI) | **fastembed-rs** | ONNX ローカル推論、API キー不要 |
| デフォルトモデル | BGE-small / BGE-M3 | **BGE-small-en-v1.5** | 既存 DB 互換。BGE-M3 は opt-in |
| Reranker | BGE-reranker-v2-m3 / Jina v2 ml / BGE-reranker-base | **optional (デフォルト none)** | DL コスト (~2.3 GB) を opt-in に。日本語推奨は BGE-v2-m3 |
| RRF k 定数 | 60 / 可変 | **60 (定数)** | 原論文値、当面チューニング不要 |
| FTS contentless | `content=''` + `contentless_delete=1` / external content | **contentless + delete 拡張** | triggers 不要、SQLite 3.43+ で DELETE 可能 |
| bm25 column weight | 1/1 / 2/1 | **heading 2.0 / content 1.0** | 見出し一致を優遇 (evaluator #5) |
| モデル切替時の挙動 | 自動再インデックス / 明示 `--force` / エラー終了 | **エラー終了 + 明示 `--force`** | 数 GB DL と破壊的操作はユーザ合意を取る |
| インデックス更新 | 手動 / ファイル監視 / 起動時自動 | **手動コマンド** | シンプル。`notify` による watcher は将来拡張 |
| 書き込み機能 | 提供 / 提供しない | **提供しない** | 既存スキルの管理フローを維持 |
| モデル配置 | バイナリ埋め込み / 自動ダウンロード | **自動ダウンロード** | バイナリサイズを抑える |
