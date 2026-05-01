# デプロイレシピ — 個人マシン HTTP デーモン (loopback)

> **English**: [README.md](./README.md)

kb-mcp デーモンを `127.0.0.1:3100` の OS 常駐サービスとして 1 本立て、
このマシン上のすべての Claude Code / Cursor セッション (複数プロジェクトを
並行で開いていても) を Streamable HTTP で接続させる。プロジェクトごとに
stdio kb-mcp 子プロセスを spawn する代わりに、共有デーモン 1 本で済ます。

> **⚠️ loopback only**。本レシピは `127.0.0.1` 固定。kb-mcp に組み込み認証
> は無い。LAN 公開する場合は [`intranet-http/`](../intranet-http/) に切り替え、
> 前段に認証付きリバースプロキシを置くこと。

## なぜこのレシピがあるか

stdio (`personal/`) はシンプルで daemon 管理不要なので可能なら優先する。
ただし stdio には kb-mcp プロセスごとのスケール制約がある:

```
        Claude Code (project A) ─→ kb-mcp 子 #1 ─→ embedder, DB, watcher
        Claude Code (project B) ─→ kb-mcp 子 #2 ─→ embedder, DB, watcher
        Claude Code (project C) ─→ kb-mcp 子 #3 ─→ embedder, DB, watcher
                                                  └─→ N × ~2.3 GB peak (BGE-M3)
```

新しいエディタウィンドウが同じ KB を開くたびに embedder ロード代を再支払い
(BGE-M3 で peak ~2.3 GB)、同一ディレクトリ上で watcher がもう 1 本立ち上がり、
あるプロジェクトで `kb-mcp index --force` を回しているところに別プロジェクトが
read mutex を保持していると DB 競合のリスクもある。

`personal-http/` はこれを 1 プロセスに集約する:

```
        Claude Code (project A) ─┐
        Claude Code (project B) ─┼─→ HTTP /mcp ─→ kb-mcp デーモン 1 本
        Claude Code (project C) ─┘
```

embedder 1 個 / DB connection 1 本 / watcher 1 本で、エディタを何セッション
開いても増えない。代償は OS service unit を 1 個維持すること。

## 想定環境

- 1 ユーザ、1 台のマシン (ノート / デスクトップ)
- 同じ KB に対して 2 つ以上の Claude Code (or Cursor / Zed 等) セッション
  を並行で開く運用。1 セッションしか開かないなら [`personal/`](../personal/)
  の方がシンプル
- ローカルディスク上の KB (NAS マウントは [`nas-shared/`](../nas-shared/))

## 同梱ファイル

| ファイル | 役割 |
| --- | --- |
| [`kb-mcp.toml`](./kb-mcp.toml) | デーモン設定 — HTTP transport、`127.0.0.1:3100` 固定、watcher on |
| [`.mcp.json`](./.mcp.json) | クライアント側 `.mcp.json`。HTTP transport directive。各プロジェクト root か `~/.config/claude/.mcp.json` に配置 |
| [`kb-mcp.user.service`](./kb-mcp.user.service) | Linux: systemd **user** unit (root 不要) |
| [`com.kb-mcp.plist`](./com.kb-mcp.plist) | macOS: launchd LaunchAgent (per-user) |
| [`kb-mcp-task.xml`](./kb-mcp-task.xml) | Windows: Task Scheduler XML (AT_LOGON、admin 不要) |

## セットアップ

OS 別の launcher テンプレを編集 → install → 各プロジェクトの `.mcp.json` を
デーモンに向ける、の順。

### Step 1 — `kb-mcp.toml` を配置してパス書換え

```bash
mkdir -p ~/kb-mcp
cp examples/deployments/personal-http/kb-mcp.toml ~/kb-mcp/kb-mcp.toml
$EDITOR ~/kb-mcp/kb-mcp.toml   # kb_path = "/絶対パス/notes" に書換え
```

