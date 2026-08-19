# Capture golden fixtures from a live WakaTime account

Blocks plan 3b (the six WakaTime-compatible endpoints) and the calibration of `durations.tail_padding_secs`. Nobody can do this from the code alone: it needs a real account.

## Run

```bash
export WAKATIME_API_KEY=waka_…          # https://wakatime.com/settings/api-key
tools/capture-wakatime-fixtures.sh                 # read-only
tools/capture-wakatime-fixtures.sh --with-writes   # also posts two probe heartbeats
```

Output lands in `fixtures/wakatime/` (gitignored — the raw capture contains real project names, file paths, machine names, and the account's email).

`--with-writes` posts two heartbeats against `/tmp/wakode-fixture-probe.txt`, one of them deliberately invalid. It exists because the bulk response shape — how WakaTime reports a single failed element without failing the request — is the part the spec could only infer from `wakatime-cli` source. It does add a blip to the account's own statistics; skip it if that matters.

## If the handshake dies

`curl: (35) TLS connect error … unexpected eof while reading` looks like blocking or a bad key and is neither. `wakatime.com` publishes both A and AAAA records; on a network with a broken IPv6 route, curl honestly prefers AAAA and dies mid-handshake, before authentication is ever attempted. The script probes for this and falls back to IPv4 on its own, announcing it. To check by hand: `curl -4 -sS -o /dev/null -w '%{http_code}\n' https://wakatime.com/api/v1` — a `404` means the path is reachable and the stack is fine (that path is not an endpoint).

Pick a `DAY` (default: yesterday) with ordinary activity. `DAY=2026-08-14 tools/capture-wakatime-fixtures.sh` overrides it.

## What each file settles

| File | Settles |
|---|---|
| `current.json` | Profile shape; which fields the plugins actually read to validate a key. |
| `statusbar-today.json` | **The one form with no documentation at all.** The spec guesses `{"cached_at", "data": <summaries element>}` and says so. |
| `all-time-since-today.json` | Field-for-field shape, including the nested `range`. |
| `summaries-one-day.json` | Shape of a single-day answer. |
| `summaries-week.json` | `cumulative_total` and `daily_average`, which degenerate on one day. |
| `summaries-month.json` | Whether empty days appear in `data[]`. The spec requires wakode to emit them; day-splitting does not produce them on its own. |
| `heartbeats-day.json` | Raw timestamps — the input side of the calibration. |
| `durations-day.json` | WakaTime's own segmentation of that day — the output side. |

## Calibrating `tail_padding_secs`

The value is `0` today because the size of WakaTime's addition to the last heartbeat of a session is documented nowhere. With `heartbeats` and `durations` for the same day it is arithmetic, not guesswork:

1. Group heartbeats into sessions with the same rule the engine uses: a gap above `timeout_secs` (900 by default) ends a session.
2. For each session compute `last_timestamp - first_timestamp`. That is what wakode reports today.
3. Compare against the matching entry in `durations-day.json`.
4. The difference, if constant across sessions, is `tail_padding_secs`.

Watch for: a difference that scales with heartbeat count means the addition is per-heartbeat, not per-session, and the engine's model is wrong — a finding worth more than the number itself. A difference that varies randomly means the sessions were grouped differently; check `timeout_secs` on the account (`current.json` reports it) before concluding anything.

## Before committing anything

Read the files. Form is what makes a fixture a fixture, not content — replacing project names, paths, and the email with placeholders costs nothing and loses nothing. Scrubbed copies go under `crates/wakode-api/tests/fixtures/`; the raw capture stays out of git.
