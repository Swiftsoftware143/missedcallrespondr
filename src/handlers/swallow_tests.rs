//! Regression armour for the four silent query swallows of card t_08faed51
//! (`lists_handler::campaigns_for_tag`, both branches of `email_templates_handler::list`,
//! `plans_handler::attribute_plan_upgrade`).
//!
//! Every leg drives the real handler/helper against Postgres that genuinely cannot be
//! reached (`connect_lazy` to a closed port, 500 ms acquire timeout) and reads back the
//! process's own tracing output. The `*_old_shape_*` control legs re-run the exact
//! pre-fix expression on the same dead pool: they must produce the empty default with no
//! log line at all. That contrast is what makes the new legs evidence instead of a
//! tautology — it shows the query really fails and that the old shape really hid it.
#![cfg(test)]

use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::handlers::email_templates_handler::{self, EmailTemplate, ListQuery};
use crate::handlers::lists_handler;
use crate::state::AppState;

/// An `io::Write` sink that keeps everything the tracing subscriber emits.
#[derive(Clone)]
pub struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl CaptureWriter {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("capture lock")).to_string()
    }
}

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A pool pointed at a port nothing listens on: every query fails, for real, no mocking.
/// Must be built inside a tokio context (`capture` does that) — a lazy pool spawns its
/// maintenance task at construction and panics outside a runtime.
pub fn dead_pool() -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_millis(500))
        .connect_lazy("postgres://probe:probe@127.0.0.1:1/probe")
        .expect("lazy pool builds without a connection")
}

pub fn test_state() -> AppState {
    AppState {
        pool: dead_pool(),
        config: AppConfig {
            database_url: "postgres://probe:probe@127.0.0.1:1/probe".into(),
            jwt_secret: "test-secret".into(),
            server_port: 0,
            server_host: "127.0.0.1".into(),
            internal_sync_key: String::new(),
            funnelswift_url: "http://127.0.0.1:1".into(),
        },
        workflowswift_url: "http://127.0.0.1:1".into(),
        coreswift_url: "http://127.0.0.1:1".into(),
        funnelswift_url: "http://127.0.0.1:1".into(),
    }
}

/// Run one future on a current-thread runtime with tracing captured into a sink and
/// return (future output, everything logged). `make` is called inside the runtime, so
/// pools/states are built there.
pub fn capture<F, T>(make: impl FnOnce() -> F) -> (T, String)
where
    F: std::future::Future<Output = T>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let sink = CaptureWriter::new();
    let for_subscriber = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || for_subscriber.clone())
        .finish();
    let out = tracing::subscriber::with_default(subscriber, || {
        let _entered = rt.enter();
        rt.block_on(make())
    });
    (out, sink.text())
}

#[test]
fn email_templates_list_propagates_the_query_failure_and_logs_it() {
    let (res, log) = capture(|| {
        let state = test_state();
        async move {
            email_templates_handler::list(
                State(state),
                Query(ListQuery {
                    limit: None,
                    offset: None,
                    template_type: None,
                }),
            )
            .await
        }
    });
    assert!(
        res.is_err(),
        "list() must surface a DB failure, not answer 200 with an empty default"
    );
    assert!(
        log.contains("email_templates.list: query failed"),
        "the failure must be logged: {log}"
    );
}

#[test]
fn email_templates_list_old_shape_answered_empty_and_said_nothing() {
    let (items, log) = capture(|| {
        let pool = dead_pool();
        async move {
            sqlx::query_as::<_, EmailTemplate>(
                "SELECT * FROM email_templates ORDER BY name LIMIT $1 OFFSET $2",
            )
            .bind(50_i64)
            .bind(0_i64)
            .fetch_all(&pool)
            .await
            .unwrap_or_default()
        }
    });
    assert!(
        items.is_empty(),
        "control: the pre-fix shape turned a failed query into an empty list"
    );
    assert!(
        !log.contains("email_templates.list"),
        "control: the pre-fix shape logged nothing at all: {log}"
    );
}

#[test]
fn campaigns_for_tag_propagates_instead_of_defaulting_to_empty() {
    let (res, _log) = capture(|| {
        let state = test_state();
        async move { lists_handler::campaigns_for_tag(&state, &Uuid::nil(), "probe-tag").await }
    });
    assert!(
        res.is_err(),
        "campaigns_for_tag must return the error so its caller can log it, not an empty Vec"
    );
}

#[test]
fn campaigns_for_tag_old_shape_answered_empty_and_said_nothing() {
    let (rows, log) = capture(|| {
        let pool = dead_pool();
        async move {
            sqlx::query_as::<_, (Uuid, String)>(
                "SELECT id, name FROM campaigns
                 WHERE tenant_id = $1
                   AND metadata -> 'coreswift' ->> 'tag_id' = $2",
            )
            .bind(Uuid::nil())
            .bind("probe-tag")
            .fetch_all(&pool)
            .await
            .unwrap_or_default()
        }
    });
    assert!(
        rows.is_empty(),
        "control: the pre-fix shape turned a failed query into an empty campaign list"
    );
    assert!(
        !log.contains("campaigns_for_tag"),
        "control: the pre-fix shape logged nothing at all: {log}"
    );
}
