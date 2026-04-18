# feature 12 — Live Sync (notify による file watcher + 増分再インデックス)

> planner エージェント (harness-kit:planner) が 2026-04-19 に生成した仕様書。
>
> **ユーザ決定事項 (2026-04-19)**:
> 1. 既定 **on** で進める
> 2. **拡張 (F12-8 frontmatter-only + F12-9 self-heal) も含める**
> 3. **feature 20 (非 Markdown 対応) を先に実装** してから feature 12 着手。watcher の path filter は feature 20 の `parser::Registry::extensions()` を流用

## 概要
`kb-mcp serve` 中に `--kb-path` を file watcher で監視し、Markdown ファイルの作成/更新/削除/rename を検知して該当ファイルだけを増分再インデックスする。PostToolUse hook では拾えない手動編集 (エディタ保存・git pull・外部スクリプト) のギャップを埋める。

## 対象ユーザー
- knowledge-base を手動で編集しながら同時に Claude Code / Cursor から検索するユーザー
- git pull / sync で複数ファイルが一度に書き換わる運用をしているチーム
- PostToolUse hook を構成していない軽量セットアップ

## 前提の棚卸し (既存コードの再利用判断)
| 既存資産 | 役割 |
|---|---|
| `indexer::rebuild_index` の per-file ループ (L182-252) | hash 変化検出 → markdown parse → embed → upsert のパスが既に per-file 単位 |
| `db.upsert_document` / `db.delete_document` / `db.rename_document` | 個別 CRUD 完備。新 API 不要 |
| `db.get_document_hash` | 「変更なし」スキップ判定に再利用可能 |
| `markdown::parse_with_excludes` + `quality::chunk_quality_score` | チャンク生成もそのまま |
| `KbServer { db: Mutex<Database>, embedder: Mutex<Embedder>, ... }` | 既存の Mutex 経由で serve と共有できる |

**結論**: `indexer` に `reindex_single_file` / `deindex_single_file` 2 関数を新設して既存ロジックを薄く切り出すのが最小侵襲。`rebuild_index` 本体は変更不要。

---

## 機能一覧

### MVP (Sprint 1)

- [ ] **F12-1: `notify` 依存追加**
  - `Cargo.toml` に `notify = "8"` および `notify-debouncer-full = "0.6"` を追加
  - 理由: debouncer-full は rename のペア化 (`from`/`to`) と重複イベントの集約を公式サポート。mini は debounce のみでペア化なし
  - 完了条件: `cargo build --release` が成功し、バイナリサイズ増分 < 1 MB

- [ ] **F12-2: `src/watcher.rs` 新設**
  - 責務: `notify-debouncer-full` を tokio の `mpsc::UnboundedChannel` に橋渡し、kb_path を recursive watch、Markdown 以外と `.obsidian/`・`.kb-mcp.db` を path filter で除外
  - 公開 API は `pub async fn run_watch_loop(state: WatcherState) -> Result<()>` 1 本。`WatcherState` は `Arc<KbServer>` 相当の共有ハンドルと `WatchConfig` を持つ
  - debounce は notify-debouncer-full の内蔵 tick を利用 (CLI/config の `debounce_ms` を反映、既定 500ms)
  - 完了条件: 単独関数の単体テストで、一時 dir に `.md` を書き込むとイベントが tokio channel で受信される (ignored テスト可)

- [ ] **F12-3: 増分再インデックス API を indexer に追加**
  - `pub fn reindex_single_file(db, embedder, kb_path, rel, exclude_headings) -> Result<SingleResult>`
    - 中身は既存 `rebuild_index` の L182-252 の per-file ブロックを抽出
    - hash 比較 → unchanged なら `SingleResult::Unchanged`、変更あれば upsert + insert_chunk
    - `SingleResult` は `Unchanged | Updated { chunks: u32 } | Skipped { reason }` の enum
  - `pub fn deindex_single_file(db, rel) -> Result<bool>`
    - `db.delete_document` 薄ラッパ、存在しなければ `Ok(false)`
  - `pub fn rename_single_file(db, embedder, kb_path, old_rel, new_rel, exclude_headings) -> Result<RenameOutcome>`
    - 既定: `db.rename_document(old, new)` → new_rel の hash を読み直し DB 側と比較 → 差があれば `reindex_single_file` で本文更新も行う (frontmatter-only 変更を含む編集+rename の複合イベントに対応)
  - 完了条件: `cargo test indexer::tests::test_reindex_single_file_*` がパス。4 ケース (新規追加 / 既存更新 / unchanged skip / 空ファイル skip) を網羅

