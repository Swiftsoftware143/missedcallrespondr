-- MissedCallRespondr — the plans table kept its price in TWO columns and only ever filled one of them.
--
-- MEASURED LIVE 2026-10-03, before touching anything:
--
--   name        | price | price_monthly | price_yearly | is_active
--   Enterprise  |   199 |          0.00 |         0.00 | t         <- Aug 8
--   Pro         |    49 |          0.00 |         0.00 | t         <- Aug 8
--   Free        |     0 |          0.00 |         0.00 | t         <- Aug 8
--   Pro Monthly |     0 |         49.00 |       490.00 | t         <- Aug 19
--
-- Two generations of rows, each filling ONE of the two columns and leaving the other at zero. And the app
-- reads them in DIFFERENT places:
--
--   * `plans_handler.rs` lists plans with `price_monthly::float8` -> Pro and Enterprise DISPLAY $0.
--   * `plans_handler.rs:374` selects `price::float8 AS price` and `:900` does
--     `let plan_price = price.unwrap_or(0.0)` -> the PURCHASE path reads the other column, so
--     "Pro Monthly" would CHECK OUT AT $0.
--
-- Neither half is a cosmetic defect. A pricing page showing $0 for a $199 plan, and a checkout willing to
-- charge nothing, are the same root cause: one value stored in two columns that disagree.
--
-- THE FIX makes them agree, per row, using GREATEST so that:
--   * a row that filled either column gets that value in BOTH;
--   * Free, whose two columns are both legitimately 0, is LEFT AT 0 and does not become chargeable;
--   * price_yearly is only derived from an annual-equivalent month (x12) when it was blank, so the $490
--     already set on "Pro Monthly" is preserved rather than overwritten.
--
-- Nobody is affected financially: `tenant_plans` puts 2 tenants on Free and NONE on Pro, Enterprise or
-- Pro Monthly, so no existing subscription is re-priced by this. Re-measure that before re-running the
-- reasoning on a busier database.
--
-- The separate question — "Pro" and "Pro Monthly" are both $49, and the Aug-19 row carries no
-- feature_limits while Aug-8 Enterprise carries 14 — is a product decision and is NOT made here. This
-- migration only stops the two columns disagreeing.

UPDATE plans
   SET price_monthly = GREATEST(price_monthly, price),
       price         = GREATEST(price_monthly, price),
       price_yearly  = CASE
                         WHEN price_yearly > 0 THEN price_yearly
                         WHEN GREATEST(price_monthly, price) > 0
                              THEN GREATEST(price_monthly, price) * 12
                         ELSE 0
                       END,
       updated_at    = NOW()
 WHERE price IS DISTINCT FROM price_monthly
    OR (price_yearly = 0 AND GREATEST(price_monthly, price) > 0);
