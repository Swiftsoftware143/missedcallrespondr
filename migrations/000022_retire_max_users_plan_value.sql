-- 000022_retire_max_users_plan_value
--
-- WHY (kanban t_b578b169). The plan model advertised three limits that NO gate read:
--
--   key                 where it lived                        live values
--   max_users           plans.features->>'max_users'          free 1, pro-monthly 5
--   max_phone_numbers   plans.features->>'max_phone_numbers'  free 1, pro-monthly 5
--   max_tags            plans.max_tags (column)              free 10, pro 50, pro-monthly 50, ent -1
--
-- A sold allowance that caps nothing is an advertised control the product does not have.
--
-- DECISION per key (measured, then decided; see src/feature_registry.rs and src/features.rs):
--
--   max_phone_numbers  WIRED — gated on POST /api/v1/telnyx/numbers (telnyx_handler::purchase_number),
--                      the one route that adds a phone_numbers row. Its value stays advertised.
--   max_tags           WIRED — gated on POST /api/v1/tags (tags_handler::create), the one route that
--                      adds a tags row. Its column value stays advertised.
--   max_users          RETIRED — this file. This app has NO surface that adds a user to an existing
--                      tenant: all three `INSERT INTO users` sites (auth::register, the admin
--                      provisioning path, the checkout credential path) create a tenant's FIRST
--                      user, and the plan is attached only afterwards, so a cap of 1/5 could never
--                      be reached by any caller. Wiring it would require inventing a team-invite
--                      surface — a new feature, not a repair. The honest state is to stop
--                      advertising the seat cap, so the raw JSON key is removed here.
--
-- This is a DATA retirement, not a schema change: nothing else reads `features.max_users`
-- (`grep -rn max_users src/` after the change names only the comments that record this decision),
-- and `GET /api/v1/me/usage` keeps reporting the live `users` count for the tenant — it just no
-- longer pretends a plan caps it.
--
-- Idempotent for the boot-time runner (src/db.rs re-executes every registered file on every boot):
-- `features - 'max_users'` on a row that no longer carries the key is a no-op, and the
-- `jsonb_typeof = 'object'` guard keeps the array-shaped plans (Pro, Enterprise carry arrays of
-- marketing tags) untouched — for an array `?` tests ELEMENT membership, which is not the intent.
--
-- Reversal (one statement, if a seat cap is ever built):
--   UPDATE plans SET features = features || jsonb_build_object('max_users', 1)
--    WHERE slug='free' AND jsonb_typeof(features)='object';

UPDATE public.plans
   SET features = features - 'max_users'
 WHERE jsonb_typeof(features) = 'object'
   AND features ? 'max_users';
