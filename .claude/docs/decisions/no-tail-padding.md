# WakaTime adds nothing to the last heartbeat of a session

Settles open question #1 of `docs/superpowers/specs/2026-08-15-wakode-design.md`: the size of the addition WakaTime makes to the last heartbeat of a session. The answer is that there is no addition, and `durations.tail_padding_secs = 0` is correct rather than provisional.

## The model, stated exactly

Given all heartbeats of a day in **global** time order — not grouped by project first:

1. For each consecutive pair, let `gap = t[i+1] - t[i]`.
2. If `gap <= timeout` (900 s on the account measured), the span `[t[i], t[i+1])` counts, and it is attributed to the project of heartbeat **i** — the earlier one.
3. If `gap > timeout`, heartbeat `i` contributes zero. The session ends there.
4. The last heartbeat of the day contributes zero.
5. Adjacent spans with the same project merge into one `durations` entry.

Total time is the sum of the counted spans. Nothing is added anywhere.

## Evidence

One day, 502 heartbeats, 62 duration entries, four projects.

| Check | Result |
|---|---|
| Segment count | 62 predicted vs 62 reported |
| Every segment (start, duration, project) | 62 of 62 identical |
| Total seconds | 21839.3 predicted vs 21839.3 reported |
| `summaries.grand_total.total_seconds` for the same day | 21839.3 — identical |
| Per-project totals vs `summaries.projects[]` | identical to the last decimal |

Reproduce with `fixtures/wakatime/heartbeats-day.json` and `durations-day.json` from `tools/capture-wakatime-fixtures.sh`.

## What this means for the engine

`build_intervals` (`crates/wakode-core/src/intervals.rs:41`) already implements exactly this model, and did before the measurement. It sorts a user's heartbeats together — **not** grouped by project — glues them chronologically, and gives each interval the attributes of the earlier heartbeat of the pair (`intervals.rs:36-37, 58-63`, held by `interval_inherits_attributes_of_the_earlier_heartbeat`). A gap longer than `timeout` ends the session and is charged to nobody.

So the calibration did not find a bug. It confirmed a design that until now rested on inference from `wakatime-cli` source, and it settled the one free parameter:

**`tail_padding` must stay zero.** At zero the tail interval is not created at all — the guard `end > hb.time` (`intervals.rs:62`) drops it — which is precisely the measured behaviour. Any non-zero value makes wakode report more than WakaTime for the same heartbeats, by exactly `padding × sessions`.

An earlier draft of this document claimed the engine grouped per project first and therefore overcounted by 13%. That was wrong: the 13% came from a per-project grouping in the throwaway analysis script, not from `build_intervals`. Recorded here rather than deleted because the wrong number was committed and may be quoted back.

## Caveat

One account, one day, `timeout = 15` minutes. The match is exact rather than approximate across 62 independent segments, which is strong, but it does not prove behaviour at settings this account never used — in particular a different `timeout`, or the `duration_slice_by` feature (`null` on this account).
