# kb-mcp: ナレッジベース MCP サーバー設計書

> **作成日**: 2026-04-17
> **ステータス**: Approved
> **対象**: `ai_organization/knowledge-base/` を外部プロジェクトから検索可能にする MCP サーバー

---

## 1. 背景と目的

### 問題

`ai_organization` リポジトリは 100+ の Markdown ファイル（deep-dive / ai-news / tech-watch / best-practices）を蓄積している。外部プロジェクトからこの知識を参照する際、ファイルを走査して必要な情報を探す必要があり、コンテキストウィンドウを圧迫する。ファイル数は今後 300〜500 に増加する見込み。

### 解決策

knowledge-base を **読み取り専用の MCP サーバー**として公開する。セマンティック検索で必要な情報だけを返し、外部プロジェクトのコンテキスト消費を最小化する。

### 非目標

- ナレッジベースへの書き込み機能（既存スキルの管理フローを維持するため提供しない）
- リアルタイムのファイル監視・自動インデックス更新（手動 rebuild で十分）
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
┌─────────────────────────────────────┐
│  kb-mcp (Rust シングルバイナリ)       │
│                                     │
│  ┌──────────┐    ┌───────────────┐  │
│  │  rmcp    │    │  fastembed-rs │  │
│  │  (MCP    │    │  (ONNX        │  │
│  │   SDK)   │    │   embedding)  │  │
│  └────┬─────┘    └──────┬────────┘  │
│       │                 │           │
│  ┌────┴─────────────────┴────────┐  │
│  │  sqlite-vec                   │  │
│  │  (ベクトル + メタデータ + FTS)  │  │
│  └───────────────────────────────┘  │
│       ▲                             │
│  knowledge-base/**/*.md (読み取り)   │
└─────────────────────────────────────┘
```

### 技術スタック

| コンポーネント | クレート | バージョン | 役割 |
|---|---|---|---|
| MCP サーバー | `rmcp` | ^1.4 | stdio トランスポート、ツール定義マクロ |
| Embedding | `fastembed` | ^5 | BGE-small-en-v1.5 (384 次元)、ONNX Runtime |
| ベクトル検索 | `sqlite-vec` | 最新 | SQLite 拡張、ベクトル近傍探索 |
| SQLite バインディング | `rusqlite` | 最新 | `bundled` feature で SQLite を静的リンク |
| Markdown パース | `pulldown-cmark` | 最新 | frontmatter 抽出、見出しベースチャンキング |
| YAML パース | `serde_yaml` | 最新 | frontmatter の YAML 解析 |
| 非同期ランタイム | `tokio` | ^1 | rmcp 依存 |
| CLI 引数 | `clap` | ^4 | `--kb-path` 等のオプション |

### シングルバイナリ戦略

- `rusqlite` の `bundled` feature で SQLite を静的リンク（OS の libsqlite に依存しない）
- `sqlite-vec` は C ソースを `cc` クレートでビルドし、`rusqlite` にロード
- `fastembed` は ONNX Runtime を動的リンク（ort クレート経由。初回ダウンロード）
- Embedding モデル（BGE-small, ~23MB）は初回実行時に `~/.cache/fastembed/` に自動ダウンロード
- 最終バイナリ: **推定 10〜20MB**（ONNX Runtime DLL 除く）

---

## 4. データモデル

### SQLite スキーマ

```sql
-- ドキュメントメタデータ
CREATE TABLE documents (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    path        TEXT UNIQUE NOT NULL,     -- knowledge-base/ からの相対パス
    title       TEXT,                     -- frontmatter の title
    topic       TEXT,                     -- ディレクトリ名（例: chromadb, claude-code）
    category    TEXT,                     -- ai-news / deep-dive / tech-watch / best-practices
    depth       TEXT,                     -- overview / getting-started / features 等
    tags        TEXT,                     -- JSON 配列 '["chromadb","rag"]'
    date        TEXT,                     -- frontmatter の date (YYYY-MM-DD)
    content_hash TEXT NOT NULL,           -- SHA-256。差分検出用
    last_indexed TEXT NOT NULL            -- ISO 8601
);

-- テキストチャンク + embedding
CREATE TABLE chunks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    document_id  INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    chunk_index  INTEGER NOT NULL,        -- ドキュメント内の順序
    heading      TEXT,                    -- 属する見出しテキスト
    content      TEXT NOT NULL,           -- チャンク本文
    token_count  INTEGER                  -- 概算トークン数
);

-- sqlite-vec 仮想テーブル（ベクトルインデックス）
CREATE VIRTUAL TABLE vec_chunks USING vec0(
    chunk_id INTEGER PRIMARY KEY,
    embedding float[384]
);

