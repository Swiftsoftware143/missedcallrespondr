#!/usr/bin/env python3
"""Live acceptance — the fleet-standard mint surface `/api/personal-api-keys` (P1c) on
MissedCall Respondr, plus CoreSwift's multi-key rule.

docs/p1c-personal-api-keys-spec-2026-11-25.md, "Proof per app". Self-cleaning Python 3, stdlib
only. Fixtures are created DIRECTLY in the database (a live mint would otherwise consume the
app's one-key rule — here the SECOND mint is the whole point of the multi-key proof), each marked
`probe_harness = 't_29efde4c'` on the tenants row. The MCP door joins `users` on
`api_keys.created_by`, so each fixture key has a LIVE creator user.

Checks (one PASS/FAIL line each, exit 1 on any FAIL):
  1. GET  /api/personal-api-keys anonymous                         -> 401
  2. GET  /api/personal-api-keys with the fixture key              -> 200, lists the fixture prefix
  3. POST /api/personal-api-keys {name:"probe-mint"} with the key  -> 201, plaintext + prefix
     (this is the SECOND live key — proves multi-key)
  4. POST /mcp tools/list with the minted key                      -> 200, result.tools non-empty
  5. DELETE /api/personal-api-keys/<minted id> with the key        -> 200
  6. same tools/list with the revoked key                          -> 401
  7. DELETE a SECOND probe tenant's key id with the first key      -> 404
  8. reveal with an API-KEY credential                          -> 403 (session-only; plan §6.2 makes
     View one click for a SESSION, and a key may never reveal keys)
  9. teardown: probe residue == 0 for probe_harness='t_29efde4c'

Never prints a full key — prefixes only.

Usage: python3 scripts/personal-api-keys-acceptance.py [base_url]
"""
import hashlib
import json
import secrets
import subprocess
import sys
import uuid
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import unquote, urlparse

BASE = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8088").rstrip("/")
APP_ENV = Path("/etc/swift/env/missedcall.env")
HARNESS = "t_29efde4c"

FAILED = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ((" — " + detail) if detail else ""))
    if not ok:
        FAILED.append(name)


def http(method, path, body=None, token=None, api_key=None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", "swift-p1c-acceptance")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    if api_key:
        req.add_header("Authorization", "Bearer " + api_key)
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
    """One statement (or a `;`-joined batch) through the app's own Postgres, tuples only."""
    u = urlparse(_dsn())
    db = unquote(u.path).lstrip("/") or "missedcallrespondr"
    r = subprocess.run(
        ["docker", "exec", "-i", "-e", f"PGPASSWORD={unquote(u.password or '')}",
         "swift-postgres", "psql", "-U", unquote(u.username or "swift"), "-d", db,
         "-tAF", "|", "-v", "ON_ERROR_STOP=1", "-c", query],
        capture_output=True, text=True, timeout=60,
    )
    if r.returncode != 0:
        raise SystemExit(f"SQL failed: {r.stderr.strip()[:400]}\n--- {query[:200]}")
    return [line.split("|") for line in r.stdout.splitlines() if line.strip()]


def scalar(query):
    rows = sql(query)
    return rows[0][0] if rows else None


def mint_fixture(email):
    """One disposable tenant + live creator user + API key, all marked with the harness tag."""
    tenant_id = str(uuid.uuid4())
    user_id = str(uuid.uuid4())
    key_id = str(uuid.uuid4())
    raw = "mcr_" + secrets.token_hex(32)
    key_hash = hashlib.sha256(raw.encode()).hexdigest()
    prefix = raw[:12]
    slug = "p1c-probe-" + uuid.uuid4().hex[:8]
    sql(
        "INSERT INTO tenants (id, name, slug, probe_harness) "
        f"VALUES ('{tenant_id}', 'P1c Probe', '{slug}', '{HARNESS}');"
    )
    sql(
        "INSERT INTO users (id, email, password_hash, name, tenant_id, role) "
        f"VALUES ('{user_id}', '{email}', 'probe-placeholder', 'P1c Probe User', "
        f"'{tenant_id}', 'account_owner');"
    )
    sql(
        "INSERT INTO api_keys (id, tenant_id, name, key_prefix, key_hash, created_by) "
        f"VALUES ('{key_id}', '{tenant_id}', 'fixture', '{prefix}', '{key_hash}', '{user_id}');"
    )
    return {
        "tenant_id": tenant_id,
        "user_id": user_id,
        "key_id": key_id,
        "raw": raw,
        "prefix": prefix,
        "email": email,
    }


def mcp_call(key):
    return http("POST", "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/list"}, api_key=key)


