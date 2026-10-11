#!/usr/bin/env bash
set -e
E=/Users/rory/Documents/forge-run/forge-cli--step-ip6/.harness/evidence-ip6
L=/Users/rory/Documents/forge-run/forge-control-plane--step-ip6
OPS=/Users/rory/Documents/forge-run/forge-platform-operations
E2E=/Users/rory/Documents/forge-run/forge-portal-e2e
RUN=$OPS/dev/run
OLD=/Users/rory/Documents/forge-run/forge-control-plane/target/release/forge-git
NEW=$L/target/release/forge-git
export FORGE_OTEL_UDS_DIR=/tmp/fdev-474938501/otel FORGE_AUTH_RATE_LIMIT_MAX=10000 FORGE_CP_URL=http://127.0.0.1:8000 \
  FORGE_CP_ADMIN_DIR=$RUN/cp-admin FORGE_DEV_CLI_BIN=/Users/rory/Documents/forge-run/forge-cli--step-ip3/target/release/forge \
  FORGE_PORTAL_BASE_URL=https://auth.forge.run:8081 FORGE_PROVISION_IMAGE_REF=/Users/rory/Documents/forge-run/forge-runtime/target/release/forge-runtime \
  FORGE_OBS_LAKE_DIR=$RUN/obs-lake FORGE_OTEL_GATEWAY_ENDPOINT=http://127.0.0.1:8090 \
  FORGE_SESSION_SHARING_ORIGINS=auth.forge.run,app.forge.run,code.forge.run FORGE_LOG_STDERR=1 \
  FORGE_TRUSTED_ORIGINS=forge.run,app.forge.run,auth.forge.run
export FORGE_E2E_SIGNUP_BOOTSTRAP=1 FORGE_E2E_DEPLOY_SERVE=1
spec() { # <log> <bin> <label>
  { echo "forge-portal-e2e HEAD: $(git -C "$E2E" rev-parse HEAD)"; echo "forge-git: $2 ($3)"; date -u +%FT%TZ; } > "$1"
  (cd "$E2E" && npx playwright test tests/lifecycle/field-references.spec.ts --project=lifecycle --reporter=list --workers=1 --retries=0) >> "$1" 2>&1
}
echo "== npm ci"
cd "$E2E"
npm ci --no-audit --no-fund > "$E/npm-ci.log" 2>&1
echo "== BEFORE: forge-git from main $(git -C /Users/rory/Documents/forge-run/forge-control-plane rev-parse --short HEAD)"
bash "$OPS/dev/forge-git-start.sh" "$RUN" "$OLD" 8010 8081 /tmp/fdev-474938501/secrets.sock
set +e
spec "$E/spec-before.log" "$OLD" "main checkout, pre-fix"
echo "spec-before rc=$?" | tee -a "$E/spec-before.log"
set -e
echo "== AFTER: forge-git from the lane $(git -C "$L" rev-parse --short HEAD)"
bash "$OPS/dev/forge-git-start.sh" "$RUN" "$NEW" 8010 8081 /tmp/fdev-474938501/secrets.sock
spec "$E/spec-after.log" "$NEW" "lane forge-control-plane--step-ip6 $(git -C "$L" rev-parse --short HEAD)"
tail -8 "$E/spec-after.log"
grep "references:" "$RUN/logs/git.log" | tail -6 > "$E/git-log-references.txt" || true
ls -R "$RUN/cp/references" >> "$E/git-log-references.txt" 2>&1 || true
echo "== clippy in the lane"
cd "$L"
.harness/cargo cargo clippy --tests --locked -- -D warnings > "$E/clippy.log" 2>&1
tail -3 "$E/clippy.log"
