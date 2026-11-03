# Window of YNAU · 云农之窗

[中文](README.md) | **English**

> Replace most campus clients with a single app: **navigate on the left, ask on the right.**
> Credentials never leave the device, privacy stays local, knowledge lives in the cloud.

---

## What it is

A desktop (Windows) and mobile (Android) client for university campus services, built around two ideas:

1. **Campus chores** — timetable, grades, library, campus portal — reachable from one sidebar;
2. **An AI assistant** — that knows your timetable and local notes, without uploading them.

### Design principles

| Principle | Consequence |
|---|---|
| **Credentials never leave the device** | Campus session cookies live in the WebView profile; API keys are DPAPI-encrypted locally. No server ever sees them. |
| **The cloud is a control plane, not a data plane** | The server proxies requests and issues tokens. It **never stores message bodies**. |
| **Local capabilities are never gated by plan** | Timetable, ICS export, local notes, local search and credential storage work forever, on every tier. |
| **No silent degradation** | If search is unconfigured, the UI says so. The model is never allowed to pretend it searched. |

## Features

- **Timetable**: SSO login → in-page extraction → weekly grid (7×10) → ICS export (week numbers expanded into discrete all-day events — we deliberately never guess class times).
- **AI chat**: streaming, three protocols (OpenAI Completions / OpenAI Responses / Anthropic Messages), BYOK direct or plan-proxied.
- **Local memory + BM25 search**: notes stored in SQLite, CJK bigram index, zero external dependencies, fully offline.
- **Context compaction**: deterministic, zero-LLM summarisation with tool-pair-safe cut points and `memory://` references to recover the original text.
- **Three themes**: minimalist monochrome, deep-blue glass, book-like warm paper. Course colours are invariant across themes.
- **Local-first privacy**: DPAPI (Windows) for secrets, no telemetry, no plaintext sync.

## Architecture

```
Flutter UI  ──FFI──▶  Rust core (yunong_core.dll / .so)
                        ├─ SQLite: timetable, memory, BM25 index, settings
                        ├─ LLM gateway: 3 protocols, streaming, cancellation
                        ├─ context compaction + memory refs
                        └─ DPAPI secret storage
                     │
                     ▼ (optional, plan channel only)
              Rust server (axum + PostgreSQL)
                ├─ auth: Argon2id, opaque tokens, refresh rotation
                ├─ streaming proxy with request sanitisation
                ├─ concurrency gate, quota, usage sampling, audit log
                └─ RAG 2.0: monthly salt → anonymous hash reports → corpus refresh
```

**Key boundary:** the client owns all user data; the server owns identity, quotas and the public knowledge corpus.

## Repository layout

```
app/            Flutter client (Windows + Android)
crates/shared/  Single source of truth for wire contracts
crates/core/    Rust side core (storage, gateway, compaction, ICS, secrets)
crates/server/  Optional cloud: proxy, auth, quota, RAG pipeline
deploy/         Dockerfile, compose, env example
docs/           Product requirements, system design, API docs, ADRs
scripts/        CI helpers, smoke tests, mock upstream, secret scanning
```

## Quick start

```bash
# Build the Windows app (produces the exe + core DLL)
pwsh scripts/build-windows-app.ps1

# Rust tests
cargo test --workspace

# Flutter tests
cd app && flutter test

# Optional: run the server locally
docker compose -f deploy/docker-compose.yml up
```

## Quality gates

CI runs nine required checks on every PR: Rust (fmt / clippy / test), Flutter (analyze / test),
docs (tables & links), migrations, PostgreSQL integration tests, container build + smoke,
end-to-end chat loop against a mock upstream, secret scanning, and `cargo-deny`
(advisories / licences / bans / sources).

Security audits are performed by an independent reviewer with its own reproduction scripts —
not by the authors of the code. Findings live under `.internal/security/` and are not public.

## Documentation

| Document | Contents |
|---|---|
| [docs/01-产品需求文档.md](docs/01-产品需求文档.md) | Requirements, milestones, acceptance criteria |
| [docs/02-系统设计文档.md](docs/02-系统设计文档.md) | Architecture, data model, key flows |
| [docs/03-接口文档.md](docs/03-接口文档.md) | HTTP API, FFI, WebView bridge |
| [docs/04-ADR.md](docs/04-ADR.md) | Architecture decision records (19 and counting) |

## Licence

Dual-licensed: **AGPL-3.0-or-later** for open use, plus a commercial licence for
proprietary integration. See `LICENSE` and ADR-14.
