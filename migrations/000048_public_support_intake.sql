-- 000048_public_support_intake.sql — a visitor who cannot sign in can reach the team.
--
-- MEASURED 2026-11-06 (dns.google and cloudflare-dns.com agree): MX = 0 for missedcallrespondr.com,
-- swiftsoftware.net, funnelswift.net, adaswift.com, incentiveswift.com, workflowswift.com and
-- coreswiftcrm.com, and nothing on this host listens on port 25. Every "email support@..." sentence
-- this app printed — the call-credit note, the text-delivery notes on the dashboard, the guide, the
-- suspension message and every transactional mail footer — therefore sent a customer's message into
-- a bounce. A locked-out customer had no way back in.
--
-- The served contact page now files the message into the operator's existing ticket queue instead:
-- POST /api/v1/public/support-request writes ONE ticket with no workspace (`tenant_id` NULL), the
-- address the visitor typed in `requester_email`, and the message as the thread's first entry. The
-- operator answers it from the same queue they already work, and the reply is mailed to that address.
--
-- Idempotent by construction: this app's boot runner re-reads ./migrations on EVERY start and keeps no
-- ledger, so each statement must be safe to re-run. DROP NOT NULL is a no-op once the column is
-- nullable; ADD COLUMN IF NOT EXISTS and CREATE INDEX IF NOT EXISTS are no-ops after the first boot.
ALTER TABLE tickets ALTER COLUMN tenant_id DROP NOT NULL;

-- The address to ANSWER. A visitor has no `users` row and no `contacts` row, so neither existing key
-- can carry it; the operator's queue shows this column and the reply mail is sent to it.
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS requester_email text;

-- The public arm counts recent messages from one address to refuse a fourth within the hour, and the
-- operator's queue resolves a row by it — both read lower(requester_email).
CREATE INDEX IF NOT EXISTS idx_tickets_requester_email
    ON tickets (lower(requester_email))
    WHERE requester_email IS NOT NULL;

-- The reply wording for someone who is NOT a customer: a visitor has no dashboard and no account, so
-- the customer notice (`ticket_reply`, which tells the reader to open /dashboard) would send them to a
-- screen they cannot reach. This row says what they can actually do — write again from the same page.
-- TEXT-ONLY like its sibling: the reply body is operator-authored free text and the renderer does no
-- escaping, so an HTML part built from it could be broken by a `<`. With no html part the sender's own
-- branding step turns the ESCAPED text into the HTML twin.
INSERT INTO email_templates (template_type, name, subject, body, html_body, is_html, is_default, aid)
SELECT
    'support_reply',
    'Visitor reply notice',
    'We answered your message: {{ticket_subject}}',
    E'Hi {{name}},\n\nThank you for contacting {{app_name}}.\n\nWe answered your message "{{ticket_subject}}":\n\n{{reply_body}}\n\nIf you need anything else, write to us again from {{app_url}}/contact.html and we will pick it up.\n\n- The {{app_name}} Team',
    NULL,
    false,
    true,
    NULL
WHERE NOT EXISTS (
    SELECT 1 FROM email_templates
    WHERE template_type = 'support_reply' AND aid IS NULL AND is_default = true
);
