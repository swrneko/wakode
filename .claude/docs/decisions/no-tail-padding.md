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

## What was wrong before

The engine's model was: group heartbeats **per project**, glue them into intervals, then add `tail_padding` to each. Three things are wrong with it, and only the third was suspected.

- **Grouping per project first is wrong.** Gaps are measured globally and only then attributed. Switching between two projects within the timeout produces no gap at all — the time belongs to whichever project was open first. Per-project grouping instead measures the gap to that project's *own* next heartbeat, which is longer, and double-counts overlapping stretches. On the measured day this inflated the total by 2926 s (13%) — in the direction of reporting more work than happened.
- **The last heartbeat of a session must contribute zero.** Not a small addition: zero.
- **There is no padding to calibrate.** The knob stays in the config because someone may want their own numbers, but its documented default is now a measured fact, not a placeholder.

## Caveat

One account, one day, `timeout = 15` minutes. The match is exact rather than approximate across 62 independent segments, which is strong, but it does not prove behaviour at settings this account never used — in particular a different `timeout`, or the `duration_slice_by` feature (`null` on this account).
