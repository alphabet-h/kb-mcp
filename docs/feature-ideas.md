# kb-mcp Feature Ideas

このファイルは **これから検討したい改善候補の vault**。思いついた順に追記し、効果・難易度・制約整合性で優先度を判断する。プラン化が進んだ項目は `docs/plans/feature-NN-xxx.md` に詳細設計を書き、本ファイルから「→ feature-NN」で参照する。

kb-mcp の中核価値 (`CLAUDE.md` / `README.md`): 「Markdown / テキストのディレクトリに 1 バイナリを置くだけで簡易 RAG 化」。この value prop を壊す提案 (外部 LLM 依存・クラウドサービス必須・Python ランタイム必須等) は `⚠️` / `❌` としてマーキングする (`TODO.md` の「借用判断の基本原則」に準拠)。

## 既存 doc との棲み分け

| ファイル | 役割 |
|---|---|
| `TODO.md` | 実装済み履歴 + 直近で plan 化された / 実装間近の候補 |
| `docs/feature-ideas.md` (本ファイル) | まだ議論が始まっていない brainstorm アイデアの vault |
| `docs/plans/feature-NN-xxx.md` | plan 化した feature の詳細設計 |
| `features.json` / `claude-progress.txt` | 実装中・完了の機械可読な状態 |

## 凡例

| 列 | 値 | 意味 |
|---|---|---|
| 効果 | **大** | ユーザ体感が明確に変わる / 中核指標 (検索精度・速度等) が数値で動く |
| 効果 | **中** | 使い勝手が改善される、痛点が減る |
| 効果 | **小** | あると便利だが無くても困らない |
| 難易度 | **低** | 100 LOC 未満 / 半日〜1 日 |
| 難易度 | **中** | 数百 LOC / 数日 |
| 難易度 | **高** | 設計論点あり / 1-2 週間 |
| 難易度 | **極高** | 研究的 / 月単位 / 既存アーキに侵襲的 |
| 制約整合 | **✅** | 単体バイナリ維持 / ランタイム依存なし (value prop 維持) |
| 制約整合 | **⚠️** | 条件付き採用可。備考欄に**どうすれば ✅ に昇格できるか**の条件を必ず明記 |
| 制約整合 | **❌** | 不採用候補。議論の節約のために記録だけ残す |
| 依存 | crate / サービス | 新規に入る依存 (既存で賄えるものは「なし」) |

**⚠️ の頻出パターン**: LLM 呼出が必要だが、(a) Claude Code の PostToolUse hook で pre-index 前処理として走らせる、(b) MCP sampling 経由で client (Claude) に逆問合せする、のいずれかの経路にすればサーバ本体は LLM 非依存を維持できる、というケース。サーバ自身が外部 LLM API を直接叩くなら `❌`。

---

## カテゴリ A: 検索精度の強化

既存の hybrid search (vec + FTS5 + RRF) + reranker パイプラインの上に乗せて retrieval 品質を直接上げる系。

| ID | 名前 | 効果 | 難易度 | 制約 | 依存 | 備考 |
|---|---|:-:|:-:|:-:|---|---|
| A-1 | Contextual Retrieval (Anthropic 手法) | 大 | 中 | ⚠️ | LLM (pre-index) | Anthropic 公表値で検索失敗率 35-50% 改善。各 chunk に「全体文書のどこに属するか」を一文付与して embed。条件: 前処理を Claude Code PostToolUse hook に外出しすればサーバ本体は階層 C 維持 |
| A-2 | Query rewriting / HyDE | 中〜大 | 低〜中 | ⚠️ | — | 質問↔文書の表現ギャップを埋める。条件: client 側 Claude が仮想回答に展開してから `search` を呼ぶか、MCP sampling で逆問合せなら ✅ |
| A-3 | MMR (Maximal Marginal Relevance) | 中 | 低 | ✅ | なし | RRF 後段に diversity 再計算。top-k の類似チャンク重複排除。純 Rust で数十 LOC |
| A-4 | Parent document retriever | 中 | 中 | ✅ | なし | 小 chunk で検索 → 親見出しブロックを返す。`chunks` に `parent_id` 追加 or 見出し階層から算出 |
| A-5 | Semantic chunking | 中 | 中 | ✅ | なし | 見出し固定ではなく段落類似度で切る。既存 embedder で取れるため追加モデル不要。chunk 数増加に注意 |
| A-6 | Binary / Matryoshka quantization | 中 | 中 | ✅ | sqlite-vec 新機能 | embedding バイナリ化で memory 50% 削減、recall 微減。BGE-M3 1024 次元で特に効く |
| A-7 | RRF k / FTS weight の自動チューニング | 中 | 中 | ✅ | なし | D-1 の eval を土台に grid search で最適値選定 |
| A-8 | Late interaction (ColBERT 系) | 中〜大 | 極高 | ⚠️ | 専用モデル + index 再設計 | token-level 類似度で高精度。index サイズ 10-20x、反映コスト大。条件: 個人〜チーム規模の KB ではオーバーキル気味 |

