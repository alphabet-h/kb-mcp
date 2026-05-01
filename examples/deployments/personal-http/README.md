# Deployment recipe — personal HTTP daemon (loopback)

> **日本語版**: [README.ja.md](./README.ja.md)

One kb-mcp daemon runs as a long-lived OS service on `127.0.0.1:3100`.
Every Claude Code / Cursor session on the machine — across as many
projects as you happen to have open — connects to that one daemon over
Streamable HTTP instead of spawning a per-session stdio kb-mcp child.

> **⚠️ Loopback only.** This recipe binds to `127.0.0.1`. kb-mcp has no
> built-in authentication; if you need LAN exposure, switch to the
> [`intranet-http/`](../intranet-http/) recipe and put a reverse proxy
> with auth in front.

## Why this exists

`personal/` (stdio) is simpler and has no daemon to manage, so prefer it
when you can. But stdio has a hard scaling limit per kb-mcp instance:

```
        Claude Code (project A) ─→ kb-mcp child #1 ─→ embedder, DB, watcher
        Claude Code (project B) ─→ kb-mcp child #2 ─→ embedder, DB, watcher
        Claude Code (project C) ─→ kb-mcp child #3 ─→ embedder, DB, watcher
                                                    └─→ N × ~2.3 GB peak (BGE-M3)
```

Each new editor window pulling the same KB pays the embedder load again
(BGE-M3 peak ~2.3 GB), spins another file watcher on the same directory,
and risks DB writer contention if you happen to run `kb-mcp index --force`
in one project while another holds the read mutex.

`personal-http/` collapses all of that to one process:

```
        Claude Code (project A) ─┐
        Claude Code (project B) ─┼─→ HTTP /mcp ─→ one kb-mcp daemon
        Claude Code (project C) ─┘
```

One embedder, one DB connection, one watcher, regardless of how many
editor sessions you open. The tradeoff is one OS service unit to manage.

## Target environment

- Single user, single physical machine (laptop / desktop).
- Two or more Claude Code (or Cursor / Zed / etc.) sessions opened in
  parallel against the same KB. If you only ever open one, prefer
  [`personal/`](../personal/) — no daemon to maintain.
- Local disk for the KB (network-mounted KBs work but are out of scope
  for this recipe; see [`nas-shared/`](../nas-shared/)).

## What's in this directory

| File | Purpose |
| --- | --- |
| [`kb-mcp.toml`](./kb-mcp.toml) | Daemon config — HTTP transport on `127.0.0.1:3100`, watcher on |
| [`.mcp.json`](./.mcp.json) | Client-side `.mcp.json`: HTTP transport pointing at the daemon. Drop into each project root or `~/.config/claude/.mcp.json` |
| [`kb-mcp.user.service`](./kb-mcp.user.service) | Linux: systemd **user** unit (no root needed) |
| [`com.kb-mcp.plist`](./com.kb-mcp.plist) | macOS: launchd LaunchAgent (per-user) |
| [`kb-mcp-task.xml`](./kb-mcp-task.xml) | Windows: Task Scheduler XML (AT_LOGON, no admin) |

## Setup

Pick the launcher template for your OS, edit paths, install, then point
every project's `.mcp.json` at the daemon.

### Step 1 — Place `kb-mcp.toml` and edit paths

```bash
mkdir -p ~/kb-mcp
cp examples/deployments/personal-http/kb-mcp.toml ~/kb-mcp/kb-mcp.toml
$EDITOR ~/kb-mcp/kb-mcp.toml   # set kb_path = "/absolute/path/to/your/notes"
```

`Config::discover()` picks up `kb-mcp.toml` from the launcher's working
directory (priority 2 — CWD). The launcher templates set
`WorkingDirectory=~/kb-mcp` (or its OS equivalent) so the discovery just
works.

### Step 2 — Build the index once

Same procedure as the stdio `personal/` recipe — index from the same
working directory the launcher will use, so the resulting `.kb-mcp.db`
ends up where the daemon expects.

```bash
cd ~/kb-mcp
kb-mcp index   # the toml provides --kb-path / --model
```

### Step 3 — Install the OS launcher

#### Linux (systemd user unit)

```bash
mkdir -p ~/.config/systemd/user/
cp examples/deployments/personal-http/kb-mcp.user.service ~/.config/systemd/user/kb-mcp.service
$EDITOR ~/.config/systemd/user/kb-mcp.service   # update ExecStart path if not at ~/.cargo/bin/kb-mcp
systemctl --user daemon-reload
systemctl --user enable --now kb-mcp.service
journalctl --user -u kb-mcp.service -f          # follow logs
```

To keep the daemon running after logout:

```bash
sudo loginctl enable-linger "$USER"
```

#### macOS (launchd LaunchAgent)

```bash
mkdir -p ~/Library/LaunchAgents
cp examples/deployments/personal-http/com.kb-mcp.plist ~/Library/LaunchAgents/com.kb-mcp.plist
$EDITOR ~/Library/LaunchAgents/com.kb-mcp.plist   # update ProgramArguments + WorkingDirectory
launchctl load ~/Library/LaunchAgents/com.kb-mcp.plist
tail -f /tmp/kb-mcp.out /tmp/kb-mcp.err
```

