#!/usr/bin/env bash
# IP-7 evidence ladder: fresh binaries from main, a fresh dev stack, the spec.
#   ladder.sh [build|boot|spec]...   (default: all three, in order)
set -euo pipefail
ROOT=/Users/rory/Documents/forge-run
E=$ROOT/forge-cli--step-ip7/.harness/evidence-ip7
DEV=$ROOT/forge-platform-operations/dev
E2E=$ROOT/forge-portal-e2e--step-ip7
STAGES="${*:-build boot spec}"

for s in $STAGES; do case "$s" in
build)
    echo "== build: every stack binary from main ($(date -u +%FT%TZ))"
    bash "$DEV/build-bins-worktree.sh" > "$E/build-bins.log" 2>&1 || { tail -30 "$E/build-bins.log"; exit 1; }
    grep -E '^\[(build|stage)\]' "$E/build-bins.log" | tail -30
    ;;
boot)
    echo "== boot: down, then up-full.sh --release ($(date -u +%FT%TZ))"
    # Node-wide configuration, set before the stack starts and never touched
    # again: where engine templates are cached, and a buildable forge-lang
    # worktree at main for a template cache miss (the fence refuses cargo in
    # the main checkout; CI has no fence and builds from ../forge-lang).
    export FORGE_ENGINE_TEMPLATE_DIR="$HOME/.cache/forge-engine-template-walk"
    SRC_ROOT="$ROOT/.harness/state/ip7-src-root"; rm -rf "$SRC_ROOT"; mkdir -p "$SRC_ROOT"
    for sib in "$ROOT"/forge-*; do
        n="$(basename "$sib")"
        case "$n" in forge-lang|*--*) continue ;; esac
        [ -d "$sib" ] && ln -sfn "$sib" "$SRC_ROOT/$n"
    done
    ln -sfn "$ROOT/forge-lang--ip7-engine" "$SRC_ROOT/forge-lang"
    export FORGE_DIALECT_SOURCE_ROOT="$SRC_ROOT"
    unset FORGE_GIT_SERVER_BUILD FORGE_GIT_SERVER_BUILD_ALLOW FORGE_GIT_INTERPRET
    "$DEV/down.sh" > "$E/down.log" 2>&1 || true
    "$DEV/up-full.sh" --release > "$E/up-full.log" 2>&1 || { tail -40 "$E/up-full.log"; exit 1; }
    tail -5 "$E/up-full.log"
    grep -q "portal converged + serving" "$E/up-full.log"
    ;;
spec)
    echo "== spec: customer-two.spec.ts ($(date -u +%FT%TZ))"
    {
        echo "forge-portal-e2e: $(git -C "$E2E" rev-parse --short HEAD) + $(git -C "$E2E" status --porcelain | wc -l | tr -d ' ') uncommitted"
        for r in forge-cli forge-runtime forge-control-plane forge-edge forge-agent forge-lang; do
            echo "$r main: $(git -C "$ROOT/$r" rev-parse --short main)"
        done
        echo "forge CLI: $ROOT/forge-cli/target/release/forge"
        date -u +%FT%TZ
    } > "$E/spec-run.log"
    cd "$E2E"
    FORGE_BIN="$ROOT/forge-cli/target/release/forge" \
        npx playwright test tests/lifecycle/customer-two.spec.ts --project=lifecycle \
        --reporter=list --workers=4 --retries=0 >> "$E/spec-run.log" 2>&1 || { tail -80 "$E/spec-run.log"; exit 1; }
    tail -20 "$E/spec-run.log"
    ;;
*) echo "unknown stage $s"; exit 2 ;;
esac; done
