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

## What the *single* endpoint says on a duplicate was never measured

The table above is the **bulk** endpoint. `POST /api/v1/users/current/heartbeats` was probed only with a fresh heartbeat, so its duplicate branch is unmeasured. Task 3 had to answer it anyway, and extrapolated: `201` with `DUPLICATE_ID` in `{"data": {"id": …}}`.

Held together by two measured facts — the success code of that endpoint is `201`, and a duplicate is a success rather than an error — plus one unmeasured leap: that the endpoint marks a duplicate the same way its bulk sibling does.

**We diverge from the only known duplicate form on two counts, not one:**

| | Only measured form (bulk element) | What we answer (single) |
|---|---|---|
| Status | `202` | `201` |
| Body | `{"id": …, "skip": "Too many duplicate heartbeats."}` | `{"data": {"id": …}}` — no `skip` |

The second one costs more than the first. `skip` is the readable mark of "not written"; without it a client can only tell a duplicate from an insert by comparing the returned `id` against the all-zeros-v4 constant — and nobody in WakaTime's documentation promises that constant is stable or that a client should compare against it. A plugin that counts what it has stored would count our duplicates as insertions. Nothing in the plugins we know of does this, which is why this is a recorded risk rather than a bug.

Adding `skip` to the single response is **not** the safe fix. `heartbeat-single.json` carries `data.id` and nothing else, so a `skip` alongside it would be a field of our own invention — a divergence we authored rather than inherited, which is worse than the one we are stuck with. Note this is an argument about the protocol, not about the tests: the shape check only ever exercises the accept branch, so a `skip` emitted on the duplicate branch alone would pass the whole suite. Nothing catches it; the reason not to do it has to be the reason above.

### How to settle it

One probe against a live account: send the same heartbeat to `/heartbeats` twice in a row, a second apart, and record status and body of the second. That needs a live WakaTime account and a valid API key, so it cannot be done from the test suite. Until then the constant in `compat::heartbeats::DUPLICATE_ID` and the `201` are held by `a_duplicate_heartbeat_is_a_success_with_the_zero_id_and_no_second_row`, whose comment says outright which half is measured.
