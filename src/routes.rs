use axum::{
    middleware,
    routing::{delete, get, post, put},
    Router,
};
use tower_http::cors::CorsLayer;

use crate::{
    auth::{handlers as auth_handlers, middleware::auth_middleware},
    handlers::{
        call_handler, call_log_handler, checkout_handler, contact_custom_field_handler,
        contact_handler, coreswift_integration_handler, dashboard_handler, follow_up_handler,
        integration_handler, integration_target_handler, lists_handler, message_handler,
        message_template_handler, portfolio_handler, provider_keys_handler, response_rule_handler,
        telnyx_handler,
    },
    state::AppState,
};

pub fn create_router(state: AppState) -> Router {
    // Body-read deadline (kanban t_7f688018): how long a request body may take to ARRIVE before the
    // request is answered 408 and its task, connection and partially-read body buffer are released.
    // The value is the one config.rs clamped and main.rs printed at boot — never re-derived here.
    let body_read_deadline =
        crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs);

    // ── Public routes (no auth required) ──
    let public_routes = Router::new()
        .route("/api/v1/health", get(health_check))
        .route("/api/v1/auth/register", post(auth_handlers::register))
        .route("/api/v1/auth/login", post(auth_handlers::login))
        .route(
            "/api/v1/auth/forgot-password",
            post(auth_handlers::forgot_password),
        )
        .route(
            "/api/v1/auth/reset-password",
            post(auth_handlers::reset_password),
        )
        .route(
            "/api/v1/internal/portfolio-companies",
            post(portfolio_handler::internal_create_portfolio_company),
        )
        .route(
            "/api/v1/internal/portfolio-sync",
            post(crate::handlers::portfolio_sync_handler::portfolio_sync_internal),
        )
        .route(
            "/api/v1/available-providers",
            get(provider_keys_handler::list_available_providers),
        )
        // Telnyx webhook (public — Telnyx sends unauthenticated requests)
        .route("/api/v1/telnyx/webhook", post(telnyx_handler::webhook))
        // Payment webhooks (public — providers send unauthenticated requests)
        .route(
            "/api/v1/webhooks/stripe",
            post(checkout_handler::stripe_webhook),
        )
        .route(
            "/api/v1/webhooks/paypal",
            post(checkout_handler::paypal_webhook),
        )
        // Thank-you page lookup (public, id-scoped, non-sensitive summary)
        .route(
            "/api/v1/checkout/session/:id",
            get(checkout_handler::get_checkout_session_public),
        )
        // REMOVED 2026-10-01 (kanban t_c2353c90): `POST /api/v1/internal/tag-provision` is gone.
        // It existed to receive FunnelSwift's per-app tag push, and that sender was retired
        // 2026-09-25 with the whole `<APP>_WEBHOOK_URL ... /api/v1/internal/tag-provision` family
        // (FunnelSwift cards t_8803c75e / t_ae84b186 / t_65084d8b / t_0a6a93f1); FunnelSwift's tag
        // path posts only to CoreSwift's `/api/v1/webhooks/cross-app/tag-sync` (tag_logic.rs).
        // Measured before deleting: zero readers of `MISSEDCALL_WEBHOOK_URL` in any tree, served
        // root, script or workflow; every nginx hit was a local audit probe; and the owner tenant
        // the handler provisioned into (`funnelswift`, migrations/000018) held 0 contacts. This app
        // PRODUCES into the hub (coreswift_external::push_lead_to_coreswift) — it does not consume
        // another app's leads. Do not re-add a receiver here without a caller: WorkflowSwift's
        // router carries the same note (card t_79d7d1d2).
        // THE PUBLIC RECEIVERS FIRST (kanban t_7f688018). `/api/v1/telnyx/webhook`, the two payment
        // webhooks, the `X-Internal-Key` `/api/v1/internal/*` push routes and the
        // `/api/v1/auth/*` receivers are the routes a stranger with no credential can reach, and
        // every one of them reads a body — so every one of them could be parked for ever by a
        // request head with a `Content-Length` and then silence. Mounted on the whole public
        // router: the layer's own scope expression (a body-carrying method + a DECLARED body) is
        // what keeps the three bodyless GET routes on this surface unchanged — proven live, N1
        // `GET /api/v1/health` with a declared-but-absent body answers 200 at t+0.00 s in BOTH
        // phases, and a `Content-Length: 0` POST is answered at once too (N2).
        // `route_layer` (kanban t_f8e7dd85): the deadline applies to this router's ROUTES, not to its
        // fallback, so `merge` cannot adopt a deadline-wrapped 404 for the whole app.
        .route_layer(middleware::from_fn_with_state(
            body_read_deadline,
            crate::body_deadline::body_read_deadline_middleware,
        ));

    // ── Protected routes (auth required) ──
    let protected_routes = Router::new()
        .route("/api/v1/auth/me", get(auth_handlers::me))
        .route("/api/v1/me/usage", get(auth_handlers::get_usage))
        .route("/api/v1/auth/profile", put(auth_handlers::update_profile))
        .route("/api/v1/auth/password", put(auth_handlers::change_password))
        // Calls
        .route(
            "/api/v1/calls",
            get(call_handler::list_calls).post(call_handler::create_call),
        )
        .route(
            "/api/v1/calls/:id",
            get(call_handler::get_call)
                .put(call_handler::update_call)
                .delete(call_handler::delete_call),
        )
        .route(
            "/api/v1/calls/:id/respond",
            post(call_handler::respond_to_call),
        )
        // Response Rules
        .route(
            "/api/v1/response-rules",
            get(response_rule_handler::list_response_rules)
                .post(response_rule_handler::create_response_rule),
        )
        .route(
            "/api/v1/response-rules/:id",
            put(response_rule_handler::update_response_rule)
                .delete(response_rule_handler::delete_response_rule),
        )
        // Follow Ups
        .route(
            "/api/v1/follow-ups",
            get(follow_up_handler::list_follow_ups).post(follow_up_handler::create_follow_up),
        )
        .route(
            "/api/v1/follow-ups/:id",
            put(follow_up_handler::update_follow_up).delete(follow_up_handler::delete_follow_up),
        )
        // Messages
        .route(
            "/api/v1/messages",
            get(message_handler::list_messages).post(message_handler::create_message),
        )
        .route("/api/v1/messages/:id", get(message_handler::get_message))
        // Message Templates
        .route(
            "/api/v1/message-templates",
            get(message_template_handler::list_message_templates)
                .post(message_template_handler::create_message_template),
        )
        .route(
            "/api/v1/message-templates/:id",
            put(message_template_handler::update_message_template)
                .delete(message_template_handler::delete_message_template),
        )
        // Contacts
        .route(
            "/api/v1/contacts",
            get(contact_handler::list_contacts).post(contact_handler::create_contact),
        )
        .route(
            "/api/v1/contacts/search",
            get(contact_handler::search_contacts),
        )
        .route(
            "/api/v1/contacts/:id",
            get(contact_handler::get_contact)
                .put(contact_handler::update_contact)
                .delete(contact_handler::delete_contact),
        )
        // Contact Custom Fields
        .route(
            "/api/v1/contacts/custom-fields",
            get(contact_custom_field_handler::list_custom_fields)
                .post(contact_custom_field_handler::create_custom_field),
        )
        .route(
            "/api/v1/contacts/custom-fields/:id",
            put(contact_custom_field_handler::update_custom_field)
                .delete(contact_custom_field_handler::delete_custom_field),
        )
        .route(
            "/api/v1/contacts/:id/fields",
            get(contact_custom_field_handler::get_contact_with_fields),
        )
        .route(
            "/api/v1/contacts/with-fields",
            get(contact_custom_field_handler::list_contacts_with_fields),
        )
        .route(
            "/api/v1/contacts/:contact_id/fields/:field_id",
            put(contact_custom_field_handler::update_contact_field_value),
        )
        // Voicemails (kanban t_1d4fc956 RETIRE-VOICEMAILS): `GET /api/v1/voicemails`,
        // `GET|PUT /api/v1/voicemails/:id` and `GET /api/v1/calls/:id/voicemail` are GONE with the
        // `voicemails` table they read. Nothing could ever write a row (the Telnyx webhook has no
        // `call.recording.saved` arm, so its `record_start` was never captured), no console screen
        // existed, and there is no STT integration in this crate — the whole surface was
        // empty-by-construction. See migrations/000026_retire_voicemails.sql.
        // Call Logs
        .route("/api/v1/call-logs", get(call_log_handler::list_call_logs))
        .route(
            "/api/v1/call-logs/export",
            get(call_log_handler::export_call_logs),
        )
        // Integrations
        .route(
            "/api/v1/integrations",
            get(integration_handler::list_integrations)
                .post(integration_handler::create_integration),
        )
        .route(
            "/api/v1/integrations/:id",
            put(integration_handler::update_integration)
                .delete(integration_handler::delete_integration),
        )
        .route(
            "/api/v1/integrations/:id/test",
            post(integration_handler::test_integration),
        )
        // CoreSwift integration surface (Zapier-style deep layer)
        .route(
            "/api/v1/integrations/coreswift/status",
            get(coreswift_integration_handler::status),
        )
        .route(
            "/api/v1/integrations/coreswift/lists",
            get(coreswift_integration_handler::lists),
        )
        // THE INBOUND PATH: proxy hub POST /api/external/contacts (manual "push now" fallback;
        // the automatic path fires from the capture handlers themselves).
        .route(
            "/api/v1/integrations/coreswift/push",
            post(coreswift_integration_handler::push),
        )
        // Dashboard
        .route(
            "/api/v1/dashboard/stats",
            get(dashboard_handler::get_dashboard_stats),
        )
        .route(
            "/api/v1/dashboard/activity",
            get(dashboard_handler::get_dashboard_activity),
        )
        // kanban t_f06b1710 — the Settings and API-Key route GROUPS were DELETED here, by measurement:
        // `tenant_settings` has exactly one reader (settings_handler's own GET) and no writer outside
        // the handler, so a Settings panel could only save keys nothing reads — a decorative control;
        // and `api_keys` is read by NO auth path in this crate (grep: no `x-api-key` anywhere, no
        // key-checking middleware — the only reader is `features::count_usage` for the plan quota), so
        // a key minted by POST /api/v1/api-keys authenticated nothing anywhere. Both were API-only
        // capabilities with no honest panel to ship, so the routes went rather than the panels.
        // Portfolio Companies
        .route(
            "/api/v1/portfolio-companies",
            get(portfolio_handler::list_portfolio_companies)
                .post(portfolio_handler::create_portfolio_company),
        )
        .route(
            "/api/v1/portfolio-companies/:id",
            get(portfolio_handler::get_portfolio_company)
                .put(portfolio_handler::update_portfolio_company)
                .delete(portfolio_handler::delete_portfolio_company),
        )
        // Integration Targets
        .route(
            "/api/v1/integration-targets",
            get(integration_target_handler::list_integration_targets)
                .post(integration_target_handler::create_integration_target),
        )
        .route(
            "/api/v1/integration-targets/:id",
            put(integration_target_handler::update_integration_target)
                .delete(integration_target_handler::delete_integration_target),
        )
        // Affiliates: retired (kanban t_5deebeb1). The affiliate system lives ONLY in FunnelSwift
        // and this app CONNECTS to it — the two outbound wires are what remains, and they never
        // touched a local table: `notify_funnelswift_upgrade` (plans_handler.rs) POSTs
        // `/api/v1/internal/affiliate/upgrade-event`, and the checkout completion handler
        // (checkout_handler.rs) POSTs `/api/v1/webhooks/conversion`. The local `affiliates` CRUD
        // (`list`/`create`/`get`/`update`/`delete` over this app's own empty `affiliates` table)
        // is gone: routes, handler and module declaration.
        // Provider Keys
        .route(
            "/api/v1/provider-keys",
            get(provider_keys_handler::list_provider_keys)
                .post(provider_keys_handler::upsert_provider_key),
        )
        .route(
            "/api/v1/provider-keys/:provider",
            delete(provider_keys_handler::delete_provider_key),
        )
        // Live "Test connection" probe for the Integration Center (CoreSwift = authed hub call).
        .route(
            "/api/v1/provider-keys/:provider/test",
            post(coreswift_integration_handler::test_provider_key),
        )
        // Lists (each campaign owns its own fresh list)
        .route(
            "/api/v1/lists",
            get(lists_handler::list).post(lists_handler::create),
        )
        .route(
            "/api/v1/lists/:id",
            put(lists_handler::update).delete(lists_handler::delete),
        )
        .route(
            "/api/v1/lists/:id/leads",
            get(lists_handler::list_leads).post(lists_handler::add_lead),
        )
        .route(
            "/api/v1/lists/:id/leads/:lead_id",
            delete(lists_handler::remove_lead),
        )
        // Telnyx numbers (authenticated)
        .route(
            "/api/v1/telnyx/numbers",
            get(telnyx_handler::list_numbers).post(telnyx_handler::purchase_number),
        )
        .route(
            "/api/v1/telnyx/numbers/:id",
            delete(telnyx_handler::delete_number),
        )
        // Campaign Triggers
        .route(
            "/api/v1/triggers/email",
            get(crate::handlers::triggers_handler::list_email_triggers)
                .post(crate::handlers::triggers_handler::create_email_trigger),
        )
        .route(
            "/api/v1/triggers/email/:id",
            get(crate::handlers::triggers_handler::get_email_trigger)
                .put(crate::handlers::triggers_handler::update_email_trigger)
                .delete(crate::handlers::triggers_handler::delete_email_trigger),
        )
        .route(
            "/api/v1/triggers/redirect",
            get(crate::handlers::triggers_handler::list_redirect_triggers)
                .post(crate::handlers::triggers_handler::create_redirect_trigger),
        )
        .route(
            "/api/v1/triggers/redirect/:id",
            get(crate::handlers::triggers_handler::get_redirect_trigger)
                .put(crate::handlers::triggers_handler::update_redirect_trigger)
                .delete(crate::handlers::triggers_handler::delete_redirect_trigger),
        )
        // SMTP Config
        .route(
            "/api/v1/portfolio-companies/:id/smtp",
            get(crate::handlers::triggers_handler::get_smtp_config)
                .put(crate::handlers::triggers_handler::update_smtp_config),
        )
        // Tags (authenticated)
        .route(
            "/api/v1/tags",
            get(crate::handlers::tags_handler::list).post(crate::handlers::tags_handler::create),
        )
        .route(
            "/api/v1/tags/:id",
            get(crate::handlers::tags_handler::get)
                .put(crate::handlers::tags_handler::update)
                .delete(crate::handlers::tags_handler::delete),
        )
        // Tag Groups (authenticated)
        .route(
            "/api/v1/tag-groups",
            get(crate::handlers::tag_groups_handler::list)
                .post(crate::handlers::tag_groups_handler::create),
        )
        .route(
            "/api/v1/tag-groups/:id",
            get(crate::handlers::tag_groups_handler::get)
                .put(crate::handlers::tag_groups_handler::update)
                .delete(crate::handlers::tag_groups_handler::delete),
        )
        // Leads
        .route(
            "/api/v1/leads",
            get(crate::handlers::leads_handler::list).post(crate::handlers::leads_handler::create),
        )
        .route(
            "/api/v1/leads/:id",
            get(crate::handlers::leads_handler::get)
                .put(crate::handlers::leads_handler::update)
                .delete(crate::handlers::leads_handler::delete),
        )
        // Deals
        .route(
            "/api/v1/deals",
            get(crate::handlers::deals_handler::list).post(crate::handlers::deals_handler::create),
        )
        .route(
            "/api/v1/deals/:id",
            get(crate::handlers::deals_handler::get)
                .put(crate::handlers::deals_handler::update)
                .delete(crate::handlers::deals_handler::delete),
        )
        // Deal stage move (pipeline)
        .route(
            "/api/v1/deals/:id/move",
            post(crate::handlers::deals_handler::move_stage),
        )
        // Campaigns
        .route(
            "/api/v1/campaigns",
            get(crate::handlers::campaigns_handler::list)
                .post(crate::handlers::campaigns_handler::create),
        )
        .route(
            "/api/v1/campaigns/:id",
            get(crate::handlers::campaigns_handler::get)
                .put(crate::handlers::campaigns_handler::update)
                .delete(crate::handlers::campaigns_handler::delete),
        )
        .route(
            "/api/v1/campaigns/:id/activate",
            post(crate::handlers::campaigns_handler::activate),
        )
        .route(
            "/api/v1/campaigns/:id/pause",
            post(crate::handlers::campaigns_handler::pause),
        )
        // Tickets
        .route(
            "/api/v1/tickets",
            get(crate::handlers::tickets_handler::list)
                .post(crate::handlers::tickets_handler::create),
        )
        .route(
            "/api/v1/tickets/:id",
            get(crate::handlers::tickets_handler::get)
                .put(crate::handlers::tickets_handler::update)
                .delete(crate::handlers::tickets_handler::delete),
        )
        .route(
            "/api/v1/tickets/stats",
            get(crate::handlers::tickets_handler::stats),
        )
        .route(
            "/api/v1/tickets/:id/messages",
            post(crate::handlers::tickets_handler::add_message),
        )
        // Email Templates
        .route(
            "/api/v1/email-templates",
            get(crate::handlers::email_templates_handler::list)
                .post(crate::handlers::email_templates_handler::create),
        )
        .route(
            "/api/v1/email-templates/:id",
            get(crate::handlers::email_templates_handler::get)
                .put(crate::handlers::email_templates_handler::update)
                .delete(crate::handlers::email_templates_handler::delete),
        )
        // Import Logs
        .route(
            "/api/v1/import-logs",
            get(crate::handlers::import_logs_handler::list)
                .post(crate::handlers::import_logs_handler::create),
        )
        .route(
            "/api/v1/import-logs/:id",
            get(crate::handlers::import_logs_handler::get)
                .put(crate::handlers::import_logs_handler::update)
                .delete(crate::handlers::import_logs_handler::delete),
        )
        // Export Templates
        .route(
            "/api/v1/export-templates",
            get(crate::handlers::export_templates_handler::list)
                .post(crate::handlers::export_templates_handler::create),
        )
        .route(
            "/api/v1/export-templates/:id",
            get(crate::handlers::export_templates_handler::get)
                .put(crate::handlers::export_templates_handler::update)
                .delete(crate::handlers::export_templates_handler::delete),
        )
        // Calendar Events
        .route(
            "/api/v1/calendar-events",
            get(crate::handlers::calendar_events_handler::list)
                .post(crate::handlers::calendar_events_handler::create),
        )
        .route(
            "/api/v1/calendar-events/:id",
            get(crate::handlers::calendar_events_handler::get)
                .put(crate::handlers::calendar_events_handler::update)
                .delete(crate::handlers::calendar_events_handler::delete),
        )
        // Clients
        .route(
            "/api/v1/clients",
            get(crate::handlers::clients_handler::list)
                .post(crate::handlers::clients_handler::create),
        )
        .route(
            "/api/v1/clients/:id",
            get(crate::handlers::clients_handler::get)
                .put(crate::handlers::clients_handler::update)
                .delete(crate::handlers::clients_handler::delete),
        )
        // `GET|POST /api/v1/workflows`, `GET|PUT|DELETE /api/v1/workflows/:id` and
        // `POST /api/v1/workflows/:id/{activate,deactivate}` are RETIRED (kanban t_66cfccff): the
        // `workflows`/`workflow_steps` store was CRUD-only with NO evaluator anywhere (no engine, no
        // cron, no webhook arm, no `response_rule_eval` arm), so every trigger the console offered
        // named an event nothing dispatched — and one of them (`voicemail`) had already been retired
        // by t_1d4fc956. The app's real automation surface is `response_rules` (see
        // `handlers/response_rule_eval.rs`), which IS evaluated on every inbound call. Migration
        // `000028_retire_workflows.sql` deletes the sold `max_workflows` plan row and drops both
        // tables, and `000011_schema_fix.sql` no longer creates them, so a fresh install never
        // builds a store nothing can name.
        // Admin endpoints (cross-app portfolio sync + impersonation)
        .route(
            "/api/v1/admin/portfolio-sync",
            post(crate::handlers::admin_handler::portfolio_sync),
        )
        .route(
            "/api/v1/admin/impersonate",
            post(crate::handlers::admin_handler::impersonate),
        )
        .route(
            "/api/v1/admin/tenants/:id",
            delete(crate::handlers::admin_handler::delete_tenant),
        )
        .route(
            "/api/v1/admin/stop-impersonation",
            post(crate::handlers::admin_handler::stop_impersonation),
        )
        .route(
            "/api/v1/admin/tenants",
            get(crate::handlers::admin_handler::list_all_tenants),
        )
        .route(
            "/api/v1/admin/tenants/:id/credits",
            post(crate::handlers::admin_handler::add_credits),
        )
        // Admin plan management
        .route(
            "/api/v1/admin/plans",
            get(crate::handlers::plans_handler::list_plans)
                .post(crate::handlers::plans_handler::create_plan),
        )
        .route(
            "/api/v1/admin/plans/assign",
            post(crate::handlers::plans_handler::admin_assign_plan),
        )
        // The plan × feature registry (kanban t_dd2f7e32): the catalogue the admin console renders
        // and the write path its "Set plan feature" control calls. Static segments, so they win
        // over `/api/v1/admin/plans/:id` below (same as `assign`).
        // The path is deliberately `/plan-registry`, NOT `/plans/registry`.
        // MEASURED 2026-10-03: as `/plans/registry` it was UNREACHABLE — `/api/v1/admin/plans/:id`
        // captured it and the handler died on `Uuid::parse_str("registry")` ("UUID parsing failed:
        // invalid character: found `r` at 0"), so the catalogue the console needs could not be fetched
        // at all. The old comment claimed "static segments win over /plans/:id" -- they do not on this
        // router, which is assembled with `merge()`. No caller depended on the old path (the console
        // only mentioned it in help strings), so the unambiguous path is the safe fix.
        .route(
            "/api/v1/admin/plan-registry",
            get(crate::handlers::plans_handler::plan_registry),
        )
        .route(
            "/api/v1/admin/plans/entitlement",
            put(crate::handlers::plans_handler::set_plan_entitlement),
        )
        .route(
            "/api/v1/admin/plans/grant-top-tier",
            post(crate::handlers::plans_handler::grant_top_tier),
        )
        .route(
            "/api/v1/admin/plans/:id",
            get(crate::handlers::plans_handler::get_plan)
                .put(crate::handlers::plans_handler::update_plan)
                .delete(crate::handlers::plans_handler::delete_plan),
        )
        .route(
            "/api/v1/admin/plans/:id/features",
            put(crate::handlers::plans_handler::admin_update_plan_features),
        )
        // Admin Telnyx config
        .route(
            "/api/v1/admin/telnyx-config",
            get(telnyx_handler::get_admin_config).put(telnyx_handler::put_admin_config),
        )
        // Admin system-mail provider (kanban t_6d575da6) — the panel-managed slot every fleet app
        // has. `/api/v1/admin/*` is platform-admin only at the middleware, so this inherits it.
        .route(
            "/api/v1/admin/email-config",
            get(crate::handlers::email_settings_handler::get_email_config)
                .put(crate::handlers::email_settings_handler::update_email_config),
        )
        .route(
            "/api/v1/admin/email-config/test",
            post(crate::handlers::email_settings_handler::test_email_config),
        )
        // Site configuration
        .route(
            "/api/v1/admin/site",
            get(crate::handlers::site_handler::get_site)
                .put(crate::handlers::site_handler::update_site),
        )
        // Payment Providers (admin)
        .route(
            "/api/v1/payment-providers",
            get(checkout_handler::list_payment_providers)
                .post(checkout_handler::upsert_payment_provider),
        )
        .route(
            "/api/v1/payment-providers/:provider_type",
            delete(checkout_handler::delete_payment_provider),
        )
        // Checkout Sessions
        .route(
            "/api/v1/checkout/create",
            post(checkout_handler::create_checkout_session),
        )
        .route(
            "/api/v1/checkout/sessions",
            get(checkout_handler::list_checkout_sessions),
        )
        // Body-read deadline INNERMOST: `route_layer` applies to the routes in THIS router and never
        // to its fallback, so the first one applied is the layer CLOSEST to the handler, and
        // `auth_middleware` — applied next — stays OUTSIDE it. That ordering is the contract, and it
        // is why mounting this at the merged router instead would be wrong: an unauthenticated
        // request must be answered 401 at t+0.00 s with its declared body never buffered, and only a
        // request that HAS a credential may be made to wait for a body. Proven live in both phases:
        // O1/O2 (no credential, declared-but-absent body) answer 401 at t+0.00 s, while
        // L11/L12/L13 (a real session, the same declared-but-absent body) park before the fix and
        // answer 408 ON the bound after it.
        //
        // BOTH layers are `route_layer` (kanban t_f8e7dd85): `Router::layer` also wraps the router's
        // FALLBACK, which flips its `default_fallback` flag, and `merge` then ADOPTS that wrapped
        // fallback — measured live, an unmounted `GET /whatever` answered this middleware's
        // `401 {"error":"Missing authorization header"}` instead of the router's own 404. With
        // `route_layer` the fallback is untouched and the committed allowlist (see
        // `crate::auth::route_policy`) is what decides, not a layering accident.
        .route_layer(middleware::from_fn_with_state(
            body_read_deadline,
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    // The one credential boundary (kanban t_f8e7dd85) — mounted with `route_layer`, so it decides
    // every MOUNTED route and an unmatched path still gets the router's own 404. It sits INSIDE the
    // CORS layer (applied last, i.e. outermost) so a refused browser request still carries the CORS
    // headers a preflight needs; the preflight itself is answered by `CorsLayer` before it reaches
    // here. See `crate::auth::route_policy` for the committed allowlist it reads.
    let boundary_state = state.clone();
    Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .with_state(state)
        .route_layer(middleware::from_fn_with_state(
            boundary_state,
            crate::auth::boundary::require_credential,
        ))
        // One place, every handler: extractor rejections (415/400/422) answer this app's own
        // JSON error shape instead of axum's unreadable text/plain — see error.rs. Layered
        // inside CORS so the rewritten response still leaves with the CORS headers.
        .layer(middleware::from_fn(crate::error::rejection_as_json))
        .layer(CorsLayer::permissive())
}

async fn health_check() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "service": "missedcallrespondr",
        "version": "0.1.0"
    }))
}
