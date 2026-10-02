//! missedcallrespondr site settings — the DB row is the SOURCE OF TRUTH; the static pages are
//! materialized by a HOST-side applier, never by the request path.
//!
//! WHY (kanban t_2f99528b, evidence /opt/swift/audits/t_2f99528b/; arm decided in t_1f427190 and
//! ported here from the ADASwift reference implementation)
//! --------------------------------------------------------------------------------------------
//! The static marketing page + the three legal pages are plain files under
//! `/opt/swift/nginx/www/missedcall/` (served by nginx for missedcallrespondr.com). The service runs
//! in a container with ZERO mounts (`docker inspect missedcallrespondr --format '{{json .Mounts}}'`
//! == `[]`, and the container has no `/opt/swift` at all), so `PUT /api/v1/admin/site` could never
//! write them: it UPSERTed the `admin_settings` row FIRST, then died in `regenerate_html`, answering
//! 500 *after* the write had landed (measured live 2026-10-02: PUT -> 500
//! "Failed to read /opt/swift/nginx/www/missedcall/index.html: No such file or directory" with the
//! settings row already stored — a partial apply behind a red toast).
//!
//! ANY instance run on the HOST, however, wrote the real served root — and that root is the target
//! of the repo->served publish gate (`/opt/swift/fleet/marketing-www-parity.py`), so the runtime
//! writer and the repo were TWO writers of one tree. That is the class that produced the
//! t_ee69c81c regression (a routine GET-then-PUT replaced the published Privacy/Terms/Refund pages
//! with title-only stubs).
//!
//! The arms were (a) mount the served root into the container and let the request path write it, or
//! (b) retire the request-path write. Arm (b) — the pattern CoreSwift-CRM proved in t_02986434 and
//! ADASwift in t_1f427190: the row is the source of truth, `update_site` writes ONLY the row and
//! answers 2xx naming the applier, and `/opt/swift/bin/mcr-site-apply.sh` runs
//! `missedcallrespondr apply-site-settings` on the HOST (where the files actually are) from cron.
//! Arm (a) was rejected because the served root is also the publish gate's target (a container
//! writing it makes two writers for one tree with no reconciliation) and it would need a root-owned
//! host path mounted read-write into an unprivileged service.
//!
//! INVARIANTS THIS MODULE KEEPS
//! --------------------------------------------------------------------------------------------
//! * No request path writes a file. `update_site` = one UPSERT + 2xx. A PUT therefore CANNOT change
//!   a tracked served page; only the publish gate or the host applier can.
//! * A blank `legal_*` can never downgrade a published page: the applier guards on the VALUE
//!   (`trim().is_empty()` -> skip + reason), and `preserve_nonempty_legal` refuses the same blank at
//!   the STORE, so the GET-then-PUT round trip (GET merges the code defaults, which carry `""`)
//!   cannot blank the row either. Guarding on the VALUE rather than on key presence is the fix
//!   t_ee69c81c shipped and it is preserved here.
//! * The applier is idempotent: a file is rewritten only when its bytes would change, and every
//!   injection is a pure function of the bytes already on disk, so a scheduled run is free and the
//!   repo/served parity gate stays clean.
//! * The authored page DESIGN is never re-authored by Rust: `index.html` is read-modify-write (head
//!   injection only), and each legal page has only its body text replaced between `<h1>…</h1>` and
//!   the trailing `<div class="back">`, so the page a visitor receives keeps the markup the repo
//!   publishes.
//! * Addresses are escaped at RENDER time (`@` -> `&#64;`), the fleet's served-page convention
//!   (docs/fleet-marketing-www.md §6.1, kanban t_d4347fb5 / t_cda04aec): the DB holds the human
//!   form, the rendered page carries the entity.
//! * `MCR_SITE_ROOT` overrides the served root so a probe/dev instance can never write production
//!   bytes (the harness hygiene the card required — the t_ee69c81c proof ran on the HOST and wrote
//!   the live root).
use crate::error::AppError;
use crate::state::AppState;
use axum::{extract::State, Json};
use serde_json::{json, Value};
use sqlx::Row;
use std::fs;

const SITE_KEY: &str = "missedcallrespondr_site";

/// The production served root. HOST path: the container has no mount for it, which is exactly why
/// the request path must not try to write it.
pub(crate) const DEFAULT_SITE_ROOT: &str = "/opt/swift/nginx/www/missedcall";

