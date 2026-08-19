# Overview

Project map (stack, commands, structure, module index) lives in `../rules/ARCHITECTURE.md`. Do not duplicate it here — this file holds data flows only.

## Data flows

### Heartbeat ingest (planned, not implemented yet)

Editor plugin → `POST /api/compat/v1/users/current/heartbeats` (WakaTime-compatible) → `wakode-api` authenticates the API key → `wakode-store::writer` single writer task → SQLite transaction. Strings (project, language, editor) are resolved to `Sid` numbers by `wakode-store::interner` before anything reaches `wakode-core`.

The writer is a dedicated OS thread, not a tokio task: the work is blocking. Its queue uses `try_send`, never `send` — waiting for room would pile requests up in memory; a full queue becomes `503` with `Retry-After`, and the editor plugin resends from its own queue.

Single-record writes (users, keys, sessions) bypass this queue on their own connections, so a login never waits behind someone else's batch. What keeps them apart is SQLite itself: WAL plus `busy_timeout`.

### Summaries

Stored heartbeats → `wakode-core::intervals` (gluing heartbeats into intervals by timeout) → `wakode-core::calendar` (splitting into local days in the user's timezone) → `wakode-core::aggregate` (per-project/language/editor totals). All of this is pure: no clock, no filesystem, no database.

### First-time setup

`GET /api/setup/status` reports `needed` and `token_required`. `POST /api/setup` creates the single administrator and closes itself forever once any user exists.

Access is decided by one function used by both endpoints — a second copy of the rules would drift, and the setup screen would hide the token field exactly where the server demands it. The decision is made **before** the database is touched and before the request body is parsed.

A setup token (32 bytes, printed to the log at startup while no administrator exists, presented in `x-wakode-setup-token`) overrides the address check entirely. It exists because the TCP peer is the reverse proxy, not the client: behind a same-host proxy, `is_loopback()` is true for anyone on the internet.

### Graceful shutdown

SIGTERM/SIGINT → the server stops accepting connections and drains in-flight requests (bounded by `GRACE`, 10s) → `store.shutdown()` stops the writer and releases the database → the process exits.

The 10s bound is deliberately smaller than systemd's default `TimeoutStopSec` of 90s: systemd's SIGKILL would destroy the very writer drain this exists to guarantee.

Known limit: `GRACE` bounds the HTTP half only. `store.shutdown()` waits for the writer with no deadline. Unreachable today (nothing goes through the queue), but it becomes reachable the moment ingest lands.
