#!/usr/bin/env python3
"""Full read+write MCP acceptance for MissedCall Respondr (kanban t_bb4d4d33).

Proves the +16 write tools the CEO write contract (`docs/mcp-write-access-2026-11-25.md`) demands,
live against http://127.0.0.1:8088/mcp and the app's /api/v1 REST surface:

  1. tools/list before/after counts (9 -> 25); every write tool read_only=false, min_role workspace.
  2. a read-only key sees no write tool in tools/list and a direct write call is refused.
  3. per entity (contact, campaign, rule, follow-up, suppression, number) with a workspace key:
     create -> read-back -> update -> archive/remove; the read tool then shows the row archived or
     gone, and a foreign id (a real row in a SECOND probe tenant) answers not_found.
  4. REST parity: POST/PUT/DELETE per entity with an mcr_ key answers 2xx, a foreign id 404s, rows
     stay tenant-scoped.
  5. platform-admin refusal: GET /api/v1/admin/stats answers 403 to the workspace key, 401 to anon.
  6. audit: one JSON line per write carrying the key label + entity id, zero full-key material.
  7. the REAL CEO key (vault `missedcall-mcp-giraudy`) sees 25 tools and does one create->archive
     cycle on a contact named probe_t_bb4d4d33, then removes it.
  8. teardown: probe tenants (real signup, X-Swift-Harness marker) and probe rows removed, residue 0.

Self-cleaning, stdlib only, exit non-zero on any failure. Never prints a full key.
"""
import datetime
import hashlib
import json
import subprocess
import sys
import uuid
import urllib.error
import urllib.request
from pathlib import Path

BASE = "http://127.0.0.1:8088"
HARNESS = "t_bb4d4d33"
APP_ENV = Path("/etc/swift/env/missedcall.env")
CRED_ENV = Path("/opt/swift/secure/cred.env")
VAULT_URL = "http://127.0.0.1:8100/vault/missedcall-mcp-giraudy"
AUDIT = Path("/opt/swift/data/missedcallrespondr/mcp-audit.log")
UA = "swift-mcp-write-acceptance/1.0"

FAILED = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ((" -- " + detail) if detail else ""))
    if not ok:
        FAILED.append(name)


