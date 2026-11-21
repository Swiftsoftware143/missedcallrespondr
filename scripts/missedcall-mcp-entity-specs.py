#!/usr/bin/env python3
"""Regenerate MissedCall Respondr's MCP page entity list from the REAL MCP service source.

WHY: the console's "What it can read" list was typed by hand into www-app/dashboard/index.html.
Every entity added to /opt/swift/mcp/src/main.rs therefore made that page quietly WRONG — the
hand-written list named 7 entities while the service exposes 16 for app "missedcallrespondr", so a
customer reading it saw less than they could actually read.

The AppSpec array is the source of truth. This reads it and rewrites the JS array in place; nothing
is invented and nothing is guessed. Run it after any coverage change, then publish the console.

Usage:  python3 missedcall-mcp-entity-specs.py [--check]
        --check exits 1 when the page and the source disagree (for a gate).
"""
import re
import sys

SRC = "/opt/swift/mcp/src/main.rs"
PAGE = "/opt/swift/apps/missedcallrespondr/www-app/dashboard/index.html"
BEGIN = "const MCP_ENTITIES = ["
END = "];"

# Human-facing label overrides, one entry. The AppSpec label for the plan subscription row is the
# table name `tenant_plans`; a customer reads that entity as "plans", so it is presented that way.
# The KEY still comes from the source and is never overridden.
LABELS = {"missedcallrespondr_plans": "plans"}

# One-line description per entity, derived from each AppSpec's `sel` column list (the columns the
# <key>_search tool actually returns). Keyed by the source key so a drift is a missing entry, never
# a silent empty line.
DESC = {
    "missedcallrespondr": "Leads: name, email, phone, status",
    "missedcallrespondr_clients": "Clients: name, email, phone, source",
    "missedcallrespondr_leads": "Leads: name, email, phone, source, status",
    "missedcallrespondr_deals": "Deals: name, value, stage, source",
    "missedcallrespondr_inbound_calls": "Inbound calls: caller name, caller number, disposition",
    "missedcallrespondr_call_logs": "Call logs: disposition, caller number, called number, notes",
    "missedcallrespondr_follow_ups": "Follow-ups: type, status, notes",
    "missedcallrespondr_campaigns": "Campaigns: name, kind, status",
    "missedcallrespondr_calendar_events": "Calendar events: title, type, description",
    "missedcallrespondr_affiliates": "Affiliates: code, commission rate, active",
    "missedcallrespondr_contacts": "Contacts: name, email, phone, company",
    "missedcallrespondr_tickets": "Tickets: subject, requester email, priority, status",
    "missedcallrespondr_ticket_messages": "Ticket messages: sender, ticket subject, body, received time",
    "missedcallrespondr_workspaces": "Workspaces: name, default flag",
    "missedcallrespondr_activity": "Activity: action, entity type, entity id",
    "missedcallrespondr_plans": "Plans: status, plan id, billing cycle",
}


def entities():
    """Every ``app: "missedcallrespondr"`` AppSpec, as (key, label, desc)."""
    text = open(SRC, encoding="utf-8").read()
    out = []
    for block in re.findall(r"AppSpec \{(.*?)\n    \},", text, re.S):
        if 'app: "missedcallrespondr"' not in block:
            continue
        key = re.search(r'key: "([^"]+)"', block)
        label = re.search(r'label: "([^"]+)"', block)
        if key and label:
            k = key.group(1)
            lab = LABELS.get(k, label.group(1))
            desc = DESC.get(k, "")
            if not desc:
                print("FAIL no description for key %s" % k)
                return []
            out.append((k, lab, desc))
    return out


def render(pairs):
    lines = [BEGIN]
    lines.append("  // GENERATED from /opt/swift/mcp/src/main.rs by")
    lines.append("  // scripts/missedcall-mcp-entity-specs.py — do not edit by hand.")
    for key, label, desc in pairs:
        lines.append("  { key: '%s', label: '%s', desc: '%s' }," % (
            key, label.replace("'", "\\'"), desc.replace("'", "\\'")))
    lines.append(END)
    return "\n".join(lines)


def main():
    check = "--check" in sys.argv
    pairs = entities()
    if not pairs:
        print("FAIL no missedcallrespondr AppSpec entries found in %s" % SRC)
        return 2
    page = open(PAGE, encoding="utf-8").read()
    start = page.find(BEGIN)
    if start < 0:
        print("FAIL could not find MCP_ENTITIES in the page")
        return 2
    end = page.find(END, start) + len(END)
    current = page[start:end]
    wanted = render(pairs)

    if check:
        have_keys = re.findall(r"key: '([^']+)'", current)
        want_keys = [p[0] for p in pairs]
        if have_keys != want_keys:
            print("FAIL page keys disagree with the service source")
            print("  page lists (%d): %s" % (len(have_keys), have_keys))
            print("  source has (%d): %s" % (len(want_keys), want_keys))
            return 1
        print("OK page lists all %d MissedCall Respondr entities, keys match the source" % len(pairs))
        return 0

    if current == wanted:
        print("no change: page already lists %d entities" % len(pairs))
        return 0
    open(PAGE, "w", encoding="utf-8").write(page[:start] + wanted + page[end:])
    print("rewrote the entity list: %d entities" % len(pairs))
    return 0


if __name__ == "__main__":
    sys.exit(main())