#### Windows (Task Scheduler, AT_LOGON)

Two options. The PowerShell cmdlet path is the **recommended** one — it
avoids two known pitfalls of the legacy `schtasks /Create /XML` flow.

**Recommended: `Register-ScheduledTask` (PowerShell)**

```powershell
$action = New-ScheduledTaskAction `
    -Execute   'C:\Users\you\.cargo\bin\kb-mcp.exe' `
    -Argument  'serve' `
    -WorkingDirectory 'C:\Users\you\kb-mcp'
$trigger   = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$settings  = New-ScheduledTaskSettingsSet `
    -RestartInterval     (New-TimeSpan -Minutes 1) `
    -RestartCount         3 `
    -ExecutionTimeLimit  (New-TimeSpan -Seconds 0)   # = unlimited
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
Start-ScheduledTask -TaskName 'kb-mcp'   # start now
```

Verify / stop / uninstall:

```powershell
Get-ScheduledTask        -TaskName 'kb-mcp'
Stop-ScheduledTask       -TaskName 'kb-mcp'
Unregister-ScheduledTask -TaskName 'kb-mcp' -Confirm:$false
```

**Alternative: `schtasks /Create /XML` (legacy)**

```powershell
# Edit kb-mcp-task.xml (Command path + WorkingDirectory) before importing
schtasks /Create /TN "kb-mcp" /XML examples\deployments\personal-http\kb-mcp-task.xml
schtasks /Run    /TN "kb-mcp"   # start now without waiting for next logon
```

> ⚠️ The XML import path can fail with **"Access denied"** even though
> no admin privileges are required for AT_LOGON tasks in their own user
> namespace. Cause appears to be a Principal-resolution quirk of the
> legacy `schtasks` implementation. If you hit this, fall back to the
> PowerShell `Register-ScheduledTask` snippet above — same end result,
> no admin needed.

If you need the daemon running before any user logs in, switch to
[nssm](https://nssm.cc/) which registers kb-mcp as a real Windows
service (admin required).

### Step 4 — Health check

```bash
curl http://127.0.0.1:3100/healthz   # → "ok"
```

### Step 5 — Point each project's `.mcp.json` at the daemon

```bash
cp examples/deployments/personal-http/.mcp.json /your/project/root/.mcp.json
# or once for every project:
cp examples/deployments/personal-http/.mcp.json ~/.config/claude/.mcp.json
```

That's it. Restart Claude Code; the HTTP MCP transport hits the
already-running daemon. No kb-mcp child spawn per session anymore.

## Operational notes

- **Watcher across all projects.** The daemon watches one `kb_path` on
  local disk. If you spread projects across different KBs, run multiple
  daemons on different ports (one toml + one launcher unit per KB) and
  point each project's `.mcp.json` at the right port. There is nothing
  in kb-mcp that aggregates multiple KBs into one HTTP endpoint.
- **No multi-tenancy guard.** Loopback HTTP means anyone with shell
  access on the machine can hit the daemon. This is the same trust
  model as stdio (anyone with shell access can run `kb-mcp` against
  the same DB). Don't use this recipe on a shared / multi-user
  workstation; prefer stdio with per-user kb-mcp.toml in that case.
- **Restart safety.** SQLite WAL + `synchronous = NORMAL` (the rusqlite
  default for kb-mcp's bundled feature) means a kill-9 mid-write loses
  at most the current chunk's commit. The next `kb-mcp index` rebuilds
  from authoritative source files anyway.
- **Memory shape.** First request after launcher start triggers the
  embedder model download / load (~130 MB BGE-small or ~2.3 GB BGE-M3).
  After that, RAM stays roughly flat — the second + third + Nth project
  session adds nothing because they're just HTTP clients now.

## Security model

Same one-paragraph version as `intranet-http/`, simplified for loopback:

| Threat | Mitigation |
| --- | --- |
| Other user accounts on the machine | Don't use this recipe on shared workstations; the daemon has no per-user auth. |
| Browser-based DNS rebinding | rmcp validates `Host` header against `["localhost", "127.0.0.1", "::1"]` (the `kb-mcp.toml` shipped here uses the rmcp default — no extra config needed for loopback-only). |
| Process accidentally exposed to LAN | The `kb-mcp.toml` shipped here pins `bind = "127.0.0.1:3100"`. If you change it, kb-mcp will warn at startup that allowed_hosts is unset and the bind is non-loopback (added in v0.5.0). Take that warn seriously. |

## When to step up

- **Multi-machine** (LAN, multiple users hitting the same KB) → switch
  to [`intranet-http/`](../intranet-http/), put nginx + auth in front.
- **One Claude Code session at a time, no daemon** → drop back to
  [`personal/`](../personal/) (stdio); save yourself the OS service.
- **KB on a NAS, multiple machines reading their own copy** →
  [`nas-shared/`](../nas-shared/).