/// The legal pages this applier maintains: (slug, <h1>/<title>, settings key).
pub(crate) const LEGAL_PAGES: [(&str, &str, &str); 3] = [
    ("terms", "Terms of Service", "legal_tos"),
    ("privacy", "Privacy Policy", "legal_privacy"),
    ("refunds", "Refund & Cancellation Policy", "legal_refunds"),
];

/// The legal keys `preserve_nonempty_legal` protects at the store.
const LEGAL_KEYS: [&str; 3] = ["legal_tos", "legal_privacy", "legal_refunds"];

/// The served root this process will render into. `MCR_SITE_ROOT` exists so a host-run probe can
/// point the applier at a throwaway directory instead of writing production bytes.
pub(crate) fn site_root() -> String {
    let raw = std::env::var("MCR_SITE_ROOT").unwrap_or_else(|_| DEFAULT_SITE_ROOT.to_string());
    if raw.is_empty() {
        DEFAULT_SITE_ROOT.to_string()
    } else if raw.ends_with('/') {
        raw
    } else {
        format!("{}/", raw)
    }
}

/// GET /api/v1/admin/site — the stored settings merged over the code defaults.
pub async fn get_site(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    Ok(Json(load_settings(&state.pool).await?))
}

/// The stored settings merged over the code defaults — the same value `get_site` serves, and the
/// input to the host-side applier.
pub(crate) async fn load_settings(db: &sqlx::PgPool) -> Result<Value, AppError> {
    let defaults = default_site_settings();
    let row = sqlx::query("SELECT value FROM admin_settings WHERE key = $1")
        .bind(SITE_KEY)
        .fetch_optional(db)
        .await?;
    Ok(match row {
        Some(r) => {
            let val: Value = r.try_get("value")?;
            merge_json(defaults, val)
        }
        None => defaults,
    })
}

/// PUT /api/v1/admin/site — save site settings.
///
/// Deliberately NO file writes on this path. The static pages are HOST paths and this service runs in
/// a container with no mount for them, so writing them here could only ever answer 500 — after the
/// row above had already committed (kanban t_2f99528b: measured PUT -> 500 with the row stored).
/// The row IS the source of truth (`get_site` reads it); `/opt/swift/bin/mcr-site-apply.sh` is the
/// only writer of the pages.
pub async fn update_site(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let existing_row = sqlx::query("SELECT value FROM admin_settings WHERE key = $1")
        .bind(SITE_KEY)
        .fetch_optional(&state.pool)
        .await?;
    let existing: Option<Value> = match existing_row {
        Some(r) => Some(r.try_get("value")?),
        None => None,
    };

    let merged = match &existing {
        Some(v) => merge_json(v.clone(), req),
        None => req,
    };
    let merged = preserve_nonempty_legal(existing.as_ref(), merged);

    sqlx::query("INSERT INTO admin_settings (key, value, description, updated_at) VALUES ($1, $2::jsonb, 'MissedCall Respondr site settings', NOW()) ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, updated_at = NOW()")
        .bind(SITE_KEY)
        .bind(merged.to_string())
        .execute(&state.pool)
        .await?;

    Ok(Json(json!({
        "message": "Site settings saved",
        "settings": merged,
        "static_pages": {
            "writer": "/opt/swift/bin/mcr-site-apply.sh (missedcallrespondr apply-site-settings)",
            "within_minutes": 5
        }
    })))
}

/// A blank string (or null, or a missing key) is how "the operator cleared this field" and "this
/// form was rendered from a GET that merged the code defaults" look identical on the wire — and the
/// defaults carry `""` for all three legal keys. Refuse the blank at the STORE when the row already
/// holds text, so a GET-then-PUT round trip can never blank a live policy's source text.
fn preserve_nonempty_legal(existing: Option<&Value>, mut merged: Value) -> Value {
    for key in LEGAL_KEYS {
        let stored_has_text = existing
            .and_then(|e| e.get(key))
            .map(|v| !is_blank(Some(v)))
            .unwrap_or(false);
        if stored_has_text && is_blank(merged.get(key)) {
            if let (Some(dst), Some(src)) = (merged.get_mut(key), existing.and_then(|e| e.get(key)))
            {
                *dst = src.clone();
                tracing::warn!(
                    key,
                    "blank legal value refused: the stored legal text was preserved"
                );
            }
        }
    }
    merged
}

fn is_blank(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(_) => false,
    }
}

