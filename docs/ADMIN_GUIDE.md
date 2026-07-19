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
- **template_type** — `welcome` / `purchase_confirmed` / `password_reset`
- **subject** — email subject (supports `{{variable}}`)
- **body** — plain text body
- **html_body** — HTML body
- **is_html** — HTML or plain text delivery
- **is_default** — fallback for this template type

### Delivery Flow

1. Triggering event (account created, payment received, reset requested)
2. `send_template_email()` called with template type + variable map
3. DB lookup by type (tenant-scoped → default)
4. `{{variable}}` placeholders rendered from map
5. Fallback to hardcoded inline if no DB template exists
6. Email queued to `outbound_messages` for async SMTP send

### Default Seeds

Three templates seeded: Welcome Email, Purchase Confirmation, Password Reset.

## Module Handlers

| Module | Handler | Description |
|---|---|---|
| Affiliates | `affiliates_handler` | Affiliate/commission tracking |
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

## Monitoring

- Logs: `journalctl -u missedcallrespondr -n 100 --no-pager`
- Health: `curl http://localhost:8088/api/health`
- DB: `docker exec -it swift-postgres-1 psql -U swift -d missedcallrespondr`

## Deployment

```bash
cd /opt/swift/MissedCallRespondr
export CARGO_BUILD_JOBS=1
cargo build --release
systemctl restart missedcallrespondr
```
