# MissedCallRespondr — Admin Guide

## System Overview

MissedCallRespondr (MCR) handles call tracking, SMS messaging, the response rules that answer a
missed call, and the call-back follow-ups those rules queue. All transactional emails use the
database-backed template system.

There is NO voicemail capability, and as of kanban t_1d4fc956 there is no voicemail SURFACE either:
see the Voicemail row of [Module Handlers](#module-handlers). Nothing in the product records, stores,
transcribes or plays a voicemail, and no console screen and no guide may claim one.

## Quick Reference

- **Backend:** Rust (Axum) @ port 8088, systemd unit `missedcallrespondr`
- **Database:** PostgreSQL (docker: swift-postgres-1) — `missedcallrespondr`
- **Admin Web App:** Served via app backend on port 8088
- **Repo:** `/opt/swift/apps/missedcallrespondr/`

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
| API Keys | **DELETED (kanban t_f06b1710)** | The handler, its module and `POST|GET /api/v1/api-keys` + `PUT|DELETE /api/v1/api-keys/:id` are GONE. Measurement: NO auth path in this crate read `api_keys` (no `x-api-key` anywhere, no key-checking middleware; the only reader was `features::count_usage` for the plan quota), so a key minted by that route authenticated nothing anywhere — a credential no endpoint would accept. The table stays for history. |
| Call Logs | `call_log_handler` | `GET /api/v1/call-logs` still reads `call_logs` (the billing log with cost — its `recorded` column was dropped with the voicemail surface, kanban t_1d4fc956). `GET /api/v1/call-logs/export` now reads **`inbound_calls`** — the table the console's Calls screen lists — and emits that screen's own columns, so the CSV cannot disagree with the rows on screen (kanban t_f06b1710: it used to serve `call_logs` while the screen rendered `inbound_calls`; the webhook writes the two 1:1, but `POST /api/v1/calls` can add rows only the screen's table has) |
| Contacts | `contact_handler` | Contact management. The console's Contacts screen now creates (`POST /api/v1/contacts` — the only caller of the `max_contacts` gate), edits (`PUT /api/v1/contacts/:id`) and deletes (`DELETE /api/v1/contacts/:id`) rows (kanban t_f06b1710) |
| Custom Fields | `contact_custom_field_handler` | Custom contact fields |
| Dashboard | `dashboard_handler` | Stats and overview |
| Follow-ups | `follow_up_handler` | The call-back queue (`follow_ups`). Rows are written by `POST /api/v1/calls/:id/respond` and by a `callback` response rule, and the console's Follow-ups screen now also creates (`POST /api/v1/follow-ups` — the only caller of the `max_follow_ups` gate), closes (`PUT /api/v1/follow-ups/:id`, status `completed` + `completed_at`) and deletes (`DELETE /api/v1/follow-ups/:id`) rows. Its first column resolves `f.call_id` against `GET /api/v1/calls`; it used to read `f.contact_name || f.contact_id`, NEITHER of which exists on `FollowUp`, so every row painted `-` (kanban t_f06b1710) |
| Integrations | `integration_handler` | Third-party integrations |
| Messages | `message_handler` | SMS sending (Telnyx transport) + the message log |
| Message Templates | `message_template_handler` | Saved message texts (a library — nothing sends one; no `type` column exists) |
| Plans | `plans_handler` | Plan tier management |
| Portfolio | `portfolio_handler` | Multi-account management |
| Provider Keys | `provider_keys_handler` | Telnyx/etc provider keys |
| Response Rules | `response_rule_handler` (store) + `response_rule_eval` (the evaluator) | What the service does automatically on an inbound call |
| Settings | **DELETED (kanban t_f06b1710)** | The handler, its module, `models/setting.rs` and the routes `GET`/`PUT /api/v1/settings` are GONE. Measurement: `tenant_settings` had exactly one reader (the handler's own GET) and no writer outside the handler, so a Settings panel could only have saved keys nothing reads — a decorative control. The console's Profile screen is unchanged and writes `/api/v1/auth/profile` + `/api/v1/auth/password` |
| Telnyx | `telnyx_handler` | Telnyx API bridge + the number inventory: `GET /api/v1/telnyx/numbers` (ACTIVE rows only), `POST /api/v1/telnyx/numbers` (the `max_phone_numbers` gate; buys on the platform credential, or registers a number the tenant already owns when BYOK is on) and `DELETE /api/v1/telnyx/numbers/:id` (soft delete = release, frees the plan slot). The console's Phone Numbers screen now adds and releases (kanban t_f06b1710); BYOK itself is Pro/Enterprise-only, so on Free every add goes through the platform credential — with none saved the route answers 500 `Telnyx not configured by admin` and adds nothing |
| Triggers | `triggers_handler` | Trigger automation rules |
| Voicemail | **REMOVED (kanban t_1d4fc956 — the dead surface t_b4cbe8bc measured)** | The `voicemail_handler`, `models/voicemail.rs`, the `voicemails` table and all three routes (`GET /api/v1/voicemails`, `GET|PUT /api/v1/voicemails/:id`, `GET /api/v1/calls/:id/voicemail`) are GONE; migration `000026_retire_voicemails.sql` drops the table and its orphan columns on live. Why retire rather than wire: `INSERT INTO voicemails` = 0 hits anywhere, `SELECT count(*) FROM voicemails` = 0, the webhook handled ONLY `call_received`/`call_initiated` (so the Telnyx `record_start` on the handled arm was never captured — that command is retired too), there is NO speech-to-text integration in this crate, and no served screen ever read the table. Wiring would mean inventing an STT provider, a transcription-status vocabulary and a console screen for a surface no tenant can currently reach. The orphan writers went with it: `inbound_calls.recording_url`, `inbound_calls.voicemail_url` and `call_logs.recorded` (whose only writer hardcoded `false`). `POST|PUT /api/v1/calls` no longer accepts those body fields (unknown fields are ignored) |

**Affiliates are not a module of this app (kanban t_5deebeb1).** The affiliate system lives in
FunnelSwift; this app only *connects* to it, over two outbound `x-internal-key` wires, with no local
affiliate store involved: `plans_handler::notify_funnelswift_upgrade` POSTs
`{FUNNELSWIFT_URL}/api/v1/internal/affiliate/upgrade-event` when a tenant moves to a PAID plan, and
`checkout_handler` POSTs `{FUNNELSWIFT_URL}/api/v1/webhooks/conversion` when a checkout completes
with referral metadata. The former in-app `affiliates_handler` CRUD (and its `/api/v1/affiliates`
routes and admin-console panel actions) is retired.

## The served user guide (and the console it must match)

`www/guide.html` is a SERVED artifact, not just documentation: `bin/publish-missedcallrespondr-frontend.sh`
installs the repo copy byte-for-byte into the served roots, and the app's own marketing footer links it:

| repo source | served destination(s) |
|---|---|
| `www/guide.html` | `/opt/swift/nginx/www/missedcall/guide.html` (missedcallrespondr.com/guide) **and** `/opt/swift/nginx/www-app/missedcall/guide.html` (app.missedcallrespondr.com/guide) |

**The console is the spec.** Every screen claim in the guide must be checkable against the served
tenant shell `www-app/dashboard/index.html`, whose nav is exactly
`Overview / Calls / Phone Numbers / Response Rules / Templates / Messages / Follow-ups / Contacts /
Tickets / Integrations / Profile`. A guide step that names a control that shell does not render is a
defect in the guide (kanban t_b4cbe8bc: the whole Call Log filter/detail section and the whole
Voicemails section did exactly that, and were retired).

Screens with write controls, so the guide may promise them (kanban t_f06b1710): **Calls** (table +
Refresh + **Export CSV** + per-row Respond), **Phone Numbers** (+ Add Number, per-row Release),
**Response Rules** (add/edit/delete), **Templates** (add/edit/delete), **Messages** (Send),
**Follow-ups** (+ New Follow-up, Mark done, Delete), **Contacts** (+ New Contact, per-row Fields /
Edit / Delete, custom-field definitions), **Tickets** (+ New Ticket, Edit), **Integrations**
(connect/disconnect/test), **Profile** (Save profile, Update password).

Still read-only by design: **Overview** (counters + credits) and the **Calls** row detail — there is
no search box, no status filter, no date range and no call detail panel on Calls.

## Outbound SMS (the send path)

`POST /api/v1/messages` (kanban t_2ed95642) is the tenant console's Send Message form and the only
writer of `messages` rows. It now transmits: an `outbound` message calls
`POST https://api.telnyx.com/v2/messages` with the stored `telnyx_config.api_key` +
`messaging_profile_id` and the caller's own ACTIVE `phone_numbers` number as `from`.

- **Configuration.** Admin console -> *Telnyx Config & Numbers* -> *Save Telnyx config*
  (`PUT /api/v1/admin/telnyx-config`, fields `api_key`, `profile_id`, `messaging_profile_id`). With
  no `api_key` or no `messaging_profile_id` the route answers `503`
  (`Text delivery is not configured: …`) and writes **no** row — nothing was attempted, so there is
  nothing to record.
- **The stored status is the provider's answer, never the request.** `queued` on an accepted send;
  `sent` / `delivered` only when Telnyx's own `to[].status` says so; `failed` on a refusal.
  `sent_at` is NULL until the provider reports a hand-off and `delivered_at` stays NULL until a
  delivery event says otherwise. A word this app has not seen is stored as `queued` (accepted,
  nothing claimed).
- **A refusal is stored before the caller is told.** A non-2xx answer (or a failure status inside a
  2xx) becomes a `failed` row carrying Telnyx's own words, and the caller gets **424** with that
  reason; the console shows it and reloads the log. `provider_message_id` holds Telnyx's message id.
  (424 Failed Dependency, not 502: Cloudflare replaces an origin **502/504** with its own error page
  — measured on this host 2026-10-02 — so a 502 would hand the operator a JavaScript parse error
  instead of Telnyx's sentence. 4xx bodies pass through the edge verbatim.)
- **Delivery events.** `POST /api/v1/telnyx/webhook` (public) applies the MESSAGE events —
  `message.sent` moves a row to `sent`, `message.finalized` to `delivered` (setting `delivered_at`)
  or `failed` — matched by `provider_message_id`. A `delivered` row is terminal; an event whose id
  matches no row changes nothing (including every `message.received`, which this app does not store).
- **`TELNYX_API_BASE`** overrides the API host (default `https://api.telnyx.com`), exactly as
  `PAYPAL_API_BASE` does for the PayPal verify call. It exists for acceptance runs against a stub
  server; unset behaviour is byte-identical to the production host.
- **Inbound** is a recording arm, not a transport: a hand-recorded `direction=inbound` row is stored
  `logged` with `sent_at` NULL and nothing is transmitted.

## Response Rules (the call-path evaluator)

`response_rules` (kanban t_31f9cf38) is **evaluated**, not just stored. Before this change it was
CRUD-only: the consoles offered the screen and both guides promised "the rule is evaluated on every
inbound call", while no inbound-call path read the table — no rule had ever fired.

- **Where it runs.** `POST /api/v1/telnyx/webhook` (`telnyx_handler::webhook`), on
  `call_received`/`call_initiated`, after the tenant is resolved by the CALLED number, the credit is
  taken and `inbound_calls` / `call_logs` are written. The evaluation is SPAWNED (like the CoreSwift
  lead push), so a slow provider call can never delay the call-control answer Telnyx is waiting for.
- **Order.** `SELECT … WHERE tenant_id = $1 AND is_active = true ORDER BY priority ASC, created_at
  ASC`. The FIRST rule whose trigger matches fires and the rest are skipped for that call; no match
  means no action. `priority` was added by migration 000025 (`INTEGER NOT NULL DEFAULT 100`); the
  console's list is ordered the same way, so the screen shows the evaluation order.
- **Triggers** (`trigger_condition`): `all_missed_calls` (every inbound call — this service records
  every ring as missed, then answers the call and gathers a digit; no recording is requested any more
  and none is ever stored, see the Voicemail row of Module Handlers, kanban t_1d4fc956),
  `specific_numbers`
  (`schedule.numbers`, matched on digits with a >= 7-digit floor so a 10-digit local form matches the
  E.164 caller), `time_of_day` (`schedule.window.start/end`, "HH:MM", a window may cross midnight),
  `day_of_week` (`schedule.days`, `mon`…`sun`, UTC — the clock the store uses).
- **Actions** (`response_type`): `sms` texts the caller from the number they dialed, through the SAME
  transport as the manual send (`message_handler::send_rule_sms` → `deliver_outbound`), so the row in
  `messages` carries the provider's own status/timestamps. `callback` inserts a `follow_ups` row
  (`follow_type='call_back'`, due in an hour, `pending`) — the same record `POST /api/v1/calls/:id/respond`
  writes, listed on the console's Follow-ups screen. Nothing dials by itself.
- **`email` and `voice` are refused** (`400`, naming the field): an inbound call carries no email
  address to reply to, and this service has no outbound-call transport. The consoles no longer offer
  them; `CREATE`/`UPDATE` validate the whole stored shape (`response_rule_eval::validate_rule`), so a
  rule that could never fire cannot be stored (an empty SMS body included).
- **What a text needs.** The same `telnyx_config` credential as the manual send. With it missing the
  rule still fires and is logged, but no row is written — nothing was attempted. The console's
  Response Rules screen and the user guide both say so.
- **Plan limit.** `max_rules` is counted from this table and enforced on `POST /api/v1/response-rules`.
  The automatic reply itself is NOT run through `max_messages`: dropping a missed-call reply mid-ring
  would break the product's one promise with nothing to show for it, and the row still counts towards
  the tenant's total.
- **Templates are not involved.** `message_templates` is a library of saved text; a rule holds its own
  `response_content.text` and no placeholder is substituted anywhere in the sent message.

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
