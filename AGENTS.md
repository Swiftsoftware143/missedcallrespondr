# AGENTS.md — Vibe Engineering Rules for AI Agents

## Rust Guardrails (MANDATORY)
- **Zero unsafe blocks** unless explicitly approved by the Lead Architect
- **Zero .unwrap() or .expect()** in non-test production code — use `thiserror`/`anyhow`
- **All async state must implement Send + Sync**
- **Parameterized SQL only** — use `sqlx::query_as!` for compile-time validation
- **Secrets in env vars only** — never hardcoded
- **cargo fmt** before commit

## Verification Sequence (NON-NEGOTIABLE)
After ANY code change:
1. `cargo check` — syntax + borrow checker. Read stderr. Fix. Repeat until clean.
2. `cargo test` — all tests must pass
3. `cargo clippy -- -D warnings` — zero warnings tolerated
4. `cargo fmt -- --check` — formatting must be consistent

## Self-Correction Loop
- Compiler error → read diagnostic → understand → fix → re-compile
- Test failure → fix logic → re-run
- Clippy warning → clean up → re-run
- **NEVER paste errors to a human. FIX THEM.**
- 3 attempts max, then escalate with evidence of what you tried.

## Hermes Delegation Pattern
For complex feature implementation:
1. Draft trait signatures and types FIRST
2. Run `cargo check` to validate types before writing method bodies
3. Then implement method logic — iterate with check/test/clippy
4. Re-run full verification before declaring done

## Build Lock Protocol
- ALWAYS use `/opt/swift/build-lock.sh <app> <command>`
- Never raw `cargo build --release` on shared repos
- Exit 2 = another bot building → wait 30s, retry once
- Stale lock >30min: clear and proceed

## Post-Deploy Smoke Test
- `curl -s -o /dev/null -w "%{http_code}" <domain>` must return 200

## Project File Architecture
```
src/auth/handlers.rs
src/auth/middleware.rs
src/auth/mod.rs
src/auth/models.rs
src/config.rs
src/db.rs
src/email.rs
src/error.rs
src/features.rs
src/handlers/admin_handler.rs
src/handlers/affiliates_handler.rs
src/handlers/api_key_handler.rs
src/handlers/calendar_events_handler.rs
src/handlers/call_handler.rs
src/handlers/call_log_handler.rs
src/handlers/campaigns_handler.rs
src/handlers/checkout_handler.rs
src/handlers/clients_handler.rs
src/handlers/contact_custom_field_handler.rs
src/handlers/contact_handler.rs
src/handlers/coreswift_push.rs
src/handlers/dashboard_handler.rs
src/handlers/deals_handler.rs
src/handlers/email_templates_handler.rs
src/handlers/export_templates_handler.rs
src/handlers/follow_up_handler.rs
src/handlers/import_logs_handler.rs
src/handlers/integration_handler.rs
src/handlers/integration_target_handler.rs
src/handlers/leads_handler.rs
src/handlers/message_handler.rs
src/handlers/message_template_handler.rs
src/handlers/mod.rs
src/handlers/plans_handler.rs
src/handlers/portfolio_handler.rs
src/handlers/portfolio_sync_handler.rs
src/handlers/provider_keys_handler.rs
src/handlers/response_rule_handler.rs
src/handlers/settings_handler.rs
src/handlers/tag_groups_handler.rs
```