- [ ] **F12-4: watcher → 増分 API の dispatch**
  - debounced event kind を以下にマッピング:
    - `Create` / `Modify` → `reindex_single_file`
    - `Remove` → `deindex_single_file`
    - `Rename { from, to }` のペア → `rename_single_file` (両方 kb_path 内の場合のみ。片側のみなら create/remove として扱う)
  - 絶対パスを kb_path 相対に変換し、`indexer::rebuild_index` と同じ forward-slash キーに正規化
  - イベント処理は tokio::spawn で直列化 (Mutex<Database> の contention を抑える)
  - 完了条件: `serve` 起動中に `.md` を touch すると 1 秒以内に `indexed: xxx` の stderr ログが出る (手動確認)

- [ ] **F12-5: CLI / config スキーマ**
  - CLI (Serve のみ): `--watch` / `--no-watch` (flag 同居の排他グループ)、`--debounce-ms <u64>`
  - `kb-mcp.toml` に `[watch]` セクション:
    ```toml
    [watch]
    enabled = true           # 既定: true
    debounce_ms = 500        # 既定: 500
    ```
  - 優先順位は既存規約通り `CLI > config > 既定値`
  - `--no-watch` が指定されたら config の `enabled=true` を上書きして無効化
  - 完了条件: `config::tests::test_watch_section_parses` がパス、`kb-mcp.toml.example` に `[watch]` セクション追記

- [ ] **F12-6: `run_server` への統合**
  - `KbServer` を `Arc<KbServer>` 化し、`service.waiting()` と `watcher::run_watch_loop` を `tokio::select!` で並走
  - watcher タスクが panic/error で落ちても MCP サーバは継続させる
  - `enabled=false` の時は watcher は起動しない (pre-feature-12 と完全に同じ挙動)
  - 完了条件: `--no-watch` で従来挙動を維持、`--watch` 時に serve ログに `watcher started (debounce 500ms)` が出る

- [ ] **F12-7: README 追記**
  - 「Keeping the index fresh via PostToolUse hook」の直後に「Live sync via file watcher」節
  - watcher と PostToolUse hook は排他ではなく併用可 (両方が同じ Mutex<Database> を叩くので Rust 側で直列化される) を明記
  - 完了条件: README に watcher 節がある

### 拡張 (Sprint 2、必要なら)

- [ ] **F12-8: frontmatter-only 編集の最適化**
  - `reindex_single_file` の入口で旧 content の chunk テキスト列と新 content の chunk テキスト列を比較し、チャンク本文が完全一致なら embedding 再計算をスキップして document 行 (title/tags/date/topic) と FTS のメタのみ更新
  - 効果: 大規模 `.md` で frontmatter だけ触ったときの再 embed (BGE-M3 で数百 ms) を回避

- [ ] **F12-9: watcher 再起動 (self-healing)**
  - notify の OS 側ハンドル喪失 (ディレクトリ削除/再作成等) 時に backoff 再購読

### 非スコープ

- `.txt` / `.rst` 等の非 Markdown 拡張子対応 (feature 20 の範疇)
- watcher の可観測化 MCP ツール
- Windows 網共有 / SMB 上の kb_path 対応
- watcher 経由の full `backfill_fts` / `backfill_quality` 再実行 (serve 起動時に既に 1 度走る)

---

## 技術スタック (新規追加分)

| クレート | 選定理由 |
|---|---|
| `notify = "8"` | 公式で最もメンテナンスされている cross-platform watcher |
| `notify-debouncer-full = "0.6"` | rename のペア化 (`from`/`to`) をサポート。`mini` はペア化なし |

`tokio` は既に `features = ["full"]` なので channel / spawn 追加不要。

---

## 設計上の論点と解

1. **既定 on/off**: **既定 on** を推奨 (`[watch].enabled = true`)
   - kb-mcp の価値提案は「ディレクトリに 1 バイナリ置くだけで RAG 化」。watcher off がデフォルトだと手動編集が永久に stale する不整合が起きる
   - opt-out は `--no-watch` / config の `enabled = false` で明示的に可能
   - 懸念 (メモリ常駐) は notify-debouncer-full + tokio task で数 MB 程度。BGE-M3 (~2.3 GB) に比べ無視できる

