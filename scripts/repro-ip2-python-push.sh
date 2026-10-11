#!/usr/bin/env bash
# scripts/repro-ip2-python-push.sh — invoice-path IP-2 acceptance 3, on the dev
# stack: a Python workspace made by `forge new --dialect python` and pushed with
# `forge push` converges and its starter op answers through the edge.
#
#   scripts/repro-ip2-python-push.sh            # boots the stack, leaves it up
#
# Stages, in order; the first failure exits nonzero with the stage named:
#
#   build      forge-cli from THIS checkout (the scaffold under test).
#   boot       forge-platform-operations/dev/up-full.sh --release, with the
#              node-wide engine-template settings exported first (the same
#              ones dev/walkthrough.sh uses). NO server-build kill switch and
#              NO per-workspace allowlist: the interpreted converge must need
#              neither.
#   provision  a fresh control-plane-minted workspace and a pusher user, the
#              way dev/walkthrough.sh provisions one.
#   scaffold   `forge new --dialect python`, then the forge remote.
#   push       `forge push` — blocks until the workspace reports the pushed
#              sha converged, or fails.
#   serve      the starter op through the TLS edge on the workspace's own
#              host: 2xx, and exactly the body the starter returns.
#
# Between provision and serve nothing but the customer's two commands runs.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LANE="$(cd "$HERE/.." && pwd)"
ROOT="$(cd "$LANE/.." && pwd)"
DEV="$ROOT/forge-platform-operations/dev"
RUN="${FORGE_DEV_DIR:-$DEV/run}"
EDGE_PORT=8081
GIT_PORT=8010
PORTAL_WS="ws-30bb36de"
WORK="$LANE/target/ip2-repro"
NAME="ip2py"

log() { echo "[ip2] $*"; }
STAGE=""
stage() { STAGE="$1"; log "── $1"; }
fail() { echo "[ip2] FAILED at stage '$STAGE': $*" >&2; exit 1; }

port_of() {
    python3 - "$RUN/state/ports.json" "$1" <<'PY' 2>/dev/null
import json, sys
print(json.load(open(sys.argv[1])).get("assignments", {}).get(sys.argv[2], ""))
PY
}
login() { # <port> <email> <password> → access token or ""
    curl -sS -m 10 -X POST "http://127.0.0.1:$1/api/v1/auth/login" \
        -H 'content-type: application/json' \
        -d "{\"lookup_value\":\"$2\",\"password\":\"$3\"}" 2>/dev/null \
        | python3 -c "import sys,json;print(json.load(sys.stdin).get('access_token',''))" 2>/dev/null || true
}

stage build
(cd "$LANE" && cargo build --locked --bin forge) || fail "forge-cli build"
FORGE="$LANE/target/debug/forge"
[ -x "$FORGE" ] || fail "no forge binary at $FORGE"
log "   forge-cli: $FORGE ($(git -C "$LANE" rev-parse --short HEAD), worktree $(git -C "$LANE" status --porcelain -- src | wc -l | tr -d ' ') dirty src files)"

stage boot
# Node-wide configuration, set before the stack starts and never touched again:
# where engine templates are cached and which forge-lang they are built from.
export FORGE_ENGINE_TEMPLATE_DIR="${FORGE_ENGINE_TEMPLATE_DIR:-$HOME/.cache/forge-engine-template-walk}"
if [ -z "${FORGE_DIALECT_SOURCE_ROOT:-}" ]; then
    LANG_HEAD="$(git -C "$ROOT/forge-lang" rev-parse HEAD)"
    LANGWT="$(git -C "$ROOT/forge-lang" worktree list --porcelain \
        | awk -v h="$LANG_HEAD" '/^worktree /{wt=$2} /^HEAD /{if ($2 == h && wt ~ /\/forge-lang--/) print wt}' \
        | head -1)"
    # No worktree at main's HEAD: any clean forge-lang worktree whose tree
    # differs from main only in harness files builds the same engine (the
    # template key hashes the engine inputs), and the fence refuses cargo in
    # the main checkout.
    if [ -z "$LANGWT" ]; then
        for wt in "$ROOT"/forge-lang--*; do
            [ -d "$wt" ] && [ -z "$(git -C "$wt" status --porcelain 2>/dev/null)" ] || continue
            git -C "$wt" diff --quiet HEAD "$LANG_HEAD" -- . ':!.harness' ':!.github' 2>/dev/null \
                && { LANGWT="$wt"; break; }
        done
    fi
    if [ -n "$LANGWT" ]; then
        SRC_ROOT="$WORK/src-root"; rm -rf "$SRC_ROOT"; mkdir -p "$SRC_ROOT"
        for sib in "$ROOT"/forge-*; do
            n="$(basename "$sib")"
            case "$n" in forge-lang|*--*) continue ;; esac
            [ -d "$sib" ] && ln -sfn "$sib" "$SRC_ROOT/$n"
        done
        ln -sfn "$LANGWT" "$SRC_ROOT/forge-lang"
        export FORGE_DIALECT_SOURCE_ROOT="$SRC_ROOT"
    else
        export FORGE_DIALECT_SOURCE_ROOT="$ROOT"
    fi
