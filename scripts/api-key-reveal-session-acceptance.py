#!/usr/bin/env python3
"""Live acceptance — the key UI ruling (plan §6.2) on MissedCall Respondr.

docs/fleet-per-account-keys-and-mcp-2026-11-25.md §6.2 (David, 2026-11-25): the key is private by
default (masked on load) and **View is ONE CLICK** — a valid session is enough, no password
re-prompt — while every reveal is still audited. David's own words: "they should be able to see the
key ... a click to view ... and also a copy button".

The card that raised this (t_a5864179) also fixes the security half: this app's middleware accepts
an API key as well as a session, so `reveal` classifies the credential and refuses a KEY with 403.
Otherwise a read-only key could reveal the account's read+write key.

Checks (one PASS/FAIL line each, exit 1 on any FAIL):
  1. signup probe A mints one key; LIST with the session shows the prefix only (no plaintext)
  2. POST reveal with the SESSION and NO BODY AT ALL            -> 200 + the full key
  3. the legacy body {"password": "…"} still answers            -> 200 (old clients keep working)
  4. every reveal wrote an activity_log row (>= 2 by now)
  5. reveal with NO credential                                  -> 401
  6. reveal with the account's API KEY as bearer                -> 403 (session-only)
  7. probe B's key id with probe A's session                    -> 404 (never another workspace)
  8. the revealed key actually authenticates (GET lists keys)    -> 200
  9. revoke the key, then reveal it with the session             -> 404 (revoked is gone, not shown)
 10. teardown: both probe workspaces erased, residue 0

Never prints a full key — prefixes only.

Usage: python3 scripts/api-key-reveal-session-acceptance.py [base_url]
"""
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import unquote, urlparse

BASE = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8088").rstrip("/")
APP_ENV = Path("/etc/swift/env/missedcall.env")
HARNESS = "t_a5864179"

FAILED = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ((" — " + detail) if detail else ""))
    if not ok:
        FAILED.append(name)


def http(method, path, body=None, token=None, api_key=None, extra_headers=None, raw_body=False):
    url = BASE + path
    if body is None:
        data = b"" if raw_body else None
    else:
        data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", "swift-key-view-acceptance")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    if api_key:
        req.add_header("Authorization", "Bearer " + api_key)
    for k, v in (extra_headers or {}).items():
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


def register_probe(tag, letter):
    email = f"probe-keyview-{letter}-{tag}@example.invalid"
    password = "ProbeKeyView!2026" + tag[-4:]
    s, b = http("POST", "/api/v1/auth/register", {
        "name": f"KeyView Probe {letter}",
        "email": email,
        "password": password,
        "account_name": f"KeyView Probe {letter} {tag}",
    }, extra_headers={"X-Swift-Harness": HARNESS})
    if s not in (200, 201):
        raise SystemExit(f"probe signup failed: {s} {b}")
    b = b if isinstance(b, dict) else {}
    tm = b.get("team_member") or {}
    return {
        "email": email,
        "password": password,
        "token": b["token"],
        "tenant_id": tm.get("account_id"),
        "user_id": tm.get("id"),
    }


def list_keys(p):
    s, b = http("GET", "/api/v1/api-keys", token=p["token"])
    keys = (b or {}).get("api_keys") or [] if s == 200 else []
    return s, keys


def erase(p):
    """The app's own erasure door is the supported teardown; residue rows are deleted after."""
    s, _ = http("POST", "/api/v1/auth/erase-data",
                {"password": p["password"], "confirm": "DELETE"}, token=p["token"])
    return s


