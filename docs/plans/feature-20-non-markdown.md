# feature 20 — Markdown 以外のテキスト形式対応 (Parser Plugin)

> planner エージェント (harness-kit:planner) が 2026-04-19 に生成した仕様書。
> feature 12 の前に着手する合意 (2026-04-19)。
>
> **ユーザ決定事項 (2026-04-19)**:
> - MVP は **`.md` + `.txt` の 2 種に絞る** (`.rst` / `.adoc` はユーザ規模が小さいため後回し)
> - `.rst` / `.adoc` / `.pdf` / `.docx` は「将来候補」として TODO.md に記録
> - Parser trait 抽象は入れておき、将来の拡張子追加は無改修で可能な状態にする
> - **キー省略時のデフォルトは `["md"]`** (pre-feature-20 と完全後方互換、`.txt` は opt-in)
> - 空配列 `enabled = []` はエラー
> - `.txt` のチャンク境界は **MVP では単一 chunk** (段落分割は EXT-4 で対応)

## 概要
kb-mcp のパイプラインから Markdown 固有の前提を剥がし、`.md` / `.txt` / `.rst` / `.adoc` を単一の `Parser` trait 抽象で扱えるようにする。拡張子セットは `kb-mcp.toml` の `[parsers]` テーブルで宣言でき、feature 12 (file watcher) の path filter も同じ設定を参照する。

## 対象ユーザー
- Sphinx / Read the Docs 形式の技術文書 (`.rst`) や AsciiDoc (`.adoc`) と Markdown が混在するナレッジベース
- プレーンテキストのメモ (`.txt` / 変換した PDF の text layer) も同じ DB で検索したいユーザー
- 現行 `.md` のみのナレッジベースを変更なしで運用し続けたいユーザー (後方互換)

---

## スコープ設計

### MVP (Sprint 1) — feature 20 本体

対象拡張子は **`.md` + `.txt` の 2 種** に絞る (ユーザ決定事項)。Parser trait 抽象は将来 `.rst` / `.adoc` / `.pdf` 等を追加できる形で入れる。

- [ ] **MVP-1: Parser trait の抽出とプラグインレジストリ**
  - `src/parser/mod.rs` に `trait Parser` を定義
  - `src/parser/markdown.rs` が `src/markdown.rs` の現行実装を trait 実装に寄せ替え
  - 既存 `cargo test markdown::` / `cargo test indexer::` が無改変で通る (regression guard)

- [ ] **MVP-2: `.txt` パーサ**
  - frontmatter なし。title はファイル名から derive (`foo-bar.txt` → `"foo bar"`)
  - 本文は正規化後 1 チャンク or ~1500 文字境界で分割
  - `tests/fixtures/plain.txt` で単体テスト

- [ ] **MVP-5: indexer の拡張子分岐**
  - `collect_md_files` → `collect_source_files(&kb_path, &registry)` にリネーム
  - 登録済みレジストリの拡張子のみ拾う、それ以外は無視
  - `extract_category_topic` は既存のパス規約を流用 (拡張子非依存)

- [ ] **MVP-6: `kb-mcp.toml` の `[parsers]` スキーマ**
  ```toml
  # 省略時はこれと等価 (pre-feature-20 と完全に同じ挙動)
  [parsers]
  enabled = ["md"]

  # .txt も indexing したい場合は明示的に opt-in
  [parsers]
  enabled = ["md", "txt"]
  ```
  - **キー省略時のデフォルトは `["md"]`** (後方互換、`.txt` は opt-in)
  - 空配列 `enabled = []` は `best_practice.path_templates = []` と同じポリシーで**エラー**
  - 未知 id (`"pdf"` / `"rst"` / `"adoc"` 等) は parse 時に reject (trait 実装が無いため)
  - 大文字小文字非区別 (`.MD` もマッチ)

- [ ] **MVP-7: DB 後方互換**
  - 既存 `.kb-mcp.db` に対して `kb-mcp index` 実行で schema 変更なしで動作
  - `documents.path` に `.rst` / `.adoc` / `.txt` が含まれてよい
  - pre-feature-20 DB のマイグレーション**不要**

- [ ] **MVP-8: CLI / `search` の path-based 応答の regression 非破壊**
  - `get_document` / `search` / `get_connection_graph` の `path` が `.rst` / `.adoc` / `.txt` を正しく返す
  - `get_best_practice` の `path_templates` は `{target}` の拡張子を利用側責務とするため変更なし

### 拡張 (Sprint 2) — feature 12 連携と高度化

- [ ] **EXT-1: feature 12 watcher との拡張子連動** (feature 12 実装時にブリッジ)
  - watcher は `[parsers].enabled` から filter 拡張子取得
  - 非対応拡張子のファイル保存は watcher イベントごと早期破棄

