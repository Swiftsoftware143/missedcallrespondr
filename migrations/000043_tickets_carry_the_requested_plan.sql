-- 000043 — a plan request carries the tier it asks for, and records when the switch was made.
--
-- WHY (cron 2026-10-20, measured live): the customer's Plan & Billing screen files a plan request as a
-- ticket and an operator "resolves" it. The resolve moved the STATUS and nothing else: the customer was
-- told their request was handled while `tenant_plans` still seated the free plan. Before-fix
-- measurement: filed a request for Pro, resolved it as an operator, `/auth/me` still answered
-- plan_name "Free".
--
-- `plan_slug` is the catalogue slug the request is for; the handler validates it against `plans` on the
-- way in, so an unknown tier is refused at filing time instead of becoming a request nobody can match.
-- `plan_applied_at` records when an operator's resolve actually seated the plan, which is what keeps a
-- second resolve (or a following "closed") from re-seating the same tier.
--
-- This runner keeps NO ledger and re-executes every file on each boot, so both statements are
-- idempotent.
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS plan_slug text;
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS plan_applied_at timestamptz;

COMMENT ON COLUMN tickets.plan_slug IS
  'plans.slug a plan request asks for (cron 2026-10-20). NULL for an ordinary support ticket.';
COMMENT ON COLUMN tickets.plan_applied_at IS
  'When an operator''s resolve seated plan_slug on the workspace. NULL = not applied yet.';