2. **CLI フラグ**: `--watch` / `--no-watch` + `--debounce-ms`。`ArgAction::SetTrue` を両方に付けて post-parse で排他チェック

3. **kb-mcp.toml**: `[watch]` セクション (`quality_filter` と同じ構造)。`deny_unknown_fields` 準拠

4. **実装位置**: **`src/watcher.rs` 新設**。`server.rs` に混ぜると ServerHandler の関心ごとが膨らむ

5. **tokio 統合**: notify-debouncer-full は `std::sync::mpsc::Sender` 受け取りを公式サポート。`std::thread::spawn` で受信スレッドを立ててそこから `tokio::sync::mpsc::UnboundedSender` に push する bridge パターンが最小

6. **増分再インデックスの粒度**: 上記 F12-3 の 3 関数。`rebuild_index` の per-file ブロックを切り出すだけなのでロジック重複ゼロ

7. **エラー耐性**: watcher タスク内の event 処理エラーは `eprintln!("watcher: {}", e)` して次のイベントに進む (silent drop はしない)。watcher 自体が死んだ場合は MCP サーバは生かして stderr にエラーログ

8. **PostToolUse hook との競合**: 問題なし。両者とも `KbServer` の `Mutex<Database>` / `Mutex<Embedder>` を経由するため Rust 側で直列化される。hook 経由の `rebuild_index` は全走査 (hash 未変化は skip) なので冪等

9. **frontmatter-only 変更**: F12-8 で対応。MVP では常に再 embed

---

## API / データ契約

### 新規公開 API (indexer.rs)
```rust
pub enum SingleResult {
    Unchanged,
    Updated { chunks: u32 },
    Skipped { reason: &'static str },
}

pub fn reindex_single_file(
    db: &Database,
    embedder: &mut Embedder,
    kb_path: &Path,           // canonicalized
    rel: &str,                // forward-slash, kb_path 相対
    exclude_headings: Option<&[String]>,
) -> Result<SingleResult>;

pub fn deindex_single_file(db: &Database, rel: &str) -> Result<bool>;

pub fn rename_single_file(
    db: &Database,
    embedder: &mut Embedder,
    kb_path: &Path,
    old_rel: &str,
    new_rel: &str,
    exclude_headings: Option<&[String]>,
) -> Result<RenameOutcome>; // Renamed | RenamedAndReindexed
```

### 新規公開 API (watcher.rs)
```rust
pub struct WatchConfig {
    pub enabled: bool,
    pub debounce_ms: u64,
}

pub async fn run_watch_loop(
    kb_path: PathBuf,                    // canonicalized
    config: WatchConfig,
    db: Arc<Mutex<Database>>,
    embedder: Arc<Mutex<Embedder>>,
    exclude_headings: Option<Vec<String>>,
) -> Result<()>;
```

### Config 追加 (config.rs)
```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchConfig {
    #[serde(default = "default_watch_enabled")]
    pub enabled: bool,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
}
```

---

## テスト観点

| 観点 | レベル | 備考 |
|---|---|---|
| `reindex_single_file` 新規追加 | unit | 既存 indexer テストと同じ tempdir パターン |
| `reindex_single_file` hash 一致 skip | unit | embedder を spy 化して call 回数 0 を確認 |
| `deindex_single_file` 存在しない path | unit | `Ok(false)` |
| `rename_single_file` 単純 rename | unit | 旧 path が消え、新 path で get_document_hash が取れる |
| `rename_single_file` rename + content 変更 | unit | RenamedAndReindexed 返却 |
| watcher path filter | unit | `.md` 以外、`.obsidian/` 配下、`.kb-mcp.db` はイベント無視 |
| watcher end-to-end | `#[ignore]` integration | 実 OS watcher を使うので CI flaky 対策として ignore |
| config `[watch]` parse | unit | 既存 test_parse_full_config と同じパターン |
| 既定 on の後方互換 | integration | `--no-watch` で pre-feature-12 と同一ログ出力 |
| PostToolUse hook との同時走行 | 手動 | rebuild_index 呼び出し中に watcher が発火しても deadlock しない |

