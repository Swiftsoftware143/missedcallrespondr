-- kanban t_38b352f8 — the Telnyx ACCOUNT Ed25519 signing key must be settable where the operator
-- already configures Telnyx, not only in the deploy env.
--
-- WHY. `POST /api/v1/telnyx/webhook` fails CLOSED while no account public key is configured: every
-- inbound call / message event is answered `503 telnyx_verification_not_configured` and nothing is
-- applied, so the product cannot do the one thing it is sold to do (text back a missed caller /
-- queue a callback). On this deployment that key existed ONLY as `TELNYX_PUBLIC_KEY` in
-- /etc/swift/env/missedcall.env — a root-owned file plus a container restart — while every other
-- Telnyx value (`api_key`, `profile_id`, `messaging_profile_id`) is set from the operator console's
-- panel 5 "Telnyx Config & Numbers".
--
-- The column is ADDITIVE: `NOT NULL DEFAULT ''` means every existing row reads exactly as before,
-- and a deployment that sets `TELNYX_PUBLIC_KEY` in its environment is byte-identical in behaviour
-- because the ENV still wins (see `telnyx_handler::resolve_signing_key`).
--
-- Idempotent, like every other file in this runner (it re-executes each file on every boot and
-- keeps no ledger).
ALTER TABLE telnyx_config
    ADD COLUMN IF NOT EXISTS public_key TEXT NOT NULL DEFAULT '';

COMMENT ON COLUMN telnyx_config.public_key IS
    'Telnyx ACCOUNT Ed25519 public key (base64, 32 bytes) used to verify inbound webhook deliveries. Empty = not configured. TELNYX_PUBLIC_KEY in the deploy environment takes precedence when set.';
