# feature 18 — HTTP (Streamable HTTP) トランスポート

> planner エージェント (harness-kit:planner) が 2026-04-19 に生成した仕様書。
> 実装前の合意形成用ドラフト。

## 概要
`kb-mcp serve --transport http --port 3100` で複数クライアント同時接続可能な MCP サーバを起動できるようにする。既定は現行通り stdio (後方互換)。rmcp 1.x の `transport-streamable-http-server` を採用。axum 0.8 でマウント。

## 対象ユーザー
- ローカル LAN 内で 1 つの kb-mcp インスタンスを複数クライアント (Claude Code / Cursor / 自前スクリプト) から共有
- kb-mcp をコンテナ化して他サービスから HTTP 経由で叩く
- integration test で `reqwest` で MCP エンドポイントを検証

## 前提の棚卸し
| 既存資産 | feature 18 での扱い |
|---|---|
| `KbServer` の Arc 化 (feature 12 完了) | **そのまま流用**。reranker のみ `Arc<Mutex<Option<Reranker>>>` に変更 |
| `#[tool_router]` impl | トランスポート非依存で変更なし |
| `run_server` | 内部を `run_stdio` / `run_http` に分岐、外側の signature は transport 引数追加のみ |
| `watcher::run_watch_loop` | `tokio::spawn` で並走、transport 非依存 |

## MVP (Sprint 1) — Streamable HTTP のみ、認証なし、stdio 既定維持

- [ ] **F18-1: Cargo.toml に rmcp HTTP features + axum 追加**
  - `rmcp = { version = "1", features = ["server", "transport-io", "transport-streamable-http-server", "macros"] }`
  - `axum = "0.8"`
  - サイズ増分 < 3 MB

- [ ] **F18-2: CLI スキーマ拡張**
  - `--transport <stdio|http>` (既定 stdio)
  - `--bind <SOCKETADDR>` (既定 127.0.0.1:3100)
  - `--port <u16>` (既定 3100)
  - `kb-mcp serve` 引数なしで従来の stdio

- [ ] **F18-3: `[transport]` config section**
  ```toml
  [transport]
  kind = "http"   # "stdio" (既定) | "http"

  [transport.http]
  bind = "127.0.0.1:3100"
  ```
  - CLI > config > 既定
  - `[transport.http]` だけ書かれた場合は `"http"` と解釈 (糖衣)

- [ ] **F18-4: `src/transport/` モジュール新設**
  - `mod.rs` (Transport enum + 解決ヘルパ)
  - `stdio.rs` (現行の stdio 部切り出し)
  - `http.rs` (StreamableHttpService + axum)

- [ ] **F18-5: `run_server` 内部分岐 + service factory**
  - `KbServerShared` 軽量ハンドルを介し、factory から Arc clone で KbServer 生成
  - reranker フィールドを `Arc<Mutex<Option<Reranker>>>` に変更
  - rmcp `StreamableHttpService::new(factory, LocalSessionManager::default().into(), Default::default())`
  - axum `Router::new().nest_service("/mcp", svc)` + `TcpListener::bind + axum::serve`
  - `.with_graceful_shutdown(ctrl_c)` で SIGINT 処理

- [ ] **F18-6: mount path `/mcp` 固定 + `/healthz`**
  - `/healthz` は 200 `"ok"` のみ
  - path 設定化は F18-15 で

- [ ] **F18-7: watcher 並走**
  - feature 12 の tokio::spawn 構造をそのまま。HTTP serve の後に watcher handle.abort()

