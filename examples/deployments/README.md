# Deployment recipes

Four opinionated deployment patterns for kb-mcp. Each subdirectory ships
ready-to-adapt `kb-mcp.toml` and `.mcp.json` files plus a short README.
Pick the one closest to your situation, copy the files into the target
machine, and adjust paths.

> **日本語版**: [README.ja.md](./README.ja.md)

| Scenario | Best for | Transport | Indexer machines |
| --- | --- | --- | --- |
| [`personal/`](./personal/) | Single user, single Claude Code session at a time | stdio | 1 (this machine) |
| [`personal-http/`](./personal-http/) | Single user, **multiple** Claude Code sessions in parallel on one machine | Streamable HTTP, loopback only | 1 (this machine, daemonized) |
| [`nas-shared/`](./nas-shared/) | KB on a NAS, multiple machines reading | stdio (each client) | 1 dedicated indexer |
| [`intranet-http/`](./intranet-http/) | Team server, multiple users at once | Streamable HTTP | 1 (the server) |

## Selection guide

```
Are you the only person using this KB?
├── Yes → personal flavors
│   ├── Only one Claude Code session at a time? → personal/  (stdio, no daemon)
│   └── Multiple Claude Code sessions in parallel on the same machine?
│       → personal-http/  (one local daemon, every session connects via HTTP)
│
└── No
    ├── Each user keeps their own copy of the KB? → personal/ on every machine
    │
    └── Single source of truth (KB lives on a NAS or shared host)
        ├── All clients on the same LAN as the host that can run kb-mcp serve?
        │   └── Yes → intranet-http/  (one server, many clients)
        │
        └── Clients want stdio simplicity (no kb-mcp serve process to manage)?
            └── nas-shared/  (mount the KB; SQLite caveats apply)
```

## Common notes

- **Embedding model cache**: First run downloads the ONNX model (BGE-small ~130 MB or BGE-M3 ~2.3 GB) per machine. Set `FASTEMBED_CACHE_DIR` in `kb-mcp.toml` to share it across all kb-mcp invocations on a given machine — see each scenario's `kb-mcp.toml`.
- **Index location**: `.kb-mcp.db` is always created in the **parent of `kb_path`** (e.g. `kb_path = /srv/kb/notes` → DB at `/srv/kb/.kb-mcp.db`). There is no CLI flag to relocate the DB. Plan disk layout with this in mind.
- **Backup policy**: The DB can be rebuilt at any time via `kb-mcp index --force --kb-path <kb_path>`. Treat the source files as authoritative; the DB is a derived artifact.

## What's not here

- **Public-internet hosting** — kb-mcp has no built-in authentication. Anything beyond an intranet needs a reverse proxy with auth + TLS terminator in front.
- **Container / Kubernetes manifests** — feasible (statically linked binary, ~10 MB image surface) but not yet packaged. Reuse the `intranet-http/` recipe inside a container.
- **High availability** — kb-mcp is single-process; index updates serialize through one `Mutex<Database>`. Run a single instance per index.
