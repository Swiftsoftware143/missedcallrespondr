pub mod admin_handler;
pub mod affiliates_handler;
pub mod api_key_handler;
pub mod calendar_events_handler;
pub mod call_handler;
pub mod call_log_handler;
pub mod campaigns_handler;
pub mod checkout_handler;
pub mod clients_handler;
pub mod contact_custom_field_handler;
pub mod contact_handler;
pub mod coreswift_external;
pub mod coreswift_integration_handler;
pub mod dashboard_handler;
pub mod deals_handler;
pub mod email_templates_handler;
pub mod export_templates_handler;
pub mod follow_up_handler;
pub mod import_logs_handler;
pub mod integration_handler;
pub mod integration_target_handler;
pub mod leads_handler;
pub mod lists_handler;
pub mod message_handler;
pub mod message_template_handler;
pub mod plans_handler;
pub mod portfolio_handler;
pub mod portfolio_sync_handler;
pub mod provider_keys_handler;
pub mod response_rule_handler;
pub mod settings_handler;
pub mod tag_groups_handler;
pub mod tag_provision_handler;
pub mod tags_handler;
pub mod telnyx_handler;
pub mod tickets_handler;
pub mod triggers_handler;
pub mod voicemail_handler;
pub mod workflows_handler;
pub mod workflowswift_push;

pub mod site_handler;

// Test-only: dead-pool + tracing-capture harness and the regression legs for the
// silent-swallow fixes (card t_08faed51). Excluded from release builds.
#[cfg(test)]
mod swallow_tests;

// Test-only: the empty-`x-internal-key` guard legs (card t_eb7736b8). Excluded from release builds.
#[cfg(test)]
mod internal_key_guard_tests;
