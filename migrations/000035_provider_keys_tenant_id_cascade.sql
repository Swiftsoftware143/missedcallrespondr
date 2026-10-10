-- 000035_provider_keys_tenant_id_cascade.sql
-- One NO ACTION edge is corrected to CASCADE so the admin console's new mass-retire control
-- (kanban t_31951eca, part of t_ac2fe688) can actually delete a workspace that owns provider keys.
--
-- MEASURED 2026-10-10, live: of the 33 foreign keys that reference `tenants`, exactly ONE was not
-- armed for a parent delete —
--     provider_keys_tenant_id_fkey  FOREIGN KEY (tenant_id) REFERENCES tenants(id)     (confdeltype 'a')
-- every other edge is ON DELETE CASCADE, and a census for `tenant_id` columns with NO edge at all
-- returned zero rows. So a `DELETE FROM tenants` on a workspace holding provider keys answered
-- 500 `23503 violates foreign key constraint "provider_keys_tenant_id_fkey"` instead of retiring it.
--
-- ARM = CASCADE, chosen from the readers, not from taste (skill fk-delete-action-arms):
--   * `provider_keys` holds the workspace's OWN provider credentials (telnyx, coreswift, …). Every
--     reader scopes by that column — `telnyx_handler.rs` (`WHERE tenant_id = $1`),
--     `coreswift_external.rs` (the integration's own key), `provider_keys_handler.rs` (upsert and
--     `DELETE … WHERE tenant_id = $1 AND provider = $2`). A key row whose workspace is gone is
--     unreachable by construction, not preserved data;
--   * the column is the ownership pointer and NOT NULL, so SET NULL is not even available.
--
-- Idempotent (the boot-time `db::run_migrations` re-executes every file on each start): the DO block
-- only acts when the edge is still unarmed, so a re-run is a true no-op and the constraint is never
-- dropped and re-taken on a healthy database.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'provider_keys'::regclass
           AND conname = 'provider_keys_tenant_id_fkey'
           AND confdeltype <> 'c'
    ) THEN
        ALTER TABLE provider_keys DROP CONSTRAINT provider_keys_tenant_id_fkey;
        ALTER TABLE provider_keys
            ADD CONSTRAINT provider_keys_tenant_id_fkey
            FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;
    END IF;
END $$;
