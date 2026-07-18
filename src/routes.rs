use axum::{
    middleware,
    routing::{get, post, put, delete},
    Router,
};
use tower_http::cors::CorsLayer;

use crate::{
    auth::{handlers as auth_handlers, middleware::auth_middleware},
    handlers::{
        api_key_handler, call_handler, call_log_handler, contact_handler, dashboard_handler,
        deals_handler, campaigns_handler, tickets_handler, email_templates_handler,
        follow_up_handler, integration_handler, portfolio_handler, integration_target_handler,
        import_logs_handler, export_templates_handler, calendar_events_handler,
        message_handler, message_template_handler,
        response_rule_handler, settings_handler, voicemail_handler, provider_keys_handler,
        telnyx_handler, triggers_handler, contact_custom_field_handler, checkout_handler,
    },
    state::AppState,
};

pub fn create_router(state: AppState) -> Router {
    // ── Public routes (no auth required) ──
    let public_routes = Router::new()
        .route("/api/v1/health", get(health_check))
        .route("/api/v1/auth/register", post(auth_handlers::register))
        .route("/api/v1/auth/login", post(auth_handlers::login))
        .route("/api/v1/auth/forgot-password", post(auth_handlers::forgot_password))
        .route("/api/v1/auth/reset-password", post(auth_handlers::reset_password))
        .route("/api/v1/internal/portfolio-companies", post(portfolio_handler::internal_create_portfolio_company))
        .route("/api/v1/available-providers", get(provider_keys_handler::list_available_providers))
        // Telnyx webhook (public — Telnyx sends unauthenticated requests)
        .route("/api/v1/telnyx/webhook", post(telnyx_handler::webhook))
        // Payment webhooks (public — providers send unauthenticated requests)
        .route("/api/v1/webhooks/stripe", post(checkout_handler::stripe_webhook))
        .route("/api/v1/webhooks/paypal", post(checkout_handler::paypal_webhook));

    // ── Protected routes (auth required) ──
    let protected_routes = Router::new()
        .route("/api/v1/auth/me", get(auth_handlers::me))
        .route("/api/v1/auth/profile", put(auth_handlers::update_profile))
        .route("/api/v1/auth/password", put(auth_handlers::change_password))
        // Calls
        .route("/api/v1/calls", get(call_handler::list_calls).post(call_handler::create_call))
        .route("/api/v1/calls/:id", get(call_handler::get_call).put(call_handler::update_call).delete(call_handler::delete_call))
        .route("/api/v1/calls/:id/voicemail", get(call_handler::get_call_voicemail))
        .route("/api/v1/calls/:id/respond", post(call_handler::respond_to_call))
        // Response Rules
        .route("/api/v1/response-rules", get(response_rule_handler::list_response_rules).post(response_rule_handler::create_response_rule))
        .route("/api/v1/response-rules/:id", put(response_rule_handler::update_response_rule).delete(response_rule_handler::delete_response_rule))
        // Follow Ups
        .route("/api/v1/follow-ups", get(follow_up_handler::list_follow_ups).post(follow_up_handler::create_follow_up))
        .route("/api/v1/follow-ups/:id", put(follow_up_handler::update_follow_up).delete(follow_up_handler::delete_follow_up))
        // Messages
        .route("/api/v1/messages", get(message_handler::list_messages).post(message_handler::create_message))
        .route("/api/v1/messages/:id", get(message_handler::get_message))
        // Message Templates
        .route("/api/v1/message-templates", get(message_template_handler::list_message_templates).post(message_template_handler::create_message_template))
        .route("/api/v1/message-templates/:id", put(message_template_handler::update_message_template).delete(message_template_handler::delete_message_template))
        // Contacts
        .route("/api/v1/contacts", get(contact_handler::list_contacts).post(contact_handler::create_contact))
        .route("/api/v1/contacts/search", get(contact_handler::search_contacts))
        .route("/api/v1/contacts/:id", get(contact_handler::get_contact).put(contact_handler::update_contact).delete(contact_handler::delete_contact))
        // Contact Custom Fields
        .route("/api/v1/contacts/custom-fields", get(contact_custom_field_handler::list_custom_fields).post(contact_custom_field_handler::create_custom_field))
        .route("/api/v1/contacts/custom-fields/:id", put(contact_custom_field_handler::update_custom_field).delete(contact_custom_field_handler::delete_custom_field))
        .route("/api/v1/contacts/:id/fields", get(contact_custom_field_handler::get_contact_with_fields))
        .route("/api/v1/contacts/with-fields", get(contact_custom_field_handler::list_contacts_with_fields))
        .route("/api/v1/contacts/:contact_id/fields/:field_id", put(contact_custom_field_handler::update_contact_field_value))
        // Voicemails
        .route("/api/v1/voicemails", get(voicemail_handler::list_voicemails))
        .route("/api/v1/voicemails/:id", get(voicemail_handler::get_voicemail).put(voicemail_handler::update_voicemail))
        // Call Logs
        .route("/api/v1/call-logs", get(call_log_handler::list_call_logs))
        .route("/api/v1/call-logs/export", get(call_log_handler::export_call_logs))
        // Integrations
        .route("/api/v1/integrations", get(integration_handler::list_integrations).post(integration_handler::create_integration))
        .route("/api/v1/integrations/:id", put(integration_handler::update_integration).delete(integration_handler::delete_integration))
        .route("/api/v1/integrations/:id/test", post(integration_handler::test_integration))
        // Dashboard
        .route("/api/v1/dashboard/stats", get(dashboard_handler::get_dashboard_stats))
        .route("/api/v1/dashboard/activity", get(dashboard_handler::get_dashboard_activity))
        // Settings
        .route("/api/v1/settings", get(settings_handler::get_settings).put(settings_handler::update_settings))
        // API Key management
        .route("/api/v1/api-keys", post(api_key_handler::create_api_key).get(api_key_handler::list_api_keys))
        .route("/api/v1/api-keys/:id", put(api_key_handler::update_api_key).delete(api_key_handler::delete_api_key))
        // Portfolio Companies
        .route("/api/v1/portfolio-companies", get(portfolio_handler::list_portfolio_companies).post(portfolio_handler::create_portfolio_company))
        .route("/api/v1/portfolio-companies/:id", get(portfolio_handler::get_portfolio_company).put(portfolio_handler::update_portfolio_company).delete(portfolio_handler::delete_portfolio_company))
        // Integration Targets
        .route("/api/v1/integration-targets", get(integration_target_handler::list_integration_targets).post(integration_target_handler::create_integration_target))
        .route("/api/v1/integration-targets/:id", put(integration_target_handler::update_integration_target).delete(integration_target_handler::delete_integration_target))
        // Affiliates
        .route("/api/v1/affiliates", get(crate::handlers::affiliates_handler::list).post(crate::handlers::affiliates_handler::create))
        .route("/api/v1/affiliates/:id", get(crate::handlers::affiliates_handler::get).put(crate::handlers::affiliates_handler::update).delete(crate::handlers::affiliates_handler::delete))
        // Provider Keys
        .route("/api/v1/provider-keys", get(provider_keys_handler::list_provider_keys).post(provider_keys_handler::upsert_provider_key))
        .route("/api/v1/provider-keys/:provider", delete(provider_keys_handler::delete_provider_key))
        // Telnyx numbers (authenticated)
        .route("/api/v1/telnyx/numbers", get(telnyx_handler::list_numbers).post(telnyx_handler::purchase_number))
        .route("/api/v1/telnyx/numbers/:id", delete(telnyx_handler::delete_number))
        // Campaign Triggers
        .route("/api/v1/triggers/email", get(crate::handlers::triggers_handler::list_email_triggers).post(crate::handlers::triggers_handler::create_email_trigger))
        .route("/api/v1/triggers/email/:id", get(crate::handlers::triggers_handler::get_email_trigger).put(crate::handlers::triggers_handler::update_email_trigger).delete(crate::handlers::triggers_handler::delete_email_trigger))
        .route("/api/v1/triggers/redirect", get(crate::handlers::triggers_handler::list_redirect_triggers).post(crate::handlers::triggers_handler::create_redirect_trigger))
        .route("/api/v1/triggers/redirect/:id", get(crate::handlers::triggers_handler::get_redirect_trigger).put(crate::handlers::triggers_handler::update_redirect_trigger).delete(crate::handlers::triggers_handler::delete_redirect_trigger))
        // SMTP Config
        .route("/api/v1/portfolio-companies/:id/smtp", get(crate::handlers::triggers_handler::get_smtp_config).put(crate::handlers::triggers_handler::update_smtp_config))
        // Tags (authenticated)
        .route("/api/v1/tags", get(crate::handlers::tags_handler::list).post(crate::handlers::tags_handler::create))
        .route("/api/v1/tags/:id", get(crate::handlers::tags_handler::get).put(crate::handlers::tags_handler::update).delete(crate::handlers::tags_handler::delete))
        // Tag Groups (authenticated)
        .route("/api/v1/tag-groups", get(crate::handlers::tag_groups_handler::list).post(crate::handlers::tag_groups_handler::create))
        .route("/api/v1/tag-groups/:id", get(crate::handlers::tag_groups_handler::get).put(crate::handlers::tag_groups_handler::update).delete(crate::handlers::tag_groups_handler::delete))
        // Leads
        .route("/api/v1/leads", get(crate::handlers::leads_handler::list).post(crate::handlers::leads_handler::create))
        .route("/api/v1/leads/{id}", get(crate::handlers::leads_handler::get).put(crate::handlers::leads_handler::update).delete(crate::handlers::leads_handler::delete))
        // Deals
        .route("/api/v1/deals", get(crate::handlers::deals_handler::list).post(crate::handlers::deals_handler::create))
        .route("/api/v1/deals/{id}", get(crate::handlers::deals_handler::get).put(crate::handlers::deals_handler::update).delete(crate::handlers::deals_handler::delete))
        // Campaigns
        .route("/api/v1/campaigns", get(crate::handlers::campaigns_handler::list).post(crate::handlers::campaigns_handler::create))
        .route("/api/v1/campaigns/{id}", get(crate::handlers::campaigns_handler::get).put(crate::handlers::campaigns_handler::update).delete(crate::handlers::campaigns_handler::delete))
        // Tickets
        .route("/api/v1/tickets", get(crate::handlers::tickets_handler::list).post(crate::handlers::tickets_handler::create))
        .route("/api/v1/tickets/{id}", get(crate::handlers::tickets_handler::get).put(crate::handlers::tickets_handler::update).delete(crate::handlers::tickets_handler::delete))
        // Email Templates
        .route("/api/v1/email-templates", get(crate::handlers::email_templates_handler::list).post(crate::handlers::email_templates_handler::create))
        .route("/api/v1/email-templates/{id}", get(crate::handlers::email_templates_handler::get).put(crate::handlers::email_templates_handler::update).delete(crate::handlers::email_templates_handler::delete))
        // Import Logs
        .route("/api/v1/import-logs", get(crate::handlers::import_logs_handler::list).post(crate::handlers::import_logs_handler::create))
        .route("/api/v1/import-logs/{id}", get(crate::handlers::import_logs_handler::get).put(crate::handlers::import_logs_handler::update).delete(crate::handlers::import_logs_handler::delete))
        // Export Templates
        .route("/api/v1/export-templates", get(crate::handlers::export_templates_handler::list).post(crate::handlers::export_templates_handler::create))
        .route("/api/v1/export-templates/{id}", get(crate::handlers::export_templates_handler::get).put(crate::handlers::export_templates_handler::update).delete(crate::handlers::export_templates_handler::delete))
        // Calendar Events
        .route("/api/v1/calendar-events", get(crate::handlers::calendar_events_handler::list).post(crate::handlers::calendar_events_handler::create))
        .route("/api/v1/calendar-events/{id}", get(crate::handlers::calendar_events_handler::get).put(crate::handlers::calendar_events_handler::update).delete(crate::handlers::calendar_events_handler::delete))
        // Clients
        .route("/api/v1/clients", get(crate::handlers::clients_handler::list).post(crate::handlers::clients_handler::create))
        .route("/api/v1/clients/{id}", get(crate::handlers::clients_handler::get).put(crate::handlers::clients_handler::update).delete(crate::handlers::clients_handler::delete))
        // Workflows
        .route("/api/v1/workflows", get(crate::handlers::workflows_handler::list).post(crate::handlers::workflows_handler::create))
        .route("/api/v1/workflows/{id}", get(crate::handlers::workflows_handler::get).put(crate::handlers::workflows_handler::update).delete(crate::handlers::workflows_handler::delete))
        // Admin endpoints (cross-app portfolio sync + impersonation)
        .route("/api/v1/admin/portfolio-sync", post(crate::handlers::admin_handler::portfolio_sync))
        .route("/api/v1/admin/impersonate", post(crate::handlers::admin_handler::impersonate))
        .route("/api/v1/admin/tenants/:id", delete(crate::handlers::admin_handler::delete_tenant))
        .route("/api/v1/admin/stop-impersonation", post(crate::handlers::admin_handler::stop_impersonation))
        .route("/api/v1/admin/tenants", get(crate::handlers::admin_handler::list_all_tenants))
        .route("/api/v1/admin/tenants/:id/credits", post(crate::handlers::admin_handler::add_credits))
        // Admin plan management
        .route("/api/v1/admin/plans", get(crate::handlers::plans_handler::list_plans).post(crate::handlers::plans_handler::create_plan))
        .route("/api/v1/admin/plans/assign", post(crate::handlers::plans_handler::admin_assign_plan))
        .route("/api/v1/admin/plans/:id", get(crate::handlers::plans_handler::get_plan).put(crate::handlers::plans_handler::update_plan).delete(crate::handlers::plans_handler::delete_plan))
        .route("/api/v1/admin/plans/:id/features", put(crate::handlers::plans_handler::admin_update_plan_features))
        // Admin Telnyx config
        .route("/api/v1/admin/telnyx-config", get(telnyx_handler::get_admin_config).put(telnyx_handler::put_admin_config))
        // Payment Providers (admin)
        .route("/api/v1/payment-providers", get(checkout_handler::list_payment_providers).post(checkout_handler::upsert_payment_provider))
        .route("/api/v1/payment-providers/{provider_type}", delete(checkout_handler::delete_payment_provider))
        // Checkout Sessions
        .route("/api/v1/checkout/create", post(checkout_handler::create_checkout_session))
        .route("/api/v1/checkout/sessions", get(checkout_handler::list_checkout_sessions))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn health_check() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "service": "missedcall-respondr",
        "version": "0.1.0"
    }))
}