def main():
    tag = time.strftime("%Y%m%d-%H%M%S")
    a = register_probe(tag, "a")
    b = register_probe(tag, "b")
    print(f"probe A {a['tenant_id']}  probe B {b['tenant_id']}")

    s, keys_a = list_keys(a)
    key = keys_a[0] if keys_a and isinstance(keys_a[0], dict) else None
    check("1 signup minted ONE key and the list is prefix-only",
          s == 200 and len(keys_a) == 1 and "token" not in (key or {}) and key.get("key_prefix"),
          f"HTTP {s} keys={len(keys_a)}")

    # ── 2. one click: session + NO BODY ─────────────────────────────────────────────────────────
    s, body = http("POST", f"/api/v1/api-keys/{key['id']}/reveal", token=a["token"], raw_body=True)
    full = body.get("token") if isinstance(body, dict) else None
    check("2 reveal with the session and no body -> 200 + the full key",
          s == 200 and isinstance(full, str) and full.startswith("mcr_"),
          f"HTTP {s} prefix={body.get('key_prefix') if isinstance(body, dict) else None}")

    # ── 3. old clients that still send a password keep working ─────────────────────────────────
    s, body = http("POST", f"/api/v1/api-keys/{key['id']}/reveal",
                   {"password": "ignored-legacy-field"}, token=a["token"])
    check("3 legacy {\"password\":…} body still answers 200", s == 200, f"HTTP {s}")

    # ── 4. the audit row ──────────────────────────────────────────────────────────────────────
    n = int(scalar("SELECT COUNT(*) FROM activity_log WHERE action = 'api_key_revealed' "
                   f"AND entity_id = '{key['id']}'") or 0)
    check("4 every reveal wrote an activity_log row", n >= 2, f"{n} row(s)")

    # ── 5. anonymous ───────────────────────────────────────────────────────────────────────────
    s, _ = http("POST", f"/api/v1/api-keys/{key['id']}/reveal", raw_body=True)
    check("5 reveal with no credential -> 401", s == 401, f"HTTP {s}")

    # ── 6. an API KEY cannot view keys ─────────────────────────────────────────────────────────
    s, body = http("POST", f"/api/v1/api-keys/{key['id']}/reveal", api_key=full, raw_body=True)
    check("6 reveal with the account's API key -> 403 (session-only)",
          s == 403, f"HTTP {s} {str(body)[:120]}")

    # ── 7. another workspace's key id is not found ──────────────────────────────────────────────
    _, keys_b = list_keys(b)
    foreign = keys_b[0]["id"] if keys_b else None
    s, _ = http("POST", f"/api/v1/api-keys/{foreign}/reveal", token=a["token"], raw_body=True)
    check("7 probe B's key id with probe A's session -> 404", s == 404, f"HTTP {s}")

    # ── 8. the value revealed is a working credential ──────────────────────────────────────────
    s, _ = http("GET", "/api/v1/api-keys", api_key=full)
    check("8 the revealed key authenticates (GET keys -> 200)", s == 200, f"HTTP {s}")

    # ── 9. a revoked key is gone, not shown ────────────────────────────────────────────────────
    s, _ = http("DELETE", f"/api/v1/api-keys/{key['id']}", token=a["token"])
    s2, _ = http("POST", f"/api/v1/api-keys/{key['id']}/reveal", token=a["token"], raw_body=True)
    check("9 revoke the key, then reveal -> 404", s == 200 and s2 == 404, f"revoke {s} reveal {s2}")

    # ── 10. teardown ───────────────────────────────────────────────────────────────────────────
    ea, eb = erase(a), erase(b)
    for p in (a, b):
        tid = p["tenant_id"]
        sql(f"DELETE FROM erasure_billing_retention WHERE erased_tenant_id = '{tid}';"
            f"DELETE FROM account_erasures WHERE erased_tenant_id = '{tid}';")
    residue = int(scalar(f"SELECT COUNT(*) FROM tenants WHERE id IN ('{a['tenant_id']}','{b['tenant_id']}')") or 0)
    check("10 both probe workspaces erased, residue 0",
          ea == 200 and eb == 200 and residue == 0, f"erase a={ea} b={eb} residue={residue}")

    print()
    if FAILED:
        print(f"{len(FAILED)} CHECK(S) FAILED: " + ", ".join(FAILED))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