fi
log "   FORGE_ENGINE_TEMPLATE_DIR=$FORGE_ENGINE_TEMPLATE_DIR FORGE_DIALECT_SOURCE_ROOT=$FORGE_DIALECT_SOURCE_ROOT"
unset FORGE_GIT_SERVER_BUILD FORGE_GIT_SERVER_BUILD_ALLOW FORGE_GIT_INTERPRET
mkdir -p "$WORK"
"$DEV/down.sh" >/dev/null 2>&1 || true
if ! "$DEV/up-full.sh" --release > "$WORK/boot.log" 2>&1; then
    tail -n 40 "$WORK/boot.log" >&2
    fail "up-full.sh failed (log: $WORK/boot.log)"
fi
tail -n 5 "$WORK/boot.log"

stage provision
PORTAL_PORT="$(port_of "$PORTAL_WS")"
[ -n "$PORTAL_PORT" ] || fail "no port for $PORTAL_WS"
OP_TOK="$(login "$PORTAL_PORT" operator@forge.local "$(cat "$RUN/keys/operator.password")")"
[ -n "$OP_TOK" ] || fail "operator login failed"
RT_BIN="${FORGE_DEV_RT_BIN:-$ROOT/forge-runtime/target/release/forge-runtime}"
PUSH_EMAIL="pusher@forge.local"
PUSH_PW="$(python3 -c "import secrets;print(secrets.token_urlsafe(18))")"
CREATE="$(curl -sS -m 180 -X POST \
    --resolve "app.forge.run:$EDGE_PORT:127.0.0.1" -k \
    "https://app.forge.run:$EDGE_PORT/api/v1/cp-admin/workspaces/provision-customer" \
    -H "Authorization: Bearer $OP_TOK" -H 'content-type: application/json' \
    -H "origin: https://app.forge.run:$EDGE_PORT" \
    -d "{\"envelope\":{\"version\":1,\"image_ref\":\"$RT_BIN\",
          \"resources\":{\"cpu_milli\":100,\"memory_mb\":256,\"max_pids\":256},
          \"network\":{\"port\":{\"kind\":\"dynamic\"}},
          \"lifecycle\":{\"mode\":\"always_on\",\"idle_timeout_secs\":300,\"wake_timeout_secs\":30},
          \"generation\":1,\"frozen\":false},
         \"email\":\"$PUSH_EMAIL\",\"password\":\"$PUSH_PW\",\"role\":\"admin\",
         \"auto_kms\":true}" 2>/dev/null || echo '{}')"
read -r WS WS_PORT <<< "$(printf '%s' "$CREATE" | python3 -c "import sys,json;d=json.load(sys.stdin);print(d.get('workspace_id',''),d.get('assigned_port',''))" 2>/dev/null || true)"
[ -n "${WS:-}" ] && [ -n "${WS_PORT:-}" ] || fail "provision-customer: $(printf '%s' "$CREATE" | cut -c1-400)"
PUSH_TOK="$(login "$WS_PORT" "$PUSH_EMAIL" "$PUSH_PW")"
[ -n "$PUSH_TOK" ] || fail "pusher login failed on :$WS_PORT"
for _ in $(seq 1 30); do
    ME="$(curl -sS -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $PUSH_TOK" \
        --resolve "$WS.forge.run:$EDGE_PORT:127.0.0.1" -k \
        "https://$WS.forge.run:$EDGE_PORT/api/v1/auth/me" 2>/dev/null || echo 000)"
    [ "$ME" = "200" ] && break
    sleep 1
