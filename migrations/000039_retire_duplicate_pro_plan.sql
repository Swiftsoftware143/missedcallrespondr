-- 000039_retire_duplicate_pro_plan.sql
--
-- WHY (measured live 2026-10-17, cron `default`):
--   The customer's Plan & Billing screen renders EVERY active plan in the `plans` table as its
--   own tile with one price line built from `price_monthly` only. Two rows were active at the
--   same $49:
--
--     slug         name         price_monthly  price_yearly  features
--     pro          Pro          49.00          588.00        ["unlimited_leads", …]   <- ARRAY, no credits
--     pro-monthly  Pro Monthly  49.00          490.00        {included_credits: 5000, bonus_credits: 1500,
--                                                            max_phone_numbers: 5}
--
--   GET /api/v1/plans answered both (`ACTIVE $49 PLANS: 2 ['Pro','Pro Monthly']`), so a buyer saw
--   two identical "$49 / month" tiles. Pressing Upgrade on `Pro` charges $49 and seats a plan that
--   carries NO credit grant, while the marketing page (www/missedcall/index.html:223-226) sells
--   "Pro Monthly — $49/mo — 5,000 included credits every month".
--
--   `pro` is the LEGACY row: it predates the credit system (its `features` is still the pre-000008
--   array shape, it has no description, sort_order 0 from the original seed) and no tenant sits on
--   it (`select count(*) from tenant_plans where plan_id = <pro>` = 0). The live tier is
--   `pro-monthly`, the row migration 000008 seeded with the credit grant the feature resolver reads.
--
--   Retiring rather than deleting: `is_active = false` keeps the row for the operator's own Plans
--   table (where it now reads as inactive) and is reversible in one UPDATE, while `checkout_handler`
--   only seats `is_active = true` plans. Nothing is repriced and no tenant's plan moves.
--
-- IDEMPOTENT: this runner keeps no ledger and re-executes every file on every boot, so the UPDATE
-- must be safe to repeat (it is — setting an already-false flag is a no-op) and must not fail when
-- the row is absent. Scoped by slug only; a plan the operator has already retired stays retired.

UPDATE plans
   SET is_active = false,
       updated_at = NOW()
 WHERE slug = 'pro'
   AND is_active = true;

