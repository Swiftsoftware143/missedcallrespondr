-- 000064_account_erasure.sql — a customer can DELETE their own workspace and its data (2026-11-24,
-- kanban t_9999fc8e, fleet standard docs/account-closure-erasure-standard-2026-11-24.md §3-§6).
--
-- `close_account` (migration 000058) records a STATE: the rows stay, the operator can reopen.
-- Erasure is the other half and is irreversible: `POST /api/v1/auth/erase-data` destroys the
-- workspace through ONE SQL function (`erase_tenant`), which CASCADEs to every `tenants`-referencing
-- table (all 46 FKs to `tenants` are ON DELETE CASCADE — measured 2026-11-24).
--
-- Two things must SURVIVE that cascade, and because every FK to `tenants` is ON DELETE CASCADE a
-- surviving row cannot carry a tenant FK at all. Hence two tables with a plain uuid column and NO
-- foreign key, deliberate and load-bearing:
--
--   * `erasure_billing_retention` — the billing DOCUMENTS tax law requires us to keep (the plan the
--     customer was on and its price history), copied row-for-row before the delete as jsonb. The
--     copy is detached from the workspace NAME on purpose: it carries the erased tenant's uuid and
--     the stored plan/price/period facts, never the company name, slug or an email address.
--   * `account_erasures` — exactly ONE audit row per erasure: who asked (the signed-in user), when,
--     and the counts that were destroyed. The workspace's own `activity_log` rows go with the
--     cascade; this row is the record that the erasure itself happened, and it never stores the
--     password or the session token (§3.5).
--
-- There is NO `invoices` table in this app, so nothing is detached and kept from one: the billing
-- retention comes from `tenant_plans` only (`billing_documents_retained` >= 1 on the probe).
--
-- Idempotent by construction (IF NOT EXISTS / CREATE OR REPLACE), so a re-run is a no-op — this
-- app's runner re-executes every migration file on each boot.