---

## 実装コスト見積もり

| タスク | 工数 (人日) | 内訳 |
|---|---|---|
| F12-1 依存追加 + 疎通 | 0.25 | Cargo.toml、`cargo build` 確認 |
| F12-2 watcher.rs | 1.0 | bridge スレッド + tokio channel + path filter + debouncer 接続 |
| F12-3 indexer の 3 API 抽出 | 0.75 | 既存ロジック切り出し + テスト |
| F12-4 dispatch | 0.5 | enum 分岐 + path 正規化 |
| F12-5 CLI / config | 0.5 | 既存 pattern に従うだけ |
| F12-6 run_server 統合 | 0.5 | Arc 化 + tokio::select! |
| F12-7 README | 0.25 | |
| 手動検証 (Windows / Linux の実機疎通) | 0.5 | |
| evaluator 指摘対応バッファ | 0.5 | |
| **合計 (MVP)** | **4.75 人日** | |
| F12-8 frontmatter-only 最適化 | +1.0 | 拡張 |
| F12-9 watcher self-heal | +0.75 | 拡張 |

---

## 想定リスクと Mitigation

| リスク | 影響 | Mitigation |
|---|---|---|
| notify の rename 検出が OS 間で不安定 (特に Windows の「delete→create」に分解されるケース) | rename が reindex + deindex の 2 イベントになり、embedding 再計算が発生 | notify-debouncer-full の `FileIdMap` がこれを吸収。取りこぼしケースは単独 create/remove として処理 → 結果的に正しく同期 (性能だけ劣化)。feature 11 の content-hash ベース rename 検出は次回 `rebuild_index` で拾われる |
| エディタの保存が複数イベントを生む (vim の backup + rename 等) | debounce 内なら 1 回に集約 | debounce_ms 既定 500ms。config で調整可能 |
| watcher task の panic で MCP サーバ道連れ | serve 不能 | `tokio::select!` で並走し、watcher 側を `catch_unwind` + ログに逃がす |
| 大量ファイル同時変更 (git pull 100 ファイル) で embedder を並列に叩いて OOM | メモリ急増 | watcher dispatch を `tokio::spawn` で直列化 (Mutex<Embedder> が自然に直列化) |
| Mutex<Database> を watcher と MCP search が取り合い、応答性が劣化 | search レイテンシ増 | 1 ファイルの reindex は数百 ms 以下、debounce 窓で集約されるので実用上許容 |
| `.kb-mcp.db` 自体への書き込みで watcher が自発的に発火 (無限ループ懸念) | CPU 燃焼 | `.kb-mcp.db` は kb_path の**親**に作られるので `kb_path` を recursive watch しても元々入らない。念のため path filter で `.kb-mcp.db*` を弾く二重防御 |
| WSL / ネットワークドライブ / SMB で inotify が動かない | watcher が無音で死ぬ | MVP では PollWatcher fallback は使わず、ローカルディスク利用を README に注記 |
| notify-debouncer-full の API が未熟でバージョン上がりで breaking change | アップデート時に手戻り | Cargo.lock コミット済み。意識的なアップデート時のみ影響 |

---

## 参考ファイル
- `src/indexer.rs` (L105-280: per-file ループが reindex_single_file の種)
- `src/server.rs` (L585-625: run_server に tokio::select! を入れる場所)
- `src/db.rs` (L319-538: upsert / delete / rename 既存 CRUD)
- `src/main.rs` (L33-52, L199-237: Serve サブコマンドと CLI → run_server 渡し)
- `src/config.rs` (`[watch]` セクション追加先)
- `kb-mcp.toml.example` (watch 例の追記)
- `Cargo.toml` (依存追加)
- `features.json` (id=12 の verification 文言改訂対象)

---

## 確認事項 (実装前に回答必要)

1. **既定 on で進めてよいか?** (opt-out は `--no-watch` / `[watch].enabled=false`)
2. **MVP スコープ (F12-1〜F12-7) に絞り、F12-8 / F12-9 は別機会でよいか?**
3. **feature 20 (非 Markdown) を先に済ませて拡張子対応付きで作る** vs **feature 12 を先に Markdown 限定で作り、feature 20 で watcher の拡張子フィルタを拡張** のどちらか?