def main():
    tag = uuid.uuid4().hex[:8]
    a = mint_fixture(f"p1c-a-{tag}@example.invalid")
    b = mint_fixture(f"p1c-b-{tag}@example.invalid")
    print(f"probe tenant A {a['tenant_id']}  key prefix {a['prefix']}")
    print(f"probe tenant B {b['tenant_id']}  key prefix {b['prefix']}")

    # ── 1. anonymous -> 401 ────────────────────────────────────────────────────────────────────
    s, _ = http("GET", "/api/personal-api-keys")
    check("1 GET anonymous -> 401", s == 401, f"HTTP {s}")

    # ── 2. fixture key lists the fixture's prefix ──────────────────────────────────────────────
    s, body = http("GET", "/api/personal-api-keys", api_key=a["raw"])
    listed = []
    if s == 200 and isinstance(body, dict):
        listed = [k.get("key_prefix") for k in (body.get("api_keys") or [])]
    check("2 GET with fixture key -> 200, lists the fixture prefix",
          s == 200 and a["prefix"] in listed,
          f"HTTP {s} prefixes={listed}")

    # ── 3. SECOND live key mints (proves multi-key) ────────────────────────────────────────────
    s, body = http("POST", "/api/personal-api-keys", {"name": "probe-mint"}, api_key=a["raw"])
    minted_id = None
    minted_token = None
    if isinstance(body, dict):
        minted_id = body.get("id")
        minted_token = body.get("token")
    check("3a POST mint -> 201 with a plaintext token and prefix",
          s == 201 and isinstance(minted_token, str) and minted_token.startswith("mcr_")
          and isinstance(body, dict) and isinstance(body.get("key_prefix"), str),
          f"HTTP {s} prefix={body.get('key_prefix') if isinstance(body, dict) else None}")
    check("3b it is the SECOND live key (multi-key, no one-key 409)",
          scalar(f"SELECT COUNT(*) FROM api_keys WHERE tenant_id = '{a['tenant_id']}' "
                 "AND revoked_at IS NULL") == "2")

    # ── 4. the minted key opens the MCP door ───────────────────────────────────────────────────
    s, body = mcp_call(minted_token) if minted_token else (None, {})
    tools = []
    if s == 200 and isinstance(body, dict) and isinstance(body.get("result"), dict):
        tools = body["result"].get("tools") or []
    check("4 tools/list with the minted key -> 200, tools non-empty",
          s == 200 and len(tools) > 0, f"HTTP {s} tools={len(tools)}")

    # ── 5. revoke the minted key ───────────────────────────────────────────────────────────────
    s, body = http("DELETE", f"/api/personal-api-keys/{minted_id}", api_key=a["raw"]) \
        if minted_id else (None, {})
    check("5 DELETE minted key -> 200", s == 200, f"HTTP {s} {str(body)[:120]}")

    # ── 6. the revoked key no longer opens the door ────────────────────────────────────────────
    s, _ = mcp_call(minted_token) if minted_token else (None, {})
    check("6 tools/list with the revoked key -> 401", s == 401, f"HTTP {s}")

    # ── 7. a foreign tenant's key id -> 404, never a cross-tenant revoke ───────────────────────
    s, _ = http("DELETE", f"/api/personal-api-keys/{b['key_id']}", api_key=a["raw"])
    check("7 DELETE a second tenant's key id with the first key -> 404", s == 404, f"HTTP {s}")

    # ── 8. a KEY cannot reveal keys: the reveal door is session-only (plan §6.2) ────────────────
    s, body = http("POST", f"/api/personal-api-keys/{a['key_id']}/reveal",
                   {}, api_key=a["raw"])
    check("8 reveal with an API key -> 403 (session-only)", s == 403, f"HTTP {s} {str(body)[:100]}")

    # ── 9. teardown + residue ──────────────────────────────────────────────────────────────────
    ids = [a, b]
    for p in ids:
        sql(f"DELETE FROM api_keys WHERE id = '{p['key_id']}';"
            f"DELETE FROM api_keys WHERE tenant_id = '{p['tenant_id']}';"
            f"DELETE FROM users WHERE id = '{p['user_id']}';"
            f"DELETE FROM tenants WHERE id = '{p['tenant_id']}';")
    residue = int(scalar(f"SELECT COUNT(*) FROM tenants WHERE probe_harness = '{HARNESS}'") or 0)
    check("9 probe residue == 0 for the harness tag", residue == 0, f"{residue} row(s) left")

    print()
    if FAILED:
        print(f"{len(FAILED)} CHECK(S) FAILED: " + ", ".join(FAILED))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