- [ ] **F18-8: integration test (2 並列 search)**
  - `tests/http_transport.rs` (#[ignore])
  - ephemeral port で spawn → initialize → 2 並列 search
  - reqwest + 生 JSON-RPC (rmcp の streamable_http_client を dev-dep 追加しても可)

- [ ] **F18-9: エラーハンドリング**
  - bind 失敗時の親切なエラーメッセージ
  - 無効な `--bind` は clap パーサで reject

- [ ] **F18-10: README / CLAUDE.md 追記**
  - `--transport http` サンプル
  - `.mcp.json` の `"type": "http"` サンプル
  - security note (0.0.0.0 bind + 認証なしは NG)

## 拡張 (需要次第で段階実装)

- [ ] **F18-11: Bearer token 認証** (`KB_MCP_TOKEN` / `[transport.http].token`)
  - loopback 以外 + token 未設定は起動拒否 (fail-safe)
- [ ] **F18-12: CORS** (`tower-http`)
- [ ] **F18-13: TLS 終端** (`axum-server` rustls)
- [ ] **F18-14: メトリクス** (`/metrics` + TraceLayer)
- [ ] **F18-15: mount path 設定化**
- [ ] **F18-16: 同時接続 limit / rate limit** (`tower::limit::ConcurrencyLimit`)

## 非スコープ
- SSE 専用トランスポート (Streamable HTTP に内包)
- WebSocket (MCP 標準にない)
- マルチテナント / KB 切替
- HTTP 経由の書き込み系 API (階層 C 違反)
- gRPC

## 技術スタック
| クレート | 選定理由 |
|---|---|
| `rmcp` `transport-streamable-http-server` | 公式 SDK のリファレンス実装 |
| `axum = "0.8"` | rmcp 1.x サンプルと同一 |
| `tower-http` (F18-12/14) | CORS / trace |
| `axum-server` (F18-13) | rustls TLS |

## 設計上の解

1. **トランスポート選択**: Streamable HTTP のみ (SSE は内包済、将来必要なら別トランスポート)
2. **Mutex<_> ボトルネック**: MVP では「正しく直列化される」が目標。10 qps 程度まで。顕在化したら F18-16 + RwLock 検討
3. **KbServer 共有**: reranker のみ Arc 化 (他は既に Arc)。factory は全 Arc clone で軽量生成
4. **認証**: MVP は無し。既定 bind=127.0.0.1 で loopback 限定。0.0.0.0 への opt-in + token 必須化は F18-11
5. **CORS**: MVP 無し (Claude Code / Cursor は server-to-server で不要)
6. **後方互換**: `kb-mcp serve` 引数なしで stdio 維持
7. **テスト**: integration は `#[ignore]` (embedding DL 前提)
8. **graceful shutdown**: `axum::serve().with_graceful_shutdown(ctrl_c)`
9. **mount**: `/mcp` 固定 (Claude Code / Cursor 互換)

## API / データ契約

```rust
// src/transport/mod.rs
#[derive(Debug, Clone)]
pub enum Transport {
    Stdio,
    Http { addr: std::net::SocketAddr },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum TransportKind { Stdio, Http }

impl Transport {
    pub fn resolve(
        cli_transport: Option<TransportKind>,
        cli_bind: Option<std::net::SocketAddr>,
        cli_port: Option<u16>,
        cfg: Option<&TransportConfig>,
    ) -> anyhow::Result<Self>;
}

// src/server.rs: run_server に transport: Transport を追加
// src/config.rs
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TransportConfig {
    pub kind: Option<TransportKindConfig>,
    pub http: Option<HttpTransportConfig>,
}
```

## テスト観点
| 観点 | レベル |
|---|---|
| `Transport::resolve` 既定で Stdio | unit |
| CLI 優先規則 | unit |
| bind 解決 (port のみ / bind のみ) | unit |
| `[transport.http].bind` パース | unit |
| HTTP 起動 + initialize + tools/list | `#[ignore]` integration |
| 2 並列 search | `#[ignore]` integration |
| ポート使用中で exit 1 | integration |
| watcher + HTTP 共存 | 手動 |

## 実装コスト見積もり
| タスク | 工数 |
|---|---|
| F18-1〜F18-10 (MVP) | 6.5 人日 |
| F18-11 Bearer token | +1.0 |
| F18-12 CORS | +0.5 |
| F18-13 TLS | +1.0 |
| F18-14 メトリクス | +1.0 |

## 想定リスク
| リスク | 深刻度 | Mitigation |
|---|---|---|
| rmcp 1.x minor breaking | 中 | Cargo.lock 固定、公式 example と揃える |
| factory 内で embedder 重複ロード | 高 | factory は Arc clone のみ、重いリソースは起動時 1 回ロード |
| Mutex contention | 中 | 直列化前提で運用、F18-16 で上限制御検討 |
| 0.0.0.0 bind + 認証なし運用ミス | 高 | README 警告 + 将来 F18-11 で reject |
| ポート衝突メッセージ不親切 | 低 | F18-9 で親切メッセージ |
| integration test port flaky | 中 | `127.0.0.1:0` ephemeral port |
| rmcp + axum のバージョン衝突 | 中 | Cargo.lock、bisect |

## 確認事項 (実装前)
1. **MVP (F18-1〜F18-10) で先に merge し、認証 / TLS / CORS / メトリクスは別 feature で段階実装**、で良いか?
2. **既定 bind を `127.0.0.1:3100`** でよいか? (0.0.0.0 は opt-in)
3. **mount path `/mcp` 固定** でよいか? (設定化は F18-15)
4. **integration test は `#[ignore]`** (embedding DL 前提) でよいか?
5. **`KbServer::reranker` を `Arc<Mutex<Option<Reranker>>>`** に変更して問題ないか? (feature 10 テスト回帰リスク)
