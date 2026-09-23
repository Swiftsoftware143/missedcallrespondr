-- ============================================================
-- MissedCall Respondr — 000018: the owner tenant of FunnelSwift-provisioned contacts
-- (Sept 22 2026, kanban t_c9669881)
--
-- tag_provision_handler.rs bound a HARDCODED tenant uuid for every contact it
-- auto-provisioned from a FunnelSwift tag assignment:
--
--     .bind("883a2a82-c7e4-4abb-b6c2-da47c119caf1".parse::<Uuid>().unwrap())
--
-- No migration (or seed, or operator action) ever created that id in ANY
-- database, so `contacts.tenant_id -> tenants(id)` (contacts_tenant_id_fkey)
-- rejected the INSERT and the endpoint answered
--
--     500 {"error":"Database error: ... violates foreign key constraint
--          \"contacts_tenant_id_fkey\""}
--
-- for every lead carrying a NEW email. Live effect: FunnelSwift's tag-provision
-- webhook could not create a single contact here; it only answered
-- `200 already_exists` when the email was already present.
--
-- The handler now resolves its owner by SLUG at runtime and creates it on first
-- use (see tag_provision_handler::resolve_provision_tenant), so the code no
-- longer depends on any uuid literal. This migration is what makes that owner
-- exist deterministically on a fresh database, with a name and slug an operator
-- can recognise in the tenants list.
--
-- Additive + idempotent: insert-if-slug-absent, never modifies existing data.
-- ON CONFLICT (slug) DO NOTHING means an operator who already created a tenant
-- with this slug keeps their row (and its id) untouched.
-- ============================================================

INSERT INTO tenants (id, name, slug, created_at, updated_at, is_active)
VALUES (
    '0347dc35-b3a2-47ec-bde5-2d46b43bf19a',
    'FunnelSwift Leads',
    'funnelswift',
    NOW(),
    NOW(),
    true
)
ON CONFLICT (slug) DO NOTHING;