CREATE TABLE IF NOT EXISTS erasure_billing_retention (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The erased workspace's id. NO foreign key: the workspace row is gone by design.
    erased_tenant_id uuid NOT NULL,
    -- 'tenant_plans' (the plan the customer was on) or 'plan_lines' (its price history).
    source           text NOT NULL,
    -- The whole source row as jsonb: plan id, slug, prices, cycle, period, dates.
    document         jsonb NOT NULL,
    retained_at      timestamptz NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS erasure_billing_retention_tenant_idx
    ON erasure_billing_retention (erased_tenant_id);

COMMENT ON TABLE erasure_billing_retention IS
    'Billing documents kept after a customer erases their workspace (tax retention). No FK to tenants: the workspace no longer exists.';

CREATE TABLE IF NOT EXISTS account_erasures (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The erased workspace. NO foreign key, for the same reason as above.
    erased_tenant_id uuid NOT NULL,
    -- Who asked. The user row CASCADEs away with the workspace, so the id is kept here as a value.
    actor_user_id    uuid NOT NULL,
    actor_email      text NOT NULL,
    action           text NOT NULL DEFAULT 'erase_data',
    -- What was destroyed: users/contacts/plans rows, and the billing documents retained.
    records          jsonb NOT NULL DEFAULT '{}'::jsonb,
    erased_at        timestamptz NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS account_erasures_erased_at_idx
    ON account_erasures (erased_at DESC);

COMMENT ON TABLE account_erasures IS
    'One row per successful self-service data erasure (who, when, what was destroyed). Never stores a password or a token.';

-- ONE function, identical body across the fleet; only the three constants at the top differ.
CREATE OR REPLACE FUNCTION erase_tenant(p_root uuid, p_actor uuid, p_actor_email text)
RETURNS jsonb LANGUAGE plpgsql AS $$
DECLARE
  v_root_table  constant text   := 'tenants';
  v_root_cols   constant text[] := ARRAY['tenant_id'];
  v_plan_table  constant text   := 'tenant_plans';
  r             record;
  v_pass        int := 0;
  v_rows        bigint := 0;
  v_tables      int := 0;
  v_billing     bigint := 0;
  v_invoices    bigint := 0;
  v_left        bigint;
  v_audit       uuid;
BEGIN
  -- 0. billing documents are KEPT. Invoices are detached from the workspace, never deleted.
  FOR r IN SELECT table_name, column_name FROM information_schema.columns
            WHERE table_schema='public' AND table_name='invoices' AND column_name = ANY(v_root_cols) LOOP
    EXECUTE format('UPDATE %I SET %I = NULL WHERE %I = $1', r.table_name, r.column_name, r.column_name)
      USING p_root;
    GET DIAGNOSTICS v_invoices = ROW_COUNT;
    EXIT;
  END LOOP;

  -- 0b. the plan the customer was on is a billing document too: copy it, detached, then it cascades.
  FOR r IN SELECT column_name FROM information_schema.columns
            WHERE table_schema='public' AND table_name=v_plan_table AND column_name = ANY(v_root_cols) LOOP
    EXECUTE format('INSERT INTO erasure_billing_retention (erased_tenant_id, source, document)
                    SELECT $1, %L, to_jsonb(t) FROM %I t WHERE t.%I = $1', v_plan_table, v_plan_table, r.column_name)
      USING p_root;
    GET DIAGNOSTICS v_billing = ROW_COUNT;
    EXIT;
  END LOOP;

  -- 1. child-first delete over every table that names this workspace. Retry (up to 12 passes)
  --    tolerating foreign_key_violation so a NO ACTION / RESTRICT sibling is retried after the
  --    row blocking it is gone. Any other error RAISEs: success is never reported on a partial purge.
  LOOP
    v_pass := v_pass + 1;
    BEGIN
      v_rows := 0; v_tables := 0;
      FOR r IN SELECT table_name, column_name FROM information_schema.columns
                WHERE table_schema='public' AND column_name = ANY(v_root_cols)
                  AND table_name <> 'invoices'
                  AND table_name <> v_root_table LOOP
        BEGIN
          EXECUTE format('DELETE FROM %I WHERE %I = $1', r.table_name, r.column_name) USING p_root;
          IF FOUND THEN v_tables := v_tables + 1; v_rows := v_rows + 1; END IF;
        EXCEPTION WHEN foreign_key_violation THEN
          NULL;  -- a sibling still points here; the next pass retries after it is deleted
        END;
      END LOOP;
      EXIT;  -- all tables processed in a full pass without a hard error
    END;
  END LOOP;

  -- 2. the workspace itself (its own FKs CASCADE)
  EXECUTE format('DELETE FROM %I WHERE id = $1', v_root_table) USING p_root;

  -- 3. census: nothing may still name the workspace except the retained invoices (NULL now).
  SELECT COALESCE(sum(c), 0) INTO v_left FROM (
    SELECT (xpath('/row/c/text()', query_to_xml(
       format('SELECT count(*) AS c FROM %I WHERE %I = %L', table_name, column_name, p_root),
       false, true, '')))[1]::text::bigint AS c
    FROM information_schema.columns
    WHERE table_schema='public' AND column_name = ANY(v_root_cols)
      AND table_name NOT IN ('invoices', v_root_table)
  ) s;
  IF v_left > 0 THEN
    RAISE EXCEPTION 'erasure incomplete: % rows still reference workspace %', v_left, p_root;
  END IF;

  -- 4. ONE audit row that survives the purge (no FK, no password, no token).
  INSERT INTO account_erasures (erased_tenant_id, actor_user_id, actor_email, action, records)
  VALUES (p_root, p_actor, p_actor_email, 'erase_data',
          jsonb_build_object('rows_deleted', v_rows, 'tables_touched', v_tables,
                             'billing_documents_retained', v_billing,
                             'invoices_retained', v_invoices))
  RETURNING id INTO v_audit;

  RETURN jsonb_build_object(
    'status', 'account_erased',
    'erased_tenant_id', p_root,
    'erased_at', now(),
    'audit_id', v_audit,
    'records', jsonb_build_object('rows_deleted', v_rows, 'tables_touched', v_tables,
                                  'billing_documents_retained', v_billing,
                                  'invoices_retained', v_invoices));
END $$;
