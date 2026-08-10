# GUARDRAILS.md — Missed Call Respondr

**Rust Guardrails — Vibe Engineering Standard**

## Non-Negotiable
- No `unwrap()` or `expect()` in production code paths (config/env loading at startup is acceptable).
- Email templates: always validate `vars` keys exist before rendering — no silent empty strings.
- Database queries: all results must be handled with `?` or pattern matching — no silent failures.
- Twilio/webhook endpoints: validate request origin before processing.
- `cargo clippy -- -D warnings` must pass before any task is declared done.
- Build through `/usr/local/bin/swift-build.sh missedcall_respondr`.

## Verification Before Deploy
1. `cargo check`
2. `cargo clippy -- -D warnings`
3. `cargo test`
4. `sqlx migrate run`
5. `curl localhost:8088/api/health`
