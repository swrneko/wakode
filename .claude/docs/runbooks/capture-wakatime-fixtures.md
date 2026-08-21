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

Substitution is by key name and deterministic: a placeholder is a function of the `(key, value)` pair and of nothing else — not of traversal order, not of which files happen to be in the input directory — so re-running produces no diff, and an eleventh capture does not renumber the ten fixtures already committed. Three rules the script has to keep. The second and third are pinned by tests in `tools/test_scrub_wakatime_fixtures.py`; the first is a rule for whoever edits fixtures by hand, and no test can hold it.

- **Nothing is truncated.** Arrays go over whole. A hand-shortened `heartbeats-day.json` would still pass the shape tests — one element is all they read — while silently destroying the calibration evidence and leaving `summaries-month.json`'s header (`days_including_holidays: 30`) contradicting a six-element body. If the committed fixtures ever stop being exactly what this script emits, they are no longer regenerable, which is the whole point of having a script.
- **`errors` is opaque.** Inside it, keys are the *names of rejected request fields* and values are protocol prose. Scrubbing there replaced `"This field is required."` with a placeholder — deleting the one thing the bulk-error fixture exists to record.
- **Protocol constants are not identifiers.** A UUID whose every hex digit outside the version and variant positions is zero says nothing about the account and everything about the server. `heartbeat-bulk.json` carries `"id": "00000000-0000-4000-a000-000000000000"` on the duplicate element — note the version-4 nibbles, so it is *not* `Uuid::nil()`. Scrubbing it once turned the fixture into a claim that an ordinary id was returned there. `crates/wakode-api/tests/shape.rs` asserts the string from the Rust side too.

Run the script's own tests after touching it:

```bash
python3 -m unittest discover -s tools
```

Check what survived before committing:

```bash
jq -r '[paths(type=="string") as $p | getpath($p)] | .[]' crates/wakode-api/tests/fixtures/wakatime/*.json \
  | grep -vE '^[a-z_]+-[0-9a-f]{6}$' | sort -u
```

Everything not matching `key-<hex>` is real. On the current capture that is, exhaustively:

- rendered totals — `text`, `digital`, `decimal` (`"1 hr 12 mins"`, `"0:47:03"`, `"0.33"`), plus `text_including_other_language`;
- times and dates — `created_at`, `start`, `end`, `start_date`, `end_date`, `date`, `modified_at`, `last_heartbeat_at`, and the human `start_text` / `end_text` (`"Mon Aug 17th 2026"`). Not `time`: it is a float, so it never reaches a string-valued remainder at all;
- the timezone, deliberately: it is what makes `range.start: …T21:00:00Z` for `range.date: 2026-08-18` legible as a form rather than a typo;
- vocabularies the server owns — `type` (`file`), `language`, `category`, `last_language`, `last_plugin_name`, `ai_subscription_plan`, `plan`, `color_scheme`, `durations_slice_by`, `default_dashboard_range`, `public_profile_time_range`;
- format templates — `date_format` (`YYYY-MM-DD`), `time_format_display`, `invoice_id_format` (`INV-{iiiii}`);
- protocol prose — `skip`, `message`, and the one string under `entity`, which lives inside `errors`;
- the one `id` that is a protocol constant, per the rule above.

`dependencies` used to be in that remainder: the plugin parses the account owner's own source files into it (`["re", "shutil", "subprocess"]` in `heartbeats-day.json`), which is file content, not a vocabulary. It is scrubbed now — an array of strings stays an array of strings of the same length, and in `summaries-*.json`, where the same key holds an array of objects, the placeholder goes into `name`.
