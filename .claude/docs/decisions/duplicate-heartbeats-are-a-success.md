# A duplicate heartbeat is a successful element, not an error

Measured against live wakatime.com, not inferred from `wakatime-cli` source. Corrects the spec's response shapes for both write endpoints.

## What the wire actually carries

`POST /api/v1/users/current/heartbeats` — `201`, body `{"data": {"id": "<uuid>"}}`. **Only `id`.** The spec previously promised `entity`, `type` and `time` alongside it; they are not there.

`POST /api/v1/users/current/heartbeats.bulk` — `202`, body `{"responses": [[body, status], …]}`. Element bodies are **not** wrapped in `data`, unlike the single-heartbeat response:

| Outcome | Element status | Element body |
|---|---|---|
| Accepted | 2xx | `{"id": "<uuid>"}` |
| Skipped as duplicate | **202** | `{"id": "00000000-0000-4000-a000-000000000000", "skip": "Too many duplicate heartbeats."}` |
| Rejected | 400 | `{"errors": {"entity": ["This field is required."]}}` |

Captured by sending two heartbeats one second apart for the same entity — the second was recognised as a duplicate — plus one with an empty `entity`.

## Why it matters to `wakode-store::dedup`

A duplicate is **not** an error and **not** a silent drop. It is a successful element carrying two distinguishing marks:

- `id` is the all-zeros UUID rather than a real one, because no row was written;
- a `skip` field explains why, in prose.

An implementation that returns an error for duplicates makes clients retry, which produces more duplicates. One that returns a freshly generated `id` lies: it hands out an identifier for a row that does not exist, and any client keeping it will later fail to find it. Both failure modes are invisible without a fixture, which is why this is written down.

Note the shape of that nil id: `00000000-0000-4000-a000-000000000000` carries the version-4 nibbles, so it is not `Uuid::nil()`. A test asserting `Uuid::nil()` would pass against a wrong implementation and fail against a right one.

`errors` is plural and maps a field name to an **array** of messages. The spec's earlier guess allowed a singular `error`; no such form appears.

## Caveat

One account, one probe. The duplicate branch fired because the two heartbeats shared an entity and were one second apart; the exact rule WakaTime uses to call something a duplicate was not measured and is not needed here — what the wire says on a duplicate is.
