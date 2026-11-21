-- 000062_portfolio_company_account.sql — a portfolio company's OWN account, kept apart from the
-- owner's (2026-11-20, kanban t_bef16030).
--
-- WHY. `portfolio_companies.tenant_id` carried ONE account reference and it meant two different
-- things depending on which door wrote the row:
--   * the cross-app sync doors (`admin_handler::portfolio_sync`, `portfolio_sync_handler::*`,
--     `portfolio_handler::internal_create_portfolio_company`) store the COMPANY'S OWN tenant there;
--   * the user-facing `POST /portfolio-companies` stores the CALLER'S tenant — the owner's account,
--     the same one every portfolio row shares.
-- So "open the row's tenant" entered the operator's own account for a row written by the second
-- door: a button that looks like it does nothing. A portfolio row needs BOTH references, said out
-- loud:
--   tenant_id         = the account that OWNS the row (operator / caller scope)   [unchanged]
--   account_tenant_id = the COMPANY'S OWN account, NULL when it does not have one yet
--
-- BACKFILL is exact-match only (tenant slug = company slug) and refuses any tenant a platform admin
-- signs in to, so it can never point a company at the owner's own account. Idempotent: it only
-- fills rows where account_tenant_id IS NULL (this runner re-executes every file at boot).
ALTER TABLE portfolio_companies ADD COLUMN IF NOT EXISTS account_tenant_id uuid;

UPDATE portfolio_companies pc
   SET account_tenant_id = t.id
  FROM tenants t
 WHERE pc.account_tenant_id IS NULL
   AND lower(t.slug) = lower(pc.slug)
   AND NOT EXISTS (
        SELECT 1 FROM users u
         WHERE u.tenant_id = t.id
           AND u.role IN ('admin', 'super_admin', 'agency_admin'))
   AND NOT EXISTS (
        SELECT 1 FROM portfolio_companies own
         WHERE own.account_tenant_id = t.id
           AND own.id <> pc.id);

COMMENT ON COLUMN portfolio_companies.account_tenant_id IS
  'The portfolio company''s OWN account (tenants.id). NULL = the company has no account in this app yet. Never the owner''s account; see kanban t_bef16030.';
