# feature 17 — Frontmatter スキーマ検証

> planner エージェント (harness-kit:planner) が 2026-04-19 に生成した仕様書。

## 概要
YAML frontmatter の構造規約 (必須キー / 型 / 正規表現 / enum) を TOML でユーザ定義し、`kb-mcp validate` サブコマンドで違反ファイルを検知・レポートする機能。階層 C (単体バイナリ・ランタイム不要) を維持するためスキーマフォーマットは独自 TOML DSL、バリデータは自前実装 (JSON Schema ライブラリは非採用)。

## 対象ユーザー
- ai_organization のような frontmatter 規約 (title 必須 / date YYYY-MM-DD / tags 非空) を持つ KB 運用者
- CI で frontmatter 違反を検出してマージブロックしたいリポジトリ管理者
- `Frontmatter::default()` への silent fallback で「title が空になっていて検索ヒットしない」のようなデータ品質バグを早期検知したい開発者

非対象: `.txt` ユーザ、個人の雑多メモ (スキーマなしなら現状挙動を完全維持)

## 背景と設計原則
1. **silent fallback が問題**: `extract_frontmatter` はパース失敗時に `Frontmatter::default()` + stderr warn で続行するため、ユーザは気付けない
2. **階層 C 維持**: JSON Schema は ~1MB の依存増。既存 `serde` / `regex` / `toml` で実現
3. **後方互換絶対**: スキーマファイル不在なら pre-feature-17 挙動を完全維持
4. **MVP 最小**: スキーマ推論 (basic-memory 由来) は非スコープ
5. **ai_organization の実コーパスで即価値**: title 必須 / date 正規表現 / topic enum / tags 非空で十分

## MVP (Sprint 1)

- [ ] **MVP-1: スキーマ定義フォーマット**
  - 専用ファイル `kb-mcp-schema.toml` を `kb_path` 直下に配置
  - `[fields.<name>]` テーブルで `required` / `type` / `pattern` / `enum` / `min_length` / `max_length` を宣言

- [ ] **MVP-2: バリデータ本体 (`src/schema.rs`)**
  - `validate(fm: &Frontmatter, schema: &Schema) -> Vec<Violation>`
  - 検証項目: MissingRequired / TypeMismatch / PatternMismatch / NotInEnum / LengthOutOfRange

- [ ] **MVP-3: `kb-mcp validate` CLI サブコマンド**
  - シグネチャ: `validate [--kb-path] [--schema] [--format text|json|github] [--strict] [--no-color] [--fail-fast]`
  - exit code: 0 (no violation) / 1 (violations) / 2 (schema load error)
  - `.md` のみ対象、`.txt` は skip してレポート

- [ ] **MVP-4: スキーマなしの後方互換**
  - `kb-mcp-schema.toml` 不在 → `validate` は "no schema found" で exit 0
  - `index` / `serve` は pre-feature-17 挙動を完全維持

- [ ] **MVP-5: 出力フォーマット (3 種)**
  - text (人間向け、TTY 自動検出で色付け、`--no-color` 無効化可)
  - json (配列、スクリプト処理用)
  - github (`::error file=...,line=...::message`、CI 用)

- [ ] **MVP-6: 必須フィールドと empty の区別**
  - `required = true` + `title: ""` や `tags: []` → 違反扱い
  - `allow_empty = true` で空も許容する opt-in

- [ ] **MVP-7: `kb-mcp-schema.toml.example` 同梱**
  - ai_organization 向けの実スキーマサンプル

## 拡張 (Sprint 2)

- [ ] **EXT-1: path glob によるスキーマ適用範囲**
  - `[[rules]]` 配列 + `glob = "deep-dive/**/*.md"` で別ルール適用
  - 最初にマッチした rule のみ適用、末尾の default rule がフォールバック
- [ ] **EXT-2: `kb-mcp index` 時の warn 統合** (`[schema] warn_on_index = true`)
- [ ] **EXT-3: watcher (feature 12) 統合** (違反時 stderr warn、60s レート制限)
- [ ] **EXT-4: MCP ツール `validate_schema`**

## 非スコープ
- スキーマ推論 (`schema_infer`)
- JSON Schema 互換
- 自動修正 (`--fix`) - kb-mcp は read-only
- `.txt` への frontmatter 概念導入
- ネスト構造のバリデーション

## スキーマ定義フォーマット案

