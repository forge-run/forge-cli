#!/usr/bin/env python3
"""IP-4 dev-stack evidence: `forge export` as a non-admin user equals what
GET /api/v1/data/{table} answers that same identity.

Against the running dev stack (dev/up-full.sh). The customer workspace is
provisioned through the operator route walkthrough.sh uses (the control plane
mints the id), with the envelope's image_ref pointing at forge-runtime--step-ip4's
release build, because the main checkout's runtime predates the read pin.

Seed: `ip4_notes` carries an owner-scoped row_security policy and 250 rows, 130 of
them the user's, so the export walks three 50-row pages and RLS decides which
rows exist for this identity. `ip4_private` has no policy. The export runs
through the TLS edge as the user; every table the manifest exports is compared
row for row against one unpinned GET as the same user, after the export.
"""
import json, os, secrets, ssl, subprocess, sys, urllib.request, urllib.error

ROOT = "/Users/rory/Documents/forge-run"
DEV = f"{ROOT}/forge-platform-operations/dev"
RUN = f"{DEV}/run"
LANE_CLI = f"{ROOT}/forge-cli--step-ip4"
RT_BIN = f"{ROOT}/forge-runtime--step-ip4/target/release/forge-runtime"
OUT = f"{LANE_CLI}/.harness/reports/ip4-devstack"
SCRATCH = f"{ROOT}/.harness/state/ip4-devstack"
os.makedirs(SCRATCH, exist_ok=True)
EDGE = 8081
PORTAL_WS = "ws-30bb36de"
CTX = ssl._create_unverified_context()  # the dev edge's self-signed cert