`Config::discover()` は launcher の WorkingDirectory から `kb-mcp.toml` を
拾う (priority 2 — CWD)。各 launcher テンプレで `WorkingDirectory=~/kb-mcp`
(or OS 相当) を設定済なので、自然にディスカバされる。

### Step 2 — index を 1 回作る

stdio `personal/` と同じ手順。launcher が使う作業ディレクトリと同じ場所で
index を作れば `.kb-mcp.db` がデーモンの期待する位置に置かれる。

```bash
cd ~/kb-mcp
kb-mcp index   # toml が --kb-path / --model を提供する
```

### Step 3 — OS launcher を install

#### Linux (systemd user unit)

```bash
mkdir -p ~/.config/systemd/user/
cp examples/deployments/personal-http/kb-mcp.user.service ~/.config/systemd/user/kb-mcp.service
$EDITOR ~/.config/systemd/user/kb-mcp.service   # ExecStart のパスを ~/.cargo/bin/kb-mcp 以外なら更新
systemctl --user daemon-reload
systemctl --user enable --now kb-mcp.service
journalctl --user -u kb-mcp.service -f          # ログ tail
```

ログアウト後もデーモンを生かしておきたい場合:

```bash
sudo loginctl enable-linger "$USER"
```

#### macOS (launchd LaunchAgent)

```bash
mkdir -p ~/Library/LaunchAgents
cp examples/deployments/personal-http/com.kb-mcp.plist ~/Library/LaunchAgents/com.kb-mcp.plist
$EDITOR ~/Library/LaunchAgents/com.kb-mcp.plist   # ProgramArguments と WorkingDirectory を更新
launchctl load ~/Library/LaunchAgents/com.kb-mcp.plist
tail -f /tmp/kb-mcp.out /tmp/kb-mcp.err
```

#### Windows (Task Scheduler, AT_LOGON)

選択肢は 2 つ。**推奨は PowerShell cmdlet 経由** — 旧来の
`schtasks /Create /XML` で踏みやすい罠 2 件 (Interval の最小値、
Principal 解決の "アクセス拒否") を回避できる。

**推奨: `Register-ScheduledTask` (PowerShell)**

```powershell
$action = New-ScheduledTaskAction `
    -Execute   'C:\Users\you\.cargo\bin\kb-mcp.exe' `
    -Argument  'serve' `
    -WorkingDirectory 'C:\Users\you\kb-mcp'
$trigger   = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$settings  = New-ScheduledTaskSettingsSet `
    -RestartInterval     (New-TimeSpan -Minutes 1) `
    -RestartCount         3 `
    -ExecutionTimeLimit  (New-TimeSpan -Seconds 0)   # = 0 で無制限
$principal = New-ScheduledTaskPrincipal `
    -UserId    $env:USERNAME `
    -LogonType Interactive `
    -RunLevel  Limited
Register-ScheduledTask `
    -TaskName  'kb-mcp' `
    -Action    $action `
    -Trigger   $trigger `
    -Settings  $settings `
    -Principal $principal `
    -Force
