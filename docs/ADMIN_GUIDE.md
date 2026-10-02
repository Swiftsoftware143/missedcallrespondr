# MissedCallRespondr — Admin Guide

## System Overview

MissedCallRespondr (MCR) handles call tracking, SMS/MMS messaging, voicemail automation, and follow-up sequences triggered by missed calls. All transactional emails use the database-backed template system.

## Quick Reference

- **Backend:** Rust (Axum) @ port 8088, systemd unit `missedcallrespondr`
- **Database:** PostgreSQL (docker: swift-postgres-1) — `missedcallrespondr`
- **Admin Web App:** Served via app backend on port 8088
- **Repo:** `/opt/swift/MissedCallRespondr/`

## Email Templates

All transactional emails use the `email_templates` table with `{{variable}}` placeholder support.

### Template Types

| Type | When Sent | Merge Fields |
|---|---|---|
| `welcome` | New account created | `{{name}}`, `{{email}}`, `{{password}}`, `{{app_url}}` |
| `purchase_confirmed` | Successful payment | `{{name}}`, `{{plan_name}}`, `{{app_url}}` |
| `password_reset` | Password reset request | `{{name}}`, `{{token}}`, `{{app_url}}` |

### API Endpoints

| Method | Path | Description |
|---|---|---|
| GET | `/api/email-templates` | List all templates |
| POST | `/api/email-templates` | Create a template |
| GET | `/api/email-templates/:id` | Get a template |
| PUT | `/api/email-templates/:id` | Update a template |
| DELETE | `/api/email-templates/:id` | Delete a template |

### Template Fields

- **name** — display label
- **template_type** — `welcome` / `welcome_credentials` / `purchase_confirmed` / `password_reset`
  - `welcome` is the self-serve signup mail: the account owner just chose the password, so this
    template gets no `password` value and its default row carries no `{password}` placeholder.
  - `welcome_credentials` is the welcome mail for flows that GENERATE the password (checkout,
    where the email is the only place the customer can learn it). This is the one welcome type
    whose default row contains the `{password}` block.
- **subject** — email subject (placeholders are written `{variable}`, e.g. `{app_name}`)
- **body** — plain text body
- **html_body** — HTML body
- **is_html** — HTML or plain text delivery
- **is_default** — fallback for this template type

### Delivery Flow

1. Triggering event (account created, payment received, reset requested)
2. `send_template_email()` called with template type + variable map
3. DB lookup by type (tenant-scoped → default)
4. `{variable}` placeholders rendered from map
5. Fallback to hardcoded inline if no DB template exists
6. Delivery through the configured provider (see **Email Provider**), which returns the provider's
   own receipt; a failure is logged at ERROR and recorded on the admin surface

### Default Seeds

Three templates seeded: Welcome Email, Purchase Confirmation, Password Reset.

## Email Provider (system mail)

Every transactional email leaves the box through ONE provider config, editable in the admin panel
(**Operator Console → 13. Email Provider (system mail)**, `admin.missedcallrespondr.com`) so it is
manageable from a phone.

**Resolution order:** `admin_settings.email` (the panel row, key `email`) **first**, then the
`EMAIL_API_URL` / `EMAIL_API_KEY` / `EMAIL_FROM` environment variables (a stopgap; the panel row is
the standard). The row's `api_key` is sealed at rest (`enc:v1:`), and a masked key returned by GET can
be saved back without clobbering the stored credential.

**Body shape is per provider** — this is not cosmetic: Mailgun takes
`application/x-www-form-urlencoded` (with `Authorization: Basic api:<key>`), while SendGrid takes JSON
`{"from":{"email":…}}` with a Bearer token. Sending JSON to Mailgun authenticates fine and then
answers `400 {"message":"from parameter is missing"}`; that defect (kanban t_6d575da6) is why the
welcome/credentials mail could never be sent. Values are parameter-encoded, so a From or Subject
carrying `&` or `=` arrives intact.

| Method | Path | Description |
|---|---|---|
| GET | `/api/v1/admin/email-config` | current config (secrets masked), live `source`, supported providers, last send |
| PUT | `/api/v1/admin/email-config` | save — **merges** into the stored row, so a blank field is left alone (`""` clears it) |
| POST | `/api/v1/admin/email-config/test` | send a real message to the calling admin's address; returns the provider's true answer |