/// `(path, rendered bytes)` for every file the applier owns and has a value for.
pub(crate) type PlanTargets = Vec<(String, String)>;
/// `(path, reason)` for every file deliberately left alone.
pub(crate) type PlanSkips = Vec<(String, String)>;

/// Render every file this applier owns, WITHOUT touching the disk.
///
/// `targets` is `(path, rendered bytes)` for every file whose source value exists; `skipped` is
/// `(path, reason)` for the ones deliberately left alone — a blank legal value lands here, never in
/// `targets`.
pub(crate) fn plan(settings: &Value) -> (PlanTargets, PlanSkips) {
    let mut targets: PlanTargets = Vec::new();
    let mut skipped: PlanSkips = Vec::new();
    let root = site_root();

    if !std::path::Path::new(&root).is_dir() {
        skipped.push((
            root.clone(),
            "directory is not present in this runtime (host-only path)".to_string(),
        ));
        return (targets, skipped);
    }

    let index = format!("{}index.html", root);
    match fs::read_to_string(&index) {
        Ok(before) => targets.push((index, render_index(&before, settings))),
        Err(e) => skipped.push((index, format!("unreadable: {}", e))),
    }

    for (slug, title, key) in LEGAL_PAGES {
        let path = format!("{}{}.html", root, slug);
        match settings.get(key).and_then(|v| v.as_str()) {
            // Blank or absent means "no policy text configured" -> leave the published page alone.
            Some(text) if !text.trim().is_empty() => match fs::read_to_string(&path) {
                Ok(before) => match render_legal(&before, title, text) {
                    Some(rendered) => targets.push((path, rendered)),
                    None => skipped.push((
                        path,
                        "the published page carries no <h1>…</h1>/back-link scaffold to replace"
                            .to_string(),
                    )),
                },
                Err(e) => skipped.push((path, format!("unreadable: {}", e))),
            },
            _ => skipped.push((
                path,
                format!(
                    "{} is blank or absent - the published page is left alone",
                    key
                ),
            )),
        }
    }

    (targets, skipped)
}

/// Materialize `settings` into the static marketing page + the three legal pages.
///
/// Returns `(written, skipped)`; every skipped entry is `(path, reason)`. Idempotent: a file is only
/// rewritten when its bytes would change, and nothing here is fatal — the caller reports the outcome
/// so no surface can claim a regeneration that did not happen.
pub(crate) fn apply_to_disk(settings: &Value) -> (Vec<String>, PlanSkips) {
    let (targets, mut skipped) = plan(settings);
    let mut written: Vec<String> = Vec::new();

    for (path, rendered) in targets {
        match fs::read_to_string(&path) {
            Ok(before) if before == rendered => skipped.push((path, "unchanged".to_string())),
            _ => match fs::write(&path, rendered.as_bytes()) {
                Ok(_) => written.push(path),
                Err(e) => skipped.push((path, e.to_string())),
            },
        }
    }

    (written, skipped)
}

/// The marketing page: the SEO/tracking injection, as a pure function of the bytes already on disk
/// (so the applier can compare before writing) — the authored body of the page is never touched.
fn render_index(before: &str, settings: &Value) -> String {
    inject_settings(before, settings)
}

/// One legal page: ONLY the body between `<h1>{title}</h1>` and the trailing `<div class="back">` is
/// replaced, so the page keeps the markup the repo publishes (a repo redesign is preserved).
///
/// `None` when the scaffold is not there — the caller skips the file rather than guessing.
fn render_legal(before: &str, title: &str, text: &str) -> Option<String> {
    let h1 = format!("<h1>{}</h1>", title);
    let open_end = before.find(&h1)? + h1.len();
    let back = "<div class=\"back\">";
    let close_start = before[open_end..].find(back)? + open_end;

    let mut out = String::with_capacity(before.len() + text.len());
    out.push_str(&before[..open_end]);
    out.push('\n');
    out.push_str(&escape_addresses(text));
    out.push('\n');
    out.push_str(&before[close_start..]);
    Some(out)
}