done
[ "$ME" = "200" ] || fail "$WS never routable through the edge (last $ME)"
log "   workspace $WS on :$WS_PORT, routable through the edge"

stage scaffold
rm -rf "$WORK/$NAME"
export GIT_AUTHOR_NAME=ip2 GIT_AUTHOR_EMAIL=ip2@forge.local GIT_COMMITTER_NAME=ip2 GIT_COMMITTER_EMAIL=ip2@forge.local
(cd "$WORK" && "$FORGE" new --template workspace --dialect python "$NAME") || fail "forge new"
TREE="$WORK/$NAME"
[ -f "$TREE/.forge-interpret" ] || fail "the scaffold carries no .forge-interpret"
git -C "$TREE" ls-files | grep -qx '.forge-interpret' || fail ".forge-interpret is not committed"
[ -e "$TREE/forge.lock" ] && fail "the scaffold carries a forge.lock — this repro is about trees without one"
DOMAIN="$(basename "$(ls -d "$TREE"/domains/*/ | head -1)")"
log "   tree $TREE, domain $DOMAIN, files:"
git -C "$TREE" ls-files | sed 's/^/       /'
git -C "$TREE" remote add forge "http://127.0.0.1:$GIT_PORT/$WS/$NAME"

stage push
GITLOG_BEFORE="$(wc -l < "$RUN/logs/git.log" | tr -d ' ')"
(cd "$TREE" && "$FORGE" --base-url "http://127.0.0.1:$WS_PORT" --token "$PUSH_TOK" push --timeout 600)
PUSH_RC=$?
log "   forge-git log for this push:"
tail -n +"$((GITLOG_BEFORE + 1))" "$RUN/logs/git.log" | grep -v '^\s*$' | cut -c1-300 | tail -n 25 | sed 's/^/       /'
[ "$PUSH_RC" -eq 0 ] || fail "forge push exited $PUSH_RC"

stage serve
URL="https://$WS.forge.run:$EDGE_PORT/api/domains/$DOMAIN/$DOMAIN/v1/hello"
WANT="{\"message\":\"hello, ip2, from $DOMAIN\"}"
GOT="$(curl -sS -m 30 -w $'\nCODE=%{http_code}' -H "Authorization: Bearer $PUSH_TOK" \
    --resolve "$WS.forge.run:$EDGE_PORT:127.0.0.1" -k "$URL?name=ip2" 2>&1)"
CODE="$(printf '%s\n' "$GOT" | sed -n 's/^CODE=//p')"
BODY="$(printf '%s\n' "$GOT" | sed '/^CODE=/d')"
if [ "${CODE:-000}" = "405" ] || [ "${CODE:-000}" = "404" ]; then
    log "   GET → $CODE; trying POST"
    GOT="$(curl -sS -m 30 -w $'\nCODE=%{http_code}' -X POST -H "Authorization: Bearer $PUSH_TOK" \
        -H 'content-type: application/json' -d '{"name":"ip2"}' \
        --resolve "$WS.forge.run:$EDGE_PORT:127.0.0.1" -k "$URL" 2>&1)"
    CODE="$(printf '%s\n' "$GOT" | sed -n 's/^CODE=//p')"
    BODY="$(printf '%s\n' "$GOT" | sed '/^CODE=/d')"
fi
log "   $URL → $CODE $BODY"
case "$CODE" in 2??) ;; *) fail "the op answered $CODE, not 2xx" ;; esac
GOT="$BODY" WANT="$WANT" python3 -c 'import json,os,sys;sys.exit(0 if json.loads(os.environ["GOT"])==json.loads(os.environ["WANT"]) else 1)' \
    || fail "body $BODY is not $WANT"
log "PASS: $WS served $DOMAIN::hello from a forge-new Python tree after one forge push"