-- メタデータ
CREATE TABLE index_meta (
    key   TEXT PRIMARY KEY,
    value TEXT
);
-- key: 'model_name', 'embedding_dim', 'last_full_rebuild', 'schema_version'
```

### チャンキング戦略

1. **見出し単位分割**: Markdown の `## ` / `### ` を区切りとしてチャンクを生成
2. **最大長制限**: 1 チャンクが 512 トークンを超える場合、段落（`\n\n`）単位で再分割
3. **最小長フィルタ**: 50 文字未満のチャンク（見出しのみ等）は前のチャンクにマージ
4. **除外ルール**:
   - `## 次の深堀り候補` セクションはインデックス対象外
   - frontmatter はチャンクに含めず `documents` テーブルに格納
   - コードブロック内のテキストはチャンクに含める（技術情報として有用）
5. **見出しの継承**: 各チャンクの `heading` フィールドに属する h2 / h3 見出しを保持し、検索結果の文脈を伝える

### コンテンツハッシュによる差分検出

`rebuild_index` 実行時:
1. `knowledge-base/` 配下の全 `.md` ファイルを走査
2. 各ファイルの SHA-256 を計算し、`documents.content_hash` と比較
3. ハッシュが一致 → スキップ、不一致 → 該当ドキュメントのチャンク + embedding を再生成
4. DB にあるがファイルシステムにないドキュメント → 削除

---

## 5. ツール定義

### `search`

セマンティック検索。クエリテキストを embedding に変換し、sqlite-vec で近傍探索。

```
入力:
  query: string          # 検索クエリ（自然言語）
  limit: u32 = 5         # 返却件数
  category: string?      # フィルタ: "deep-dive" / "ai-news" / "best-practices" / "tech-watch"
  topic: string?         # フィルタ: "chromadb" / "claude-code" 等

出力:
  results: [
    {
      score: f32,            # コサイン類似度
      path: string,          # ファイルパス
      title: string,         # ドキュメントタイトル
      heading: string,       # チャンクが属する見出し
      content: string,       # チャンク本文（トリミング済み）
      topic: string,
      date: string
    }
  ]
```

### `list_topics`

ナレッジベースのトピック一覧を返す。

```
入力: なし

出力:
  topics: [
    {
      category: string,      # "deep-dive" / "ai-news" 等
      topic: string,         # "chromadb" / "claude-code" 等
      file_count: u32,
      last_updated: string,  # 最新ファイルの date
      titles: [string]       # 含まれるドキュメントのタイトル一覧
    }
  ]
```

### `get_document`

指定パスのドキュメントを全文取得。

```
入力:
  path: string           # knowledge-base/ からの相対パス

出力:
  content: string,       # ドキュメント全文
  title: string,
  date: string,
  tags: [string],
  topic: string
```

### `get_best_practice`

PERFECT.md の特定セクションを返す。全文ではなくカテゴリ指定で必要な部分だけ取得可能。

```
入力:
  target: string         # "claude-code" 等（targets.yaml の name）
  category: string?      # "hooks" / "skills" / "plugins" 等。省略時は目次 + 概要

出力:
  content: string,       # 該当セクションのテキスト
  version: string,       # 対象バージョン（例: "v2.1.110"）
  last_updated: string,
  categories: [string]   # 利用可能なカテゴリ一覧（category 省略時のナビゲーション用）
```

### `rebuild_index`

インデックスを再構築する。

```
入力:
  force: bool = false    # true で全ファイル再インデックス（ハッシュ差分を無視）

出力:
  total_documents: u32,
  updated: u32,
  deleted: u32,
  total_chunks: u32,
  duration_ms: u64
```

---

## 6. CLI インターフェース

```bash
# MCP サーバーとして起動（通常の使い方。外部から stdio で接続）
kb-mcp serve --kb-path /path/to/knowledge-base

# インデックス再構築（スタンドアロンで実行）
kb-mcp index --kb-path /path/to/knowledge-base [--force]

# インデックス状態の確認
kb-mcp status --kb-path /path/to/knowledge-base

# バージョン表示
kb-mcp --version
```

`serve` サブコマンドは stdio で MCP プロトコルを喋る。`index` / `status` は MCP なしで直接実行する管理コマンド。

---

## 7. ファイル配置

