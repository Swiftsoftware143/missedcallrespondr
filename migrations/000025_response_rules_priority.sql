-- kanban t_31f9cf38 — the Response Rules evaluator.
--
-- `response_rules` was CRUD-only: nothing has ever read a rule on an inbound call, while the tenant
-- console, the admin console and both guides promised "the rule is evaluated on every inbound call"
-- and "Rules are evaluated in priority order (1 = highest). The first matching rule fires and its
-- action is executed."
--
-- The evaluator now runs (src/handlers/response_rule_eval.rs), and it needs the one field the
-- documented order was always written in terms of: a priority. Additive and idempotent; the default
-- 100 is the documented "no preference" position, so every pre-existing rule keeps a total order
-- decided by created_at.
ALTER TABLE response_rules ADD COLUMN IF NOT EXISTS priority INTEGER NOT NULL DEFAULT 100;

-- The evaluation read is (tenant, active, priority, created_at) — give it an index so an inbound
-- call's rule lookup does not scan a tenant's whole rule set on every ring.
CREATE INDEX IF NOT EXISTS idx_response_rules_eval
    ON response_rules (tenant_id, priority, created_at);
