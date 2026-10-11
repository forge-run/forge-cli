# Brief — IP-2-every-dialect-serves-on-push (plan invoice-path-2026-10)

Working root: /Users/rory/Documents/forge-run (flat siblings, not a git repo).
Intent record (read it): forge-cli--step-ip2/.harness/intent/invoice-path-2026-10-ip-2-every-dialect-serves-on-push.json

## Lanes (work ONLY here; never edit the main checkouts)
- forge-cli:           /Users/rory/Documents/forge-run/forge-cli--step-ip2
- forge-control-plane: /Users/rory/Documents/forge-run/forge-control-plane--step-ip2
Build through each lane's wrapper: `<lane>/.harness/cargo cargo <subcommand>`.
Use /forge-build for the exact per-repo test command; don't improvise flags.

## Defect
The interpreted converge fires only for trees with `.forge-interpret` at the
root (forge-control-plane src/git/reconcile/interp.rs:51-64; push.rs:137).
`write_dialect_tree` in forge-cli src/cmd/new.rs:490-493 pushes the marker only
`if lang == Lang::Rust`. An unmarked Python/TypeScript/Java tree with no
forge.lock stops at push.rs:~847 ("no committed forge.lock; desired-state not
updated") — it pushes and never converges, though
templates/workspace-dialect/README.md:31 promises `git push forge main` serves.

## Changes
1. forge-cli src/cmd/new.rs: write `.forge-interpret` for EVERY dialect in
   `write_dialect_tree` (drop the Lang::Rust condition). Rewrite the comment
   above it: the marker is the dialect tree's opt-in to the interpreted route
   for all four dialects; for Rust it is also the ruling that `.rs` sources are
   dialect, not raw wasm (dialect.rs). Match the file's comment density.
2. forge-cli test: a test in new.rs's test module that scaffolds via
   `write_dialect_tree` for [Python, TypeScript, Java, Rust] and asserts
   `dir.join(".forge-interpret").is_file()` for each, AND that the marker is in
   `git ls-files` after `git add -A` (it must be pushed, not ignored). Model it
   on `a_dialect_scaffold_emits_the_generated_skill_and_an_admitted_starter`
   (~line 1053) and its `git` helper; a separate named test, not an assertion
   buried in that one.
   PROVE IT FAILS FIRST: run the new test against the unfixed code, capture the
   failure output naming the three non-Rust dialects (assert per-lang with the
   lang in the message, collecting failures so all three show, or run one
   case per lang). Then fix, then green. Paste both outputs.
3. forge-control-plane src/git/reconcile/interp.rs module doc lines 14-18:
   delete the claim that the route "still runs behind the same server-build
   gate". Replace with what is true: since 6d666cd (PS-2) the interpreted route
   is open to every workspace; the only switch on it is FORGE_GIT_INTERPRET,
   a node-wide kill switch. VERIFY that against the code (grep
   FORGE_GIT_INTERPRET and the allowlist in push.rs / build gate) before
   writing it; the forge-control-plane--step-ip1 lane's interp.rs:15 has a
   draft wording — read it, don't copy blindly. Doc-only change; it must
   still build (`cargo doc` not required; `cargo check` is).
4. Fleet search: grep forge-cli (templates/, src/, README, CLAUDE.md) for any
   other text saying only Rust dialect trees get the marker or that non-Rust
   dialects need a forge.lock/build; fix what is now false. Report what you
   found even if nothing.

## Dev-stack repro (acceptance 3)
Write `forge-cli--step-ip2/scripts/repro-ip2-python-push.sh`, a scripted repro:
bring up the dev stack (forge-platform-operations/dev/up-full.sh — read
dev/README.md and walkthrough.sh first for how a workspace is provisioned and
how forge-cli is pointed at the local stack), build forge-cli from THIS lane,
`forge new --dialect python <name>`, provision/select a workspace, `forge push`,
wait for converge, then curl the starter op through the edge and assert a 2xx
with the expected body. No operator action between provisioning and the served
op. Run it; save its full output to
`forge-cli--step-ip2/.harness/evidence/ip2-dev-stack-repro.log`.
If the dev stack itself cannot come up for a reason outside this step, STOP and
report the exact failure — do not patch the dev stack.

## Long jobs
Anything over ~30s (cargo test, up-full.sh, the repro) starts detached with the
sh -c 'D=/Users/rory/Documents/forge-run/.harness/state/sdk-bg-e3449462-bcba-4486-baa1-259dcb69e56a/<NAME>; ...' line
from the harness prompt, and you wait with
`python3 /Users/rory/Documents/forge-run/.harness/sdk-driver.py wait <D>`
(exit 75 = still running, call again). Never sleep-loop in one call.

## Rules
- Lint waivers `#[expect(..., reason=...)]` only, never `#[allow]`.
- Commit in each lane (message ends with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`); do not push or merge.
- Do not touch forge-runtime, forge-lang or the dev stack scripts.

## Report back
Per file changed, with the diff summary; the failing-then-passing test output
(pasted); the cargo test result per lane; the repro log path and its last 40
lines; anything you could not do and why.