def http(method, path, body=None, token=None, headers=None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", UA)
    if token:
        req.add_header("Authorization", "Bearer " + token)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read().decode(errors="replace")
            try:
                return r.status, (json.loads(raw) if raw else {})
            except json.JSONDecodeError:
                return r.status, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode(errors="replace")
        try:
            return e.code, (json.loads(raw) if raw else {})
        except json.JSONDecodeError:
            return e.code, raw
    except urllib.error.URLError as e:
        raise SystemExit(f"cannot reach {url}: {e}")


def _dsn():
    for line in APP_ENV.read_text().splitlines():
        if line.startswith("DATABASE_URL="):
            return line.split("=", 1)[1].strip().strip('"')
    raise SystemExit(f"no DATABASE_URL in {APP_ENV}")


def sql(query):
    r = subprocess.run(
        ["psql", _dsn(), "-tAF", "|", "-v", "ON_ERROR_STOP=1", "-c", query],
        capture_output=True, text=True, timeout=60,
    )
    if r.returncode != 0:
        raise SystemExit(f"SQL failed: {r.stderr.strip()[:400]}\n--- {query[:200]}")
    return [line.split("|") for line in r.stdout.splitlines() if line.strip()]


def scalar(query):
    rows = sql(query)
    return rows[0][0] if rows else None


def mcp_post(token, payload):
    return http("POST", "/mcp", payload, token=token)


def mcp_call(token, name, args):
    """tools/call -> (status, result_dict) where result_dict is structuredContent."""
    status, body = mcp_post(
        token,
        {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
         "params": {"name": name, "arguments": args}},
    )
    result = body.get("result") if isinstance(body, dict) else {}
    if isinstance(result, dict) and "structuredContent" in result:
        return status, result.get("structuredContent")
    return status, result


def list_tools(token):
    """tools/list paged to exhaustion -> {name: annotations}."""
    tools = {}
    cursor = None
    while True:
        params = {}
        if cursor:
            params["cursor"] = cursor
        status, body = mcp_post(
            token, {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": params}
        )
        if status != 200 or not isinstance(body, dict):
            return None, tools
        result = body.get("result") or {}
        for t in result.get("tools") or []:
            tools[t["name"]] = t.get("annotations") or {}
        cursor = result.get("nextCursor")
        if not cursor:
            return status, tools


def vault_ceo_key():
    token = None
    for line in CRED_ENV.read_text().splitlines():
        if line.startswith("CRED_TOKEN="):
            token = line.split("=", 1)[1].strip().strip('"')
    if not token:
        raise SystemExit("no CRED_TOKEN in /opt/swift/secure/cred.env")
    req = urllib.request.Request(VAULT_URL)
    req.add_header("Authorization", "Bearer " + token)
    req.add_header("User-Agent", UA)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            body = json.loads(r.read().decode())
    except Exception as e:
        raise SystemExit(f"vault read failed: {e}")
    key = (body or {}).get("password", "")
    if not key.startswith("mcr_"):
        raise SystemExit(f"vault entry has no mcr_ key in `password` ({key[:6]}...)")
    return key


def register_probe(tag):
    """One probe tenant through the REAL signup, marked X-Swift-Harness, plus a minted key."""
    email = f"probe-{tag}-{uuid.uuid4().hex[:10]}@example.invalid"
    status, body = http(
        "POST", "/api/v1/auth/register",
        {"email": email, "name": "Probe", "password": "probe-password-not-mailed"},
        headers={"X-Swift-Harness": HARNESS},
    )
    if status != 200 or not isinstance(body, dict):
        raise SystemExit(f"signup failed for {tag}: HTTP {status} {str(body)[:200]}")
    jwt = body.get("token")
    tm = body.get("team_member") or {}
    tenant_id = tm.get("account_id")
    user_id = tm.get("id")
    if not jwt or not tenant_id or not user_id:
        raise SystemExit(f"signup response missing token/team_member: {list(body)}")
    # Mint a labelled key through the REAL mint route (proves the signup JWT + mint path).
    s2, key_body = http("POST", "/api/v1/api-keys", {"name": f"probe-{HARNESS}-{tag}"}, token=jwt)
    if s2 != 201 or not isinstance(key_body, dict) or not key_body.get("token"):
        raise SystemExit(f"key mint failed for {tag}: HTTP {s2} {str(key_body)[:200]}")
    return {
        "tag": tag,
        "email": email,
        "tenant_id": tenant_id,
        "user_id": user_id,
        "jwt": jwt,
        "key": key_body["token"],
        "prefix": key_body.get("key_prefix", ""),
        "label": f"probe-{HARNESS}-{tag}",
    }


def insert_key_fixture(tenant_id, user_id, email, role, label):
    """A key with a KNOWN plaintext, inserted directly (the read-only key has no mint route)."""
    key_id = str(uuid.uuid4())
    raw = "mcr_" + __import__("secrets").token_hex(32)
    key_hash = hashlib.sha256(raw.encode()).hexdigest()
    prefix = raw[:12]
    sql(
        f"INSERT INTO users (id, email, password_hash, name, tenant_id, role) "
        f"VALUES ('{user_id}', '{email}', 'probe-placeholder', 'RO Probe', '{tenant_id}', '{role}');"
    )
    sql(
        f"INSERT INTO api_keys (id, tenant_id, name, key_prefix, key_hash, created_by) "
        f"VALUES ('{key_id}', '{tenant_id}', '{label}', '{prefix}', '{key_hash}', '{user_id}');"
    )
    return raw


def insert_call(tenant_id):
    call_id = str(uuid.uuid4())
    sql(
        f"INSERT INTO inbound_calls (id, caller_number, called_number, disposition, tenant_id) "
        f"VALUES ('{call_id}', '+15550001111', '+15550002222', 'missed', '{tenant_id}');"
    )
    return call_id


def insert_number(tenant_id, number):
    num_id = str(uuid.uuid4())
    sql(
        f"INSERT INTO phone_numbers (id, tenant_id, number, friendly_name, provider) "
        f"VALUES ('{num_id}', '{tenant_id}', '{number}', 'probe number', 'telnyx');"
    )
    return num_id


def write_tool_names():
    return {
        "missedcall_create_contact", "missedcall_update_contact", "missedcall_archive_contact",
        "missedcall_create_campaign", "missedcall_update_campaign", "missedcall_archive_campaign",
        "missedcall_create_rule", "missedcall_update_rule", "missedcall_archive_rule",
        "missedcall_create_followup", "missedcall_update_followup", "missedcall_archive_followup",
        "missedcall_create_suppression", "missedcall_remove_suppression",
        "missedcall_update_number", "missedcall_archive_number",
    }


def tool_result_error(payload):
    return payload.get("error") if isinstance(payload, dict) else None


def main():
    # ── 0. liveness ─────────────────────────────────────────────────────────────────────────────
    s, _ = http("GET", "/api/v1/health")
    check("0 app health is 200", s == 200, f"HTTP {s}")
    audit_before = 0
    if AUDIT.exists():
        audit_before = sum(1 for _ in open(AUDIT, encoding="utf-8", errors="replace"))

    # ── fixtures: two probe tenants via the real signup + a read-only key + the CEO key ────────
    a = register_probe("a")
    b = register_probe("b")
    ro_raw = insert_key_fixture(
        a["tenant_id"], str(uuid.uuid4()),
        f"probe-ro-{uuid.uuid4().hex[:8]}@example.invalid", "read_only", "probe-ro",
    )
    ceo_key = vault_ceo_key()
    print(f"probe tenant A {a['tenant_id']}  key prefix {a['prefix']}")
    print(f"probe tenant B {b['tenant_id']}  key prefix {b['prefix']}")

    # ── 1. tools/list counts and annotations ────────────────────────────────────────────────────
    s, tools_a = list_tools(a["key"])
    s, tools_b = list_tools(b["key"])
    s, tools_ro = list_tools(ro_raw)
    s, tools_ceo = list_tools(ceo_key)
    check("1a workspace key sees 25 tools", len(tools_a) == 25, f"{len(tools_a)}")
    check("1b every write tool present and read_only=false",
          all(n in tools_a and tools_a[n].get("readOnlyHint") is False for n in write_tool_names()),
          f"missing={sorted(write_tool_names() - set(tools_a))}")
    check("1c the 9 reads stay read_only=true",
          all(tools_a[n].get("readOnlyHint") is True for n in tools_a if n.startswith("missedcall_list_")),
          "")
    check("1d CEO key sees 25 tools", len(tools_ceo) == 25, f"{len(tools_ceo)}")

    # ── 2. read-only key sees no write tool and a direct call is refused ────────────────────────
    check("2a read-only key sees 9 read tools only",
          len(tools_ro) == 9 and not (set(tools_ro) & write_tool_names()),
          f"count={len(tools_ro)} writes={sorted(set(tools_ro) & write_tool_names())}")
    s, payload = mcp_call(ro_raw, "missedcall_create_contact", {"name": "x", "phone": "1"})
    check("2b read-only write call refused (forbidden)",
          tool_result_error(payload) == "forbidden", f"err={tool_result_error(payload)}")

    # ── 3. per-entity MCP create -> read-back -> update -> archive, foreign id not_found ────────
    # contact
    s, created = mcp_call(a["key"], "missedcall_create_contact",
                          {"name": f"probe {HARNESS}", "phone": "+15550003333", "tags": ["a", "b"]})
    cid = created.get("id") if isinstance(created, dict) else None
    check("3a.contact create returns id", s == 200 and bool(cid), f"{created}")
    s, readback = mcp_call(a["key"], "missedcall_list_contacts", {"id": cid})
    check("3a.contact read-back returns the row",
          s == 200 and isinstance(readback, dict) and readback.get("count") == 1, f"{readback}")
    s, _ = mcp_call(a["key"], "missedcall_update_contact", {"id": cid, "company": "ProbeCo"})
    check("3a.contact update ok", s == 200, f"HTTP {s}")
    s, _ = mcp_call(a["key"], "missedcall_archive_contact", {"id": cid})
    check("3a.contact archive ok", s == 200, f"HTTP {s}")
    s, gone = mcp_call(a["key"], "missedcall_list_contacts", {"id": cid})
    check("3a.contact archived row leaves the default list",
          tool_result_error(gone) == "not_found", f"{gone}")

    # campaign
    s, created = mcp_call(a["key"], "missedcall_create_campaign", {"name": f"probe {HARNESS}"})
    cid = created.get("id") if isinstance(created, dict) else None
    check("3b.campaign create returns id", s == 200 and bool(cid), f"{created}")
    s, readback = mcp_call(a["key"], "missedcall_list_campaigns", {"id": cid})
    check("3b.campaign read-back", readback.get("count") == 1, f"{readback}")
    mcp_call(a["key"], "missedcall_update_campaign", {"id": cid, "kind": "manual"})
    mcp_call(a["key"], "missedcall_archive_campaign", {"id": cid})
    s, readback = mcp_call(a["key"], "missedcall_list_campaigns", {"id": cid})
    item = (readback.get("items") or [{}])[0] if isinstance(readback, dict) else {}
    check("3b.campaign archived (is_active=false)",
          item.get("is_active") is False, f"{readback}")

    # rule
    s, created = mcp_call(a["key"], "missedcall_create_rule",
                          {"name": f"probe {HARNESS}", "trigger_condition": "all_missed_calls",
                           "response_type": "sms", "response_content": {"text": "hi"}})
    cid = created.get("id") if isinstance(created, dict) else None
    check("3c.rule create returns id", s == 200 and bool(cid), f"{created}")
    s, bad = mcp_call(a["key"], "missedcall_create_rule",
                      {"name": "dead", "trigger_condition": "banana", "response_type": "sms",
                       "response_content": {"text": "x"}})
    check("3c.rule invalid shape refused (same vocabulary as REST)",
          tool_result_error(bad) == "invalid", f"{bad}")
    s, readback = mcp_call(a["key"], "missedcall_list_rules", {"id": cid})
    check("3c.rule read-back", readback.get("count") == 1, f"{readback}")
    mcp_call(a["key"], "missedcall_update_rule", {"id": cid, "priority": 50})
    mcp_call(a["key"], "missedcall_archive_rule", {"id": cid})
    s, readback = mcp_call(a["key"], "missedcall_list_rules", {"id": cid})
    item = (readback.get("items") or [{}])[0] if isinstance(readback, dict) else {}
    check("3c.rule archived (is_active=false)", item.get("is_active") is False, f"{readback}")

    # follow-up (needs an inbound call in the same tenant)
    call_id = insert_call(a["tenant_id"])
    soon = (datetime.datetime.utcnow() + datetime.timedelta(days=1)).strftime("%Y-%m-%dT%H:%M:%S")
    s, created = mcp_call(a["key"], "missedcall_create_followup",
                          {"call_id": call_id, "follow_type": "call_back", "scheduled_at": soon})
    cid = created.get("id") if isinstance(created, dict) else None
    check("3d.followup create returns id", s == 200 and bool(cid), f"{created}")
    s, readback = mcp_call(a["key"], "missedcall_list_followups", {"id": cid})
    check("3d.followup read-back", readback.get("count") == 1, f"{readback}")
    mcp_call(a["key"], "missedcall_update_followup", {"id": cid, "status": "scheduled"})
    mcp_call(a["key"], "missedcall_archive_followup", {"id": cid})
    s, gone = mcp_call(a["key"], "missedcall_list_followups", {"id": cid})
    check("3d.followup archived row leaves the default list",
          tool_result_error(gone) == "not_found", f"{gone}")

    # suppression (create + hard remove)
    addr = f"probe-{uuid.uuid4().hex[:8]}@example.invalid"
    s, created = mcp_call(a["key"], "missedcall_create_suppression",
                          {"address": addr, "reason": "probe"})
    cid = created.get("id") if isinstance(created, dict) else None
    check("3e.suppression create returns id", s == 200 and bool(cid), f"{created}")
    s, readback = mcp_call(a["key"], "missedcall_list_suppressions", {"id": cid})
    check("3e.suppression read-back", readback.get("count") == 1, f"{readback}")
    s, refused = mcp_call(a["key"], "missedcall_remove_suppression", {"id": cid, "hard": False})
    check("3e.suppression remove without hard:true is refused",
          tool_result_error(refused) == "invalid", f"{refused}")
    s, removed = mcp_call(a["key"], "missedcall_remove_suppression", {"id": cid, "hard": True})
    check("3e.suppression hard remove ok", s == 200, f"{removed}")
    s, gone = mcp_call(a["key"], "missedcall_list_suppressions", {"id": cid})
    check("3e.suppression removed row is gone", tool_result_error(gone) == "not_found", f"{gone}")

    # number (inserted in DB, then MCP update + archive)
    num_id = insert_number(a["tenant_id"], f"+1555{uuid.uuid4().hex[:6]}")
    s, _ = mcp_call(a["key"], "missedcall_update_number", {"id": num_id, "friendly_name": "Probe line"})
    check("3f.number update ok", s == 200, f"HTTP {s}")
    mcp_call(a["key"], "missedcall_archive_number", {"id": num_id})
    s, readback = mcp_call(a["key"], "missedcall_list_numbers", {"id": num_id})
    item = (readback.get("items") or [{}])[0] if isinstance(readback, dict) else {}
    check("3f.number archived (is_active=false)", item.get("is_active") is False, f"{readback}")

    # foreign id -> not_found (a real row in tenant B, addressed with tenant A's key)
    s, b_contact = mcp_call(b["key"], "missedcall_create_contact", {"name": "B", "phone": "2"})
    b_cid = b_contact.get("id") if isinstance(b_contact, dict) else None
    s, f = mcp_call(a["key"], "missedcall_archive_contact", {"id": b_cid})
    check("3g.foreign contact id answers not_found", tool_result_error(f) == "not_found", f"{f}")
    s, b_camp = mcp_call(b["key"], "missedcall_create_campaign", {"name": "B-campaign"})
    b_camp_id = b_camp.get("id") if isinstance(b_camp, dict) else None
    s, f = mcp_call(a["key"], "missedcall_archive_campaign", {"id": b_camp_id})
    check("3g.foreign campaign id answers not_found", tool_result_error(f) == "not_found", f"{f}")

    # ── 4. REST parity with the mcr_ key ────────────────────────────────────────────────────────
    s, body = http("POST", "/api/v1/contacts", {"name": "REST", "phone": "+15550004444"}, token=a["key"])
    rest_contact = body.get("id") if isinstance(body, dict) else None
    check("4.contact POST 2xx + scoped create", s in (200, 201) and bool(rest_contact), f"HTTP {s}")
    s, _ = http("PUT", f"/api/v1/contacts/{rest_contact}", {"company": "RESTCo"}, token=a["key"])
    check("4.contact PUT 2xx", s == 200, f"HTTP {s}")
    s, _ = http("DELETE", f"/api/v1/contacts/{b_cid}", token=a["key"])
    check("4.contact foreign DELETE 404", s == 404, f"HTTP {s}")

    s, body = http("POST", "/api/v1/campaigns", {"name": "REST-campaign"}, token=a["key"])
    rest_camp = body.get("id") if isinstance(body, dict) else None
    check("4.campaign POST 2xx", s in (200, 201) and bool(rest_camp), f"HTTP {s}")
    s, _ = http("PUT", f"/api/v1/campaigns/{rest_camp}", {"kind": "manual"}, token=a["key"])
    check("4.campaign PUT 2xx", s == 200, f"HTTP {s}")
    s, _ = http("DELETE", f"/api/v1/campaigns/{b_camp_id}", token=a["key"])
    check("4.campaign foreign DELETE 404", s == 404, f"HTTP {s}")

    s, body = http("POST", "/api/v1/response-rules",
                   {"name": "REST-rule", "trigger_condition": "all_missed_calls",
                    "response_type": "sms", "response_content": {"text": "hi"}}, token=a["key"])
    rest_rule = body.get("id") if isinstance(body, dict) else None
    check("4.rule POST 2xx", s in (200, 201) and bool(rest_rule), f"HTTP {s}")
    s, _ = http("PUT", f"/api/v1/response-rules/{rest_rule}", {"priority": 60}, token=a["key"])
    check("4.rule PUT 2xx", s == 200, f"HTTP {s}")
    s, _ = http("DELETE", f"/api/v1/response-rules/{rest_rule}", token=a["key"])
    check("4.rule DELETE 2xx", s == 200, f"HTTP {s}")

    s, body = http("POST", "/api/v1/follow-ups",
                   {"call_id": call_id, "follow_type": "call_back", "scheduled_at": soon},
                   token=a["key"])
    rest_fu = body.get("id") if isinstance(body, dict) else None
    check("4.followup POST 2xx", s in (200, 201) and bool(rest_fu), f"HTTP {s}")
    s, _ = http("PUT", f"/api/v1/follow-ups/{rest_fu}", {"status": "scheduled"}, token=a["key"])
    check("4.followup PUT 2xx", s == 200, f"HTTP {s}")
    s, _ = http("DELETE", f"/api/v1/follow-ups/{rest_fu}", token=a["key"])
    check("4.followup DELETE 2xx", s == 200, f"HTTP {s}")

    s, body = http("POST", "/api/v1/settings/suppressions",
                   {"address": f"rest-{uuid.uuid4().hex[:8]}@example.invalid"}, token=a["key"])
    check("4.suppression POST 2xx", s == 200, f"HTTP {s}")
    # suppression REST delete of a foreign id (tenant B's row made above is already gone; use B's fresh one)
    s, b_sup = http("POST", "/api/v1/settings/suppressions",
                    {"address": f"b-{uuid.uuid4().hex[:8]}@example.invalid"}, token=b["key"])
    b_sup_id = None
    if s == 200:
        b_sup_id = scalar(
            f"SELECT id FROM email_suppressions WHERE tenant_id = '{b['tenant_id']}' "
            f"AND address LIKE 'b-%' ORDER BY created_at DESC LIMIT 1"
        )
    s, _ = http("DELETE", f"/api/v1/settings/suppressions/{b_sup_id}", token=a["key"])
    check("4.suppression foreign DELETE 404", s == 404 and b_sup_id, f"HTTP {s}")

    # number REST: DELETE (release) 2xx + foreign 404. No POST (purchase spends money).
    s, _ = http("DELETE", f"/api/v1/telnyx/numbers/{num_id}", token=a["key"])
    check("4.number DELETE (release) 2xx", s == 200, f"HTTP {s}")

    # rows stay scoped: tenant B's list never returns tenant A's REST contact
    s, b_list = http("GET", "/api/v1/contacts", token=b["key"])
    b_ids = [c.get("id") for c in b_list] if isinstance(b_list, list) else []
    check("4.scoped B's contact list has no A rows", rest_contact not in b_ids, "")

    # ── 5. platform-admin refusal ───────────────────────────────────────────────────────────────
    s, _ = http("GET", "/api/v1/admin/stats", token=a["key"])
    check("5a workspace key on platform-only route -> 403", s == 403, f"HTTP {s}")
    s, _ = http("GET", "/api/v1/admin/stats")
    check("5b anon on platform-only route -> 401", s == 401, f"HTTP {s}")

    # ── 6. audit: label + entity id, no full key material ───────────────────────────────────────
    lines = open(AUDIT, encoding="utf-8", errors="replace").read().splitlines()[audit_before:]
    check("6a audit wrote new lines for the writes", len(lines) > 0, f"{len(lines)} new lines")
    label_ok = any(("probe-" + HARNESS) in ln for ln in lines)
    check("6b audit lines carry the key label", label_ok, "")
    def audit_has_entity_id():
        for ln in lines:
            try:
                args = json.loads(ln).get("args", "")
                if '"id"' in args:
                    return True
            except Exception:
                pass
        return False
    check("6c audit lines carry an entity id", audit_has_entity_id(), "")
    joined = "\n".join(lines)
    check("6d no full key material in the audit",
          a["key"] not in joined and b["key"] not in joined and "mcr_" not in joined,
          f"mcr_={joined.count('mcr_')}")
    check("6e audit file stays 0600", oct(AUDIT.stat().st_mode & 0o777) == "0o600",
          oct(AUDIT.stat().st_mode & 0o777))

    # ── 7. real CEO key: create -> archive cycle on a probe-marked contact, then removed ────────
    s, created = mcp_call(ceo_key, "missedcall_create_contact",
                          {"name": f"probe_{HARNESS}", "phone": "+15550009999"})
    ceo_cid = created.get("id") if isinstance(created, dict) else None
    check("7a CEO key create_contact ok", s == 200 and bool(ceo_cid), f"{created}")
    mcp_call(ceo_key, "missedcall_archive_contact", {"id": ceo_cid})
    s, gone = mcp_call(ceo_key, "missedcall_list_contacts", {"id": ceo_cid})
    check("7b CEO-key contact archived", tool_result_error(gone) == "not_found", f"{gone}")
    if ceo_cid:
        sql(f"DELETE FROM contacts WHERE id = '{ceo_cid}' AND name = 'probe_{HARNESS}';")
    check("7c CEO-key probe contact removed",
          scalar(f"SELECT COUNT(*) FROM contacts WHERE id = '{ceo_cid}'") == "0", "")

    # ── 8. teardown + residue ───────────────────────────────────────────────────────────────────
    for p in (a, b):
        sql(
            f"DELETE FROM api_keys WHERE tenant_id = '{p['tenant_id']}';"
            f"DELETE FROM users WHERE tenant_id = '{p['tenant_id']}';"
            f"DELETE FROM tenants WHERE id = '{p['tenant_id']}';"
        )
    # probe residue: signup-marked tenants and any leftover probe rows
    residue = int(scalar(f"SELECT COUNT(*) FROM tenants WHERE probe_harness = '{HARNESS}'") or 0)
    leftover_contacts = int(scalar(f"SELECT COUNT(*) FROM contacts WHERE name LIKE 'probe%{HARNESS}%'") or 0)
    check("8a probe tenant residue == 0", residue == 0, f"{residue} left")
    check("8b probe contact residue == 0", leftover_contacts == 0, f"{leftover_contacts} left")

    print()
    if FAILED:
        print(f"{len(FAILED)} CHECK(S) FAILED: " + "; ".join(FAILED))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
