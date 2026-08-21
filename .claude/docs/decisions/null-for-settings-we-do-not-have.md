# `null` where WakaTime always sends a value

`GET /api/v1/users/current` returns all 59 fields of the WakaTime response (`crates/wakode-api/src/compat/user.rs`). For **18** of them WakaTime always sends a value and wakode always sends `null`. This is a deliberate divergence from the protocol, and it is written down here because the shape check that guards every other compat endpoint cannot catch it — by construction.

## The 18 fields

| Field | WakaTime sends | Why we send `null` |
|---|---|---|
| `last_heartbeat_at` | timestamp | No heartbeats are stored yet — task 3 of the wave-0 plan. Debt, not a decision. |
| `last_project` | string | Same. |
| `last_language` | string | Same. |
| `last_branch` | string | Same. |
| `last_plugin` | string | Same. |
| `last_plugin_name` | string | Same. |
| `plan` | `"free"` | There are no subscription tiers on a selfhosted instance. `"free"` would name a tier where no tier exists. |
| `weekday_start` | `0` | A profile setting we do not have. `0` would assert that weeks start on Sunday — a convention wakode has never adopted. |
| `color_scheme` | `"Dark"` | UI preference; there is no UI yet. |
| `date_format` | `"YYYY-MM-DD"` | Same. |
| `time_format_display` | `"text"` | Same. |
| `default_dashboard_range` | `"Last 7 Days"` | Same. |
| `durations_slice_by` | `"Language"` | Dashboard setting; the engine has no slicing feature (see `no-tail-padding.md`, caveat). |
| `public_profile_time_range` | `"last_7_days"` | Setting of a public profile that does not exist. |
| `profile_url` | string | No public profile pages. |
| `profile_url_escaped` | string | Same. |
| `photo` | string | No avatars. |
| `invoice_id_format` | `"INV-{iiiii}"` | No invoicing. |

Thirteen further fields are `null` on our side too, but WakaTime sends `null` for them on an unconfigured account as well (`bio`, `city`, `location`, `website`, `human_readable_website`, `github_username`, `twitter_username`, `linkedin_username`, `wonderfuldev_username`, `public_email`, `share_all_time_badge`, `share_last_year_days`, `time_format_24hr`). Those are not a divergence and are not counted above.

## Count it yourself

The number is computed, not eyeballed. From the repository root:

```bash
python3 - <<'PY'
import json, re
data = json.load(open('crates/wakode-api/tests/fixtures/wakatime/current.json'))['data']
body = open('crates/wakode-api/src/compat/user.rs').read().split('CurrentUserData {', 2)[2]
ours_null = {m.group(1) for m in re.finditer(r'^\s{12}(\w+): None,$', body, re.M)}
theirs_set = {k for k, v in data.items() if v is not None}
print(len(ours_null & theirs_set), sorted(ours_null & theirs_set))
PY
```

Today it prints `18`. If it prints something else, this document is stale — most likely because task 3 filled in the `last_*` group, which would leave 12.

## Why `null` and not a plausible value

Two options were on the table: echo WakaTime's default (`plan: "free"`, `weekday_start: 0`, `color_scheme: "Dark"`), or send `null`.

`null` means "no value". A copied default means "the value is this", which is a claim about a subsystem wakode does not have. The project treats a comment that promises more than the code does as a defect; a field that promises more than the server knows is the same defect on the wire. So: fields we genuinely have come from `User`; concepts we lack whose emptiness is a fact (`needs_payment_method`, every `*_public`) are `false`; concepts we lack where any concrete value would be invented are `null`.

The alternative of *omitting* these fields was rejected separately: the protocol is frozen and not ours, and a plugin reading `timeout` or `has_premium_features` is not obliged to survive a missing key.

## What we are risking

A client that reads one of these fields **for display** gets `null` where WakaTime guarantees a string, and may or may not survive it. The realistic candidates are `date_format`, `time_format_display` and `color_scheme` — anything rendering a dashboard rather than sending heartbeats. Which fields editor plugins actually read was not verified against `wakatime-cli` sources and should not be quoted as if it were: the expectation is `id`, `username`, `email`, `timeout` and `timezone`, of which none is in the table above. Confirming that is cheap and worth doing before trusting this paragraph.

The risk is real but not measurable from here: we have no client that reads these fields, so there is nothing to test against. Adding a fabricated value to avoid a hypothetical crash would trade a known honest answer for an unknown false one.

## Why the tests cannot find this

`crates/wakode-api/tests/shape.rs` compares our response against the captured fixture: same keys, same value types. Its `null` branch (both directions) is deliberate — the fixture comes from someone else's account, where a field can be `null` simply because that user never filled it in. So a `null` on our side matches anything, and for these 18 fields the check degrades to "the key is present".

That is the correct behaviour for the helper and the reason this file exists: the divergence is invisible to the test suite forever, not by oversight.

## How we would find out

- **A plugin misbehaves against wakode but not against wakatime.com** on a screen that shows dates, times or theming. First thing to compare: this table against what the plugin reads.
- **The web frontend** will be the first real consumer, and it arrives sooner than this table suggests: the spec puts settings and a base16 theme system in wave 0 (`docs/superpowers/specs/2026-08-15-wakode-design.md`, §10), not in some later wave. When it needs `date_format` or `color_scheme`, that is the signal to give the setting a home in `users` and move the field from this table into group 1 — not to hardcode a default in the serializer.
- **A new capture** of `current.json` from a live account: if a field listed here starts arriving as `null` there too, it drops out of the table.
