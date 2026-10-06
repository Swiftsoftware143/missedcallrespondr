-- 000030_retire_funnelswift_tenant.sql  (kanban t_1d08bd9a)
--
-- Retire the user-less holder tenant the deleted tag receiver used to file every FunnelSwift lead
-- into: `FunnelSwift Leads` / slug `funnelswift` / id 0347dc35-b3a2-47ec-bde5-2d46b43bf19a.
--
-- WHY IT IS STILL HERE. `migrations/000018_funnelswift_tenant.sql` seeded it so that
-- `POST /api/v1/internal/tag-provision` had somewhere to write, and its handler bound that id to
-- every contact it created. Both were retired on 2026-10-01 (kanban t_c2353c90): the sender family
-- was gone, the route had no caller, and the migration's file was deleted from disk AND from the
-- boot list in `src/db.rs` — so a FRESH database no longer creates the row. The row itself was left
-- behind on live ("A database that already has the row keeps it — nothing reads it"), because the
-- retirement could not prove what else pointed at it. That is what this file settles: it is the
-- exact shape the tag → free account card replaces — a lead holder NOBODY CAN LOG INTO (0 users),
-- whose sibling is a real account with an owner user and a plan.
--
-- WHAT IT DOES. Deletes the row ONLY when nothing anywhere references it, and says so in the boot
-- log either way. The census is read from the CATALOG, not from a hand-written table list: every
-- single-column foreign key to `tenants` is walked by name, plus `email_templates.aid` (the app's
-- one tenant-ish column with NO foreign key — a dangling reference there would be silent). The
-- guard is not decoration: `tenants` is referenced by 32 tables, most `ON DELETE CASCADE`, so an
-- unguarded DELETE would quietly take any children with it.
--
-- WHY A MIGRATION AND NOT AN OPERATOR DELETE. This runner re-executes every registered file on every
-- boot and has NO tracking table (`src/db.rs`; there is no `_sqlx_migrations` here), so the delete
-- is idempotent by construction: the second pass finds the row already absent and logs that. A
-- fresh install never had the row at all — 000018 is no longer registered — so this file is a no-op
-- there and a one-time cleanup on live.
--
-- MEASURED BEFORE WRITING (live, 2026-10-06): 32 single-column FKs to `tenants`, 0 rows in every one
-- of them for this id; `users` 0, `tenant_plans` 0, `email_templates` 0. Control on the same query
-- for a live tenant (`SwiftSoftware`) returned 3 tables with rows, so the census is not vacuous.

DO $$
DECLARE
    tid   uuid := '0347dc35-b3a2-47ec-bde5-2d46b43bf19a';
    r     record;
    n     bigint;
    gone  bigint;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM tenants WHERE id = tid AND slug = 'funnelswift') THEN
        RAISE NOTICE 'retire_funnelswift_tenant: absent (fresh install or already retired), nothing to do';
        RETURN;
    END IF;

    -- The one tenant-ish column this app has with NO foreign key.
    SELECT count(*) INTO n FROM email_templates WHERE aid = tid;
    IF n > 0 THEN
        RAISE NOTICE 'retire_funnelswift_tenant: SKIPPED, % email_templates row(s) reference it', n;
        RETURN;
    END IF;

    -- Every single-column FK to tenants, read from the catalog by column name.
    FOR r IN
        SELECT c.relname AS tbl, a.attname AS col
          FROM pg_constraint con
          JOIN pg_class c     ON c.oid = con.conrelid
          JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = ANY (con.conkey)
         WHERE con.contype = 'f'
           AND con.confrelid = 'public.tenants'::regclass
           AND array_length(con.conkey, 1) = 1
           AND c.relkind = 'r'
    LOOP
        EXECUTE format('SELECT count(*) FROM %I WHERE %I = $1', r.tbl, r.col)
            INTO n USING tid;
        IF n > 0 THEN
            RAISE NOTICE 'retire_funnelswift_tenant: SKIPPED, % row(s) in %.% reference it',
                n, r.tbl, r.col;
            RETURN;
        END IF;
    END LOOP;

    DELETE FROM tenants WHERE id = tid;
    GET DIAGNOSTICS gone = ROW_COUNT;
    RAISE NOTICE 'retire_funnelswift_tenant: retired % tenant row(s) (id=%, slug=funnelswift, 0 referencing rows in any table)',
        gone, tid;
END $$;
