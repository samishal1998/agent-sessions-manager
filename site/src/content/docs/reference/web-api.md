---
title: Web API
description: The local HTTP API behind the web UI, and the hub protocol.
---

:::caution
These are the web UI's own endpoints. They are **not a stable public API**: they change with the UI. The CLI's `--json` output is the stable scripting surface.
:::

All routes are under `asm serve` (default `http://127.0.0.1:7433`). Mutating (`POST`, `PUT`, `DELETE`) requests must carry an `X-Asm-Request: 1` header and are rejected from other origins; this stops other web pages, not a direct request. There is no authentication.

## Read

| Route | Returns |
|---|---|
| `GET /api/meta` | `{"home": …}` — the home directory, so the UI can shorten paths. |
| `GET /api/sessions` | Sessions. `?all=true` includes children; `?stream=1` sends one JSON object per line as stores give sessions up. |
| `GET /api/projects` | Projects with counts. |
| `GET /api/doctor` | The `asm doctor` report. |
| `GET /api/search?q=&agent=&project=&limit=` | Full-text hits with snippets. |
| `GET /api/index` | Index statistics. |
| `GET /api/session/{agent}/{id}/ir` | The session as IR. |
| `GET /api/hub` | Hub status: joined, connected, url, machine, error, a summary, one row per session with its sync state — and `daemon`: the local daemon's state and last-written status (or `null`). |

## Act

| Route | Body |
|---|---|
| `POST /api/index/refresh` | — |
| `POST /api/session/{agent}/{id}/rename` | `{"title": "…"}` |
| `POST /api/session/{agent}/{id}/archive` · `/unarchive` · `/delete` | — |
| `POST /api/session/{agent}/{id}/move` | `{"dir": "…"}` |
| `POST /api/session/{agent}/{id}/import` | `{"to": "opencode", "seed": false}` |
| `POST /api/session/{agent}/{id}/send` | `{"message": "…"}` — streams the reply as NDJSON. |
| `POST /api/bulk` | `{"sessions": [{"agent","native_id"}…], "action": "archive" \| "unarchive" \| "delete" \| "move" \| "export" \| "import" \| "push", …}` — `move` and `export` add `"dir"`, `import` adds `"to"`. One verb over many sessions, with a per-session result. |
| `POST /api/hub/pull` | `{"agent","id","project_dir"?}` |
| `POST /api/hub/pull-all` | — a bulk-style report. |

## The hub protocol

`asm hub serve` exposes a separate router, all under `/hub/v1/`, behind a bearer credential (except join). Clients are `asm` itself; this is for reference.

| Route | |
|---|---|
| `POST /hub/v1/join` | `{token, name}` → machine id and credential (no bearer). |
| `GET /hub/v1/machines` | Joined machines. |
| `GET /hub/v1/sessions` | Heads (manifest summaries). |
| `GET /hub/v1/sessions/{agent}/{id}` | Head and revision list. |
| `PUT /hub/v1/sessions/{agent}/{id}` | A revision manifest; `201` or `409` with the current head, `422` if a blob is missing. |
| `GET /hub/v1/sessions/{agent}/{id}/revisions/{rev}` | One revision's manifest. |
| `POST /hub/v1/missing` | Which of these blob hashes does the hub lack? |
| `GET` / `PUT /hub/v1/blobs/{sha}` | A blob; `PUT` is hash-verified and size-capped. |

## The hub admin API

Only present once `asm hub admin-token` has minted a token, and only for that token (`Authorization: Bearer asma_…`); a machine credential or the join token gets `401`. Responses are `Cache-Control: no-store`. See [Administering the hub](/hub/admin/).

| Route | |
|---|---|
| `GET /hub/v1/admin/overview` | Stats, the join token, version, store path, file cap. |
| `GET /hub/v1/admin/machines` | Machines with the number of sessions each pushed. |
| `POST /hub/v1/admin/machines/{id}/revoke` | Remove a machine's access. |
| `POST /hub/v1/admin/join-token/rotate` | Replace the join token; returns the new one. |
| `GET /hub/v1/admin/sessions` | Every session: project, pusher, revisions, size, pushed time. |
| `GET /hub/v1/admin/sessions/{agent}/{id}` | One session's head and revision list. |
| `DELETE /hub/v1/admin/sessions/{agent}/{id}` | Delete every revision of a session. |
| `POST /hub/v1/admin/collect?dry_run=true` | Remove (or preview removing) files no revision uses. |
| `GET /hub/v1/admin/log` | The last 50 administrative actions. |
| `GET /admin` | The admin page itself (and its `/assets/*`), served only while an admin token exists. |