```toml
# kb-mcp-schema.toml — kb_path 直下に配置

[fields.title]
required = true
type = "string"
min_length = 1

[fields.date]
required = true
type = "string"
pattern = '^\d{4}-\d{2}-\d{2}$'

[fields.topic]
required = true
type = "string"
enum = ["mcp", "rag", "ai", "tooling", "ops"]

[fields.depth]
required = false
type = "string"
enum = ["1", "2", "3"]

[fields.tags]
required = true
type = "array"
min_length = 1
max_length = 10

[options]
allow_unknown_fields = true
```

## CLI 仕様
```
kb-mcp validate [OPTIONS]

  --kb-path <PATH>      KB root
  --schema <PATH>       Schema TOML (既定: <kb_path>/kb-mcp-schema.toml)
  --format <FORMAT>     text | json | github (既定: TTY → text、else json)
  --strict              未知フィールドも違反扱い
  --no-color            text で色無効化
  --fail-fast           最初の違反で exit 1

Exit codes: 0 (no violation), 1 (violations), 2 (schema load error)
```

## 想定 text 出力例
```
kb-mcp validate — 3 files with violations (47 files OK, 2 skipped)

deep-dive/mcp/overview.md
  line 2: title is required but missing
  line 4: date "2026/04/10" does not match pattern ^\d{4}-\d{2}-\d{2}$

deep-dive/rag/intro.md
  line 5: topic "general" is not in enum [mcp, rag, ai, tooling, ops]

ai-news/2026-04-19.md
  line 7: tags is required but array is empty
```

## 技術スタック
| コンポーネント | 選択 | 理由 |
|---|---|---|
| スキーマ言語 | 独自 TOML DSL | 既存 `toml` で完結、JSON Schema は重い |
| 正規表現 | `regex` crate | ~500KB、既存ツリーにあるか要確認 |
| glob (EXT-1) | `globset` | notify-debouncer-full 経由で既依存の可能性 |
| YAML 読取 | 既存 `serde_yaml` | 変更なし |
| TTY 検出 | `std::io::IsTerminal` | std 1.70+、追加依存なし |

## 実装コスト見積もり
| 項目 | LOC | 備考 |
|---|---|---|
| MVP-1 スキーマ型 | 100 | serde + toml |
| MVP-2 バリデータ | 200-250 | 検証 + violation 型 |
| MVP-3 CLI | 150 | walkdir + parser registry 流用 |
| MVP-5 3 format | 150 | |
| テスト | 300 | violation 種別 + format 別 snapshot |
| **MVP 合計** | **~900 + tests** | 1-2 日実装 |
| EXT-1 glob | +150 | |
| EXT-2 index warn | +80 | |
| EXT-3 watcher | +100 | |
| EXT-4 MCP tool | +80 | |

## 想定リスク
| リスク | 影響 | mitigation |
|---|---|---|
| 既存ワークフロー破壊 | 高 | MVP は validate 専用、index 統合は EXT-2 で opt-out 可 |
| TOML の正規表現記述煩雑 | 中 | example で `'^...$'` 推奨明示 |
| regex 依存の階層 C 影響 | 低 | pure Rust、静的、~500KB |
| Frontmatter 構造拡張との整合 | 中 | `allow_unknown_fields = true` 既定 |
| glob 優先順位がわかりにくい (EXT-1) | 中 | 「最初にマッチ」明記 |

## 階層 C 維持検討
| 項目 | 影響 |
|---|---|
| 外部プロセス | なし |
| ランタイム依存 | なし |
| バイナリサイズ増 | ~1MB |
| 既存ユーザ体感 | ゼロ (スキーマなし時) |
| 設定ファイル分離 | `kb-mcp.toml` と別、`kb_path` 直下 |

階層 C を維持したまま追加可能。ただし優先度は低 (TODO.md 自己評価と一致)。

## 確認事項 (実装前)
1. **MVP スコープ (MVP-1〜MVP-7) に絞り、EXT-1〜EXT-4 は別 feature で段階実装**、で良いか?
2. **スキーマファイル名 `kb-mcp-schema.toml` + 配置は kb_path 直下**、で良いか?
3. **MVP は `.md` のみ対象**、`.txt` は skip でよいか?
4. **CLI format は text / json / github の 3 種**、TTY 自動検出で text デフォルト、でよいか?
5. **exit code は 0 (no) / 1 (violations) / 2 (schema load error)** でよいか?
6. **`regex` crate の追加依存 (~500KB)** を受け入れるか?
