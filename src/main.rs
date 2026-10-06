mod auth;
mod body_deadline;
mod config;
mod db;
mod email;
mod email_provider;
mod error;
mod feature_registry;
mod features;
mod handlers;
mod models;
mod routes;
mod security;
mod state;
mod validation;

use std::net::SocketAddr;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // ── Host-side applier mode ────────────────────────────────────────────────────────────────
    // The static marketing page + the three legal pages live on the HOST
    // (/opt/swift/nginx/www/missedcall/) and the server runs in a container with ZERO mounts for
    // them, so the request path never writes them (that attempt is what made PUT /api/v1/admin/site
    // answer 500 after its row had already committed — kanban t_2f99528b). Running the SAME binary
    // on the host with this argument is what materializes them; /opt/swift/bin/mcr-site-apply.sh
    // drives it from cron. Checked BEFORE the config, the migration run and the listener — it needs
    // DATABASE_URL and nothing else, and it must never start a second API against the live port.
    //
    //   missedcallrespondr apply-site-settings            write only the files whose bytes change
    //   missedcallrespondr apply-site-settings --check    render and report, write NOTHING
    //   missedcallrespondr apply-site-settings --emit DIR also write the rendered bytes under DIR
    //                                                     (a comparison artifact for a host probe)
    if std::env::args().nth(1).as_deref() == Some("apply-site-settings") {
        apply_site_settings_mode().await;
        return Ok(());
    }

    let cfg = config::AppConfig::from_env();
    let pool = sqlx::PgPool::connect(&cfg.database_url).await?;

    tracing::info!("Running migrations...");
    db::run_migrations(&pool).await?;
    tracing::info!("Migrations complete");

    // BYOK at-rest encryption posture (PROVIDER_KEY_ENC_SECRET). DISABLED means provider key
    // writes fail closed rather than storing a plaintext credential.
    tracing::info!(
        "Provider key encryption: {} (AES-256 at rest, enc:v1 format)",
        if security::provider_key_crypto::is_configured() {
            "enabled"
        } else {
            "DISABLED - provider key writes will fail closed"
        }
    );

    let workflowswift_url = std::env::var("WORKFLOWSWIFT_URL")
        .unwrap_or_else(|_| "http://localhost:8085/api/incoming".into());

    // Stripe signature freshness (kanban t_4754e612): how far a delivery's `t=` stamp may be from
    // this host's clock before the receiver refuses it even though its HMAC verified. In the boot
    // log for the same reason as the provider-key posture above — an operator diagnosing "every
    // Stripe delivery is being refused" has to be able to read the value in force, and it is also
    // the value that says whether THIS box's clock is the suspect.
    tracing::info!(
        "Stripe webhook signature tolerance: {}s from this host's clock; a correctly-signed \
         delivery further away is answered 503 stripe_signature_timestamp_out_of_tolerance and \
         logged (STRIPE_WEBHOOK_TOLERANCE_SECS, clamped 30..86400, default {})",
        cfg.stripe_signature_tolerance_secs,
        handlers::checkout_handler::DEFAULT_STRIPE_SIGNATURE_TOLERANCE_SECS
    );

    // Telnyx delivery verification (kanban t_0e4ae7b7): the receiver verifies Telnyx's Ed25519
    // signature on every delivery and FAILS CLOSED when this deployment has no account key. In the
    // boot log for the same reason as the bounds above: an operator whose inbound telephony is
    // refusing needs to read the posture at startup, not wait for a refused delivery to find out.
    tracing::info!(
        "Telnyx webhook signature verification: {} (Ed25519 over `<telnyx-timestamp>|<body>` against \
         TELNYX_PUBLIC_KEY; freshness tolerance {}s from this host's clock, \
         TELNYX_SIGNATURE_TOLERANCE_SECS clamped 30..86400, default {})",
        if cfg.telnyx_public_key.is_some() {
            "ENABLED".to_string()
        } else {
            "NOT CONFIGURED - every delivery is refused 503 telnyx_verification_not_configured and \
             nothing is applied; set the account public key from Telnyx Mission Control"
                .to_string()
        },
        cfg.telnyx_signature_tolerance_secs,
        security::telnyx_signature::DEFAULT_SIGNATURE_TOLERANCE_SECS
    );

    // Body-read deadline (kanban t_7f688018): how long a request body may take to arrive before the
    // request is answered 408 and its task, connection and partially-read buffer are released. In
    // the boot log for the same reason as the bounds above — an operator diagnosing "a webhook
    // sender is being refused / the app is holding connections" has to read the value in force, and
    // it is also the number that says whether this host's inbound path is the suspect.
    tracing::info!(
        "Request body-read deadline: {}s on every route that reads a body, 408 above that \
         (BODY_READ_DEADLINE_SECS overrides, clamped {}..={})",
        cfg.body_read_deadline_secs,
        body_deadline::MIN_BODY_READ_DEADLINE_SECS,
        body_deadline::MAX_BODY_READ_DEADLINE_SECS
    );

    let coreswift_url =
        std::env::var("CORESWIFT_URL").unwrap_or_else(|_| "http://localhost:8084".into());

    let funnelswift_url =
        std::env::var("FUNNELSWIFT_URL").unwrap_or_else(|_| "http://localhost:8080".into());

    let app_state = state::AppState {
        pool,
        config: cfg.clone(),
        workflowswift_url,
        coreswift_url,
        funnelswift_url,
    };

    let app = routes::create_router(app_state);
    let addr: SocketAddr = format!("{}:{}", cfg.server_host, cfg.server_port).parse()?;
    tracing::info!("MissedCall Respondr starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// The host-side applier (`missedcallrespondr apply-site-settings`, driven by
/// /opt/swift/bin/mcr-site-apply.sh from cron). It is the ONLY writer of
/// /opt/swift/nginx/www/missedcall/*.html.
///
/// It prints one machine-readable summary line — `site artifacts: written=N skipped=M` — plus one
/// line per file it touched or deliberately left alone, so the cron log and the card's proof can both
/// read what happened without a second probe. `--check` renders and writes NOTHING; `--emit DIR` also
/// drops the rendered bytes elsewhere so they can be compared with the served file byte-for-byte
/// (`MCR_SITE_ROOT` points the whole thing at a throwaway root for a host-run probe).
async fn apply_site_settings_mode() {
    let args: Vec<String> = std::env::args().collect();
    let check = args.iter().any(|a| a == "--check");
    let emit_dir = args
        .iter()
        .position(|a| a == "--emit")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("apply-site-settings: DATABASE_URL is not set");
            std::process::exit(2);
        }
    };

    let pool = match sqlx::PgPool::connect(&url).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("apply-site-settings: cannot connect to the database: {}", e);
            std::process::exit(1);
        }
    };

    let settings = match handlers::site_handler::load_settings(&pool).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "apply-site-settings: cannot read the site settings row: {:?}",
                e
            );
            std::process::exit(1);
        }
    };

    let (targets, skipped) = handlers::site_handler::plan(&settings);

    // --emit: drop the rendered bytes somewhere else so they can be compared byte-for-byte with the
    // served file WITHOUT this process writing anything under the site root.
    if let Some(dir) = emit_dir {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!(
                "apply-site-settings: cannot create --emit dir {}: {}",
                dir, e
            );
            std::process::exit(1);
        }
        for (path, rendered) in &targets {
            let name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "rendered".to_string());
            let dest = std::path::Path::new(&dir).join(name);
            if let Err(e) = std::fs::write(&dest, rendered.as_bytes()) {
                eprintln!(
                    "apply-site-settings: cannot write {}: {}",
                    dest.display(),
                    e
                );
                std::process::exit(1);
            }
            println!("emit {} -> {}", path, dest.display());
        }
    }

    let sha = |b: &[u8]| hex::encode(ring::digest::digest(&ring::digest::SHA256, b));

    if check {
        for (path, reason) in &skipped {
            println!("skip {} {}", path, reason);
        }
        for (path, rendered) in &targets {
            let now = std::fs::read_to_string(path).unwrap_or_default();
            let state = if now == *rendered {
                "unchanged"
            } else {
                "would-write"
            };
            println!(
                "check {} state={} sha256={} served_sha256={}",
                path,
                state,
                sha(rendered.as_bytes()),
                sha(now.as_bytes())
            );
        }
        println!(
            "site artifacts (check, nothing written): root={} targets={} skipped={}",
            handlers::site_handler::site_root(),
            targets.len(),
            skipped.len()
        );
        return;
    }

    let (written, skipped) = handlers::site_handler::apply_to_disk(&settings);
    for path in &written {
        println!("write {} (the rendered bytes differ from the file)", path);
    }
    for (path, reason) in &skipped {
        println!("skip {} {}", path, reason);
    }
    println!(
        "site artifacts: written={} skipped={}",
        written.len(),
        skipped.len()
    );
}