def call(method, url, token=None, body=None, headers=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("content-type", "application/json")
    if token:
        req.add_header("authorization", f"Bearer {token}")
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=180, context=CTX) as r:
            raw = r.read()
            return r.status, dict(r.headers), (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, dict(e.headers), json.loads(raw)
        except Exception:
            return e.code, dict(e.headers), raw.decode(errors="replace")


def die(msg):
    print(f"[ip4] FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def login(base, email, pw):
    s, _, b = call("POST", f"{base}/api/v1/auth/login", body={"lookup_value": email, "password": pw})
    if s != 200 or not isinstance(b, dict) or not b.get("access_token"):
        die(f"login {email} at {base}: {s} {b}")
    return b["access_token"]


os.makedirs(OUT, exist_ok=True)
if not os.access(RT_BIN, os.X_OK):
    die(f"no lane runtime at {RT_BIN}")
ports = json.load(open(f"{RUN}/state/ports.json"))
ports = ports.get("assignments", ports) if isinstance(ports, dict) else ports
portal_port = ports.get(PORTAL_WS) if isinstance(ports, dict) else None
if isinstance(portal_port, dict):
    portal_port = portal_port.get("port")
if not portal_port:
    die(f"no port for {PORTAL_WS} in ports.json: {str(ports)[:300]}")
op_pw = open(f"{RUN}/keys/operator.password").read().strip()
op_tok = login(f"http://127.0.0.1:{portal_port}", "operator@forge.local", op_pw)

# ── provision (control-plane-minted id, lane runtime) ───────────────────────
admin_email, admin_pw = "owner@ip4.local", secrets.token_urlsafe(18)
portal = f"https://app.forge.run:{EDGE}"
s, _, created = call("POST", f"{portal}/api/v1/cp-admin/workspaces/provision-customer", op_tok, {
    "envelope": {
        "version": 1, "image_ref": RT_BIN,
        "resources": {"cpu_milli": 100, "memory_mb": 256, "max_pids": 256},
        "network": {"port": {"kind": "dynamic"}},
        "lifecycle": {"mode": "always_on", "idle_timeout_secs": 300, "wake_timeout_secs": 30},
        "generation": 1, "frozen": False,
    },
    "email": admin_email, "password": admin_pw, "role": "admin", "auto_kms": True,
}, {"origin": portal})
if s != 200 or not isinstance(created, dict) or not created.get("workspace_id"):
    die(f"provision-customer: {s} {str(created)[:400]}")
ws, ws_port = created["workspace_id"], created.get("assigned_port")
direct = f"http://127.0.0.1:{ws_port}"
edge = f"https://{ws}.forge.run:{EDGE}"
print(f"[ip4] workspace {ws} on :{ws_port} (minted), image_ref {RT_BIN}")
owner_tok = login(direct, admin_email, admin_pw)
json.dump({"workspace": ws, "port": ws_port, "owner": [admin_email, admin_pw]},
          open(f"{SCRATCH}/creds.json", "w"))  # outside the lane: never committed

# ── a non-admin user ────────────────────────────────────────────────────────
user_email, user_pw = "reader@ip4.local", "Rt7!wQz4$Km9Vp2c"
s, _, u = call("POST", f"{direct}/api/v1/manage/users", owner_tok,
               {"email": user_email, "password": user_pw, "role": "user"}, {"origin": direct})
if s not in (200, 201):
    die(f"manage/users: {s} {u}")
user_id = str(u["user_id"])
user_tok = login(direct, user_email, user_pw)
if user_tok.startswith(("fr_a_", "fr_p_")):
    die(f"the user's bearer is privileged: {user_tok[:5]}")

# ── schema + seed ───────────────────────────────────────────────────────────
notes = {
    "name": "ip4_notes", "archetype": "Base",
    "label": "Note", "plural_label": "Notes", "header_fields": ["body"],
    "columns": [
        {"name": "owner", "type": "string", "required": True},
        {"name": "body", "type": "string", "required": True},
    ],
    "row_security": [{"name": "own_rows",
                      "filter": {"column": "owner", "op": "eq", "value": "$user_id"}}],
}
private = {"name": "ip4_private", "archetype": "Base",
           "label": "Private", "plural_label": "Privates", "header_fields": ["body"],
           "columns": [{"name": "body", "type": "string", "required": True}]}
for schema in (notes, private):
    s, _, b = call("POST", f"{direct}/api/v1/manage/schema/apply", owner_tok, {"schema": schema},
                   {"origin": direct})
    if s != 200:
        die(f"schema apply {schema['name']}: {s} {b}")
# Grants: storage is deny-by-default, so a `user` reads a table only through a
# declared role. A role named `user` is implicitly assigned to every caller
# whose role claim is `user` (forge-runtime-auth validation.rs
# mint_permission_claim), so declare it with read on both tables.
s, _, b = call("POST", f"{direct}/api/v1/manage/wasm/deploy", owner_tok, {
    "services": [], "wasm_modules": [],
    "app_bundle": {"pages": [], "components": [], "roles": [
        {"name": "user", "capabilities": ["ip4_notes:read", "ip4_private:read"]}]},
}, {"origin": direct})
if s not in (200, 201):
    die(f"roles deploy: {s} {str(b)[:400]}")

# The router hot-swaps newly applied tables on its registry poll, so the data
# routes 404 for a few seconds after apply. Wait for both before seeding.
import time
for table in ("ip4_notes", "ip4_private"):
    for _ in range(60):
        s, _, b = call("GET", f"{direct}/api/v1/data/{table}?limit=1", owner_tok)
        if s != 404:
            break
        time.sleep(1)
    else:
        die(f"{table}: no data route 60s after schema apply")
for i in range(250):
    owner = user_id if i % 25 < 13 else "someone-else"
    s, _, b = call("POST", f"{direct}/api/v1/data/ip4_notes", owner_tok,
                   {"owner": owner, "body": f"note {i}"}, {"origin": direct})
    if s not in (200, 201):
        die(f"seed ip4_notes {i}: {s} {b}")
for i in range(3):
    s, _, b = call("POST", f"{direct}/api/v1/data/ip4_private", owner_tok, {"body": f"p{i}"},
                   {"origin": direct})
    if s not in (200, 201):
        die(f"seed ip4_private {i}: {s} {b}")

# The role grant is resolved per cache-miss validate; wait for the user's read.
for _ in range(60):
    s, _, b = call("GET", f"{direct}/api/v1/data/ip4_notes?limit=1", user_tok)
    if s == 200:
        break
    time.sleep(1)
else:
    die(f"the user still cannot read ip4_notes 60s after the role deploy: {s} {b}")

# ── the customer's config tree: the two tables, committed ──────────────────
tree = f"{SCRATCH}/config-tree"  # its own git repo, so outside the lane
subprocess.run(["rm", "-rf", tree], check=True)
os.makedirs(f"{tree}/domains/ip4/schemas")
for schema in (notes, private):
    json.dump(schema, open(f"{tree}/domains/ip4/schemas/{schema['name']}.table.json", "w"), indent=2)
for cmd in (["init", "-q"], ["add", "."], ["-c", "user.name=ip4", "-c", "user.email=ip4@local",
                                           "commit", "-qm", "ip4 config tree"]):
    subprocess.run(["git", "-C", tree, *cmd], check=True)

# ── export as the user, through the edge ────────────────────────────────────
# The edge routes a workspace once the CP's routing table marks it healthy,
# which trails provisioning by a few polls.
for _ in range(90):
    s, _, b = call("GET", f"{edge}/api/v1/data/ip4_notes?limit=1", user_tok)
    if s == 200:
        break
    time.sleep(1)
else:
    die(f"the edge still does not route {ws} 90s after provisioning: {s} {str(b)[:200]}")
export_dir = f"{OUT}/export"
subprocess.run(["rm", "-rf", export_dir], check=True)
env = dict(os.environ, FORGE_INSECURE_TLS="1", FORGE_BASE_URL=edge, FORGE_TOKEN=user_tok)
r = subprocess.run(["cargo", "run", "-q", "--package", "forge-cli", "--locked", "--bin", "forge", "--",
                    "export", "--out", export_dir, "--manifest-dir", tree, "--page-size", "50"],
                   cwd=LANE_CLI, env=env, capture_output=True, text=True)
open(f"{OUT}/export.stderr", "w").write(r.stderr)
print(r.stderr.strip())
if r.returncode != 0:
    die(f"forge export exited {r.returncode}")
manifest = json.load(open(f"{export_dir}/manifest.json"))

# ── compare: every exported table vs GET as the same identity ───────────────
results = []
for t in manifest["tables"]:
    if not t["exported"]:
        continue
    rows = [json.loads(l) for l in open(f"{export_dir}/{t['file']}")]
    s, h, got = call("GET", f"{edge}/api/v1/data/{t['name']}?limit=1000", user_tok)
    if s != 200 or not isinstance(got, list):
        results.append({"table": t["name"], "equal": False, "get_status": s})
        continue
    key = lambda r: json.dumps(r, sort_keys=True)
    results.append({
        "table": t["name"], "export_rows": len(rows), "get_rows": len(got),
        "equal": sorted(map(key, rows)) == sorted(map(key, got)),
        "manifest_rows": t["rows"], "note": t.get("note"),
        "get_x_forge_rls": h.get("x-forge-rls") or h.get("X-Forge-Rls"),
    })
notes_rows = [json.loads(l) for l in open(f"{export_dir}/ip4_notes.jsonl")]
summary = {
    "workspace": ws, "image_ref": RT_BIN, "edge": edge, "identity": user_email,
    "bearer_prefix": user_tok[:5], "as_of": manifest["as_of"],
    "config_tree": manifest["config_tree"], "tables_from": manifest["tables_from"],
    "ip4_notes_owned_by_caller": sum(r.get("owner") == user_id for r in notes_rows),
    "ip4_notes_seeded_total": 250, "ip4_notes_seeded_for_caller": 130,
    "tables": results,
    "not_exported": [{"name": t["name"], "note": t.get("note")} for t in manifest["tables"] if not t["exported"]],
}
json.dump(summary, open(f"{OUT}/summary.json", "w"), indent=2)
print(json.dumps(summary, indent=2))
bad = [x for x in results if not x["equal"]]
if bad or summary["ip4_notes_owned_by_caller"] != 130 or len(notes_rows) != 130:
    die(f"export != GET for {[x['table'] for x in bad]} or RLS scoping wrong")
print("[ip4] PASS: every exported table equals GET /api/v1/data/{table} for the same identity")
