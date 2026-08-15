use crate::error::AppError;
use crate::state::AppState;
use axum::{extract::State, Json};
use serde_json::json;
use sqlx::Row;
use std::fs;

const SITE_KEY: &str = "missedcallrespondr_site";

pub async fn get_site(State(state): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let defaults = default_site_settings();
    let row = sqlx::query("SELECT value FROM admin_settings WHERE key = $1")
        .bind(SITE_KEY)
        .fetch_optional(&state.pool)
        .await?;
    let settings = match row {
        Some(r) => {
            let val: serde_json::Value = r.try_get("value")?;
            merge_json(defaults, val)
        }
        None => defaults,
    };
    Ok(Json(settings))
}

pub async fn update_site(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let existing_row = sqlx::query("SELECT value FROM admin_settings WHERE key = $1")
        .bind(SITE_KEY)
        .fetch_optional(&state.pool)
        .await?;
    let merged = match existing_row {
        Some(r) => {
            let v: serde_json::Value = r.try_get("value")?;
            merge_json(v, req)
        }
        None => req,
    };
    sqlx::query("INSERT INTO admin_settings (key, value, description, updated_at) VALUES ($1, $2::jsonb, 'MissedCall Respondr site settings', NOW()) ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, updated_at = NOW()")
        .bind(SITE_KEY).bind(merged.to_string()).execute(&state.pool).await?;
    regenerate_html(&merged)?;
    Ok(Json(json!({"message": "Site settings updated"})))
}

fn regenerate_html(settings: &serde_json::Value) -> Result<(), AppError> {
    let html_path = "/opt/swift/nginx/www/missedcall/index.html";
    let html = fs::read_to_string(html_path)
        .map_err(|e| AppError::Internal(format!("Failed to read {}: {}", html_path, e)))?;
    let html = inject_settings(&html, settings);
    fs::write(html_path, &html)
        .map_err(|e| AppError::Internal(format!("Failed to write: {}", e)))?;
    // Regenerate legal pages
    if let (Some(t), Some(p), Some(r)) = (
        settings.get("legal_tos").and_then(|v| v.as_str()),
        settings.get("legal_privacy").and_then(|v| v.as_str()),
        settings.get("legal_refunds").and_then(|v| v.as_str()),
    ) {
        regen_legal(
            "terms",
            "Terms of Service",
            t,
            "/opt/swift/nginx/www/missedcall/",
        )?;
        regen_legal(
            "privacy",
            "Privacy Policy",
            p,
            "/opt/swift/nginx/www/missedcall/",
        )?;
        regen_legal(
            "refunds",
            "Refund & Cancellation Policy",
            r,
            "/opt/swift/nginx/www/missedcall/",
        )?;
    }
    Ok(())
}

