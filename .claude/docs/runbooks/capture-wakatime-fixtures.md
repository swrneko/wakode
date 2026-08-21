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

Read the files. Form is what makes a fixture a fixture, not content — replacing project names, paths, and the email with placeholders costs nothing and loses nothing. The raw capture stays out of git (`.gitignore` has `/fixtures/`).

```bash
python3 tools/scrub-wakatime-fixtures.py   # fixtures/wakatime → crates/wakode-api/tests/fixtures/wakatime
```

Substitution is by key name and deterministic: the same input value always yields the same placeholder, so re-running produces no diff. Two rules the script has to keep:

- **Nothing is truncated.** Arrays go over whole. A hand-shortened `heartbeats-day.json` would still pass the shape tests — one element is all they read — while silently destroying the calibration evidence and leaving `summaries-month.json`'s header (`days_including_holidays: 30`) contradicting a six-element body. If the committed fixtures ever stop being exactly what this script emits, they are no longer regenerable, which is the whole point of having a script.
- **`errors` is opaque.** Inside it, keys are the *names of rejected request fields* and values are protocol prose. Scrubbing there replaced `"This field is required."` with a placeholder — deleting the one thing the bulk-error fixture exists to record.

Check what survived before committing:

```bash
jq -r '[paths(type=="string") as $p | getpath($p)] | .[]' crates/wakode-api/tests/fixtures/wakatime/*.json \
  | grep -vE '^[a-z_]+-[0-9]+$' | sort -u
```

Everything not matching `key-N` is real. On the current capture that leaves the timezone (deliberately — it is what makes `range.start: …T21:00:00Z` for `range.date: 2026-08-18` legible as a form rather than a typo), language and category names, plan names, and real timestamps.
