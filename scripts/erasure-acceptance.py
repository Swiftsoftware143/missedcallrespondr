#!/usr/bin/env python3
"""Live acceptance — fleet account-closure/erasure standard §6, on MissedCall Respondr.

docs/account-closure-erasure-standard-2026-11-24.md §6 says every app must prove, against the
RUNNING app:

  1. POST close-account with a valid API key   -> 401, rows intact
  2. POST erase-data with a valid API key      -> 401, rows intact
  3. POST erase-data with no credential        -> 401
  4. erase with a session but WRONG password   -> 401, rows intact
  5. erase with a session, right password, WRONG word -> 400, rows intact
  6. successful erase -> rows gone, billing documents kept, one erasure audit row,
     old session 401, old key 401
  7. a teammate (second user, same root, non-owner role) -> 403 (the ownership refusal)

The static legs (route mounted/private, handler carries every lock, no fleet tool can erase) are
pinned as unit tests in `src/auth/route_policy.rs`; this script proves the behavioural legs against
the live binary, then cleans up after itself.

What it creates: ONE disposable workspace through the app's own signup door (the `X-Swift-Harness`
header marks `tenants.probe_harness`), a second teammate user for the ownership refusal, and reads
the auto-minted API key back through the reveal door.

Usage: python3 scripts/erasure-acceptance.py [base_url]
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

FAILED = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ((" — " + detail) if detail else ""))
    if not ok:
        FAILED.append(name)


def http(method, path, body=None, token=None, api_key=None, extra_headers=None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
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
                return r.status, json.loads(raw) if raw else {}
            except json.JSONDecodeError:
                return r.status, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode(errors="replace")
        try:
            return e.code, json.loads(raw) if raw else {}
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


def count(table, tenant_id):
    return int(scalar(f"SELECT COUNT(*) FROM {table} WHERE tenant_id = '{tenant_id}'") or 0)


def register_probe(tag):
    email = f"probe-erasure-{tag}@example.invalid"
    password = "ProbeErase!2026" + tag[-4:]
    status, body = http("POST", "/api/v1/auth/register", {
        "name": "Erasure Probe",
        "email": email,
        "password": password,
        "account_name": f"Erasure Probe {tag}",
    }, extra_headers={"X-Swift-Harness": "erasure-acceptance"})
    if status not in (200, 201):
        raise SystemExit(f"probe signup failed: {status} {body}")
    body = body if isinstance(body, dict) else {}
    tm = body.get("team_member") or {}
    return {
        "email": email,
        "password": password,
        "token": body["token"],
        # TeamMemberResponse renames tenant_id -> account_id on the wire (config.rs).
        "tenant_id": tm.get("account_id"),
        "user_id": tm.get("id"),
    }


def read_api_key(p):
    """The signup auto-mints ONE key ('default'); read its plaintext back through the reveal door."""
    s, b = http("GET", "/api/v1/api-keys", token=p["token"])
    keys = (b or {}).get("api_keys") or [] if s == 200 else []
    if not keys:
        raise SystemExit(f"no API key listed for the probe: HTTP {s} {b}")
    key_id = keys[0]["id"]
    s, b = http("POST", f"/api/v1/api-keys/{key_id}/reveal",
                {"password": p["password"]}, token=p["token"])
    if s != 200:
        raise SystemExit(f"API key reveal failed: {s} {b}")
    return b["token"]


def snapshot(p):
    return {
        "tenants": int(scalar(f"SELECT COUNT(*) FROM tenants WHERE id = '{p['tenant_id']}'") or 0),
        "users": count("users", p["tenant_id"]),
        "api_keys": count("api_keys", p["tenant_id"]),
        "tenant_plans": count("tenant_plans", p["tenant_id"]),
    }


def total_tenants():
    return int(scalar("SELECT COUNT(*) FROM tenants") or 0)


def main():
    tag = time.strftime("%Y%m%d-%H%M%S")
    probe = register_probe(tag)
    print(f"probe workspace {probe['tenant_id']} ({probe['email']})")
    probe["api_key"] = read_api_key(probe)
    before = snapshot(probe)
    tenants_before = total_tenants()
    print("before:", before, "| tenants_total:", tenants_before)

    # ── 1./2. a VALID issued API key, both doors -> 401, rows intact ────────────────────────────
    s, b = http("POST", "/api/v1/auth/close-account",
                {"password": probe["password"], "confirm": "CLOSE"}, api_key=probe["api_key"])
    check("1 close-account with a valid API key -> 401", s == 401, f"HTTP {s} {str(b)[:120]}")
    s, b = http("POST", "/api/v1/auth/erase-data",
                {"password": probe["password"], "confirm": "DELETE"}, api_key=probe["api_key"])
    check("2a erase-data with a valid API key -> 401", s == 401, f"HTTP {s} {str(b)[:120]}")
    check("2b refused by key: rows untouched", snapshot(probe) == before, str(snapshot(probe)))

    # ── 3. no credential -> 401 (the boundary) ──────────────────────────────────────────────────
    s, b = http("POST", "/api/v1/auth/erase-data",
                {"password": probe["password"], "confirm": "DELETE"})
    check("3 no credential on erase -> 401", s == 401, f"HTTP {s} {str(b)[:120]}")

    # ── 4. wrong password -> 401, rows intact ───────────────────────────────────────────────────
    s, b = http("POST", "/api/v1/auth/erase-data",
                {"password": "definitely-not-the-password", "confirm": "DELETE"}, token=probe["token"])
    check("4a wrong password on erase -> 401", s == 401, f"HTTP {s} {str(b)[:120]}")
    check("4b ...and every probe row still exists", snapshot(probe) == before, str(snapshot(probe)))
    check("4c ...and no erasure audit row was written",
          scalar(f"SELECT COUNT(*) FROM account_erasures WHERE erased_tenant_id = '{probe['tenant_id']}'") == "0")

    # ── 5. right password, wrong typed word -> 400, rows intact ─────────────────────────────────
    s, b = http("POST", "/api/v1/auth/erase-data",
                {"password": probe["password"], "confirm": "delete everything"}, token=probe["token"])
    check("5a wrong typed word on erase -> 400", s == 400, f"HTTP {s} {str(b)[:120]}")
    check("5b ...and every probe row still exists", snapshot(probe) == before, str(snapshot(probe)))

    # ── 7. a TEAMMATE (second user, same root, non-owner role) -> 403 (ownership refusal) ──────
    teammate_email = f"probe-erasure-teammate-{tag}@example.invalid"
    sql(
        "INSERT INTO users (id, email, password_hash, name, tenant_id, role, created_at, updated_at, password_undelivered) "
        "SELECT gen_random_uuid(), " + f"'{teammate_email}', password_hash, 'Erasure Teammate', tenant_id, 'team_member', NOW(), NOW(), false "
        f"FROM users WHERE tenant_id = '{probe['tenant_id']}' AND role = 'account_owner' LIMIT 1;"
    )
    s, lb = http("POST", "/api/v1/auth/login", {"email": teammate_email, "password": probe["password"]})
    check("7a teammate can sign in (seeded, shares the owner's hash)", s == 200, f"HTTP {s}")
    tm_token = (lb or {}).get("token") if s == 200 else None
    s, b = http("POST", "/api/v1/auth/erase-data",
                {"password": probe["password"], "confirm": "DELETE"}, token=tm_token)
    check("7b teammate erase -> 403 (ownership refusal, not a blanket ban)", s == 403, f"HTTP {s} {str(b)[:120]}")
    check("7c ...and every probe row still exists", snapshot(probe)["users"] == before["users"] + 1,
          str(snapshot(probe)))

    # ── 6. the real thing, as the OWNER ─────────────────────────────────────────────────────────
    s, b = http("POST", "/api/v1/auth/erase-data",
                {"password": probe["password"], "confirm": "DELETE"}, token=probe["token"])
    check("6a erase with session + password + DELETE -> 200", s == 200, f"HTTP {s} {str(b)[:200]}")
    bd = b if isinstance(b, dict) else {}
    if s == 200:
        rec = bd.get("records") or {}
        check("6b reports status=account_erased and billing retention >= 1",
              bd.get("status") == "account_erased" and int(rec.get("billing_documents_retained") or 0) >= 1,
              json.dumps({k: rec.get(k) for k in ("rows_deleted", "tables_touched", "billing_documents_retained")}))
    check("6c tenant row is gone",
          scalar(f"SELECT COUNT(*) FROM tenants WHERE id = '{probe['tenant_id']}'") == "0")
    check("6d users/api_keys/tenant_plans rows are gone",
          all(snapshot(probe)[k] == 0 for k in ("users", "api_keys", "tenant_plans")), str(snapshot(probe)))
    kept = int(scalar(
        f"SELECT COUNT(*) FROM erasure_billing_retention WHERE erased_tenant_id = '{probe['tenant_id']}'") or 0)
    check("6e billing documents kept (retention rows present)", kept >= 1, f"kept {kept}")
    audit = sql(f"SELECT actor_email, action, records::text FROM account_erasures "
                f"WHERE erased_tenant_id = '{probe['tenant_id']}'")
    check("6f exactly ONE erasure audit row", len(audit) == 1, f"{len(audit)} row(s)")
    if len(audit) == 1:
        check("6g audit names the actor and the action",
              audit[0][0] == probe["email"] and audit[0][1] == "erase_data", str(audit[0][:2]))
        check("6h audit carries no password or token",
              probe["password"] not in audit[0][2] and probe["token"] not in audit[0][2])
    s, _ = http("GET", "/api/v1/auth/me", token=probe["token"])
    check("6i the erased session is dead (old token -> 401)", s == 401, f"HTTP {s}")
    s, _ = http("GET", "/api/v1/auth/me", api_key=probe["api_key"])
    check("6j the erased API key is dead (old key -> 401)", s == 401, f"HTTP {s}")

    # ── cleanup: the retention and audit rows are the only survivors; take them too ─────────────
    tid = probe["tenant_id"]
    sql(f"DELETE FROM erasure_billing_retention WHERE erased_tenant_id = '{tid}';"
        f"DELETE FROM account_erasures WHERE erased_tenant_id = '{tid}';")
    residue = int(scalar(f"SELECT COUNT(*) FROM tenants WHERE id = '{tid}'") or 0) + int(
        scalar(f"SELECT COUNT(*) FROM account_erasures WHERE erased_tenant_id = '{tid}'") or 0) + int(
        scalar(f"SELECT COUNT(*) FROM erasure_billing_retention WHERE erased_tenant_id = '{tid}'") or 0)
    check("8 probe residue cleaned", residue == 0, f"{residue} row(s) left")
    tenants_after = total_tenants()
    print(f"tenants_total before={tenants_before} after={tenants_after}")

    print()
    if FAILED:
        print(f"{len(FAILED)} CHECK(S) FAILED: " + ", ".join(FAILED))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