/// Write every address in an HTML body as the entity `&#64;`.
///
/// The served-page convention for this fleet: a literal address in a served HTML page is rewritten
/// per-request by the edge (Cloudflare) and is flagged as a hazard by the repo/served parity gate, so
/// an address in page TEXT is authored as the entity, which renders identically and is left alone
/// (docs/fleet-marketing-www.md §6.1, kanban t_d4347fb5 / t_cda04aec). The DB holds the human form
/// (`support@swiftsoftware.net`) because that is what an operator types in the panel; the applier is
/// the single enforcement point that turns it into the entity on the page. An already-escaped value
/// is left as it is (the transform is idempotent), which is what makes `render(DB) == served` on the
/// pages that already carry the entity.
fn escape_addresses(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'@' && looks_like_address(b, i) {
            out.push_str("&#64;");
            i += 1;
        } else {
            // Copy one full UTF-8 char, never a byte: the legal text is prose and may carry any
            // character.
            let ch = match text[i..].chars().next() {
                Some(c) => c,
                None => break,
            };
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn looks_like_address(b: &[u8], at: usize) -> bool {
    // Local part: at least one local char immediately before the '@'.
    let mut ls = at;
    while ls > 0 && is_local_char(b[ls - 1]) {
        ls -= 1;
    }
    if ls == at {
        return false;
    }

    // Domain: a run of domain chars, at least one dot, an alphabetic TLD of 2+ chars.
    //
    // The run may end in sentence punctuation that IS a domain char — a policy sentence ends
    // `...notice to support@example.com.` and the '.' belongs to the sentence, not the domain — so
    // trailing dots/hyphens are trimmed before the labels are validated; otherwise the address would
    // be skipped and a literal '@' published (measured on ADASwift, kanban t_1f427190).
    let mut de = at + 1;
    while de < b.len() && is_domain_char(b[de]) {
        de += 1;
    }
    while de > at + 1 && (b[de - 1] == b'.' || b[de - 1] == b'-') {
        de -= 1;
    }
    let domain = match std::str::from_utf8(&b[at + 1..de]) {
        Ok(d) => d,
        Err(_) => return false,
    };
    let parts: Vec<&str> = domain.split('.').collect();
    if parts.len() < 2 {
        return false;
    }
    let tld = parts[parts.len() - 1];
    if tld.len() < 2 || !tld.bytes().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && !p.starts_with('-') && !p.ends_with('-'))
}

fn is_local_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_domain_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'.' || c == b'-'
}

/// Every injection whose content cannot be found again by a stable pattern (GA/GTM snippets, the
/// operator's head/body scripts, an injected favicon link) is written between `<!--mcr:id-->` and
/// `<!--/mcr:id-->` markers. A re-render removes its OWN previous block and re-inserts it at the same
/// anchor in the same fixed order, so it REPLACES rather than appends: without this a non-empty
/// `head_scripts` value would grow the file on every 5-minute apply (a cron-driven content leak).
/// The newline lives INSIDE the marker pair, so removal is byte-exact.
fn remove_marked(r: &mut String, id: &str) {
    let open = format!("<!--mcr:{}-->", id);
    let close = format!("<!--/mcr:{}-->", id);
    while let Some(p) = r.find(&open) {
        let e = match r[p..].find(&close) {
            Some(e) => e,
            None => break,
        };
        r.replace_range(p..p + e + close.len(), "");
    }
}

fn inject_marked(r: &mut String, id: &str, content: &str, anchor: &str) {
    remove_marked(r, id);
    if content.is_empty() {
        return;
    }
    let block = format!("<!--mcr:{id}-->\n  {content}\n  <!--/mcr:{id}-->");
    if let Some(p) = r.rfind(anchor) {
        r.insert_str(p, &block);
    }
}

fn inject_marked_head(r: &mut String, id: &str, content: &str) {
    inject_marked(r, id, content, "</head>");
}

fn inject_marked_body(r: &mut String, id: &str, content: &str) {
    inject_marked(r, id, content, "</body>");
}

/// Replace an existing `<link rel="…" href>` in place, or inject one when the page has none. The
/// in-place form is what keeps this idempotent for the values a tracked page already carries.
fn upsert_link(r: &mut String, rel: &str, href: &str) {
    let pat = format!("<link rel=\"{}\"", rel);
    if let Some(p) = r.find(&pat) {
        if let Some(e) = r[p..].find('>') {
            r.replace_range(
                p..p + e + 1,
                &format!("<link rel=\"{}\" href=\"{}\">", rel, href),
            );
            return;
        }
    }
    inject_marked_head(
        r,
        &format!("link-{}", rel.replace(' ', "-")),
        &format!("<link rel=\"{}\" href=\"{}\">", rel, href),
    );
}