- [ ] **EXT-4: `.txt` のスマートチャンク分割**
  - 空行 2+ を段落境界として ~800〜1500 字のチャンク生成

- [ ] **EXT-5: 拡張子ごとの heading 重み上書き**
  - `.txt` は heading 空のため FTS bm25 heading 重みの regression テスト

### 非スコープ (MVP 外、将来候補として TODO.md に記録)

| 形式 | 位置づけ | 備考 |
|---|---|---|
| `.rst` | 将来候補 (中優先) | Python エコシステム限定。需要はあるが kb-mcp ユーザとの重なりは中以下。trait 実装追加で対応可 |
| `.adoc` | 将来候補 (低優先) | 技術ライター community 限定。需要小。trait 実装追加で対応可 |
| `.pdf` | **将来候補 (中〜高優先)** | 企業/学術で需要大。`pdf-extract` crate 依存で実装重いが value は高い。text layer のみ抽出 (OCR 非対応) |
| `.docx` | 将来候補 (中優先) | 企業文書。`docx-rs` / `dotext` 等の crate 依存 |
| `.org` / `.typ` / `.wiki` / `.tex` | 将来候補 (低優先) | trait で受けられるが需要を見極め中 |
| `pandoc` / `asciidoctor` 外部プロセス呼び出し | **恒久非スコープ** | 階層 C (単体バイナリ) 違反 |
| 同一ファイル名の拡張子違い重複排除 | 非スコープ | `foo.md` と `foo.txt` は別文書として index |

---

## 技術スタック

| コンポーネント | 選択 | 理由 |
|---|---|---|
| `.txt` パーサ | **自前実装** | 改行正規化 + paragraph 検出のみ |
| `.md` パーサ | **現行 `pulldown-cmark` 維持** | 変更なし |
| Parser レジストリ | **静的 `Vec<Box<dyn Parser>>`** | 動的 dylib plugin は単体バイナリの value prop に反する |

**バイナリサイズ影響**: `.txt` 自前実装で依存追加ゼロ。増分 **<50 KB** (無視できる)。

---

## Parser trait API

```rust
// src/parser/mod.rs

pub trait Parser: Send + Sync {
    fn extension(&self) -> &'static str;
    fn id(&self) -> &'static str { self.extension() }

    fn parse(
        &self,
        raw: &str,
        path_hint: &str,
        exclude_headings: &[&str],
    ) -> ParsedDocument;
}

pub struct ParsedDocument {
    pub frontmatter: Frontmatter,  // 既存の型を再利用
    pub chunks: Vec<Chunk>,
    pub raw_content: String,
}
```

### レジストリ

```rust
pub struct Registry {
    parsers: Vec<Box<dyn Parser>>,
}

impl Registry {
    pub fn from_enabled(ids: &[String]) -> Result<Self>;  // 空配列はエラー
    pub fn defaults() -> Self;                            // ["md"] のみ (後方互換)
    pub fn by_extension(&self, ext: &str) -> Option<&dyn Parser>;
    pub fn extensions(&self) -> Vec<&'static str>;        // feature 12 が流用
}
```

### 既存 `src/markdown.rs` への影響
- 公開 API `markdown::parse()` / `markdown::parse_with_excludes()` は shim として残す (`MarkdownParser::parse()` へ委譲)
- `Frontmatter` / `Chunk` / `ParsedDocument` は `src/parser/mod.rs` へ移動し re-export
- 既存 Markdown テスト (CRLF 正規化、exclude_headings 3 状態) は一字一句触らず通り続ける

### indexer の変更点
`collect_md_files` → `collect_source_files(&kb_path, &registry)`。以降のループは拡張子ごとに `registry.by_extension(ext)` で Parser を選ぶだけ。hash diff / rename detection / embed / quality_score は拡張子非依存のため変更なし。

---

## Frontmatter 相当の抽出ルール (MVP)

| 形式 | title | date | tags | topic | 見出し/チャンク境界 |
|---|---|---|---|---|---|
| `.md` | YAML `title` | YAML `date` | YAML `tags` | YAML `topic` | `##` / `###` (既存) |
| `.txt` | ファイル名から derive | なし | なし | パス規約 | 段落 (MVP は単一 chunk、EXT-4 で ~800-1500 字) |

`.txt` のファイル名 → title: `"deep-dive-2026-notes.txt"` → `"deep dive 2026 notes"` (ハイフン/アンダースコア → 空白、拡張子除去、小文字のまま、non-ASCII はそのまま)。

---

## Config schema (`kb-mcp.toml`)

```toml
# デフォルト (キー省略時と等価、pre-feature-20 挙動)
[parsers]
enabled = ["md"]

# .txt も含める場合は明示的に opt-in
[parsers]
enabled = ["md", "txt"]
```