Start-ScheduledTask -TaskName 'kb-mcp'   # 即起動 (次回ログオン待ち不要)
```

確認 / 停止 / アンインストール:

```powershell
Get-ScheduledTask        -TaskName 'kb-mcp'
Stop-ScheduledTask       -TaskName 'kb-mcp'
Unregister-ScheduledTask -TaskName 'kb-mcp' -Confirm:$false
```

**代替: `schtasks /Create /XML` (旧来)**

```powershell
# kb-mcp-task.xml の Command パスと WorkingDirectory を編集してから import
schtasks /Create /TN "kb-mcp" /XML examples\deployments\personal-http\kb-mcp-task.xml
schtasks /Run    /TN "kb-mcp"   # 次回ログオン待たずに即起動
```

> ⚠️ XML import は AT_LOGON で admin 権限不要のはずなのに **"アクセスが
> 拒否されました"** で失敗するケースがある (旧来 `schtasks` 実装の Principal
> 解決の癖と推定)。詰まったら上の PowerShell `Register-ScheduledTask`
> snippet にフォールバック。同じ結果が admin 権限なしで得られる。

**ユーザがログオンする前** にデーモンを動かす必要があれば
[nssm](https://nssm.cc/) で本物の Windows サービスとして登録する
(admin 必要)。

### Step 4 — ヘルスチェック

```bash
curl http://127.0.0.1:3100/healthz   # → "ok"
```

### Step 5 — 各プロジェクトの `.mcp.json` をデーモンに向ける

```bash
cp examples/deployments/personal-http/.mcp.json /your/project/root/.mcp.json
# あるいは全プロジェクトで共有:
cp examples/deployments/personal-http/.mcp.json ~/.config/claude/.mcp.json
```

以上。Claude Code を再起動すると、HTTP MCP transport が既に走っている
デーモンに接続する。セッションごとの kb-mcp 子プロセス spawn はもう発生しない。

## 運用上の注意

- **watcher は全プロジェクトで 1 本**。デーモンは 1 つの `kb_path` をローカル
  ディスク上で見ている。プロジェクトごとに別 KB を持っているなら、KB ごとに
  別ポートで別デーモンを立てる (toml + launcher unit を KB ごとに用意し、
  各プロジェクトの `.mcp.json` を正しいポートに向ける)。kb-mcp 側で複数 KB を
  1 HTTP エンドポイントに集約する仕組みは無い
- **マルチテナント保護なし**。loopback HTTP はマシン上のシェルアクセスを持つ
  全ユーザがデーモンに到達できるという意味。これは stdio と同じ信頼モデル
  (シェルアクセスを持つ人は同じ DB に対して `kb-mcp` を実行できる)。共有 /
  マルチユーザのワークステーションでは本レシピは不適、stdio + ユーザごとの
  kb-mcp.toml が筋
- **再起動安全性**。SQLite WAL + `synchronous = NORMAL` (kb-mcp が使う
  rusqlite の bundled feature の default) なので、書込中に kill-9 されても
  失うのは最大 chunk 1 個ぶんの commit。次回 `kb-mcp index` で source files
  から再構築されるので問題なし
- **メモリ形状**。launcher 起動後の最初のリクエストで embedder が
  download / load する (~130 MB BGE-small or ~2.3 GB BGE-M3)。それ以降は
  RAM はほぼ flat — 2 個目 / 3 個目 / N 個目のプロジェクトセッションは
  HTTP クライアントとして繋ぐだけなので追加コスト無し

## セキュリティモデル

`intranet-http/` のものを loopback-only に簡略化:

| 脅威 | 対策 |
| --- | --- |
| マシン上の他ユーザアカウント | 共有ワークステーションで本レシピを使わない。デーモンに per-user auth は無い |
| ブラウザ経由の DNS rebinding | rmcp が `Host` ヘッダを `["localhost", "127.0.0.1", "::1"]` に対して検証 (本ディレクトリの `kb-mcp.toml` は rmcp default を使う — loopback only なら追加設定不要) |
| 誤って LAN に公開 | 本ディレクトリの `kb-mcp.toml` は `bind = "127.0.0.1:3100"` 固定。これを変えると kb-mcp が「allowed_hosts 未設定 + 非 loopback bind」を起動時 warn する (v0.5.0+)。この warn は無視しないこと |

## いつ別レシピに乗り換えるか

- **複数マシン** (LAN、複数ユーザが同じ KB) → [`intranet-http/`](../intranet-http/)
  に切替えて nginx + auth を前段に
- **同時に Claude Code を 1 本だけ、daemon を持ちたくない** →
  [`personal/`](../personal/) (stdio) に戻す
- **NAS 上の KB を複数マシンが各自 read** → [`nas-shared/`](../nas-shared/)
