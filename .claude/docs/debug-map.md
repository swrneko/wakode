# Debug map

Where to look first, by symptom. Add a row whenever an investigation costs more than it should have.

| Symptom | Look here first |
|---|---|
| Test suite red about once in four runs, no code change | Journal tests outside `crates/wakode-api/tests/log.rs`. `tracing` caches callsite Interest globally per process; neighbours in `api.rs` poison it. |
| Heartbeats lost on restart | `crates/wakode/src/signal.rs` and `main.rs::run` — did `store.shutdown()` actually run? An early `?` between `startup::start` and `shutdown` skips it. Broken twice already. |
| Setup screen refuses on the owner's own machine | `crates/wakode-api/src/setup.rs::address_allows_setup`. Any of six proxy headers present ⇒ refusal, by presence, not content. |
| Setup screen open to the internet | Same function. `ConnectInfo` is the TCP peer: behind a same-host reverse proxy it is always `127.0.0.1`. |
| Secret in the log | `crates/wakode-api/src/lib.rs::request_span` writes `path`, not `uri`. A route carrying a secret in the path would leak past every check in that file. |
| `Debug` printed a secret | `crates/wakode-auth/src/lib.rs`, secrets policy. Every such type has a hand-written `Debug` plus a test comparing the **exact** string — substring searches were green on a leaked secret three times. |
| Writer stops responding, everything returns `WriterGone` | `crates/wakode-store/src/writer.rs`. A panic inside the insert is caught and turns into `TaskPanicked`, but a poisoned interner `RwLock` keeps failing every subsequent job. |
| `405` with an empty body | A route added *below* `method_not_allowed_fallback` in `router()`. New routes go above it. |
| A test passes but proves nothing | Mutate the line it claims to guard and rerun. This project's dominant defect class; see the conventions in `AGENTS.md`. |