Providers offered: `mailgun`, `sendgrid`, `sendiio` — exactly the arms the app can deliver through; a
save of any other value is rejected rather than stored as a dead setting.

`admin_settings.email_last_send` records the outcome of the last **system-mail** send (kind, recipient,
provider, receipt) and is shown by the GET above, so "the customer never got the mail" is answerable
without reading container logs. A deliberate *test* send reports its result inline and does not
overwrite that row. A failed credential mail is never fatal to account creation — the loud
`account created but the WELCOME/CREDENTIALS EMAIL FAILED` line plus `email_last_send` are the signal.

## Plans, tiers and the plan feature registry

Four live tiers: **Enterprise** (the TOP tier), Pro, Pro Monthly and Free. "Top" is established from
LIVE data, not from the name — the first ACTIVE row in the panel's own order (`sort_order`, then
`price_monthly`, then `price`, then slug). The registry catalogue reports the winner as `top_plan`.

**The standing rule (David, 2026-09-23): the top tier gets everything.** Every key the app's feature
registry defines must be granted on the top tier. Two keys were granted by no plan at all before
this was applied, which meant the gate REFUSED the top tier: `has_calendar` (the Calendar flag) and
`bring_your_own_key` (Bring Your Own Telnyx Key).

### Where the plan × feature controls are

Operator console → **"3b. Plan Feature Registry (every gated feature × every plan)"**. It lists every
registry key with its current value for EVERY plan in one table, plus the columns `what`, `kind`,
`if_unset` (what a missing value means) and `enforced_by` (the route that enforces it).

Three controls:

| Control | What it does |
|---|---|
| **Set plan feature** | Writes one registry key on one plan. Fields: `plan` (slug, e.g. `enterprise`), `feature` (a key from the `feature` column), `value`. The response echoes the value the gate now resolves. |
| **Grant the TOP tier every missing registry key** | Re-applies the standing rule, gap-filling only: a key the top tier already grants is left untouched (a cap you set is never raised to unlimited by pressing it). Safe to press again — a second press grants nothing. |
| **Show the raw catalogue (JSON)** | The same data as JSON: `registry` (the keys), `plans` (values + `sources`), `grant_matrix`, `superset_ok`/`superset_violations`. |

### The value vocabulary (identical in the panel, the API and the gate)

* **`-1`** = unlimited / granted
* **`0`** = NOT available on this plan — the gate refuses the action (402)
* **`N` > 0** = a cap of N (the gate refuses once usage reaches N)
* On/off features (`has_calendar`, `bring_your_own_key`): non-zero = on, `0` = off

**`if_unset` matters.** A key with no value at all is NOT a denial: a *limit* with no row is allowed
(the gate is inert), while an *on/off* feature with no row is REFUSED. That asymmetry is why
`has_calendar` refused every tier: nobody had granted it anywhere.

Plan keys are stored in two places and the panel writes the right one for you: `feature_limits`
(one row per plan × key — the panel-managed grant) for most keys, and the plan's own column
(`max_leads`) where the plan model has one. An explicit `features."<key>"` override written with
"Set plan features (JSON)" resolves BEFORE a column, so if one exists the Set-plan-feature response
returns a `warning` telling you so.

### "Set plan features (JSON)" — object-shaped plans only

That action merges raw JSON into `plans.features`. Enterprise and Pro carry `features` as a JSON
**ARRAY** of marketing tags, and merging an object into an array appends an element no gate can read
— so the action now answers **400** instead of pretending to save. Grant registry keys with
**Set plan feature** in section 3b.

### What the registry does NOT cover

Nothing advertised, as of t_b578b169. The three plan limits that no gate read are now resolved:

| key | where its value lives | state |
|---|---|---|
| `max_tags` | `plans.max_tags` column (Free 10 / Pro 50 / Pro Monthly 50 / Enterprise -1) | **enforced** on `POST /api/v1/tags` — the 11th tag on Free is refused with 402 `Tags limit reached (10/10)…`. Releasing a tag (DELETE is a hard delete) frees the slot. Set it with **Set plan feature** (storage `plans.max_tags`). |
| `max_phone_numbers` | `plans.features` (Free 1 / Pro Monthly 5; Pro and Enterprise declare nothing, so unset ⇒ allowed) | **enforced** on `POST /api/v1/telnyx/numbers`, before the provider is called. Counts ACTIVE numbers only, so a released number frees the slot. Set it with **Set plan feature** (storage `feature_limits`). |
| `max_users` | `plans.features->>'max_users'` (Free 1 / Pro Monthly 5) | **retired** — the value was removed from the plan data (migration `000022`). This app has no surface that adds a user to an existing tenant (every signup/provisioning path creates a tenant's FIRST user, before a plan is attached), so a seat cap could never be reached. `GET /api/v1/me/usage` still reports the live user count; no plan pretends to cap it. If seat-selling is wanted later it needs a team-invite surface first, then a registry key. |

A plan value no gate reads is a setting, not a control — if you add a limit to the plan model, add it
to `src/feature_registry.rs` and call the gate on the route that ADDS the counted row, or the two
anti-drift tests in that file will fail the build (that is deliberate).

## Module Handlers

| Module | Handler | Description |
|---|---|---|
| API Keys | `api_key_handler` | API key management |
| Call Logs | `call_log_handler` | Inbound/outbound call records |
| Contacts | `contact_handler` | Contact management |
| Custom Fields | `contact_custom_field_handler` | Custom contact fields |
| Dashboard | `dashboard_handler` | Stats and overview |
| Follow-ups | `follow_up_handler` | Automated follow-up rules |
| Integrations | `integration_handler` | Third-party integrations |
| Messages | `message_handler` | SMS/MMS handling |
| Message Templates | `message_template_handler` | SMS template CRUD |
| Plans | `plans_handler` | Plan tier management |
| Portfolio | `portfolio_handler` | Multi-account management |
| Provider Keys | `provider_keys_handler` | Telnyx/etc provider keys |
| Response Rules | `response_rule_handler` | Auto-response logic |
| Settings | `settings_handler` | Account settings |
| Telnyx | `telnyx_handler` | Telnyx API bridge |
| Triggers | `triggers_handler` | Trigger automation rules |
| Voicemail | `voicemail_handler` | Voicemail detection + handling |

**Affiliates are not a module of this app (kanban t_5deebeb1).** The affiliate system lives in
FunnelSwift; this app only *connects* to it, over two outbound `x-internal-key` wires, with no local
affiliate store involved: `plans_handler::notify_funnelswift_upgrade` POSTs
`{FUNNELSWIFT_URL}/api/v1/internal/affiliate/upgrade-event` when a tenant moves to a PAID plan, and
`checkout_handler` POSTs `{FUNNELSWIFT_URL}/api/v1/webhooks/conversion` when a checkout completes
with referral metadata. The former in-app `affiliates_handler` CRUD (and its `/api/v1/affiliates`
routes and admin-console panel actions) is retired.

## Monitoring

- Logs: `journalctl -u missedcallrespondr -n 100 --no-pager`
- Health: `curl http://localhost:8088/api/health`
- DB: `docker exec -it swift-postgres-1 psql -U swift -d missedcallrespondr`

## Payments and the PayPal webhook receiver

- Checkout sessions: `/api/v1/checkout/create`, `/api/v1/checkout/sessions`; providers are
  configured via `/api/v1/payment-providers` (admin console -> Payment providers).
- `POST /api/v1/webhooks/paypal` is **signature-verified before anything is written or dispatched**
  (kanban t_5cf44e1b). The four `paypal-transmission-*` headers are required and the signature is
  checked against PayPal's `verify-webhook-signature` API, authenticated with the REST
  `client_id:client_secret` and verified against `PAYPAL_WEBHOOK_ID` (or the `webhook_secret` of the
  active `paypal` provider row — no redeploy needed). Fail-closed replies, in order:
  `401 missing_paypal_signature_headers`, `503 paypal_not_configured` (no webhook id or no
  credential: PayPal is **not** called and nothing is written),
  `401 paypal_verification_api_error` / `401 paypal_verification_unreachable` (the verdict itself
  could not be obtained), `401 signature_verification_failed` (a real FAILURE verdict). Only a
  verified event reaches `payment_webhook_events` and fulfilment.

## Deployment

The binary is IMAGE-BAKED (no bind mount), so `systemctl restart` / `docker restart` re-runs the
OLD binary and is **not** a deploy:

```bash
/opt/swift/bin/deploy-missedcallrespondr.sh        # rebuilds image, recreates the container,
                                                   # proves sha256(repo) == sha256(container)
/opt/swift/bin/deploy-missedcallrespondr.sh --verify   # parity + health only
```