## カテゴリ B: MCP / Claude 特化

MCP サーバとしての独自性を出す系。汎用ベクトル DB ではなく「Claude を相方にした RAG」になる。

| ID | 名前 | 効果 | 難易度 | 制約 | 依存 | 備考 |
|---|---|:-:|:-:|:-:|---|---|
| B-1 | Citations 構造化 | 中 | 低 | ✅ | なし | `search` return に snippet + 文字 offset + 確度を追加。Claude の引用精度 / ハルシネーション抑制に直接寄与 |
| B-2 | MCP resources capability | 中 | 中 | ✅ | なし | rmcp の resources を実装。文書を URI で公開し Claude の「読んだ」記録を残せる |
| B-3 | MCP prompts capability | 小〜中 | 低 | ✅ | なし | 典型問い (要約 / 深堀り / FAQ) をテンプレ化。`kb-mcp.toml` で定義可能に |
| B-4 | MCP sampling で `answer_question` ツール | 中 | 中 | ✅ | rmcp sampling | サーバから client LLM に逆問合せして RAG の G 部分を担う。server 本体は LLM 非依存のまま |
| B-5 | `find_related(chunk_id)` | 中 | 低 | ✅ | なし | 既存 `get_connection_graph` の単一ノード版。軽量ラッパ |
| B-6 | `summarize_topic(topic)` | 中 | 低 | ✅ | なし | `list_topics` を拡張し、関連 chunk 束を返却。client (Claude) に要約素材を供給 |
| B-7 | `compare_documents(a, b)` | 小〜中 | 中 | ✅ | なし | chunk 差分 + 類似度 + 相互引用チェック |
| B-8 | `explain(query, result)` | 小 | 中 | ✅ | なし | RRF 内訳 / FTS vs vec スコア / reranker 寄与を返す。デバッグ用 |
| B-9 | `what_changed(since)` | 中 | 中 | ✅ | なし | watcher 更新履歴を MCP 経由で公開。「最近の変更を踏まえて」系の問いに強い |

## カテゴリ C: 入力多様化 (Parser trait 活用)

feature 20 で入った Parser trait / Registry を実戦投入して対応形式を広げる。現在は `.md` + `.txt` のみ。

| ID | 名前 | 効果 | 難易度 | 制約 | 依存 | 備考 |
|---|---|:-:|:-:|:-:|---|---|
| C-1 | コード対応 (tree-sitter) | 大 | 高 | ✅ | tree-sitter + 言語 grammar crate | 関数・クラス単位 chunk、定義⇔呼出を `connection_graph` に接続。コード検索は RAG の主要ユースケースゆえ効果大 |
| C-2 | PDF 対応 | 大 | 中 | ✅ | `pdf-extract` or `pdfium-render` | text layer のみ (OCR 非対応)。企業文書 / 学術論文の需要が大きい |
| C-3 | `.docx` 対応 | 中 | 中 | ✅ | `docx-rs` / `dotext` | 企業文書。図表は非対応 |
| C-4 | `.pptx` 対応 | 小〜中 | 中 | ✅ | ooxmlsdk 系 | slide 単位 chunk |
| C-5 | HTML readability 抽出 | 中 | 中 | ✅ | `readability` crate | クリップした Web 記事を取込 |
| C-6 | Obsidian vault 互換強化 | 中 | 中 | ✅ | なし | `[[wiki link]]` を正式に `connection_graph` にマップ。`.obsidian/` 除外は feature 20 で済 |
| C-7 | `.kb-mcpignore` (gitignore 互換) | 中 | 低 | ✅ | `ignore` crate | 除外制御の一級市民化。Cursor `.cursorignore` 相当 |
| C-8 | URL クローラ (opt-in) | 小〜中 | 高 | ⚠️ | `reqwest` + `readability` | 条件: 外部ネットワーク接続が入るので opt-in / サブコマンド分離が必須。本体は階層 C 維持 |

