-- The TOP plan must actually work (measured 2026-11-18, cron default).
--
-- Enterprise's `feature_limits` rows grant every ceiling as the fleet's unlimited sentinel -1
-- (`max_calls`, `max_phone_numbers`, `max_rules`, `max_deals`, …) but carried NO call-credit grant
-- at all. The meter (`telnyx_handler::deduct_credit`) only spends while `credit_balance > 0`, and
-- NO operator door ever granted a balance — `plans_handler::seat_plan_for_tenant` and
-- `plan_seating::seat_unseated_workspaces` both inserted the row without the credit columns — so an
-- Enterprise workspace created by the panel's *Assign plan* sat at 0 and its calls were hung up.
-- Live state that proved it: two real Enterprise customers (BV Premiere Products, Xride Drivers)
-- and one portfolio tenant all at `credit_balance = 0` on the $199 plan.
--
-- `included_credits = -1` is the SAME sentinel the plan already uses for every other ceiling, read
-- through the same `resolve_limit` the console's allowance tile uses (which renders a negative cap
-- as "unlimited"). The meter now answers "allowed" without spending a balance it does not have.
--
-- Idempotent: this app's runner re-executes every file on every boot, and the unique index
-- `feature_limits_plan_id_feature_key_key` makes a repeat a no-op.
INSERT INTO feature_limits (id, plan_id, feature_key, limit_value)
SELECT gen_random_uuid(), p.id, 'included_credits', -1
  FROM plans p
 WHERE p.slug = 'enterprise'
   AND NOT EXISTS (
         SELECT 1 FROM feature_limits fl
          WHERE fl.plan_id = p.id AND fl.feature_key = 'included_credits')
ON CONFLICT (plan_id, feature_key) DO NOTHING;