制約:
- **キー省略時は `["md"]`** (後方互換)
- 空配列 `enabled = []` はエラー (silent failure 防止)
- 未知 id は parse 時 reject
- 拡張子比較は大文字小文字非区別
- `id` と `extension` を分離設計 (将来の alias 対応に備える、MVP では一致)

---

## AI 統合ポイント

feature 20 自体は AI 機能を増やさないが、以下が自動的に波及:
- **Embedder 流用**: `.rst` / `.adoc` / `.txt` チャンクもそのまま BGE-small / BGE-M3 で embedding
- **Reranker 流用**: cross-encoder は plain text なので形式を問わない
- **Quality filter (feature 13) 流用**: `chunk_quality_score` は形式非依存。`.txt` は heading 空のため「短文ペナルティ」のみが効く (意図通り)
- **Connection graph (feature 15)**: `.rst` から `.md` の隣接ノード横断探索が自動で可能に

---

## 実装コスト見積もり

| 項目 | 見積もり LOC | 実装時間 | テスト LOC |
|---|---|---|---|
| Parser trait + Registry | ~100 | 1 日 | ~80 |
| `.md` 実装の trait 適合 | ~50 差分 | 0.5 日 | 既存テストを流用 |
| `.txt` パーサ | ~80 | 0.5 日 | ~60 |
| indexer 拡張子分岐 + config | ~60 | 0.5 日 | ~40 |
| **MVP 合計** | **~290** | **~2.5 日** | **~180** |
| 拡張 (EXT-1 / EXT-4 / EXT-5) | ~150 | 1 日 | ~80 |
| 将来拡張 (`.rst` / `.adoc` / `.pdf` / `.docx`) | ~500+ | 4-6 日 | ~400+ |

---

## テスト戦略

| レベル | 対象 | 方法 |
|---|---|---|
| ユニット | `parser::markdown` | 既存 `sample.md` / `no_frontmatter.md` を流用 (regression guard) |
| ユニット | `parser::txt` | `plain.txt` / `no_title.txt` / title-from-filename の複数 case |
| ユニット | `parser::registry` | `from_enabled` の valid/invalid/empty、`by_extension` 大小無視 |
| 統合 | `indexer::rebuild_index` | `.md` + `.txt` 混在ディレクトリで chunk count / path / category 期待通り |
| 統合 | `search` regression | `.txt` にしか含まれない語で `search` がその `.txt` を返す |
| 後方互換 | `.md` のみの DB | `cargo test -- --ignored` の embedder 系テスト無改変 pass |

---

## 想定リスク

| リスク | 深刻度 | 緩和 |
|---|---|---|
| `.txt` 巨大ファイル (log 等) で embedding token 超過 | 中 | 既存 embedder の truncate (fastembed 側 max_length) に委ねる。EXT-4 で解消 |
| 既存 `markdown.rs` テスト破壊 | 低 | trait 導入と互換 shim を同じ PR で揃え、既存テストは一切編集しない |
| feature 12 との開発順序で watcher 側に拡張子直書きしてしまう | 中 | MVP-1 と MVP-6 (trait + config schema) を feature 12 着手前に merge する (→ 合意済み) |
| CJK ファイル名 `"日本語ファイル.txt"` の title 変換 | 低 | non-ASCII はそのまま、`-` / `_` だけ空白化 |
| 大文字拡張子 (`.MD` / `.TXT`) | 低 | `eq_ignore_ascii_case` で比較 |

---

## 関連ファイル

新規:
- `src/parser/mod.rs` (trait + Frontmatter/Chunk/ParsedDocument 移動)
- `src/parser/registry.rs`
- `src/parser/txt.rs`
- `src/parser/markdown.rs` (現 `src/markdown.rs` から移動)
- fixtures: `tests/fixtures/plain.txt`

変更:
- `src/markdown.rs` (shim に縮退)
- `src/indexer.rs` (`collect_source_files` + registry 経由の parser 選択)
- `src/config.rs` (`parsers: Option<ParsersConfig>` 追加、empty array reject)
- `kb-mcp.toml.example` (`[parsers]` 追記)

更新:
- `docs/design.md` (§4 チャンキング戦略と §11 決定ログに Parser Plugin 項)
- `README.md` / `CLAUDE.md` (対応拡張子、`[parsers]` 設定例)
- `features.json` (feature 20 を passing に、実装完了後)

依存 (feature 12 実装時):
- `registry.extensions()` をそのまま watcher filter に流用

---

## 確認事項 (すべて決定済み 2026-04-19)

1. ~~**MVP 対象拡張子**~~: **`.md` + `.txt`** (`.txt` は opt-in)
2. ~~**`.txt` のチャンク境界**~~: **MVP は単一 chunk** (段落分割は EXT-4)
3. ~~**`[parsers].enabled = []`**~~: **エラー扱い**
4. ~~**キー省略時のデフォルト**~~: **`["md"]` のみ** (後方互換)