## カテゴリ D: 品質・運用基盤

回帰防止と継続改善の基盤。**D-1 を先に入れないとカテゴリ A の精度改善が全部「体感」で盲目的**になる。

| ID | 名前 | 効果 | 難易度 | 制約 | 依存 | 備考 |
|---|---|:-:|:-:|:-:|---|---|
| D-1 | `kb-mcp eval` サブコマンド | 大 | 中 | ✅ | なし | golden query セット (YAML/JSON) で recall@k / MRR / nDCG を測定。A 系改善の前提基盤。モデル変更・RRF k チューニングの回帰防止 |
| D-2 | Query log + relevance feedback | 中 | 中 | ✅ | なし | 検索結果に trace id、thumbs up/down を DB に蓄積。eval データ化 / ranking 学習に転用 |
| D-3 | Duplicate / contradiction detection | 中 | 中 | ✅ | なし | embedding 近傍 (dup) / 逆向き (contradiction) の chunk を警告。`kb-mcp doctor` に統合 |
| D-4 | Index migration (差分再計算) | 小〜中 | 中 | ✅ | なし | モデル変更時の段階的再計算 + 進捗表示 |
| D-5 | Freshness decay | 小 | 低 | ✅ | なし | frontmatter `date` に指数減衰を乗算。`[ranking].freshness_half_life` で設定 |
| D-6 | Prometheus metrics endpoint | 小〜中 | 低 | ✅ | `metrics` crate | HTTP transport 時のみ。QPS / latency / rerank hit rate |
| D-7 | OpenTelemetry tracing | 小 | 中 | ✅ | `tracing-opentelemetry` | 既存 `tracing` の素直な拡張 |
| D-8 | `kb-mcp doctor` サブコマンド | 中 | 低〜中 | ✅ | なし | 壊れた chunk / orphan embedding / schema 違反 / 重複を一括レポート |

## カテゴリ E: GraphRAG / 構造化 (研究的ネタ)

既存 `get_connection_graph` の発展形。難易度が高いが当たれば独自性大。

| ID | 名前 | 効果 | 難易度 | 制約 | 依存 | 備考 |
|---|---|:-:|:-:|:-:|---|---|
| E-1 | Community summaries (Microsoft GraphRAG 風) | 大 | 高 | ⚠️ | LLM (pre-index) | グラフを cluster → community 毎に要約。「このトピック全体を教えて」系に強い。条件: 要約生成を Claude Code hook 経由にすれば ✅。A-1 の前処理パイプラインを流用可能 |
| E-2 | Entity / relation extraction (NER + RE) | 中 | 高 | ⚠️ | LLM or 軽量 NER | Rust 純 NER は精度低め。条件: LLM なら hook 経由で ✅ |
| E-3 | Graph 可視化 (`--format svg` / `--format dot`) | 小〜中 | 低 | ✅ | `layout-rs` 等 | 既存 `graph` CLI の出力を SVG / DOT に拡張。人間向けナビゲーション改善 |

## カテゴリ F: UX 小粒

効果は中規模だが実装が軽く、触感をすぐ改善できる系。束で 1 スプリントに収まる。

| ID | 名前 | 効果 | 難易度 | 制約 | 依存 | 備考 |
|---|---|:-:|:-:|:-:|---|---|
| F-1 | "I don't know" 判定 | 中 | 低 | ✅ | なし | top score が閾値未満なら空返却 or `low_confidence` フラグ。Claude の hallucination 抑制 |
| F-2 | Snippet highlighting | 小 | 低 | ✅ | なし | マッチ位置を `<mark>` でラップして返却 |
| F-3 | Path / tag / date filter | 中 | 低 | ✅ | なし | `search` に `path_glob` / `tags` / `date_range` パラメータ追加 |
| F-4 | `-term` exclusion 構文 | 小 | 低 | ✅ | なし | FTS5 の NOT に写像 |
| F-5 | REPL mode (`kb-mcp repl`) | 小 | 低 | ✅ | `rustyline` | 対話検索シェル |
| F-6 | Web UI (`/ui` 同梱) | 中 (公開用途) | 中 | ✅ | axum + 静的 HTML | HTTP transport で検索 + preview + graph 可視化 |
| F-7 | `list_topics` 階層ツリー化 | 小 | 低 | ✅ | なし | path segment ベースのツリー構造を返す |
| F-8 | VS Code / Obsidian 拡張 | 中 | 高 | ✅ (本体無影響) | 各エディタ SDK | 編集中ノート近傍を sidebar 表示。本体は CLI / MCP のまま |

