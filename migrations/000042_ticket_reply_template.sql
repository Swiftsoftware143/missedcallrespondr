-- 000042_ticket_reply_template.sql — the customer is TOLD when an operator answers their ticket.
--
-- Measured 2026-10-19: `ticket_messages` was written only by `tickets_handler::create()` (the opening
-- description), so neither console could hold a conversation and a customer who asked for a plan
-- change — the Plan & Billing screen files exactly that ticket — had no way to hear back except
-- reloading and guessing. The handler now emails the tenant's own address when an operator replies.
--
-- This row makes that notice EDITABLE on the admin Email Templates screen instead of living only in
-- the compiled-in fallback. It is deliberately TEXT-ONLY (`is_html = false`, `html_body` NULL): the
-- reply body is operator-authored free text and the renderer does no escaping, so an HTML part built
-- from it could be broken (or injected into) by a `<` in a sentence. With no html part, the sender's
-- own branding step turns the ESCAPED text into the HTML twin, so a branded account still gets the
-- logo and the colour rule.
--
-- Idempotent: this app's boot runner re-reads ./migrations on every start and keeps NO ledger, so the
-- statement must be safe to re-run. It matches the same partial unique key the table already carries
-- (`template_type, COALESCE(aid,'000…'), is_default where aid IS NULL and is_default`).
INSERT INTO email_templates (template_type, name, subject, body, html_body, is_html, is_default, aid)
SELECT
    'ticket_reply',
    'Ticket reply notice',
    'We answered your request: {{ticket_subject}}',
    E'Hi {{name}},\n\nWe answered your request "{{ticket_subject}}".\n\n{{reply_body}}\n\nOpen {{app_url}}/dashboard and go to Tickets to reply or see the whole thread.\n\n- The {{app_name}} Team',
    NULL,
    false,
    true,
    NULL
WHERE NOT EXISTS (
    SELECT 1 FROM email_templates
    WHERE template_type = 'ticket_reply' AND aid IS NULL AND is_default = true
);
