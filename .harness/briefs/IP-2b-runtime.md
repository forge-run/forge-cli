# Brief — IP-2 (part b): an app-less bundle deploys (forge-runtime)

Working root: /Users/rory/Documents/forge-run (flat siblings, not a git repo).
Plan invoice-path-2026-10, step IP-2-every-dialect-serves-on-push (read its
`notes` in forge-cli--step-ip2/.harness/roadmap-invoice-path.json).

## Lane (work ONLY here)
/Users/rory/Documents/forge-run/forge-runtime--step-ip2
Build via `<lane>/.harness/cargo cargo <subcommand>`; the exact command set is
in /forge-build and forge-runtime/CLAUDE.md:13-24 (`--workspace --tests
--locked --features adapters`). Toolchain pinned 1.95.0.

## Defect (confirmed)
Since forge-control-plane a818087 (R-8, "an API-only workspace can deploy"),
the CP OMITS `app_manifest` from the app bundle when the tree declares no app
(forge-control-plane/src/git/workspace_derive.rs:670-682, 775-779): a missing
key means "nothing declared". R-8 checked the runtime's trigger reconcilers
(reconcile.rs:494, :804) and config.rs:120, but NOT the deploy path:
- crates/forge-adapters/src/http/manage_routes/deploy/ingest.rs:645-647
  `validate_app_bundle` → `bad_request("app_bundle missing app_manifest")`
- same file :87-89 `ingest_app_bundle` → `ingest_fatal(...)`
So every app-less tree (every `forge new --dialect` scaffold) converges on the
CP and is refused by the runtime: "converge FAILED CLOSED ... app_bundle
validation failed" (run.rs:57 discards the detail).

## Change
Make an ABSENT `app_manifest` mean "nothing declared" in both functions, as
R-8's contract says. A PRESENT but undecodable or invalid one stays an error,
exactly as now. Pages/components/etc. checks still run.
- In `ingest_app_bundle`, read every use of `app_manifest` (:87-150, :590-620)
  and decide per use what "no app declared" means: branding overrides none,
  authored surface config none, app name absent in the returned summary /
  logs. Choose the reading that matches what an app with nothing declared
  would register; if any use has NO obvious "nothing declared" reading (e.g.
  it would silently clear state a previous deploy registered and you cannot
  tell whether that is right), STOP and report it rather than guess.
- Keep `validate_app_bundle` and `ingest_app_bundle` in lock-step (the doc
  comment on validate requires it).
- Check run.rs:265 and :534-540 also tolerate absence (read; fix only if they
  don't).
- If an `Option` ripples into a returned struct that crosses a process
  boundary (forge-platform-wire), STOP and report — that is a wire change.
- Fix run.rs:57 so the failure reason carries the validator's message, not
  only "app_bundle validation failed", IF it is a one-line change; otherwise
  leave it and say so.

## Tests (fail first)
Add tests beside the existing ones (run.rs:1285/1402 show bundle fixtures):
1. a bundle with NO `app_manifest` key passes `validate_app_bundle` and
   `ingest_app_bundle` (or the deploy entry point the existing tests drive);
   prove it FAILS on the unfixed code and paste that output;
2. control: a bundle whose `app_manifest` is present but invalid is still
   refused (so this doesn't open a hole).
Then the CI sequence: fmt --all --check, clippy (the CI flags) -D warnings,
the test suite for the forge-adapters crate at minimum, and the workspace
`cargo test` if it fits — say which you ran.

## Long jobs
Start detached with the sh -c 'D=/Users/rory/Documents/forge-run/.harness/state/sdk-bg-e3449462-bcba-4486-baa1-259dcb69e56a/<NAME>; ...' line
(same as the harness prompt; NAME unique, e.g. ip2b-test), wait with
`python3 /Users/rory/Documents/forge-run/.harness/sdk-driver.py wait <D>`
(exit 75 = still running, call again). Never sleep-loop in one call.

## Rules
- `#[expect(..., reason=...)]` only, never `#[allow]`.
- One commit in the lane, message ends with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; no push/merge.
- Touch nothing outside the lane. Do NOT run the dev stack — the orchestrator
  re-runs forge-cli--step-ip2/scripts/repro-ip2-python-push.sh afterwards.

## Report back
Files changed with the per-use decision for each `app_manifest` read; red
then green test output (pasted); which cargo commands ran and their result
lines; anything you stopped on.