## カテゴリ G: 他ツールからの輸入候補

類似ツールに実装があり、kb-mcp にも欲しいもの。出典を明記。

| ID | 名前 | 出典 | 効果 | 難易度 | 制約 | 依存 | 備考 |
|---|---|---|:-:|:-:|:-:|---|---|
| G-1 | Router / Sub-question engine | LlamaIndex | 中〜大 | 高 | ⚠️ | — | クエリ分解で多段検索。条件: MCP sampling で client に分解させれば ✅ |
| G-2 | Factual consistency score | Vectara | 中 | 高 | ⚠️ | 専用モデル DL | 回答↔出典の整合性スコア。条件: 既存 cross-encoder で近似可 |
| G-3 | Smart Connections sidebar | Obsidian plugin | 中 | 高 | ✅ | エディタ SDK | F-8 の具体形 |
| G-4 | `.cursorignore` 相当 | Cursor / Codeium | 中 | 低 | ✅ | — | C-7 と同じ (実質 alias) |
| G-5 | Incremental reindex progress UI | Cursor | 小〜中 | 低 | ✅ | なし | watcher ログを構造化 + tty で進捗表示 |
| G-6 | Ranking formula DSL | Algolia | 小 | 高 | ✅ | — | 設定式で weight を書ける。YAGNI 気味、現状 RRF + reranker でほぼ足りる |

---

## 優先度ピック (初版 2026-04-19)

**「次に手を付けるなら」個人的ベット順**

1. **D-1 (`kb-mcp eval`)** — 他の精度改善 (A 系) 全てがこの基盤に依存する。先入れしないと以降が盲目的になる
2. **B-1 + F-1 + F-3 バンドル (軽量 UX 束)** — `search` ツールの signature 拡張でまとめて入る 2-3 日 feature。Citations / I don't know / filter は相補的
3. **A-3 + A-4 (MMR + Parent retriever)** — 純 Rust で足せる retrieval 改善。D-1 の eval で効果測定しながら tune
4. **A-1 (Contextual Retrieval)** — 効果大の本命。D-1 + Claude Code PostToolUse hook の整備後に着手。pre-index 前処理パイプラインは E-1 と共通化できる
5. **C-2 (PDF)** — ユースケース横展開の王道。C-1 (tree-sitter) より着手コスト低め
6. **B-4 + B-5 + B-6 (MCP ツール束)** — MCP サーバとしての独自性。A 系が落ち着いたら
7. **E-1 (Community summaries)** — A-1 の前処理基盤を流用できる段階に入ってから

**意識的に後回し**

- **A-8 (ColBERT)** — 個人〜チーム規模の KB でペイオフが薄い。index サイズ 10-20x 増は割に合わない
- **G-6 (Ranking formula DSL)** — YAGNI。現状の RRF + reranker + `kb-mcp.toml` でチューニング需要はほぼ賄える
- **E-2 (NER + RE)** — Rust 純 NER は精度低、LLM 利用は基盤整備が先。ペイオフが読みにくい

---

## 更新ルール

- 新しいアイデアは下の **ペンディング** 欄に自由記述で追記。ID 付与は後でよい
- 議論が進み「やるかもしれない」段階になったら ID (カテゴリ-番号) を振って上の該当カテゴリ表に移す
- `docs/plans/feature-NN-xxx.md` として plan 化したら `TODO.md` の「未実装: 機能追加」に昇格移動。本ファイル側は「→ feature-NN」の参照リンクだけ残すか、削除する
- 実装完了したら `TODO.md` の「実装済み」表と `features.json` / `claude-progress.txt` を更新
- 優先度ピックは定期的に見直す (リリース節目 or 四半期)。見直し時は日付付きセクションを追記し、前のピックは残す (判断の履歴として)

## ペンディング (未整理のアイデア)

- _(新しいアイデアは ここに自由記述で追記)_