fn regen_legal(slug: &str, title: &str, text: &str, dir: &str) -> Result<(), AppError> {
    let page = format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{} — MissedCall Respondr</title><style>body{{font-family:system-ui,sans-serif;background:#0f0f0f;color:#e5e5e5;line-height:1.7;margin:0;padding:0}}.container{{max-width:800px;margin:0 auto;padding:60px 24px}}h1{{font-size:2rem;color:#10b981}}a{{color:#10b981}}</style></head><body><div class="container"><h1>{}</h1>{}</div></body></html>"#,
        title, title, text
    );
    fs::write(format!("{}{}.html", dir, slug), &page)
        .map_err(|e| AppError::Internal(format!("Failed: {}", e)))?;
    Ok(())
}

fn inject_settings(html: &str, s: &serde_json::Value) -> String {
    let mut r = html.to_string();
    if let Some(t) = s.get("title").and_then(|v| v.as_str()) {
        replace_title(&mut r, t);
    }
    if let Some(d) = s.get("description").and_then(|v| v.as_str()) {
        upsert_meta(&mut r, "description", d);
    }
    if let Some(k) = s.get("keywords").and_then(|v| v.as_str()) {
        upsert_meta(&mut r, "keywords", k);
    }
    upsert_og(&mut r, "og:title", s.get("og_title"));
    upsert_og(&mut r, "og:description", s.get("og_description"));
    upsert_og(&mut r, "og:image", s.get("og_image_url"));
    if let Some(sj) = s.get("schema_json").and_then(|v| v.as_str()) {
        upsert_schema(&mut r, sj);
    }
    let ga = s.get("ga_id").and_then(|v| v.as_str()).unwrap_or("");
    let gtm = s.get("gtm_id").and_then(|v| v.as_str()).unwrap_or("");
    remove_ga_gtm(&mut r);
    if !ga.is_empty() {
        inject_head(&mut r, &format!("<script async src=\"https://www.googletagmanager.com/gtag/js?id={}\"></script><script>window.dataLayer=window.dataLayer||[];function gtag(){{dataLayer.push(arguments);}}gtag('js',new Date());gtag('config','{}');</script>", ga, ga));
    }
    if !gtm.is_empty() {
        inject_head(&mut r, &format!("<script>(function(w,d,s,l,i){{w[l]=w[l]||[];w[l].push({{'gtm.start':new Date().getTime(),event:'gtm.js'}});var f=d.getElementsByTagName(s)[0],j=d.createElement(s);j.async=true;j.src='https://www.googletagmanager.com/gtm.js?id='+i;f.parentNode.insertBefore(j,f);}})(window,document,'script','dataLayer','{}');</script>", gtm));
    }
    if let Some(hs) = s.get("head_scripts").and_then(|v| v.as_str()) {
        if !hs.is_empty() {
            inject_head(&mut r, hs);
        }
    }
    if let Some(bs) = s.get("body_scripts").and_then(|v| v.as_str()) {
        if !bs.is_empty() {
            inject_body_end(&mut r, bs);
        }
    }
    r
}

fn replace_title(r: &mut String, t: &str) {
    if let Some(p) = r.find("<title>") {
        let a = p + 7;
        if let Some(e) = r[a..].find("</title>") {
            r.replace_range(a..a + e, t);
        }
    } else {
        inject_head(r, &format!("<title>{}</title>", t));
    }
}
fn upsert_meta(r: &mut String, n: &str, c: &str) {
    let pat = format!("<meta name=\"{}\"", n);
    if let Some(p) = r.find(&pat) {
        let a = &r[p..];
        if let Some(e) = a.find('>') {
            r.replace_range(
                p..p + e + 1,
                &format!("<meta name=\"{}\" content=\"{}\">", n, c),
            );
        }
    } else {
        inject_head(r, &format!("<meta name=\"{}\" content=\"{}\">", n, c));
    }
}
fn upsert_og(r: &mut String, p: &str, v: Option<&serde_json::Value>) {
    if let Some(c) = v.and_then(|v| v.as_str()) {
        let pat = format!("<meta property=\"{}\"", p);
        if let Some(pos) = r.find(&pat) {
            let a = &r[pos..];
            if let Some(e) = a.find('>') {
                r.replace_range(
                    pos..pos + e + 1,
                    &format!("<meta property=\"{}\" content=\"{}\">", p, c),
                );
            }
        } else {
            inject_head(r, &format!("<meta property=\"{}\" content=\"{}\">", p, c));
        }
    }
}
fn upsert_schema(r: &mut String, s: &str) {
    let o = r#"<script type="application/ld+json">"#;
    if let Some(p) = r.find(o) {
        let a = p + o.len();
        if let Some(e) = r[a..].find("</script>") {
            r.replace_range(a..a + e, s);
        }
    } else {
        inject_head(
            r,
            &format!(r#"<script type="application/ld+json">{}</script>"#, s),
        );
    }
}
fn remove_ga_gtm(r: &mut String) {
    for (sp, ep) in &[
        (
            r#"<script async src="https://www.googletagmanager.com/gtag/js"#,
            "</script>",
        ),
        (r#"<script>window.dataLayer"#, "</script>"),
        (r#"<script>(function(w,d,s,l,i)"#, "</script>"),
        (
            r#"<noscript><iframe src="https://www.googletagmanager.com/ns.html"#,
            "</noscript>",
        ),
    ] {
        loop {
            if let Some(p) = r.find(sp) {
                if let Some(e) = r[p..].find(ep) {
                    r.replace_range(p..p + e + ep.len(), "");
                    continue;
                }
            }
            break;
        }
    }
    while r.contains("\n\n\n") {
        *r = r.replace("\n\n\n", "\n\n");
    }
}
fn inject_head(r: &mut String, c: &str) {
    if let Some(p) = r.rfind("</head>") {
        r.insert_str(p, &format!("\n  {}", c));
    }
}
fn inject_body_end(r: &mut String, c: &str) {
    if let Some(p) = r.rfind("</body>") {
        r.insert_str(p, &format!("\n  {}", c));
    }
}
fn merge_json(a: serde_json::Value, b: serde_json::Value) -> serde_json::Value {
    match (a, b) {
        (serde_json::Value::Object(mut am), serde_json::Value::Object(bm)) => {
            for (k, v) in bm {
                am.insert(k, v);
            }
            serde_json::Value::Object(am)
        }
        (_, b) => b,
    }
}

fn default_site_settings() -> serde_json::Value {
    json!({
        "title": "MissedCall Respondr | Never Miss a Business Call Again",
        "description": "Automatically respond to missed calls with SMS, follow-ups, and smart routing. Turn missed calls into booked appointments.",
        "keywords": "missed call auto reply, SMS auto responder, call automation, lead capture, missed call text back",
        "og_title": "MissedCall Respondr — Never Miss a Lead Again",
        "og_description": "Automatically respond to missed calls with instant SMS replies, follow-ups, and intelligent routing.",
        "og_image_url": "", "favicon_url": "", "canonical_url": "https://missedcallrespondr.com",
        "ga_id": "", "gtm_id": "", "head_scripts": "", "body_scripts": "",
        "schema_json": "{\"@context\":\"https://schema.org\",\"@type\":\"SoftwareApplication\",\"name\":\"MissedCall Respondr\",\"applicationCategory\":\"BusinessApplication\",\"description\":\"Automated missed call SMS response platform.\"}",
        "legal_tos": "", "legal_privacy": "", "legal_refunds": "",
        "homepage": { "headline": "Never Miss a Business Call Again", "subheadline": "Instant SMS replies, smart follow-ups, turn missed calls into revenue." }
    })
}
