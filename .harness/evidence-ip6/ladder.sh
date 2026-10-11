#!/usr/bin/env bash
set -e
E=/Users/rory/Documents/forge-run/forge-cli--step-ip6/.harness/evidence-ip6
L=/Users/rory/Documents/forge-run/forge-control-plane--step-ip6
OPS=/Users/rory/Documents/forge-run/forge-platform-operations
E2E=/Users/rory/Documents/forge-run/forge-portal-e2e
RUN=$OPS/dev/run
cd "$L"
echo "== fmt"; cargo fmt -- --check
echo "== fail-before: the test against the pre-fix source"
git apply -R "$E/fix.patch"
set +e
.harness/cargo cargo test --lib --locked the_index_root_sits_beside -- --test-threads=1 > "$E/test-before.log" 2>&1
echo "test-before rc=$?" | tee -a "$E/test-before.log"
set -e
git apply "$E/fix.patch"
echo "== after: the test and every references test"
.harness/cargo cargo test --lib --locked references -- --test-threads=1 > "$E/test-after.log" 2>&1
tail -5 "$E/test-after.log"
echo "== spec BEFORE (running forge-git = main $(git -C /Users/rory/Documents/forge-run/forge-control-plane rev-parse --short HEAD))"
export FORGE_E2E_SIGNUP_BOOTSTRAP=1 FORGE_E2E_DEPLOY_SERVE=1
cd "$E2E"
{ echo "forge-portal-e2e HEAD: $(git rev-parse HEAD)"; date -u +%FT%TZ; } > "$E/spec-before.log"
set +e
npx playwright test tests/lifecycle/field-references.spec.ts --project=lifecycle --reporter=list --workers=1 --retries=0 >> "$E/spec-before.log" 2>&1
echo "spec-before rc=$?" | tee -a "$E/spec-before.log"
set -e
echo "== release build of forge-git in the lane"
cd "$L"
.harness/cargo cargo build --release --locked --bin forge-git > "$E/build.log" 2>&1
BIN=$(ls -t "$L"/target/release/forge-git 2>/dev/null | head -1)
test -x "$BIN"
echo "== restart forge-git on $BIN"
export FORGE_OTEL_UDS_DIR=/tmp/fdev-474938501/otel FORGE_AUTH_RATE_LIMIT_MAX=10000 FORGE_CP_URL=http://127.0.0.1:8000 \
  FORGE_CP_ADMIN_DIR=$RUN/cp-admin FORGE_DEV_CLI_BIN=/Users/rory/Documents/forge-run/forge-cli--step-ip3/target/release/forge \
  FORGE_PORTAL_BASE_URL=https://auth.forge.run:8081 FORGE_PROVISION_IMAGE_REF=/Users/rory/Documents/forge-run/forge-runtime/target/release/forge-runtime \
  FORGE_OBS_LAKE_DIR=$RUN/obs-lake FORGE_OTEL_GATEWAY_ENDPOINT=http://127.0.0.1:8090 \
  FORGE_SESSION_SHARING_ORIGINS=auth.forge.run,app.forge.run,code.forge.run FORGE_LOG_STDERR=1 \
  FORGE_TRUSTED_ORIGINS=forge.run,app.forge.run,auth.forge.run
bash "$OPS/dev/forge-git-start.sh" "$RUN" "$BIN" 8010 8081 /tmp/fdev-474938501/secrets.sock
echo "== spec AFTER"
cd "$E2E"
{ echo "forge-portal-e2e HEAD: $(git rev-parse HEAD)"; echo "forge-git: $BIN (lane forge-control-plane--step-ip6 on $(git -C "$L" rev-parse --short HEAD) + working tree)"; date -u +%FT%TZ; } > "$E/spec-after.log"
npx playwright test tests/lifecycle/field-references.spec.ts --project=lifecycle --reporter=list --workers=1 --retries=0 >> "$E/spec-after.log" 2>&1
echo "== git.log references lines since restart"
grep "references:" "$RUN/logs/git.log" | tail -6 > "$E/git-log-references.txt" || true
ls "$RUN/cp/references" >> "$E/git-log-references.txt" 2>&1 || true
tail -15 "$E/spec-after.log"