```
ai_organization/
├── kb-mcp/                           # MCP サーバープロジェクト
│   ├── Cargo.toml
│   ├── build.rs                      # sqlite-vec の C ソースビルド
│   ├── src/
│   │   ├── main.rs                   # CLI パース + サブコマンド分岐
│   │   ├── server.rs                 # MCP サーバー（rmcp）+ ツール登録
│   │   ├── tools/
│   │   │   ├── mod.rs
│   │   │   ├── search.rs             # search ツール
│   │   │   ├── list_topics.rs        # list_topics ツール
│   │   │   ├── get_document.rs       # get_document ツール
│   │   │   ├── get_best_practice.rs  # get_best_practice ツール
│   │   │   └── rebuild_index.rs      # rebuild_index ツール
│   │   ├── indexer/
│   │   │   ├── mod.rs
│   │   │   ├── scanner.rs            # ファイルシステム走査 + ハッシュ差分
│   │   │   ├── markdown.rs           # frontmatter 抽出 + チャンキング
│   │   │   └── embedder.rs           # fastembed でベクトル生成
│   │   ├── db.rs                     # SQLite スキーマ + CRUD
│   │   └── config.rs                 # CLI 引数 + 設定
│   ├── tests/
│   │   ├── search_test.rs
│   │   ├── indexer_test.rs
│   │   └── fixtures/                 # テスト用の小さな MD ファイル群
│   └── README.md
├── knowledge-base/                   # 既存（読み取り対象）
├── .kb-mcp.db                        # SQLite DB（.gitignore 対象）
└── .gitignore                        # .kb-mcp.db を追加
```

---

## 8. 外部プロジェクトからの接続

### .mcp.json への追加

```json
{
  "mcpServers": {
    "ai-knowledge": {
      "command": "/path/to/kb-mcp",
      "args": ["serve", "--kb-path", "/path/to/ai_organization/knowledge-base"],
      "type": "stdio"
    }
  }
}
```

### Claude Code の CLAUDE.md での利用ガイド

```markdown
## AI 技術情報の参照

MCP サーバー `ai-knowledge` が接続されている場合、以下のツールで技術情報を検索できる:
- `ai-knowledge:search` — セマンティック検索（「ChromaDB のハイブリッド検索」等）
- `ai-knowledge:get_best_practice` — Claude Code 等のベストプラクティス参照
- `ai-knowledge:list_topics` — 調査済みトピックの一覧
```

---

## 9. エラーハンドリング

| エラーケース | 対処 |
|---|---|
| DB ファイルが存在しない | `rebuild_index` の実行を促すメッセージを返す |
| `--kb-path` が無効 | 起動時にエラーメッセージを stderr に出力して終了 |
| Embedding モデル未ダウンロード | 初回の `index` / `serve` 時に自動ダウンロード。失敗時はエラー |
| 検索時に DB が空 | 空結果 + `"Index is empty. Run rebuild_index first."` メッセージ |
| ファイル読み取り権限エラー | 該当ファイルをスキップし、ログに記録 |
| sqlite-vec ロード失敗 | 起動時にエラー。ビルドの問題を示すメッセージ |

---

## 10. テスト戦略

| レベル | 対象 | 方法 |
|---|---|---|
| ユニット | markdown.rs（チャンキング） | fixture MD → チャンク配列を検証 |
| ユニット | db.rs（CRUD） | インメモリ SQLite で INSERT/SELECT |
| 統合 | indexer 全体 | fixtures/ の MD 群をインデックス → search で期待結果 |
| 統合 | MCP サーバー | stdio で JSON-RPC を送受信、ツール応答を検証 |
| 手動 | 実データ | `knowledge-base/` 全体でインデックス → 実際のクエリで品質確認 |

---

## 11. 将来の拡張候補（スコープ外）

- **ハイブリッド検索**: sqlite-vec のベクトル検索 + SQLite FTS5 のキーワード検索を RRF で統合
- **Reranking**: fastembed-rs の reranker モデルで search 結果を再ランク
- **Embedding モデルの切り替え**: 多言語モデル（BGE-M3）への変更オプション
- **HTTP トランスポート**: stdio に加えて HTTP/SSE で複数クライアント同時接続
- **PostToolUse hook 連携**: このプロジェクトのスキル実行後に自動で `rebuild_index` を呼ぶ hook

---

## 12. 決定ログ

| 決定 | 選択肢 | 選択 | 理由 |
|---|---|---|---|
| 言語 | Rust / TypeScript / Python | **Rust** | シングルバイナリ要件、ディレクトリを汚さない |
| MCP SDK | rmcp / TypeScript SDK | **rmcp** | Rust 一本に統一。v1.4 で実用レベル |
| ベクトル検索 | ChromaDB / Qdrant / LanceDB / sqlite-vec | **sqlite-vec** | 100〜500 ファイル規模に最適。組み込み、依存最小 |
| Embedding | fastembed-rs / EmbedAnything / API (OpenAI) | **fastembed-rs** | ONNX ローカル推論、30+ モデル、API キー不要 |
| デフォルトモデル | BGE-small / BGE-base / all-MiniLM | **BGE-small-en-v1.5** | 384 次元、23MB、品質と速度のバランス |
| インデックス更新 | 手動 / ファイル監視 / 起動時自動 | **手動コマンド** | シンプル。確実 |
| 書き込み機能 | 提供 / 提供しない | **提供しない** | 既存スキルの管理フローを維持 |
| モデル配置 | バイナリ埋め込み / 自動ダウンロード | **自動ダウンロード** | バイナリサイズを抑える |