fn inject_settings(html: &str, s: &Value) -> String {
    let mut r = html.to_string();

    if let Some(t) = s.get("title").and_then(|v| v.as_str()) {
        if !t.is_empty() {
            replace_title(&mut r, t);
        }
    }
    if let Some(d) = s.get("description").and_then(|v| v.as_str()) {
        if !d.is_empty() {
            upsert_meta(&mut r, "description", d);
        }
    }
    if let Some(k) = s.get("keywords").and_then(|v| v.as_str()) {
        if !k.is_empty() {
            upsert_meta(&mut r, "keywords", k);
        }
    }
    upsert_og(&mut r, "og:title", s.get("og_title"));
    upsert_og(&mut r, "og:description", s.get("og_description"));
    upsert_og(&mut r, "og:image", s.get("og_image_url"));
    if let Some(c) = s.get("canonical_url").and_then(|v| v.as_str()) {
        if !c.is_empty() {
            upsert_link(&mut r, "canonical", c);
        }
    }
    if let Some(f) = s.get("favicon_url").and_then(|v| v.as_str()) {
        inject_marked_head(
            &mut r,
            "link-icon",
            &if f.is_empty() {
                String::new()
            } else {
                format!("<link rel=\"icon\" href=\"{}\">", f)
            },
        );
    }
    if let Some(sj) = s.get("schema_json").and_then(|v| v.as_str()) {
        if !sj.is_empty() {
            upsert_schema(&mut r, sj);
        }
    }

    // GA/GTM are marker-managed: an operator who clears the field REMOVES the snippet, and a re-run
    // replaces the applier's own previous block instead of stacking another copy. Nothing authored in
    // the page is stripped — these fields are an override, not a rewrite of the tracked page.
    let ga = s.get("ga_id").and_then(|v| v.as_str()).unwrap_or("");
    let gtm = s.get("gtm_id").and_then(|v| v.as_str()).unwrap_or("");
    inject_marked_head(
        &mut r,
        "ga",
        &if ga.is_empty() {
            String::new()
        } else {
            format!("<script async src=\"https://www.googletagmanager.com/gtag/js?id={}\"></script><script>window.dataLayer=window.dataLayer||[];function gtag(){{dataLayer.push(arguments);}}gtag('js',new Date());gtag('config','{}');</script>", ga, ga)
        },
    );
    inject_marked_head(
        &mut r,
        "gtm",
        &if gtm.is_empty() {
            String::new()
        } else {
            format!("<script>(function(w,d,s,l,i){{w[l]=w[l]||[];w[l].push({{'gtm.start':new Date().getTime(),event:'gtm.js'}});var f=d.getElementsByTagName(s)[0],j=d.createElement(s);j.async=true;j.src='https://www.googletagmanager.com/gtm.js?id='+i;f.parentNode.insertBefore(j,f);}})(window,document,'script','dataLayer','{}');</script>", gtm)
        },
    );

    inject_marked_head(
        &mut r,
        "head-scripts",
        s.get("head_scripts").and_then(|v| v.as_str()).unwrap_or(""),
    );
    inject_marked_body(
        &mut r,
        "body-scripts",
        s.get("body_scripts").and_then(|v| v.as_str()).unwrap_or(""),
    );

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
fn upsert_og(r: &mut String, p: &str, v: Option<&Value>) {
    if let Some(c) = v.and_then(|v| v.as_str()) {
        if c.is_empty() {
            return;
        }
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
fn inject_head(r: &mut String, c: &str) {
    if let Some(p) = r.rfind("</head>") {
        r.insert_str(p, &format!("\n  {}", c));
    }
}
fn merge_json(a: Value, b: Value) -> Value {
    match (a, b) {
        (Value::Object(mut am), Value::Object(bm)) => {
            for (k, v) in bm {
                am.insert(k, v);
            }
            Value::Object(am)
        }
        (_, b) => b,
    }
}

fn default_site_settings() -> Value {
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

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "<!DOCTYPE html><html><head><title>old</title>\n<meta name=\"description\" content=\"d\">\n<link rel=\"canonical\" href=\"https://x/\">\n<script type=\"application/ld+json\">\n{\"a\":1}\n</script>\n</head><body><h1>hi</h1></body></html>";

    fn settings() -> Value {
        json!({
            "title": "new title",
            "description": "new d",
            "keywords": "k",
            "og_title": "ot",
            "og_description": "od",
            "og_image_url": "https://x/i.png",
            "canonical_url": "https://missedcallrespondr.com",
            "favicon_url": "https://x/f.ico",
            "ga_id": "G-1",
            "gtm_id": "",
            "head_scripts": "<script>var a=1;</script>",
            "body_scripts": "<script>var b=2;</script>",
            "schema_json": "{\"a\":2}"
        })
    }

    #[test]
    fn index_render_is_idempotent() {
        // The cron applies every 5 minutes: a second render over the first render's own output MUST
        // be byte-identical, or the applier would grow the page forever.
        let once = render_index(PAGE, &settings());
        let twice = render_index(&once, &settings());
        assert_eq!(
            once, twice,
            "a re-render changed the bytes it had just written"
        );
    }

    #[test]
    fn index_render_does_not_grow_with_scripts() {
        let once = render_index(PAGE, &settings());
        let twice = render_index(&once, &settings());
        assert_eq!(once.len(), twice.len());
        assert_eq!(once.matches("mcr:head-scripts").count(), 2);
        assert_eq!(twice.matches("mcr:head-scripts").count(), 2);
    }

    #[test]
    fn cleared_script_fields_remove_their_block() {
        let with = render_index(PAGE, &settings());
        let mut s = settings();
        s["head_scripts"] = json!("");
        s["body_scripts"] = json!("");
        let without = render_index(&with, &s);
        assert!(!without.contains("mcr:head-scripts"));
        assert!(!without.contains("mcr:body-scripts"));
    }

    #[test]
    fn legal_body_is_replaced_between_h1_and_back_link() {
        let before = "<html>\n<h1>Terms of Service</h1>\nAuthored body.\n<div class=\"back\"><a href=\"/\">← Back</a></div>\n</html>";
        let out = render_legal(before, "Terms of Service", "New body.").unwrap();
        assert!(out.contains("<h1>Terms of Service</h1>\nNew body.\n<div class=\"back\">"));
        assert!(!out.contains("Authored body."));
        // stable on the second pass
        assert_eq!(
            out,
            render_legal(&out, "Terms of Service", "New body.").unwrap()
        );
    }

    #[test]
    fn legal_render_is_none_without_the_scaffold() {
        assert!(render_legal(
            "<html><body>no scaffold</body></html>",
            "Terms of Service",
            "x"
        )
        .is_none());
    }

    #[test]
    fn an_address_at_the_end_of_a_sentence_is_still_escaped() {
        // The sentence's full stop is a domain char: trimming it is what keeps the address escaped.
        assert_eq!(
            escape_addresses("notice to support@swiftsoftware.net."),
            "notice to support&#64;swiftsoftware.net."
        );
    }

    #[test]
    fn an_already_escaped_address_is_not_double_escaped() {
        assert_eq!(
            escape_addresses("Email: support&#64;swiftsoftware.net (no literal @)"),
            "Email: support&#64;swiftsoftware.net (no literal @)"
        );
    }

    #[test]
    fn a_bare_at_sign_is_not_escaped() {
        assert_eq!(escape_addresses("schema.org @ 2026"), "schema.org @ 2026");
    }

    #[test]
    fn a_blank_legal_value_never_reaches_the_plan() {
        let mut s = settings();
        s["legal_tos"] = json!("");
        s["legal_privacy"] = json!("   ");
        let (targets, skips) = plan(&s);
        assert!(
            !targets
                .iter()
                .any(|(p, _)| p.ends_with("terms.html") || p.ends_with("privacy.html")),
            "a blank legal value became a write target"
        );
        assert!(skips
            .iter()
            .any(|(p, r)| p.ends_with("terms.html") && r.contains("blank")));
    }

    #[test]
    fn a_blank_legal_value_cannot_blank_the_stored_text() {
        let existing = json!({"legal_tos": "the real policy text"});
        let merged = preserve_nonempty_legal(Some(&existing), json!({"legal_tos": ""}));
        assert_eq!(merged["legal_tos"], json!("the real policy text"));
    }

    #[test]
    fn a_first_write_of_a_legal_value_is_kept() {
        let merged = preserve_nonempty_legal(None, json!({"legal_tos": "fresh text"}));
        assert_eq!(merged["legal_tos"], json!("fresh text"));
    }

    #[test]
    fn site_root_env_override_is_honoured_and_slash_normalised() {
        // The card's harness-hygiene requirement: a host-run probe must be able to point the applier
        // somewhere other than the production root.
        std::env::set_var("MCR_SITE_ROOT", "/tmp/mcr-probe-root");
        assert_eq!(site_root(), "/tmp/mcr-probe-root/");
        std::env::remove_var("MCR_SITE_ROOT");
        assert_eq!(site_root(), "/opt/swift/nginx/www/missedcall/");
    }
}
